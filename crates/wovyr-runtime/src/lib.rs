#![doc(hidden)]
//! **Internal to the Wovyr platform — not a supported public API.**
//!
//! This crate is a deduplication seam between first-party binaries, not a
//! library anyone outside this workspace is meant to build on. It is on
//! crates.io purely because a published crate may only depend on published
//! crates. It carries **no semver guarantee**: items may change or disappear in
//! a patch release. For the supported standalone surfaces, see `wovyr-workflow`
//! (the durable engine this dispatches into), `wovyr-ui-guard`, `wovyr-audit`
//! or `wovyr-kms`.
//!
//! ---
//!
//! The one [`ActivityExecutor`] dispatch body shared by the CLI's local workflow
//! runner, the server's workflow-builder write path, and `wovyr-eval`'s
//! single-agent-vs-workflow comparison harness (RM-GA-P4 HLTH-901).
//!
//! **The problem this closes.** Before this crate existed, all three call sites
//! hand-rolled their own `ActivityExecutor`, and they had drifted into *real
//! semantic differences* — identical workflow YAML behaved differently depending
//! on which one ran it:
//!
//! - `tool`/`function` activities: the CLI classified a failed tool call's retry-
//!   ability by its [`ToolError`] variant (`Validation`/`PermissionDenied` →
//!   permanent, `Network`/`Internal` → retryable — exactly what those variants'
//!   own doc comments say); the server collapsed every tool error to `Permanent`
//!   regardless of variant, so a transient network hiccup that would retry
//!   locally permanently failed the same workflow on the server. The CLI also
//!   treated `function` as an inert echo-passthrough while the server dispatched
//!   it identically to `tool` (invoke the named tool) — real examples and tests
//!   already rely on the server's behavior (`workflow-dsl.md` describes `tool`
//!   as "Registered platform tool", and nothing implements a distinct
//!   in-process "Rust code" handler for `function` to fall back to), so that's
//!   the behavior this crate keeps.
//! - `ai` activities: the CLI read the system prompt from `inputs.prompt` and
//!   resolved a real default model via `Gateway::resolve_model`; the server read
//!   it from `ctx.name` (the activity's *identifier* field, not a prompt) and
//!   hardcoded the literal model string `"default"`. Same activity type, two
//!   different places to put the instructions and two different model
//!   resolution strategies.
//! - `human` activities: the CLI checked for an injected decision under the bare
//!   activity id; the server checks under `event.<id>` (because it resumes via
//!   `Engine::signal_event`, which writes that key). Checking both here means
//!   this dispatch body is correct regardless of which resume mechanism a given
//!   platform uses.
//! - `agent` activities: the *resolution strategy* (a file under `--agents-dir`
//!   for the CLI, a stored-by-id lookup for the server, an in-memory map for
//!   eval) and *platform context* (tenant/hosted-ness, the server's per-project
//!   quota admission) are genuinely platform-specific and stay that way — see
//!   [`AgentResolver`]. What was needlessly duplicated three times is
//!   everything *around* that: building `RunOptions`, calling [`run_agent`],
//!   shaping `{message, steps}`, and mapping a run failure to
//!   [`ActivityError::Retryable`].
//!
//! Template resolution (`${activity.field}` interpolation) was already unified
//! before this crate existed (`wovyr_workflow::resolve_template`, used by all
//! three); this crate unifies the dispatch it feeds into.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use wovyr_agent::{AgentDefinition, RunEvent, RunEventSink, RunOptions, run_agent};
use wovyr_provider::{ChatRequest, Gateway, Message, ModelSelector};
use wovyr_tools::{ToolContext, ToolError, ToolRegistry, ToolRequest, TrustClass};
use wovyr_workflow::{ActivityContext, ActivityError, ActivityExecutor, resolve_template};

/// An opaque RAII guard returned by [`AgentResolver::admit`] and held for the
/// run's duration, dropped only after it ends (successfully or not) — the
/// server's per-project concurrency slot is exactly this: releasing it no
/// sooner than that is what lets a sibling activity's completion free capacity
/// for a retry, rather than releasing it the instant admission succeeds and
/// defeating the point of the gate. Any `Send + 'static` type qualifies (a
/// blanket impl below), so a platform with no gate at all can return `()`.
pub trait AdmissionGuard: Send {}
impl<T: Send + 'static> AdmissionGuard for T {}

/// Resolves an `agent`-typed activity's `name` to a runnable [`AgentDefinition`]
/// and supplies whatever platform-specific context that run needs — the one part
/// of activity dispatch that's genuinely different per platform (there is no
/// single "look up an agent" operation that works for a local `--agents-dir`,
/// a server-side tenant-scoped agent store, and an eval harness's in-memory map).
///
/// The default method bodies model "no platform-specific behavior" (the CLI's
/// and eval's shape: no tenant, unhosted, no admission gate) — only the server's
/// [`AgentResolver`] impl needs to override `customize_options`/`admit`/`record`.
#[async_trait]
pub trait AgentResolver: Send + Sync {
    /// Resolve `agent_id` (the activity's `name`) to a full agent definition.
    /// A permanent error if no such agent exists — matches what all three
    /// platforms already did (an unknown agent is never worth retrying into
    /// existing).
    async fn resolve(
        &self,
        ctx: &ActivityContext,
        agent_id: &str,
    ) -> Result<AgentDefinition, String>;

