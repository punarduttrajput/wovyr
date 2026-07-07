//! The agent run loop.
//!
//! Implements the core loop from the
//! [Agent Runtime spec §14](../../docs/03-workflow-engine/agent-runtime.md):
//! the model is called; if it requests tools, they are executed and their results
//! fed back; this repeats until the model returns a final answer or a step budget
//! is exhausted.

use crate::definition::AgentDefinition;
use crate::events::{RunEvent, RunEventSink};
use crate::memory::{ContextRetriever, RetrievedContext};
use apex_common::{Error, Result, Usage};
use apex_provider::{ChatRequest, Gateway, Message, ToolSpec};
use apex_tools::{ToolContext, ToolRegistry, ToolRequest};
use serde_json::Value;

/// Default cap on model/tool iterations to prevent runaway loops.
const DEFAULT_MAX_STEPS: usize = 8;

/// Options for a single agent run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Run input as JSON. An object with a string `message` field is used as the
    /// user turn; otherwise the raw JSON is passed through.
    pub input: Value,
    /// Maximum model/tool iterations.
    pub max_steps: usize,
    /// The tenant this run acts in — propagated to each tool's [`ToolContext`] so a
    /// plugin tool's secret references resolve within it. Empty for the unscoped default.
    pub tenant: String,
    /// Whether this run is network-facing/hosted (SEC-303) — a manifest with no
    /// `permissions:` block then means **deny-all** for permissioned tools, not
    /// unrestricted. `false` (unrestricted, back-compat) by default: this is what
    /// preserves the CLI's `agents run --local`/eval-harness/test ergonomics; the
    /// server's run endpoints opt in via [`Self::with_hosted`].
    pub hosted: bool,
}

impl RunOptions {
    /// Construct options with the default step budget.
    pub fn new(input: Value) -> Self {
        Self {
            input,
            max_steps: DEFAULT_MAX_STEPS,
            tenant: String::new(),
            hosted: false,
        }
    }

    /// Set the tenant the run acts in (for tenant-scoped plugin secret resolution).
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = tenant.into();
        self
    }

    /// Override the model/tool iteration cap (default [`DEFAULT_MAX_STEPS`]).
    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Mark this run as network-facing/hosted (SEC-303): a manifest without an
    /// explicit `permissions:` block gets **no** tool permissions rather than an
    /// unrestricted grant. `APEX_UNRESTRICTED_TOOLS=1` is the escape hatch for a
    /// trusted first-party deployment that still wants the old behavior.
    pub fn with_hosted(mut self, hosted: bool) -> Self {
        self.hosted = hosted;
        self
    }
}

/// The result of an agent run.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// Final assistant text.
    pub text: String,
    /// Cumulative token/cost usage across all model calls.
    pub usage: Usage,
    /// Number of model calls made.
    pub steps: usize,
}

