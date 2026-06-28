//! Chaos/resilience test at the agent level: the steady-state hypothesis from the
//! [chaos spec](../../../docs/15-testing/chaos-testing.md#2-hypothesis-driven-approach)
//! — "agent runs succeed" — must hold through a provider outage while a healthy
//! failover provider exists.

use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::{Error, Result, Usage};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message};
use apex_tools::ToolRegistry;
use async_trait::async_trait;
use serde_json::json;

/// A provider that is either always-down (transient) or healthy.
struct FaultProvider {
    name: &'static str,
    healthy: bool,
}

#[async_trait]
impl AIProvider for FaultProvider {
    fn name(&self) -> &str {
        self.name
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        if !self.healthy {
            return Err(Error::provider("503 service unavailable"));
        }
        // A plain assistant turn (no tool calls) ends the agent loop in one step.
        Ok(ChatResponse {
            message: Message::assistant("handled by the healthy provider"),
            model: request.model,
            usage: Usage::new(5, 4, 0.02),
            finish_reason: "stop".to_string(),
        })
    }
}

#[tokio::test]
async fn agent_run_survives_primary_provider_outage() {
    let def = AgentDefinition::from_yaml(
        "metadata:\n  name: resilient\nspec:\n  instructions: Be helpful.\n",
    )
    .unwrap();

    // Primary is down; the gateway must fail over to the healthy secondary.
    let gateway = Gateway::with_providers(vec![
        Box::new(FaultProvider {
            name: "primary",
            healthy: false,
        }),
        Box::new(FaultProvider {
            name: "secondary",
            healthy: true,
        }),
    ]);
    let registry = ToolRegistry::with_builtins();

    let out = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({"message": "hello"})),
        &mut NullSink,
    )
    .await
    .expect("agent run should succeed despite the primary outage");

    assert_eq!(out.text, "handled by the healthy provider");
    assert!(out.usage.total_tokens > 0);
}