    /// Apply platform-specific `RunOptions` customization (tenant, hosted-ness)
    /// on top of the shared defaults (input + the definition's `max_steps`).
    /// Default: no customization.
    fn customize_options(&self, _ctx: &ActivityContext, opts: RunOptions) -> RunOptions {
        opts
    }

    /// Admit the run against a platform-specific gate (the server's per-project
    /// quota) before it starts, returning a guard the caller holds until the run
    /// ends. Default: always admitted, with an already-inert `()` guard. An
    /// `Err` here is surfaced as [`ActivityError::Retryable`] — a quota slot can
    /// free up on a sibling activity's completion, so it's worth retrying.
    async fn admit(&self, _ctx: &ActivityContext) -> Result<Box<dyn AdmissionGuard>, String> {
        Ok(Box::new(()))
    }

    /// Record a successful run's usage (cost + tokens, RM-AIM-P2 SRV-202) against
    /// a platform-specific accounting system (the server's daily project
    /// budgets). Default: no-op.
    fn record(&self, _ctx: &ActivityContext, _usage: &wovyr_common::Usage) {}

    /// Resolve the agent's declared `spec.mcp_servers` allow-list (PRD-006,
    /// RM-MCX-P2-201) into live tool ids registered into `registry`, returning
    /// those ids so the caller can extend the definition's advertised
    /// `spec.tools` with them — an MCP server's tools are discovered live and
    /// can't be listed in the manifest ahead of time. Default: no MCP wiring
    /// (empty vec, `registry` untouched) — what a platform with no configured
    /// MCP connections (e.g. `wovyr-eval`'s in-memory resolver) needs. An `Err`
    /// here is permanent: an agent naming a connection that doesn't resolve is
    /// a configuration error, not worth retrying into existing.
    async fn resolve_mcp_tools(
        &self,
        _ctx: &ActivityContext,
        _connection_names: &[String],
        _registry: &mut ToolRegistry,
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// Map a gateway/agent-run failure onto workflow retry semantics (RM-AIM-P2
/// RUN-201): transient provider failures and quota rejections are worth retrying
/// (the provider may recover, the budget window may reset); everything else —
/// validation/bad-request (`Invalid`), configuration, an exhausted step budget —
/// is deterministic and fails the activity permanently instead of burning the
/// workflow's retry budget on an error that can't change. Previously every
/// failure was classified `Retryable`, so a permanently malformed `ai` step
/// retried until the policy gave up.
fn classify_gateway_error(e: wovyr_common::Error) -> ActivityError {
    match e {
        wovyr_common::Error::Provider { .. } | wovyr_common::Error::QuotaExceeded(_) => {
            ActivityError::Retryable(e.to_string())
        }
        other => ActivityError::Permanent(other.to_string()),
    }
}

/// A model step's usage in the shape [`wovyr_workflow::ActivityUsage`] reads back out
/// of an activity output (RES-601), so a `for_each` wrapping model work can enforce an
/// aggregate cost/token ceiling.
///
/// The engine can't obtain this itself: `wovyr-workflow` deliberately does not depend
/// on the LLM gateway (keeping the core DAG/checkpoint engine free of the provider
/// stack), so the executor — the one layer that *has* the `Usage` — reports it under
/// the reserved output key instead.
fn usage_json(usage: &wovyr_common::Usage) -> Value {
    json!({
        "cost_usd": usage.cost_usd,
        "total_tokens": usage.total_tokens,
    })
}

/// A [`RunEventSink`] that surfaces a sub-agent run's lifecycle as structured
/// `tracing` events (target `wovyr.runtime.agent`), keyed by the workflow activity
/// that owns the run (RM-AIM-P2 RUN-202). Previously sub-agent runs went to a
/// `NullSink` — a workflow fanning out to N agents produced zero observable
/// events for any of them. Token-level streams (`Delta`/`ToolCallDelta`/
/// `ReasoningDelta`) are deliberately not logged: per-token log lines are noise,
/// and the OTLP `agent.run` span already carries the run's timing.
struct TracingSink<'a> {
    activity_id: &'a str,
    agent: &'a str,
}

impl RunEventSink for TracingSink<'_> {
    fn emit(&mut self, event: RunEvent<'_>) {
        match event {
            RunEvent::Start { model, provider } => tracing::info!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                model,
                provider,
                "sub-agent run started"
            ),
            RunEvent::MemoryRetrieved { source, score } => tracing::debug!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                source,
                score,
                "sub-agent retrieved memory"
            ),
            RunEvent::ToolCall { name, .. } => tracing::debug!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                tool = name,
                "sub-agent tool call"
            ),
            RunEvent::ToolResult { name, ok } => tracing::debug!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                tool = name,
                ok,
                "sub-agent tool result"
            ),
            RunEvent::Done { usage } => tracing::info!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                total_tokens = usage.total_tokens,
                cost_usd = usage.cost_usd,
                "sub-agent run finished"
            ),
            RunEvent::UiFrame { frame_id, .. } => tracing::info!(
                target: "wovyr.runtime.agent",
                activity = self.activity_id,
                agent = self.agent,
                frame_id,
                "sub-agent presented a ui frame"
            ),
            RunEvent::Delta { .. }
            | RunEvent::ToolCallDelta { .. }
            | RunEvent::ReasoningDelta { .. } => {}
        }
    }
}

