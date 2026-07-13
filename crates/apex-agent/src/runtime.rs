//! The agent run loop.
//!
//! Implements the core loop from the
//! [Agent Runtime spec §14](../../docs/03-workflow-engine/agent-runtime.md):
//! the model is called; if it requests tools, they are executed and their results
//! fed back; this repeats until the model returns a final answer or a step budget
//! is exhausted.

use crate::context::{ContextPolicy, compact};
use crate::definition::AgentDefinition;
use crate::events::{RunEvent, RunEventSink};
use crate::memory::{ContextRetriever, RetrievedContext};
use apex_common::{Error, Result, Usage};
use apex_provider::{
    ChatRequest, Gateway, HeuristicTokenizer, Message, PER_TOOL_OVERHEAD, TokenCounter, ToolSpec,
};
use apex_tools::{ToolContext, ToolRegistry, ToolRequest, TrustClass};
use serde_json::Value;

/// Default cap on model/tool iterations to prevent runaway loops.
const DEFAULT_MAX_STEPS: usize = 8;

/// Options for a single agent run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Run input as JSON. An object with a string `message` field is used as the
    /// user turn; otherwise the raw JSON is passed through.
    pub input: Value,
    /// Caller-supplied cap on model/tool iterations. `None` (the default) means
    /// "defer to the manifest's `spec.max_steps`, then the built-in
    /// [`DEFAULT_MAX_STEPS`]" (AIC-103); `Some(n)` is an explicit override that wins
    /// over the manifest.
    pub max_steps: Option<usize>,
    /// The tenant this run acts in — propagated to each tool's [`ToolContext`] so a
    /// plugin tool's secret references resolve within it. Empty for the unscoped default.
    pub tenant: String,
    /// Whether this run is network-facing/hosted (SEC-303) — a manifest with no
    /// `permissions:` block then means **deny-all** for permissioned tools, not
    /// unrestricted. `false` (unrestricted, back-compat) by default: this is what
    /// preserves the CLI's `agents run --local`/eval-harness/test ergonomics; the
    /// server's run endpoints opt in via [`Self::with_hosted`].
    pub hosted: bool,
    /// Per-tenant egress allow-list threaded to each tool's [`ToolContext`]
    /// (SEC-304) — `http_get` refuses any host not on this list when set. `None`
    /// (the default) allows any public host (internal/loopback/link-local/private
    /// ranges are refused regardless, allow-list or not).
    pub egress_allowlist: Option<Vec<String>>,
    /// The run's trust classification (SEC-305), derived from provenance (first-
    /// party manifest vs. installed plugin vs. untrusted/marketplace) — drives
    /// sandbox backend selection for tools that run one (e.g. `shell`). Defaults to
    /// [`TrustClass::FirstParty`] (today's behavior, back-compat).
    pub trust_class: TrustClass,
    /// Context-window policy (AIC-101): before each model call the message history is
    /// compacted to stay under [`ContextPolicy::max_prompt_tokens`], dropping the
    /// oldest tool rounds first. The default budget is generous, so short runs are
    /// never touched.
    pub context: ContextPolicy,
}

impl RunOptions {
    /// Construct options with the default step budget.
    pub fn new(input: Value) -> Self {
        Self {
            input,
            max_steps: None,
            tenant: String::new(),
            hosted: false,
            egress_allowlist: None,
            trust_class: TrustClass::FirstParty,
            context: ContextPolicy::default(),
        }
    }

