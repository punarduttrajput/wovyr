//! Integration test for the agent tool-calling loop.
//!
//! Drives the real [`run_agent`] loop with the real [`ToolRegistry`] and built-in
//! `echo` tool, using a *scripted* provider that requests a tool on its first turn
//! and answers on its second. This exercises the full
//! model → tool → model → respond cycle end to end
//! ([Agent Runtime spec §14](../../../docs/03-workflow-engine/agent-runtime.md)) —
//! the core path the offline mock provider deliberately does not trigger.

use apex_agent::{AgentDefinition, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::{Result, Usage};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role, ToolCall};
use apex_tools::{
    Tool, ToolContext, ToolError, ToolMetadata, ToolRegistry, ToolRequest, ToolResponse,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Serializes the two SEC-303 tests that read/mutate the process-global
/// `APEX_UNRESTRICTED_TOOLS` env var, so they can't race each other. `tokio::sync`
/// (not `std::sync`) since both tests hold the guard across an `.await`.
static UNRESTRICTED_TOOLS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A provider whose behavior is fixed for the test: ask for the `echo` tool once,
/// then summarize the tool result. Deterministic — no clocks, no randomness.
struct ScriptedProvider {
    calls: AtomicUsize,
}

impl ScriptedProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AIProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        // Once a tool result is in the history, produce a final answer.
        if let Some(tool_msg) = request.messages.iter().rev().find(|m| m.role == Role::Tool) {
            let observed = tool_msg.content.clone().unwrap_or_default();
            return Ok(ChatResponse {
                message: Message::assistant(format!("done: {observed}")),
                model: request.model,
                usage: Usage::new(5, 5, 0.0),
                finish_reason: "stop".to_string(),
            });
        }

        // First turn: request the echo tool.
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "echo".to_string(),
                    arguments: json!({ "ping": "pong" }).to_string(),
                }],
                tool_call_id: None,
                name: None,
            },
            model: request.model,
            usage: Usage::new(5, 0, 0.0),
            finish_reason: "tool_calls".to_string(),
        })
    }
}

/// A sink that records events as simple strings so we can assert on the stream.
#[derive(Default)]
struct Capture {
    events: Vec<String>,
}

impl RunEventSink for Capture {
    fn emit(&mut self, event: RunEvent<'_>) {
        let line = match event {
            RunEvent::Start { provider, .. } => format!("start:{provider}"),
            RunEvent::MemoryRetrieved { source, .. } => format!("memory:{source}"),
            RunEvent::Delta { .. } => "delta".to_string(),
            RunEvent::ToolCall { name, .. } => format!("toolcall:{name}"),
            RunEvent::ToolResult { name, ok } => format!("toolresult:{name}:{ok}"),
            RunEvent::Done { .. } => "done".to_string(),
        };
        self.events.push(line);
    }
}

fn tool_agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: tooler\nspec:\n  instructions: Use tools to answer.\n  tools: [echo]\n",
    )
    .unwrap()
}

#[tokio::test]
async fn agent_completes_a_tool_round_trip() {
    let def = tool_agent();
    let gateway = Gateway::new(Box::new(ScriptedProvider::new()));
    let registry = ToolRegistry::with_builtins();

    let mut capture = Capture::default();
    let out = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({ "message": "go" })),
        &mut capture,
    )
    .await
    .unwrap();

    // Two model calls: one to request the tool, one to answer.
    assert_eq!(out.steps, 2, "expected a model → tool → model round trip");
    // The final answer incorporates the echo tool's returned payload.
    assert!(out.text.starts_with("done:"), "got: {}", out.text);
    assert!(
        out.text.contains("ping"),
        "tool payload should reach the model: {}",
        out.text
    );
    // Usage accumulated across both calls (5+0 then 5+5).
    assert_eq!(out.usage.total_tokens, 15);

    // The event stream reflects the tool call and its successful result.
    assert!(capture.events.contains(&"start:scripted".to_string()));
    assert!(capture.events.contains(&"toolcall:echo".to_string()));
    assert!(capture.events.contains(&"toolresult:echo:true".to_string()));
    assert!(capture.events.contains(&"delta".to_string()));
    assert!(capture.events.contains(&"done".to_string()));
}

