//! The `ui_present` built-in tool (PRD-005 HIL-304): lets a **bare agent
//! run** — not only a workflow's `ui` activity (RM-GUI-P1) — present a
//! generative-UI frame to a human and receive a validated decision.
//!
//! **Never part of [`crate::ToolRegistry::with_builtins`].** Presenting UI
//! requires a caller-supplied [`UiInteraction`] to actually reach a human;
//! there is no safe default (silently rendering nowhere, or silently
//! auto-approving, are both worse than the tool not existing). A caller
//! wires it in explicitly via [`crate::ToolRegistry::register`].
//!
//! **Durability note.** A bare agent run has no checkpoint to resume from at
//! all (see `apex-server`'s `RunStore` doc comment: "an agent run has no
//! checkpoint to resume from... not resumable"). This tool's `execute`
//! simply awaits [`UiInteraction::present`] in place — there is no
//! crash-survives-the-decision guarantee the way the workflow `ui` activity
//! gives (durable suspend/resume across a restart). A host that needs that
//! guarantee should model the interaction as a workflow instead.
//!
//! The trust layer still applies (ADR-0011: the layer is never optional).
//! [`UiPresentTool`] evaluates every frame through a configured
//! [`apex_ui_guard::UiPolicy`], or the [`apex_ui_guard::hosted_floor`]
//! default (deny interactive) when none is set — mirroring GRD-207 — unless
//! explicitly marked [`UiPresentTool::unrestricted`] (the same trusted-
//! first-party escape hatch `APEX_UNRESTRICTED_UI` is server-side). And
//! whatever [`UiInteraction::present`] returns is still validated against
//! the frame it answered (HIL-302) before reaching the model — an
//! interaction implementation is arbitrary caller code, not a trusted
//! boundary, so its output gets no more benefit of the doubt than an HTTP
//! request body would.

use crate::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
use apex_ui::{UiDecision, UiFrame, validate_decision};
use apex_ui_guard::{UiPolicy, Verdict, evaluate, hosted_floor};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// A host's mechanism for actually showing a human a validated frame and
/// getting a decision back. Implemented by e.g. the CLI's stdin-based
/// prompt (`apex-cli`'s `StdinUiPresenter`); a hosted-server equivalent is a
/// later slice — P1's `ui` workflow activity already covers the durable,
/// hosted case for workflows.
#[async_trait]
pub trait UiInteraction: Send + Sync {
    /// Present `frame` and return the human's decision. An `Err` aborts the
    /// tool call (surfaces to the model as a tool error, per the workspace's
    /// standard tool-error handling) rather than fabricating a decision.
    async fn present(&self, frame: &UiFrame) -> apex_common::Result<UiDecision>;
}

/// The `ui_present` tool. See the module docs for the trust-layer and
/// durability posture.
pub struct UiPresentTool {
    interaction: Arc<dyn UiInteraction>,
    policy: Option<UiPolicy>,
    unrestricted: bool,
}

impl UiPresentTool {
    /// A tool that applies the hosted-floor default (GRD-207: interactive
    /// frames denied) until [`with_policy`](Self::with_policy) or
    /// [`unrestricted`](Self::unrestricted) says otherwise.
    pub fn new(interaction: Arc<dyn UiInteraction>) -> Self {
        Self {
            interaction,
            policy: None,
            unrestricted: false,
        }
    }