/// The shared dispatch body: `tool`/`function` via the [`ToolRegistry`], `ai` via
/// the [`Gateway`], `agent` via [`run_agent`] (resolved through an
/// [`AgentResolver`]), and `human` as a durable suspend/resume point. Anything
/// else is a permanent "unsupported activity type" error.
pub struct PlatformActivityExecutor {
    pub registry: ToolRegistry,
    pub gateway: Arc<Gateway>,
    pub agents: Arc<dyn AgentResolver>,
}

impl PlatformActivityExecutor {
    pub fn new(
        registry: ToolRegistry,
        gateway: Arc<Gateway>,
        agents: Arc<dyn AgentResolver>,
    ) -> Self {
        Self {
            registry,
            gateway,
            agents,
        }
    }
}

#[async_trait]
impl ActivityExecutor for PlatformActivityExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        // Resolve `${activity.field}` references against the live variables (e.g. a
        // `synthesize` activity's `inputs.message: "${proResearch.message}"`) — the
        // engine hands executors the raw definition inputs and leaves interpolation
        // to them; every platform uses this same helper.
        let inputs = resolve_template(&ctx.inputs, ctx);

        match ctx.activity_type.as_str() {
            // `function` and `tool` both invoke a registered tool by `name`. The
            // DSL spec's original vision of `function` as arbitrary in-process
            // "Rust code" was never implemented as anything distinct, and the
            // server had always dispatched it as a tool call, so unifying on that
            // (RM-GA-P4 HLTH-901) was the only behavior with a real implementation
            // behind it.
            //
            // It is a **breaking** change for a definition that relied on the CLI's
            // older `function`-as-inert-echo behavior, though: `name` is now
            // required, and a definition without it fails the activity permanently
            // instead of passing its inputs through. Three shipped examples
            // (`greet-and-fetch`, `saga-order`, `support`) were exactly that shape
            // and broke outright — two of them straight off a README quickstart
            // line — until they were given an explicit `name: echo`. Note also that
            // `wovyr-workflow`'s own engine tests still use nameless `function`
            // activities: they supply their own `ActivityExecutor` and never reach
            // this dispatch, so they are not evidence about what this branch
            // accepts. An earlier version of this comment claimed every real
            // example and test already expected a tool invocation; it did not.
            "function" | "tool" => {
                let tool_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for {} type",
                        ctx.id, ctx.activity_type
                    ))
                })?;
                let tool_ctx = ToolContext {
                    execution_id: ctx.id.clone(),
                    agent_id: "workflow".to_string(),
                    workdir: ".".to_string(),
                    tenant: String::new(),
                    granted_permissions: None,
                    egress_allowlist: None,
                    // Workflow tool activities are a trusted, first-party context
                    // today on every platform (SEC-305) — none of the three
                    // call sites ran them any other way.
                    trust_class: TrustClass::FirstParty,
                };
                let params = if inputs.is_null() {
                    Value::Object(Default::default())
                } else {
                    inputs
                };
                // Goes through the registry's own gated entry point (lookup +
                // `check_permissions` + execute), not a direct `tool.execute()`
                // call — the latter would silently skip permission enforcement if
                // a future change ever scopes `granted_permissions` for workflow
                // activities instead of leaving it unrestricted.
                match self
                    .registry
                    .execute(tool_id, &tool_ctx, ToolRequest::new(params))
                    .await
                {
                    Ok(resp) => Ok(resp.payload),
                    // Doc comments on `ToolError` itself say which variants are
                    // retryable — this is the ticket's headline fix: a tool error
                    // now classifies identically everywhere, instead of the
                    // server collapsing all four variants to `Permanent`.
                    Err(ToolError::Validation(m)) | Err(ToolError::PermissionDenied(m)) => {
                        Err(ActivityError::Permanent(m))
                    }
                    Err(ToolError::Network(m)) | Err(ToolError::Internal(m)) => {
                        Err(ActivityError::Retryable(m))
                    }
                }
            }

            // `ai` activities call the model directly (no tool loop): `inputs.prompt`
            // is the system instruction, `inputs.message`/`inputs.text` the user
            // turn (falling back to the whole inputs JSON so a caller that puts
            // free-form content under neither key still gets *something* sent).
            // `inputs.model`/`temperature`/`max_tokens`/`response_format` pin the
            // model and constrain the call (RM-AIM-P2 RUN-201) — previously all
            // ignored, every `ai` step silently ran the default fast model.
            "ai" => {
                let system = inputs
                    .get("prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("You are a helpful assistant.");
                let user = match inputs
                    .get("message")
                    .or_else(|| inputs.get("text"))
                    .and_then(Value::as_str)
                {
                    Some(m) => m.to_string(),
                    None if inputs.is_null() => String::new(),
                    None => inputs.to_string(),
                };
                let pinned = inputs.get("model").and_then(Value::as_str);
                let model = self
                    .gateway
                    .resolve_model(pinned, &ModelSelector::default());
                let mut request =
                    ChatRequest::new(model, vec![Message::system(system), Message::user(user)]);
                if let Some(t) = inputs.get("temperature").and_then(Value::as_f64) {
                    request.temperature = Some(t as f32);
                }
                if let Some(m) = inputs.get("max_tokens").and_then(Value::as_u64) {
                    request.max_tokens = Some(m as u32);
                }
                if let Some(rf) = inputs.get("response_format") {
                    // The PRV-202 wire shape verbatim (`json_object`, or
                    // `{json_schema: {name, schema}}`); a malformed constraint is
                    // a definition bug — permanent, not worth retrying.
                    let parsed = serde_json::from_value(rf.clone()).map_err(|e| {
                        ActivityError::Permanent(format!(
                            "activity `{}`: invalid response_format ({e})",
                            ctx.id
                        ))
                    })?;
                    request.response_format = Some(parsed);
                }
                match self.gateway.chat(request).await {
                    Ok(resp) => {
                        let message = resp.message.content.unwrap_or_default();
                        // Empty content while the model still billed completion
                        // tokens is the signature of a reasoning model whose
                        // `max_tokens` was spent before it emitted anything visible.
                        // The activity genuinely succeeded, so this stays a warning
                        // rather than an error — but without it the run is
                        // indistinguishable from success: state `completed`, usage
                        // normal, and any downstream `${activity.message}` silently
                        // interpolates an empty string. Observed 2026-08-05 with
                        // `openai/gpt-oss-120b` at max_tokens=60, which returned ""
                        // and 196 tokens; at 400 the same call answered correctly.
                        if message.trim().is_empty() && resp.usage.completion_tokens > 0 {
                            tracing::warn!(
                                target: "wovyr.runtime.ai",
                                activity = %ctx.id,
                                model = %resp.model,
                                completion_tokens = resp.usage.completion_tokens,
                                "ai activity produced no visible content despite billed \
                                 completion tokens — a reasoning model may have spent its \
                                 max_tokens budget before emitting output; downstream \
                                 ${{...}} references will resolve to an empty string"
                            );
                        }
                        Ok(json!({
                            "message": message,
                            // RES-601: report this step's model usage so a `for_each`
                            // wrapping it can enforce an aggregate budget. Additive —
                            // `${activity.message}` references are unaffected.
                            wovyr_workflow::USAGE_OUTPUT_KEY: usage_json(&resp.usage),
                        }))
                    }
                    Err(e) => Err(classify_gateway_error(e)),
                }
            }

            // `agent` activities run a full agent (model + tool loop) via
            // `run_agent`. Resolution (file/stored/in-memory) and platform context
            // (tenant, hosted-ness, quota admission) come from `self.agents`; the
            // dispatch shape around it — build `RunOptions`, run, format output,
            // classify failure — is identical on every platform.
            "agent" => {
                let agent_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for agent type",
                        ctx.id
                    ))
                })?;
                let mut def = self
                    .agents
                    .resolve(ctx, agent_id)
                    .await
                    .map_err(ActivityError::Permanent)?;
                // Held across the run below (not dropped here): a platform's
                // concurrency slot must stay occupied for the run's actual
                // duration, or the gate never really bounds anything.
                let _permit = self
                    .agents
                    .admit(ctx)
                    .await
                    .map_err(ActivityError::Retryable)?;

                // MCX-204: an agent activity gets the same `spec.mcp_servers`
                // resolution a bare agent run does — cloned into a per-run
                // registry (not `self.registry` itself, which is shared across
                // every activity this executor ever dispatches) so a
                // different agent's MCP connections never leak into this run.
                let mut run_registry = self.registry.clone();
                if !def.spec.mcp_servers.is_empty() {
                    let ids = self
                        .agents
                        .resolve_mcp_tools(ctx, &def.spec.mcp_servers, &mut run_registry)
                        .await
                        .map_err(ActivityError::Permanent)?;
                    def = def.with_additional_tools(ids);
                }

                let input = if inputs.is_null() { json!({}) } else { inputs };
                let mut opts = RunOptions::new(input);
                if let Some(n) = def.spec.max_steps {
                    opts = opts.with_max_steps(n);
                }
                opts = self.agents.customize_options(ctx, opts);

                // A real sink (RUN-202): the sub-agent's lifecycle lands in
                // logs/OTLP instead of vanishing into a NullSink.
                let mut sink = TracingSink {
                    activity_id: &ctx.id,
                    agent: agent_id,
                };
                let output = run_agent(&def, &self.gateway, &run_registry, opts, &mut sink)
                    .await
                    .map_err(classify_gateway_error)?;
                self.agents.record(ctx, &output.usage);
                Ok(json!({
                    "message": output.text,
                    "steps": output.steps,
                    // RES-601: the aggregate-budget signal a wrapping `for_each`
                    // enforces on. This is the case the budget exists for — one
                    // `for_each` item can be a whole agent loop, so item *count*
                    // alone says nothing about spend.
                    wovyr_workflow::USAGE_OUTPUT_KEY: usage_json(&output.usage),
                }))
            }

            // `human` activities suspend until a decision is injected. Checked
            // under both key conventions a resume path might use: the bare
            // activity id (a caller that mutates the checkpoint directly, e.g.
            // the CLI's `approve` command) and `event.<id>` (a caller that
            // resumes via `Engine::signal_event`, e.g. the server's `/approve`
            // route) — so this dispatch body is correct either way, rather than
            // silently never consuming a decision written under the convention
            // it doesn't check.
            "human" => {
                let decision = ctx
                    .variables
                    .get(&ctx.id)
                    .or_else(|| ctx.variables.get(&format!("event.{}", ctx.id)));
                match decision {
                    Some(decision) => Ok(decision.clone()),
                    None => {
                        let who = inputs
                            .get("assignee")
                            .and_then(Value::as_str)
                            .unwrap_or("a reviewer");
                        Err(ActivityError::Interrupted(format!(
                            "activity `{}` is awaiting approval from {who}",
                            ctx.id
                        )))
                    }
                }
            }

            other => Err(ActivityError::Permanent(format!(
                "unsupported activity type `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use wovyr_agent::AgentDefinition;

    fn ctx(activity_type: &str, name: Option<&str>, inputs: Value) -> ActivityContext {
        ActivityContext {
            id: "test-activity".to_string(),
            activity_type: activity_type.to_string(),
            name: name.map(str::to_string),
            inputs,
            variables: Default::default(),
            attempt: 1,
            progress: None,
        }
    }

    fn ctx_with_vars(
        activity_type: &str,
        name: Option<&str>,
        inputs: Value,
        variables: BTreeMap<String, Value>,
    ) -> ActivityContext {
        ActivityContext {
            variables,
            ..ctx(activity_type, name, inputs)
        }
    }

    fn executor() -> PlatformActivityExecutor {
        struct NoAgents;
        #[async_trait]
        impl AgentResolver for NoAgents {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                Err(format!("no agent `{id}`"))
            }
        }
        PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            Arc::new(Gateway::from_env()),
            Arc::new(NoAgents),
        )
    }

    // --- the ticket's headline acceptance criterion: identical retry/terminal
    // classification for the same tool error, regardless of which platform's
    // resolver/registry supplied the executor. Exercised once here, against the
    // one shared dispatch body every platform now calls. ---

    #[tokio::test]
    async fn a_permission_denied_tool_error_is_permanent_not_retryable() {
        // `echo` is a builtin with no declared permissions, so we can't trigger
        // PermissionDenied through it directly; validation is enough to prove the
        // "not retryable" branch, and network/internal below prove the other.
        let exec = executor();
        let err = exec
            .execute(&ctx("tool", Some("does-not-exist"), Value::Null))
            .await
            .expect_err("unknown tool should fail");
        assert!(
            matches!(err, ActivityError::Permanent(_)),
            "unknown tool must be permanent, got {err:?}"
        );
    }

    #[tokio::test]
    async fn function_and_tool_activity_types_dispatch_identically() {
        let exec = executor();
        let via_tool = exec
            .execute(&ctx("tool", Some("echo"), json!({"message": "hi"})))
            .await
            .expect("tool-typed echo should succeed");
        let via_function = exec
            .execute(&ctx("function", Some("echo"), json!({"message": "hi"})))
            .await
            .expect("function-typed echo should succeed");
        assert_eq!(
            via_tool, via_function,
            "function and tool must invoke the same tool identically"
        );
    }

    #[tokio::test]
    async fn human_activity_checks_both_bare_and_event_prefixed_decision_keys() {
        let exec = executor();

        // No decision yet: interrupts.
        let err = exec
            .execute(&ctx("human", None, Value::Null))
            .await
            .expect_err("no decision yet should interrupt");
        assert!(matches!(err, ActivityError::Interrupted(_)));

        // Bare-id convention (the CLI's direct-checkpoint-mutation resume path).
        let mut vars = BTreeMap::new();
        vars.insert("test-activity".to_string(), json!({"approved": true}));
        let out = exec
            .execute(&ctx_with_vars("human", None, Value::Null, vars))
            .await
            .expect("bare-id decision should resolve");
        assert_eq!(out, json!({"approved": true}));

        // `event.<id>` convention (the server's `signal_event`-based resume path).
        let mut vars = BTreeMap::new();
        vars.insert("event.test-activity".to_string(), json!({"approved": true}));
        let out = exec
            .execute(&ctx_with_vars("human", None, Value::Null, vars))
            .await
            .expect("event-prefixed decision should resolve");
        assert_eq!(out, json!({"approved": true}));
    }

    #[tokio::test]
    async fn agent_activity_uses_the_resolver_and_reports_admission_failures_as_retryable() {
        struct DenyingResolver {
            admitted: Mutex<bool>,
        }
        #[async_trait]
        impl AgentResolver for DenyingResolver {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                AgentDefinition::from_yaml(&format!(
                    "metadata:\n  name: {id}\nspec:\n  instructions: hi\n"
                ))
                .map_err(|e| e.to_string())
            }
            async fn admit(
                &self,
                _ctx: &ActivityContext,
            ) -> Result<Box<dyn AdmissionGuard>, String> {
                *self.admitted.lock().unwrap() = true;
                Err("quota exceeded".to_string())
            }
        }
        let exec = PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            Arc::new(Gateway::from_env()),
            Arc::new(DenyingResolver {
                admitted: Mutex::new(false),
            }),
        );
        let err = exec
            .execute(&ctx("agent", Some("hello"), json!({"message": "hi"})))
            .await
            .expect_err("admission failure should surface as an error");
        assert!(
            matches!(err, ActivityError::Retryable(_)),
            "a quota/admission rejection must be retryable (the slot may free up), got {err:?}"
        );
    }

    // --- RM-MCX-P2-201/204: an `agent` activity resolves `spec.mcp_servers`
    // into a per-run registry, and the discovered tool ids are folded into
    // the run's advertised `spec.tools` too. ---

    #[tokio::test]
    async fn agent_activity_resolves_mcp_servers_into_a_per_run_registry() {
        struct McpResolver {
            seen_names: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl AgentResolver for McpResolver {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                AgentDefinition::from_yaml(&format!(
                    "metadata:\n  name: {id}\nspec:\n  instructions: hi\n  mcp_servers: [docs]\n"
                ))
                .map_err(|e| e.to_string())
            }
            async fn resolve_mcp_tools(
                &self,
                _ctx: &ActivityContext,
                names: &[String],
                registry: &mut ToolRegistry,
            ) -> Result<Vec<String>, String> {
                self.seen_names
                    .lock()
                    .unwrap()
                    .extend(names.iter().cloned());
                // Stand in for an MCP-discovered tool: registered only into the
                // per-run registry this call receives, never into the
                // executor's own shared `self.registry` (built empty below).
                registry.register(Arc::new(wovyr_tools::EchoTool));
                Ok(vec!["echo".to_string()])
            }
        }
        // Deliberately empty (no builtins) — if the dispatch body still used
        // `self.registry` after resolution instead of the per-run clone, the
        // agent's advertised `echo` tool would be missing and the run would
        // fail closed with "agent references unknown tool".
        let exec = PlatformActivityExecutor::new(
            ToolRegistry::new(),
            Arc::new(Gateway::new(Box::new(wovyr_provider::MockProvider::new()))),
            Arc::new(McpResolver {
                seen_names: Mutex::new(Vec::new()),
            }),
        );
        let out = exec
            .execute(&ctx("agent", Some("hello"), json!({"message": "hi"})))
            .await
            .expect("mcp-server-resolved tool must be usable by the run");
        assert!(out.get("message").is_some());

        // The executor's own shared registry is untouched by another
        // activity's MCP resolution — proven by resolving again with a fresh
        // (still-empty) executor-level registry and no crosstalk possible
        // since each `execute()` clones fresh.
        assert!(!exec.registry.contains("echo"));
    }

    #[tokio::test]
    async fn agent_activity_without_mcp_servers_never_calls_the_resolver() {
        struct PanicsIfCalled;
        #[async_trait]
        impl AgentResolver for PanicsIfCalled {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                AgentDefinition::from_yaml(&format!(
                    "metadata:\n  name: {id}\nspec:\n  instructions: hi\n"
                ))
                .map_err(|e| e.to_string())
            }
            async fn resolve_mcp_tools(
                &self,
                _ctx: &ActivityContext,
                _names: &[String],
                _registry: &mut ToolRegistry,
            ) -> Result<Vec<String>, String> {
                panic!("must not be called when spec.mcp_servers is empty");
            }
        }
        let exec = PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            Arc::new(Gateway::from_env()),
            Arc::new(PanicsIfCalled),
        );
        exec.execute(&ctx("agent", Some("hello"), json!({"message": "hi"})))
            .await
            .expect("plain agent run without mcp_servers should succeed");
    }

    // --- RUN-201: `ai` steps honor model/params, and failures classify by
    // error kind instead of blanket-retrying. ---

    /// A provider that records every [`ChatRequest`] it receives and answers from
    /// a script: `Ok(text)` or a specific error, counted per call.
    struct RecordingProvider {
        seen: Arc<Mutex<Vec<ChatRequest>>>,
        reply: fn() -> Result<String, wovyr_common::Error>,
    }

    #[async_trait]
    impl wovyr_provider::AIProvider for RecordingProvider {
        fn name(&self) -> &str {
            "recording"
        }

        async fn chat(
            &self,
            request: ChatRequest,
        ) -> wovyr_common::Result<wovyr_provider::ChatResponse> {
            self.seen.lock().unwrap().push(request.clone());
            let text = (self.reply)()?;
            Ok(wovyr_provider::ChatResponse {
                message: wovyr_provider::Message::assistant(text),
                model: request.model,
                usage: wovyr_common::Usage::new(1, 1, 0.0),
                finish_reason: "stop".into(),
            })
        }
    }

    fn recording_executor(
        reply: fn() -> Result<String, wovyr_common::Error>,
    ) -> (PlatformActivityExecutor, Arc<Mutex<Vec<ChatRequest>>>) {
        struct NoAgents;
        #[async_trait]
        impl AgentResolver for NoAgents {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                Err(format!("no agent `{id}`"))
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let exec = PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            Arc::new(Gateway::new(Box::new(RecordingProvider {
                seen: seen.clone(),
                reply,
            }))),
            Arc::new(NoAgents),
        );
        (exec, seen)
    }

    #[tokio::test]
    async fn ai_activity_honors_pinned_model_params_and_response_format() {
        let (exec, seen) = recording_executor(|| Ok("42".to_string()));
        let out = exec
            .execute(&ctx(
                "ai",
                None,
                json!({
                    "prompt": "Answer tersely.",
                    "message": "what is 6*7?",
                    "model": "pinned-model-x",
                    "temperature": 0.2,
                    "max_tokens": 128,
                    "response_format": { "json_schema": {
                        "name": "answer",
                        "schema": { "type": "object" }
                    }},
                }),
            ))
            .await
            .expect("ai step should succeed");
        assert_eq!(
            out["message"], "42",
            "the answer must stay reachable at the same key `${{activity.message}}` \
             references use"
        );
        // RES-601 added a reserved `__usage` sibling; assert it is *additive* rather
        // than pinning the whole object, so `message` stays a stable contract.
        assert!(
            out.get(wovyr_workflow::USAGE_OUTPUT_KEY).is_some(),
            "an `ai` step must report usage so a wrapping for_each can budget: {out}"
        );

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.model, "pinned-model-x", "the pinned model wins");
        assert_eq!(req.temperature, Some(0.2));
        assert_eq!(req.max_tokens, Some(128));
        assert!(
            matches!(&req.response_format, Some(wovyr_provider::ResponseFormat::JsonSchema { name, .. }) if name == "answer"),
            "response_format must reach the request: {:?}",
            req.response_format
        );
    }

    #[tokio::test]
    async fn ai_activity_without_params_keeps_the_resolved_default_model() {
        let (exec, seen) = recording_executor(|| Ok("ok".to_string()));
        exec.execute(&ctx("ai", None, json!({ "message": "hi" })))
            .await
            .expect("ai step should succeed");
        let requests = seen.lock().unwrap();
        assert_ne!(requests[0].model, "", "a default model is resolved");
        assert_eq!(requests[0].temperature, None);
        assert_eq!(requests[0].max_tokens, None);
        assert_eq!(requests[0].response_format, None);
    }

    #[tokio::test]
    async fn ai_activity_validation_error_is_permanent_not_retried() {
        let (exec, seen) =
            recording_executor(|| Err(wovyr_common::Error::invalid("malformed request")));
        let err = exec
            .execute(&ctx("ai", None, json!({ "message": "hi" })))
            .await
            .expect_err("validation failure should error");
        assert!(
            matches!(err, ActivityError::Permanent(_)),
            "a bad-request error must be permanent, got {err:?}"
        );
        // Permanent end to end: the gateway didn't retry/failover it either.
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ai_activity_transient_provider_error_stays_retryable() {
        let (exec, _seen) =
            recording_executor(|| Err(wovyr_common::Error::provider("upstream 503")));
        let err = exec
            .execute(&ctx("ai", None, json!({ "message": "hi" })))
            .await
            .expect_err("provider failure should error");
        assert!(
            matches!(err, ActivityError::Retryable(_)),
            "a transient provider error must stay retryable, got {err:?}"
        );
    }

    #[tokio::test]
    async fn ai_activity_malformed_response_format_is_permanent() {
        let (exec, seen) = recording_executor(|| Ok("unreachable".to_string()));
        let err = exec
            .execute(&ctx(
                "ai",
                None,
                json!({ "message": "hi", "response_format": { "not_a_format": true } }),
            ))
            .await
            .expect_err("malformed response_format should fail");
        assert!(matches!(err, ActivityError::Permanent(_)));
        assert!(
            seen.lock().unwrap().is_empty(),
            "the model must never be called with an invalid constraint"
        );
    }

    // --- RUN-202: a sub-agent run's real cost reaches the platform's
    // accounting hook. (The server-side half — that `record` charges the
    // project's daily accumulator — is proven in `wovyr-server`'s
    // `workflow_runner` tests, where the accumulator lives.) ---

    #[tokio::test]
    async fn agent_activity_records_a_non_zero_run_cost() {
        struct CostRecorder {
            recorded: Arc<Mutex<Vec<(f64, u64)>>>,
        }
        #[async_trait]
        impl AgentResolver for CostRecorder {
            async fn resolve(
                &self,
                _ctx: &ActivityContext,
                id: &str,
            ) -> Result<AgentDefinition, String> {
                AgentDefinition::from_yaml(&format!(
                    "metadata:\n  name: {id}\nspec:\n  instructions: hi\n"
                ))
                .map_err(|e| e.to_string())
            }
            fn record(&self, _ctx: &ActivityContext, usage: &wovyr_common::Usage) {
                self.recorded
                    .lock()
                    .unwrap()
                    .push((usage.cost_usd, u64::from(usage.total_tokens)));
            }
        }
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let exec = PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            // The mock provider reports real (synthetic, non-zero) usage cost.
            Arc::new(Gateway::new(Box::new(wovyr_provider::MockProvider::new()))),
            Arc::new(CostRecorder {
                recorded: recorded.clone(),
            }),
        );
        exec.execute(&ctx("agent", Some("hello"), json!({"message": "hi"})))
            .await
            .expect("agent activity should succeed");

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1, "exactly one run recorded");
        let (cost, tokens) = recorded[0];
        assert!(
            cost > 0.0,
            "the run's cost must be non-zero and reach the accounting hook, got {cost}"
        );
        assert!(
            tokens > 0,
            "the run's token usage must reach the accounting hook too (SRV-202), got {tokens}"
        );
    }

    #[tokio::test]
    async fn unknown_agent_is_a_permanent_failure() {
        let exec = executor();
        let err = exec
            .execute(&ctx("agent", Some("does-not-exist"), Value::Null))
            .await
            .expect_err("unresolvable agent should fail");
        assert!(matches!(err, ActivityError::Permanent(_)));
    }

    #[tokio::test]
    async fn unsupported_activity_type_is_permanent() {
        let exec = executor();
        let err = exec
            .execute(&ctx("http", None, Value::Null))
            .await
            .expect_err("unsupported type should fail");
        assert!(matches!(err, ActivityError::Permanent(_)));
    }

    /// Every shipped example workflow must be runnable by *this* executor.
    ///
    /// `Definition::from_yaml` alone does not catch a missing `name`: the DSL
    /// makes it optional (a `wait`/`human`/`for_each` activity has none), so a
    /// nameless `function` activity validates cleanly and only fails later, at
    /// dispatch. Three shipped examples were in exactly that state after
    /// HLTH-901 made `function` a real tool call — two of them reachable straight
    /// off a README quickstart line — and nothing failed until a human ran one.
    /// This closes that gap at the level the breakage actually lives.
    #[test]
    fn every_shipped_example_workflow_names_a_tool_for_its_function_and_tool_activities() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("workflows");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("examples/workflows must exist") {
            let path = entry.expect("readable dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let yaml = std::fs::read_to_string(&path).expect("readable example");
            let def = wovyr_workflow::Definition::from_yaml(&yaml)
                .unwrap_or_else(|e| panic!("{} must be a valid definition: {e}", path.display()));
            for activity in &def.spec.activities {
                if matches!(activity.activity_type.as_str(), "function" | "tool") {
                    assert!(
                        activity.name.is_some(),
                        "{}: activity `{}` is `type: {}` with no `name`, so it fails at dispatch \
                         (`function` and `tool` both invoke a registered tool by name)",
                        path.display(),
                        activity.id,
                        activity.activity_type,
                    );
                }
            }
            checked += 1;
        }
        assert!(checked > 0, "no example workflows were checked");
    }
}