/// A tool that records the tenant it sees on its [`ToolContext`] — so we can assert the
/// run's tenant is threaded all the way to tool execution (the input a plugin tool needs
/// to resolve tenant-scoped secrets).
struct RecordingTool {
    seen_tenant: Arc<Mutex<String>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new("recorder", "1.0.0", "test", "records the tenant it sees")
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        _request: ToolRequest,
    ) -> std::result::Result<ToolResponse, ToolError> {
        *self.seen_tenant.lock().unwrap() = ctx.tenant.clone();
        Ok(ToolResponse::success(json!({ "ok": true })))
    }
}

/// A provider that calls the `recorder` tool once, then answers.
struct CallsRecorder {
    calls: AtomicUsize,
}

#[async_trait]
impl AIProvider for CallsRecorder {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.messages.iter().any(|m| m.role == Role::Tool) {
            return Ok(ChatResponse {
                message: Message::assistant("done"),
                model: request.model,
                usage: Usage::new(1, 1, 0.0),
                finish_reason: "stop".to_string(),
            });
        }
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "recorder".to_string(),
                    arguments: "{}".to_string(),
                }],
                tool_call_id: None,
                name: None,
            },
            model: request.model,
            usage: Usage::new(1, 0, 0.0),
            finish_reason: "tool_calls".to_string(),
        })
    }
}

#[tokio::test]
async fn run_tenant_is_threaded_to_tool_context() {
    let def = AgentDefinition::from_yaml(
        "metadata:\n  name: tooler\nspec:\n  instructions: Use tools.\n  tools: [recorder]\n",
    )
    .unwrap();
    let gateway = Gateway::new(Box::new(CallsRecorder {
        calls: AtomicUsize::new(0),
    }));
    let seen = Arc::new(Mutex::new(String::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RecordingTool {
        seen_tenant: seen.clone(),
    }));

    let mut capture = Capture::default();
    run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({ "message": "go" })).with_tenant("acme"),
        &mut capture,
    )
    .await
    .unwrap();

    // The run's tenant reached the tool's execution context.
    assert_eq!(*seen.lock().unwrap(), "acme");
}

#[tokio::test]
async fn run_loop_terminates_on_step_budget() {
    // A provider that always asks for a tool would loop forever without the budget.
    struct AlwaysToolProvider;
    #[async_trait]
    impl AIProvider for AlwaysToolProvider {
        fn name(&self) -> &str {
            "always-tool"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "c".to_string(),
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    }],
                    tool_call_id: None,
                    name: None,
                },
                model: request.model,
                usage: Usage::new(1, 0, 0.0),
                finish_reason: "tool_calls".to_string(),
            })
        }
    }

    let def = tool_agent();
    let gateway = Gateway::new(Box::new(AlwaysToolProvider));
    let registry = ToolRegistry::with_builtins();

    let mut opts = RunOptions::new(json!("loop"));
    opts.max_steps = 3;

    let err = run_agent(&def, &gateway, &registry, opts, &mut Capture::default())
        .await
        .unwrap_err();
    assert!(matches!(err, apex_common::Error::Runtime(_)));
}

#[tokio::test]
async fn agent_denies_tool_requiring_an_ungranted_permission() {
    // A provider that requests `shell` (declares `shell.execute`), then answers.
    struct RequestsShell;
    #[async_trait]
    impl AIProvider for RequestsShell {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            if let Some(tool_msg) = request.messages.iter().rev().find(|m| m.role == Role::Tool) {
                let observed = tool_msg.content.clone().unwrap_or_default();
                return Ok(ChatResponse {
                    message: Message::assistant(format!("done: {observed}")),
                    model: request.model,
                    usage: Usage::new(1, 1, 0.0),
                    finish_reason: "stop".to_string(),
                });
            }
            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".to_string(),
                        name: "shell".to_string(),
                        arguments: json!({ "command": "echo hi" }).to_string(),
                    }],
                    tool_call_id: None,
                    name: None,
                },
                model: request.model,
                usage: Usage::new(1, 0, 0.0),
                finish_reason: "tool_calls".to_string(),
            })
        }
    }

    // The agent is granted only `net.egress`, so `shell` (needs `shell.execute`) is denied.
    let def = AgentDefinition::from_yaml(
        "metadata:\n  name: restricted\nspec:\n  instructions: x\n  tools: [shell]\n  permissions: [net.egress]\n",
    )
    .unwrap();
    let gateway = Gateway::new(Box::new(RequestsShell));
    // shell is opt-in (SEC-301) — this test exercises permission denial, not
    // registration, so it needs shell actually registered.
    let registry = ToolRegistry::with_privileged_builtins();

    let mut capture = Capture::default();
    let out = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({})),
        &mut capture,
    )
    .await
    .unwrap();

    // The tool was denied (not executed) and the model received the denial.
    assert!(
        capture
            .events
            .contains(&"toolresult:shell:false".to_string()),
        "expected a denied tool result, got {:?}",
        capture.events
    );
    assert!(
        out.text.contains("permission denied"),
        "model should see the permission denial, got: {}",
        out.text
    );
}