/// Extract the user-facing prompt from the run input.
fn user_prompt(input: &Value) -> String {
    match input {
        Value::Object(map) => match map.get("message").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => input.to_string(),
        },
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Format retrieved memories into a grounding block for the system prompt. With no
/// hits, the model is told the knowledge base had nothing — so a well-instructed
/// agent answers "I don't know" rather than hallucinating.
fn format_context(hits: &[RetrievedContext]) -> String {
    if hits.is_empty() {
        return "Retrieved knowledge: (none found in the knowledge base)".to_string();
    }
    let mut block =
        String::from("Retrieved knowledge (answer using only this; cite the source):\n");
    for hit in hits {
        block.push_str(&format!("[{}] {}\n", hit.source, hit.content));
    }
    block
}

/// `APEX_UNRESTRICTED_TOOLS=1` — the documented escape hatch (SEC-303) letting a
/// trusted first-party *hosted* deployment keep today's unrestricted-by-default
/// behavior for a manifest with no `permissions:` block, instead of the new deny-all
/// default.
fn unrestricted_tools_escape_hatch() -> bool {
    std::env::var("APEX_UNRESTRICTED_TOOLS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Build the tool specs advertised to the model from the agent's allowed tools.
///
/// Fails closed: an agent referencing a tool that isn't registered is a
/// configuration error rather than a silently-dropped capability.
fn resolve_tools(def: &AgentDefinition, registry: &ToolRegistry) -> Result<Vec<ToolSpec>> {
    let mut specs = Vec::new();
    for id in &def.spec.tools {
        let tool = registry
            .get(id)
            .ok_or_else(|| Error::config(format!("agent references unknown tool `{id}`")))?;
        let meta = tool.metadata();
        specs.push(ToolSpec {
            name: meta.id,
            description: meta.description,
            parameters: tool.input_schema(),
        });
    }
    Ok(specs)
}

/// Run an agent to completion against the given gateway and tool registry.
pub async fn run_agent(
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    opts: RunOptions,
    sink: &mut dyn RunEventSink,
) -> Result<AgentOutput> {
    run_agent_inner(def, gateway, registry, opts, None, sink).await
}

/// Run an agent with retrieval-augmented grounding: when the agent enables memory,
/// `retriever` supplies context that is injected before the model call
/// ([RAG agent](../../docs/16-examples/rag-agent.md)).
pub async fn run_agent_with_memory(
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    opts: RunOptions,
    retriever: &dyn ContextRetriever,
    sink: &mut dyn RunEventSink,
) -> Result<AgentOutput> {
    run_agent_inner(def, gateway, registry, opts, Some(retriever), sink).await
}

#[tracing::instrument(
    name = "agent.run",
    skip_all,
    fields(agent = %def.metadata.name, max_steps = opts.max_steps)
)]
async fn run_agent_inner(
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    opts: RunOptions,
    retriever: Option<&dyn ContextRetriever>,
    sink: &mut dyn RunEventSink,
) -> Result<AgentOutput> {
    let model = gateway.resolve_model(def.spec.model.as_deref(), &def.selector());
    let provider = gateway.provider_name().to_string();
    sink.emit(RunEvent::Start {
        model: &model,
        provider: &provider,
    });

    let tools = resolve_tools(def, registry)?;

    let prompt = user_prompt(&opts.input);
    let mut messages = vec![Message::system(def.spec.instructions.clone())];

    // Retrieval-augmented grounding: if the agent enables memory and a retriever is
    // available, fetch relevant context and inject it as a system message before the
    // user turn. Each hit is surfaced as an event so the trace shows what grounded
    // the answer.
    if let (Some(mem), Some(retriever)) = (&def.spec.memory, retriever)
        && mem.enabled
    {
        let hits = retriever.retrieve(&prompt, mem).await?;
        for hit in &hits {
            sink.emit(RunEvent::MemoryRetrieved {
                source: &hit.source,
                score: hit.score,
            });
        }
        messages.push(Message::system(format_context(&hits)));
    }

    messages.push(Message::user(prompt));

    let mut usage = Usage::default();
    let mut final_text = String::new();
    let mut steps = 0usize;

    for step in 0..opts.max_steps {
        let mut request = ChatRequest::new(model.clone(), messages.clone());
        request.temperature = def.spec.temperature;
        request.max_tokens = def.spec.max_tokens;
        request.tools = tools.clone();

        // Stream the model call, emitting deltas as content arrives, and use the
        // completed response to decide whether to call tools or finish.
        let response = stream_chat(gateway, request, sink).await?;
        usage.add(response.usage);
        steps += 1;

        // No tool calls → the model produced a final answer (already streamed).
        if response.message.tool_calls.is_empty() {
            final_text = response.message.content.clone().unwrap_or_default();
            break;
        }

        // Record the assistant's tool-calling turn, then execute each call.
        let tool_calls = response.message.tool_calls.clone();
        messages.push(response.message);

        for (idx, call) in tool_calls.iter().enumerate() {
            sink.emit(RunEvent::ToolCall {
                name: &call.name,
                arguments: &call.arguments,
            });

            let result_text = execute_tool_call(def, registry, &opts, step, idx, call, sink).await;

            messages.push(Message::tool_result(&call.id, &call.name, result_text));
        }
    }

    if steps == opts.max_steps && final_text.is_empty() {
        return Err(Error::Runtime(format!(
            "agent did not finish within {} steps",
            opts.max_steps
        )));
    }

    sink.emit(RunEvent::Done { usage });
    Ok(AgentOutput {
        text: final_text,
        usage,
        steps,
    })
}

/// Drive one streamed model call: emit a `Delta` per content chunk and return the
/// completed [`ChatResponse`] from the terminal `Done` event.
async fn stream_chat(
    gateway: &Gateway,
    request: apex_provider::ChatRequest,
    sink: &mut dyn RunEventSink,
) -> Result<apex_provider::ChatResponse> {
    use apex_provider::ChatStreamEvent;
    use futures::StreamExt;

    let mut stream = gateway.chat_stream(request).await?;
    let mut completed = None;
    while let Some(event) = stream.next().await {
        match event? {
            ChatStreamEvent::Delta(text) => {
                if !text.is_empty() {
                    sink.emit(RunEvent::Delta { text: &text });
                }
            }
            ChatStreamEvent::Done(response) => completed = Some(response),
        }
    }
    completed.ok_or_else(|| Error::provider("model stream ended without a final response"))
}

/// Execute a single tool call and return a string result suitable to feed back to
/// the model. Errors are returned (not propagated) so the model can react to them.
async fn execute_tool_call(
    def: &AgentDefinition,
    registry: &ToolRegistry,
    opts: &RunOptions,
    step: usize,
    idx: usize,
    call: &apex_provider::ToolCall,
    sink: &mut dyn RunEventSink,
) -> String {
    let tool = match registry.get(&call.name) {
        Some(t) => t,
        None => {
            sink.emit(RunEvent::ToolResult {
                name: &call.name,
                ok: false,
            });
            return format!("error: tool `{}` is not available", call.name);
        }
    };

    let parameters: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    // Deterministic execution id: no clocks or randomness in core logic. The agent's
    // declared `permissions` (if any) form the grant set enforced against each tool.
    // Absent (`None`) is unrestricted for an unhosted (CLI/local/eval) run — back-compat
    // — but **deny-all** (`Some(vec![])`) for a hosted run (SEC-303): a network-facing
    // deployment must not hand every agent every permissioned tool just because its
    // manifest forgot to list one. `APEX_UNRESTRICTED_TOOLS=1` is the documented escape
    // hatch for a trusted first-party hosted deployment that still wants the old
    // behavior.
    let granted_permissions = match &def.spec.permissions {
        Some(perms) => Some(perms.clone()),
        None if opts.hosted && !unrestricted_tools_escape_hatch() => Some(Vec::new()),
        None => None,
    };
    let ctx = ToolContext {
        execution_id: format!("{}-s{step}-t{idx}", def.metadata.name),
        agent_id: def.metadata.name.clone(),
        workdir: ".".to_string(),
        // The run's tenant — scopes plugin secret resolution to this tenant's namespace.
        tenant: opts.tenant.clone(),
        granted_permissions,
    };

    // Fail closed: a tool requiring a permission the agent wasn't granted is denied.
    if let Err(e) = apex_tools::check_permissions(&tool.metadata().permissions, &ctx) {
        sink.emit(RunEvent::ToolResult {
            name: &call.name,
            ok: false,
        });
        return format!("error: {e}");
    }

    match tool.execute(&ctx, ToolRequest::new(parameters)).await {
        Ok(resp) => {
            sink.emit(RunEvent::ToolResult {
                name: &call.name,
                ok: resp.success,
            });
            resp.payload.to_string()
        }
        Err(e) => {
            sink.emit(RunEvent::ToolResult {
                name: &call.name,
                ok: false,
            });
            format!("error: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullSink;
    use apex_provider::{Gateway, MockProvider};
    use serde_json::json;

    fn hello_def() -> AgentDefinition {
        AgentDefinition::from_yaml(
            "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn runs_to_completion_with_mock() {
        let def = hello_def();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();
        let out = run_agent(
            &def,
            &gw,
            &reg,
            RunOptions::new(json!({"message": "hi there"})),
            &mut NullSink,
        )
        .await
        .unwrap();

        assert_eq!(out.steps, 1);
        assert!(out.text.contains("hi there"));
        assert!(out.usage.total_tokens > 0);
    }

    #[tokio::test]
    async fn with_max_steps_overrides_the_default_budget() {
        let def = hello_def();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();
        // 0 steps can't complete even the mock's single-turn reply.
        let opts = RunOptions::new(json!({"message": "hi"})).with_max_steps(0);
        assert_eq!(opts.max_steps, 0);
        let err = run_agent(&def, &gw, &reg, opts, &mut NullSink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("0 steps"), "{err}");
    }

    #[tokio::test]
    async fn unknown_tool_is_config_error() {
        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: x\nspec:\n  instructions: hi\n  tools: [does_not_exist]\n",
        )
        .unwrap();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();
        let err = run_agent(&def, &gw, &reg, RunOptions::new(json!("hi")), &mut NullSink)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    // ---- retrieval-augmented grounding ----------------------------------------

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A retriever that records what it was asked and returns canned hits.
    #[derive(Default)]
    struct FakeRetriever {
        hits: Vec<RetrievedContext>,
        seen_query: Mutex<Option<String>>,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ContextRetriever for FakeRetriever {
        async fn retrieve(
            &self,
            query: &str,
            _spec: &crate::definition::MemorySpec,
        ) -> Result<Vec<RetrievedContext>> {
            *self.seen_query.lock().unwrap() = Some(query.to_string());
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.hits.clone())
        }
    }

    /// Captures the sources of `MemoryRetrieved` events.
    #[derive(Default)]
    struct EventCapture {
        memories: Vec<String>,
    }

    impl RunEventSink for EventCapture {
        fn emit(&mut self, event: RunEvent<'_>) {
            if let RunEvent::MemoryRetrieved { source, .. } = event {
                self.memories.push(source.to_string());
            }
        }
    }

    fn rag_def() -> AgentDefinition {
        AgentDefinition::from_yaml(
            "metadata:\n  name: rag\nspec:\n  instructions: Answer from memory.\n  memory:\n    enabled: true\n    namespace: kb\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn memory_enabled_retrieves_injects_and_emits_event() {
        let def = rag_def();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();
        let retriever = FakeRetriever {
            hits: vec![RetrievedContext {
                source: "doc-1".into(),
                content: "Refunds take 14 days.".into(),
                score: 0.91,
            }],
            ..Default::default()
        };
        let mut sink = EventCapture::default();

        run_agent_with_memory(
            &def,
            &gw,
            &reg,
            RunOptions::new(json!({"message": "how long do refunds take"})),
            &retriever,
            &mut sink,
        )
        .await
        .unwrap();

        // The retriever saw the user prompt, and the hit surfaced in the trace.
        assert_eq!(retriever.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            retriever.seen_query.lock().unwrap().as_deref(),
            Some("how long do refunds take")
        );
        assert_eq!(sink.memories, vec!["doc-1".to_string()]);
    }

    #[tokio::test]
    async fn memory_disabled_skips_retrieval() {
        // hello_def has no `memory` block, so retrieval must not run even if a
        // retriever is supplied.
        let def = hello_def();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();
        let retriever = FakeRetriever::default();

        run_agent_with_memory(
            &def,
            &gw,
            &reg,
            RunOptions::new(json!({"message": "hi"})),
            &retriever,
            &mut NullSink,
        )
        .await
        .unwrap();

        assert_eq!(retriever.calls.load(Ordering::SeqCst), 0);
    }
}