    /// Set the tenant the run acts in (for tenant-scoped plugin secret resolution).
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = tenant.into();
        self
    }

    /// Explicitly override the model/tool iteration cap. Takes precedence over the
    /// manifest's `spec.max_steps` and the built-in [`DEFAULT_MAX_STEPS`] (AIC-103).
    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = Some(max_steps);
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

    /// Set the per-tenant egress allow-list (SEC-304) threaded to each tool call's
    /// [`ToolContext`].
    pub fn with_egress_allowlist(mut self, hosts: Vec<String>) -> Self {
        self.egress_allowlist = Some(hosts);
        self
    }

    /// Set the run's trust classification (SEC-305), threaded to each tool call's
    /// [`ToolContext`] to drive sandbox backend selection.
    pub fn with_trust_class(mut self, trust_class: TrustClass) -> Self {
        self.trust_class = trust_class;
        self
    }

    /// Override the context-window policy (AIC-101).
    pub fn with_context_policy(mut self, context: ContextPolicy) -> Self {
        self.context = context;
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
            strict: false,
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
    fields(
        agent = %def.metadata.name,
        max_steps = opts.max_steps.or(def.spec.max_steps).unwrap_or(DEFAULT_MAX_STEPS),
    )
)]
async fn run_agent_inner(
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    opts: RunOptions,
    retriever: Option<&dyn ContextRetriever>,
    sink: &mut dyn RunEventSink,
) -> Result<AgentOutput> {
    // Resolve the step budget: an explicit `RunOptions::max_steps` wins, else the
    // manifest's `spec.max_steps`, else the built-in default (AIC-103) — so the eval
    // runner and any direct `run_agent` caller honor a manifest budget too, not only
    // the callers that hand-wire it into `RunOptions`.
    let max_steps = opts
        .max_steps
        .or(def.spec.max_steps)
        .unwrap_or(DEFAULT_MAX_STEPS);

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

    // Token cost of the advertised tool specs — constant across the loop, but part of
    // every request's prompt, so the compactor must budget for it (AIC-101).
    let counter = HeuristicTokenizer;
    let tools_overhead: usize = tools
        .iter()
        .map(|t| {
            counter.count(&t.name)
                + counter.count(&t.description)
                + counter.count(&t.parameters.to_string())
                + PER_TOOL_OVERHEAD
        })
        .sum();

    for step in 0..max_steps {
        // Keep the prompt under the model's context window: drop the oldest tool
        // rounds first, always preserving the system prompt + first user turn.
        let compacted = compact(messages, tools_overhead, &opts.context, &counter);
        if compacted.dropped_messages > 0 {
            tracing::debug!(
                target: "apex.context",
                dropped = compacted.dropped_messages,
                tokens_after = compacted.tokens_after,
                budget = opts.context.max_prompt_tokens,
                "compacted agent history to fit the context budget"
            );
        }
        messages = compacted.messages;

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

        // Announce every requested call up front (deterministic order), then execute
        // the whole batch **concurrently** (AIC-102): the model can request several
        // independent, I/O-bound tools in one turn, and serializing them wastes
        // wall-clock. `join_all` runs the futures on this task (no `Send`/spawn needed,
        // so the sink stays a plain `&mut`) and returns results in input order, so the
        // history fed back to the model is deterministic regardless of which tool
        // finished first.
        for call in &tool_calls {
            sink.emit(RunEvent::ToolCall {
                name: &call.name,
                arguments: &call.arguments,
            });
        }

        let outcomes = futures::future::join_all(
            tool_calls
                .iter()
                .enumerate()
                .map(|(idx, call)| execute_tool_call(def, registry, &opts, step, idx, call)),
        )
        .await;

        for (call, outcome) in tool_calls.iter().zip(outcomes) {
            sink.emit(RunEvent::ToolResult {
                name: &call.name,
                ok: outcome.ok,
            });
            messages.push(Message::tool_result(
                &call.id,
                &call.name,
                outcome.result_text,
            ));
        }
    }

    if steps == max_steps && final_text.is_empty() {
        return Err(Error::Runtime(format!(
            "agent did not finish within {max_steps} steps"
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

/// The result of executing one tool call: the text to feed back to the model plus
/// whether it succeeded (for the `ToolResult` event). Event emission is the caller's
/// job so the batch can run concurrently without sharing the `&mut` sink (AIC-102).
struct ToolOutcome {
    result_text: String,
    ok: bool,
}

/// Execute a single tool call and return its [`ToolOutcome`]. Errors are returned
/// (not propagated) so the model can react to them, and no event is emitted here —
/// the caller emits `ToolResult` in deterministic input order after the batch joins.
async fn execute_tool_call(
    def: &AgentDefinition,
    registry: &ToolRegistry,
    opts: &RunOptions,
    step: usize,
    idx: usize,
    call: &apex_provider::ToolCall,
) -> ToolOutcome {
    let tool = match registry.get(&call.name) {
        Some(t) => t,
        None => {
            return ToolOutcome {
                result_text: format!("error: tool `{}` is not available", call.name),
                ok: false,
            };
        }
    };

    // Malformed arguments are a *model* error, and the model is who can fix
    // them: feed the parse failure back as the tool result instead of silently
    // invoking the tool with `null` (RM-AIM-P2 PRV-203 — the old behavior made
    // the tool fail on an input it never should have seen, or worse, succeed
    // with defaults). An empty string is the conventional no-argument call
    // (providers normalize it to `{}`), not an error.
    let parameters: Value = if call.arguments.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutcome {
                    result_text: format!(
                        "error: the arguments for tool `{}` are not valid JSON ({e}). \
                         Re-issue the tool call with well-formed JSON arguments.",
                        call.name
                    ),
                    ok: false,
                };
            }
        }
    };
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
        egress_allowlist: opts.egress_allowlist.clone(),
        trust_class: opts.trust_class,
    };

    // Fail closed: a tool requiring a permission the agent wasn't granted is denied.
    if let Err(e) = apex_tools::check_permissions(&tool.metadata().permissions, &ctx) {
        return ToolOutcome {
            result_text: format!("error: {e}"),
            ok: false,
        };
    }

    match tool.execute(&ctx, ToolRequest::new(parameters)).await {
        Ok(resp) => ToolOutcome {
            result_text: resp.payload.to_string(),
            ok: resp.success,
        },
        Err(e) => ToolOutcome {
            result_text: format!("error: {e}"),
            ok: false,
        },
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
        assert_eq!(opts.max_steps, Some(0));
        let err = run_agent(&def, &gw, &reg, opts, &mut NullSink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("0 steps"), "{err}");
    }

    #[tokio::test]
    async fn manifest_max_steps_is_the_default_budget() {
        // A manifest `max_steps: 0` with no `RunOptions` override must cap the run at
        // 0 steps (AIC-103) — proving the manifest budget is honored by `run_agent`
        // itself, not only by callers that hand-wire it into `RunOptions`.
        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: capped\nspec:\n  instructions: Be brief.\n  max_steps: 0\n",
        )
        .unwrap();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();

        let opts = RunOptions::new(json!({"message": "hi"}));
        assert_eq!(opts.max_steps, None, "no explicit override");
        let err = run_agent(&def, &gw, &reg, opts, &mut NullSink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("0 steps"), "{err}");
    }

    #[tokio::test]
    async fn explicit_max_steps_overrides_the_manifest_budget() {
        // A manifest sets an unworkable budget, but an explicit `RunOptions` override
        // wins, so the run completes (precedence: RunOptions > manifest > default).
        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: capped\nspec:\n  instructions: Be brief.\n  max_steps: 0\n",
        )
        .unwrap();
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let reg = ToolRegistry::with_builtins();

        let opts = RunOptions::new(json!({"message": "hi"})).with_max_steps(4);
        let out = run_agent(&def, &gw, &reg, opts, &mut NullSink)
            .await
            .unwrap();
        assert_eq!(out.steps, 1, "the mock finishes in one turn");
    }

    // ---- context-window management (AIC-101) ----------------------------------

    /// A provider that drives a long tool loop: it requests a `echo` tool call for the
    /// first `tool_turns` calls (each with a large payload to inflate history), then a
    /// final answer. It records the largest request it ever saw (in estimated tokens)
    /// and whether the system + first user turn were present on every request.
    struct ScriptedLoop {
        tool_turns: usize,
        calls: std::sync::atomic::AtomicUsize,
        max_request_tokens: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        invariant_violations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl apex_provider::AIProvider for ScriptedLoop {
        fn name(&self) -> &str {
            "scripted-loop"
        }

        async fn chat(
            &self,
            request: apex_provider::ChatRequest,
        ) -> Result<apex_provider::ChatResponse> {
            use std::sync::atomic::Ordering;
            let counter = HeuristicTokenizer;
            let tools_overhead: usize = request
                .tools
                .iter()
                .map(|t| {
                    counter.count(&t.name)
                        + counter.count(&t.description)
                        + counter.count(&t.parameters.to_string())
                        + PER_TOOL_OVERHEAD
                })
                .sum();
            let req_tokens = tools_overhead
                + request
                    .messages
                    .iter()
                    .map(|m| counter.count_message(m))
                    .sum::<usize>();
            self.max_request_tokens
                .fetch_max(req_tokens, Ordering::SeqCst);

            // System prompt + first user turn must be present on *every* request.
            let preserved = request
                .messages
                .first()
                .is_some_and(|m| m.role == apex_provider::Role::System)
                && request
                    .messages
                    .get(1)
                    .is_some_and(|m| m.role == apex_provider::Role::User);
            if !preserved {
                self.invariant_violations.fetch_add(1, Ordering::SeqCst);
            }

            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let message = if n < self.tool_turns {
                apex_provider::Message {
                    role: apex_provider::Role::Assistant,
                    content: None,
                    parts: Vec::new(),
                    tool_calls: vec![apex_provider::ToolCall {
                        id: format!("call-{n}"),
                        name: "echo".into(),
                        arguments: format!("{{\"payload\":\"{}\"}}", "x".repeat(300)),
                    }],
                    tool_call_id: None,
                    name: None,
                }
            } else {
                apex_provider::Message::assistant("final answer reached")
            };
            Ok(apex_provider::ChatResponse {
                message,
                model: request.model,
                usage: Usage::new(1, 1, 0.0),
                finish_reason: "stop".into(),
            })
        }
    }

    #[tokio::test]
    async fn long_tool_loop_request_stays_under_context_budget() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: loop\nspec:\n  instructions: You are a careful assistant that uses tools.\n  tools: [echo]\n",
        )
        .unwrap();

        let max_request_tokens = Arc::new(AtomicUsize::new(0));
        let invariant_violations = Arc::new(AtomicUsize::new(0));
        let provider = ScriptedLoop {
            tool_turns: 30,
            calls: AtomicUsize::new(0),
            max_request_tokens: max_request_tokens.clone(),
            invariant_violations: invariant_violations.clone(),
        };
        let gw = Gateway::new(Box::new(provider));
        let reg = ToolRegistry::with_builtins();

        let budget = 300usize;
        let opts = RunOptions::new(json!({
            "message": "Please research the topic thoroughly, calling tools as needed."
        }))
        .with_max_steps(40)
        .with_context_policy(crate::ContextPolicy {
            max_prompt_tokens: budget,
            strategy: crate::CompactionStrategy::DropOldest,
        });

        let out = run_agent(&def, &gw, &reg, opts, &mut NullSink)
            .await
            .unwrap();

        assert!(
            out.text.contains("final answer"),
            "loop finished: {}",
            out.text
        );
        // The loop ran many tool turns, so without compaction the request would be
        // far over budget — yet the largest request the provider ever saw fit.
        let max = max_request_tokens.load(Ordering::SeqCst);
        assert!(max > 0, "the provider was called");
        assert!(
            max <= budget,
            "largest request {max} tokens must stay under the {budget}-token budget"
        );
        assert_eq!(
            invariant_violations.load(Ordering::SeqCst),
            0,
            "system prompt + first user turn must be preserved on every request"
        );
    }

    // ---- concurrent tool-call execution (AIC-102) -----------------------------

    /// A provider that, on the first turn, requests two independent tool calls, then
    /// on the second turn records the order the tool-result messages came back in and
    /// finishes. Used to prove the batch ran concurrently and results stayed ordered.
    struct TwoToolProvider {
        calls: std::sync::atomic::AtomicUsize,
        second_turn_tool_order: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl apex_provider::AIProvider for TwoToolProvider {
        fn name(&self) -> &str {
            "two-tool"
        }

        async fn chat(
            &self,
            request: apex_provider::ChatRequest,
        ) -> Result<apex_provider::ChatResponse> {
            use std::sync::atomic::Ordering;
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let message = if n == 0 {
                apex_provider::Message {
                    role: apex_provider::Role::Assistant,
                    content: None,
                    parts: Vec::new(),
                    tool_calls: vec![
                        apex_provider::ToolCall {
                            id: "call-slow".into(),
                            name: "slow".into(),
                            arguments: "{}".into(),
                        },
                        apex_provider::ToolCall {
                            id: "call-fast".into(),
                            name: "fast".into(),
                            arguments: "{}".into(),
                        },
                    ],
                    tool_call_id: None,
                    name: None,
                }
            } else {
                // Record the order the tool results were fed back in (by tool name).
                let order: Vec<String> = request
                    .messages
                    .iter()
                    .filter(|m| m.role == apex_provider::Role::Tool)
                    .filter_map(|m| m.name.clone())
                    .collect();
                *self.second_turn_tool_order.lock().unwrap() = order;
                apex_provider::Message::assistant("done")
            };
            Ok(apex_provider::ChatResponse {
                message,
                model: request.model,
                usage: Usage::new(1, 1, 0.0),
                finish_reason: "stop".into(),
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn independent_tool_calls_run_concurrently_with_deterministic_order() {
        use apex_tools::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        /// A tool that sleeps `delay_ms` then echoes its name — the artificial delay
        /// that makes serial-vs-concurrent execution observable in (paused) wall-clock.
        struct SleepyTool {
            name: &'static str,
            delay_ms: u64,
        }

        #[async_trait::async_trait]
        impl Tool for SleepyTool {
            fn metadata(&self) -> ToolMetadata {
                ToolMetadata::new(self.name, "1.0.0", "test", "sleeps then echoes")
            }
            fn input_schema(&self) -> Value {
                json!({ "type": "object", "additionalProperties": true })
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _req: ToolRequest,
            ) -> std::result::Result<ToolResponse, ToolError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(ToolResponse::success(json!({ "tool": self.name })))
            }
        }

        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: multi\nspec:\n  instructions: Use tools.\n  tools: [slow, fast]\n",
        )
        .unwrap();

        let order = Arc::new(Mutex::new(Vec::new()));
        let provider = TwoToolProvider {
            calls: AtomicUsize::new(0),
            second_turn_tool_order: order.clone(),
        };
        let gw = Gateway::new(Box::new(provider));
        let mut reg = ToolRegistry::with_builtins();
        // "slow" is requested first but finishes last, so completion order (fast, slow)
        // differs from call order (slow, fast) — exercising the ordering guarantee.
        reg.register(Arc::new(SleepyTool {
            name: "slow",
            delay_ms: 300,
        }));
        reg.register(Arc::new(SleepyTool {
            name: "fast",
            delay_ms: 100,
        }));

        let start = tokio::time::Instant::now();
        let out = run_agent(
            &def,
            &gw,
            &reg,
            RunOptions::new(json!({ "message": "go" })),
            &mut NullSink,
        )
        .await
        .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(out.text, "done");
        // Concurrent: total ≈ max(300, 100) = 300ms, not the 400ms a serial loop takes.
        assert!(
            elapsed < Duration::from_millis(350),
            "tool calls serialized (took {elapsed:?}); expected ≈ max(delays)"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "both tool delays should have elapsed (took {elapsed:?})"
        );
        // Deterministic: results feed back in call order, not completion order.
        assert_eq!(
            *order.lock().unwrap(),
            vec!["slow".to_string(), "fast".to_string()],
            "tool results must be ordered by call, regardless of which finished first"
        );
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