/// A provider that requests `fs_read` on `Cargo.toml` (present in every crate root),
/// then answers.
struct RequestsFsRead;
#[async_trait]
impl AIProvider for RequestsFsRead {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        if let Some(tool_msg) = request.messages.iter().rev().find(|m| m.role == Role::Tool) {
            let observed = tool_msg.content.clone().unwrap_or_default();
            return Ok(ChatResponse {
                message: Message::assistant(format!("done: {observed}")),
                model: request.model,
                usage: Usage::new(1, 1, 0.0),
                finish_reason: "stop".to_string(),
            });
        }
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "fs_read".to_string(),
                    arguments: json!({ "path": "Cargo.toml" }).to_string(),
                }],
                tool_call_id: None,
                name: None,
            },
            model: request.model,
            usage: Usage::new(1, 0, 0.0),
            finish_reason: "tool_calls".to_string(),
        })
    }
}

fn no_permissions_block_def() -> AgentDefinition {
    // Deliberately no `permissions:` block.
    AgentDefinition::from_yaml(
        "metadata:\n  name: unscoped\nspec:\n  instructions: x\n  tools: [fs_read]\n",
    )
    .unwrap()
}

/// RM-GA-P1 SEC-303: a hosted run's manifest with no `permissions:` block gets
/// **no** tool permissions — `fs_read` (which declares `filesystem.read`) is denied,
/// not silently allowed the way an unhosted (CLI/local/eval) run still is.
#[tokio::test]
async fn hosted_run_denies_a_permissioned_tool_when_manifest_has_no_permissions_block() {
    let _guard = UNRESTRICTED_TOOLS_ENV_LOCK.lock().await;
    let def = no_permissions_block_def();
    let gateway = Gateway::new(Box::new(RequestsFsRead));
    let registry = ToolRegistry::with_builtins();

    let mut capture = Capture::default();
    run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({})).with_hosted(true),
        &mut capture,
    )
    .await
    .unwrap();

    assert!(
        capture
            .events
            .contains(&"toolresult:fs_read:false".to_string()),
        "expected a denied tool result for a hosted run, got {:?}",
        capture.events
    );
}

/// The same manifest, run *unhosted* (the CLI/local/eval default) — still
/// unrestricted, preserving today's back-compat behavior.
#[tokio::test]
async fn unhosted_run_still_allows_the_same_manifest() {
    let def = no_permissions_block_def();
    let gateway = Gateway::new(Box::new(RequestsFsRead));
    let registry = ToolRegistry::with_builtins();

    let mut capture = Capture::default();
    run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({})),
        &mut capture,
    )
    .await
    .unwrap();

    assert!(
        capture
            .events
            .contains(&"toolresult:fs_read:true".to_string()),
        "expected the unhosted run to still allow the tool, got {:?}",
        capture.events
    );
}

/// `APEX_UNRESTRICTED_TOOLS=1` restores the old unrestricted behavior even for a
/// hosted run — the documented escape hatch for a trusted first-party deployment.
#[tokio::test]
async fn unrestricted_tools_escape_hatch_restores_old_behavior_for_hosted_runs() {
    let _guard = UNRESTRICTED_TOOLS_ENV_LOCK.lock().await;
    unsafe { std::env::set_var("APEX_UNRESTRICTED_TOOLS", "1") };

    let def = no_permissions_block_def();
    let gateway = Gateway::new(Box::new(RequestsFsRead));
    let registry = ToolRegistry::with_builtins();

    let mut capture = Capture::default();
    run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({})).with_hosted(true),
        &mut capture,
    )
    .await
    .unwrap();

    unsafe { std::env::remove_var("APEX_UNRESTRICTED_TOOLS") };

    assert!(
        capture
            .events
            .contains(&"toolresult:fs_read:true".to_string()),
        "expected the escape hatch to restore unrestricted access, got {:?}",
        capture.events
    );
}