    /// Evaluate every presented frame against `policy` instead of the
    /// hosted floor.
    pub fn with_policy(mut self, policy: UiPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The trusted-first-party escape hatch (mirrors `APEX_UNRESTRICTED_UI`):
    /// frames pass protocol validation only, no policy/floor check. Intended
    /// for a genuinely trusted local/CLI run, never a hosted default.
    pub fn unrestricted(mut self) -> Self {
        self.unrestricted = true;
        self
    }
}

#[async_trait]
impl Tool for UiPresentTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "ui_present",
            "1.0.0",
            "interaction",
            "Present a generative-UI frame to the human and wait for their decision. The \
             `frame` parameter is a UiFrame document (schema_version, optional title, and a \
             `root` component tree — see the generative-UI protocol docs). Returns the human's \
             decision as `{action, values}`.",
        )
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["frame"],
            "properties": {
                "frame": {
                    "type": "object",
                    "description": "A UiFrame document: {schema_version, title?, root}."
                }
            }
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let Some(frame_value) = request.parameters.get("frame") else {
            return Err(ToolError::Validation(
                "missing required `frame` field".into(),
            ));
        };
        let frame = UiFrame::from_value(frame_value)
            .map_err(|e| ToolError::Validation(format!("invalid ui frame: {e}")))?;

        let verdict = if self.unrestricted {
            Verdict::Allow
        } else if let Some(policy) = &self.policy {
            evaluate(policy, &frame)
        } else {
            hosted_floor(&frame)
        };
        let frame = match verdict {
            Verdict::Allow => frame,
            Verdict::Redact { frame, .. } => *frame,
            Verdict::Block { rule, reason } => {
                return Err(ToolError::PermissionDenied(format!(
                    "ui frame blocked by policy rule `{rule}`: {reason}"
                )));
            }
        };

        let decision = self
            .interaction
            .present(&frame)
            .await
            .map_err(|e| ToolError::Internal(format!("ui interaction failed: {e}")))?;
        // HIL-302, applied even to a trusted interaction implementation's own
        // output — see the module doc comment's "belt and suspenders" note.
        validate_decision(&frame, &decision)
            .map_err(|e| ToolError::Validation(format!("invalid decision: {e}")))?;

        Ok(ToolResponse::success(json!({
            "action": decision.action,
            "values": decision.values,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolRequest;
    use apex_ui_guard::PolicyRules;
    use serde_json::json;

    struct FixedInteraction(UiDecision);

    #[async_trait]
    impl UiInteraction for FixedInteraction {
        async fn present(&self, _frame: &UiFrame) -> apex_common::Result<UiDecision> {
            Ok(self.0.clone())
        }
    }

    fn confirm_frame() -> Value {
        json!({
            "schema_version": "1.0.0",
            "root": {
                "type": "column",
                "children": [
                    { "type": "text_input", "name": "note", "label": "Note" },
                    { "type": "button", "action": "ok", "label": "OK", "class": "confirm" }
                ]
            }
        })
    }

    #[tokio::test]
    async fn presents_and_returns_a_valid_decision() {
        let interaction = Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: [("note".to_string(), json!("hi"))].into_iter().collect(),
        }));
        let tool = UiPresentTool::new(interaction).unrestricted();
        let resp = tool
            .execute(
                &ToolContext::default(),
                ToolRequest::new(json!({ "frame": confirm_frame() })),
            )
            .await
            .expect("tool call succeeds");
        assert!(resp.success);
        assert_eq!(resp.payload["action"], json!("ok"));
        assert_eq!(resp.payload["values"]["note"], json!("hi"));
    }

    #[tokio::test]
    async fn hosted_floor_denies_an_interactive_frame_with_no_policy_configured() {
        let interaction = Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: Default::default(),
        }));
        let tool = UiPresentTool::new(interaction); // no policy, not unrestricted
        let err = tool
            .execute(
                &ToolContext::default(),
                ToolRequest::new(json!({ "frame": confirm_frame() })),
            )
            .await
            .expect_err("hosted floor should deny an interactive frame");
        assert!(matches!(err, ToolError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn an_out_of_vocabulary_decision_from_the_interaction_is_rejected() {
        // Even a "trusted" interaction implementation's output is validated
        // (HIL-302's belt-and-suspenders stance) — this one hands back an
        // action the frame never declared.
        let interaction = Arc::new(FixedInteraction(UiDecision {
            action: "launch_missiles".into(),
            values: Default::default(),
        }));
        let tool = UiPresentTool::new(interaction).unrestricted();
        let err = tool
            .execute(
                &ToolContext::default(),
                ToolRequest::new(json!({ "frame": confirm_frame() })),
            )
            .await
            .expect_err("undeclared action must be rejected");
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn a_configured_policy_still_blocks_a_sensitive_input() {
        let interaction = Arc::new(FixedInteraction(UiDecision {
            action: "pay".into(),
            values: Default::default(),
        }));
        let policy = UiPolicy {
            name: "test".into(),
            version: 1,
            rules: PolicyRules::default(),
        };
        let tool = UiPresentTool::new(interaction).with_policy(policy);
        let card_frame = json!({
            "schema_version": "1.0.0",
            "root": {
                "type": "column",
                "children": [
                    { "type": "text_input", "name": "card_number", "label": "Card number" },
                    { "type": "button", "action": "pay", "label": "Pay", "class": "submit" }
                ]
            }
        });
        let err = tool
            .execute(
                &ToolContext::default(),
                ToolRequest::new(json!({ "frame": card_frame })),
            )
            .await
            .expect_err("sensitive-input rule should block");
        assert!(matches!(err, ToolError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn a_malformed_frame_is_a_validation_error() {
        let interaction = Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: Default::default(),
        }));
        let tool = UiPresentTool::new(interaction).unrestricted();
        let err = tool
            .execute(
                &ToolContext::default(),
                ToolRequest::new(json!({ "frame": { "type": "html", "content": "<script>" } })),
            )
            .await
            .expect_err("a frame missing schema_version/root is invalid");
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
