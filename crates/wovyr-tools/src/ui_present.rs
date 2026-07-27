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
//! all (see `wovyr-server`'s `RunStore` doc comment: "an agent run has no
//! checkpoint to resume from... not resumable"). This tool's `execute`
//! simply awaits [`UiInteraction::present`] in place — there is no
//! crash-survives-the-decision guarantee the way the workflow `ui` activity
//! gives (durable suspend/resume across a restart). A host that needs that
//! guarantee should model the interaction as a workflow instead.
//!
//! The trust layer still applies (ADR-0011: the layer is never optional).
//! [`UiPresentTool`] evaluates every frame through a configured
//! [`wovyr_ui_guard::UiPolicy`], or the [`wovyr_ui_guard::hosted_floor`]
//! default (deny interactive) when none is set — mirroring GRD-207 — unless
//! explicitly marked [`UiPresentTool::unrestricted`] (the same trusted-
//! first-party escape hatch `WOVYR_UNRESTRICTED_UI` is server-side). And
//! whatever [`UiInteraction::present`] returns is still validated against
//! the frame it answered (HIL-302) before reaching the model — an
//! interaction implementation is arbitrary caller code, not a trusted
//! boundary, so its output gets no more benefit of the doubt than an HTTP
//! request body would.

use crate::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use wovyr_ui::{UiDecision, UiFrame, validate_decision};
use wovyr_ui_guard::{UiPolicy, Verdict, evaluate, hosted_floor};

/// A host's mechanism for actually showing a human a validated frame and
/// getting a decision back. Implemented by e.g. the CLI's stdin-based
/// prompt (`wovyr-cli`'s `StdinUiPresenter`); a hosted-server equivalent is a
/// later slice — P1's `ui` workflow activity already covers the durable,
/// hosted case for workflows.
#[async_trait]
pub trait UiInteraction: Send + Sync {
    /// Present `frame` and return the human's decision. An `Err` aborts the
    /// tool call (surfaces to the model as a tool error, per the workspace's
    /// standard tool-error handling) rather than fabricating a decision.
    async fn present(&self, frame: &UiFrame) -> wovyr_common::Result<UiDecision>;
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

    /// The trusted-first-party escape hatch (mirrors `WOVYR_UNRESTRICTED_UI`):
    /// frames pass protocol validation only, no policy/floor check. Intended
    /// for a genuinely trusted local/CLI run, never a hosted default.
    pub fn unrestricted(mut self) -> Self {
        self.unrestricted = true;
        self
    }
}

/// The `frame` parameter's JSON Schema, derived from [`UiFrame`]'s own
/// `schemars::JsonSchema` impl (GUI-501) rather than the hand-written,
/// prose-only stub this tool shipped with originally. PT-19's real-model test
/// (OpenRouter gpt-4o-mini, 10 generated frames) found **10/10** failed
/// `UiFrame::from_value`'s fail-closed parse when the model was only told
/// "it's an object" — the trust layer's rejection was correct, but no real
/// model can drive `ui_present` without seeing the actual component
/// vocabulary and required fields to draw from.
fn ui_frame_tool_schema() -> Value {
    let mut frame_schema = serde_json::to_value(schemars::schema_for!(UiFrame))
        .expect("UiFrame's derived JsonSchema always serializes to JSON");
    // `schemars` hoists recursive/shared types (UiNode, ActionClass, ...) into
    // a `$defs` map at the schema's *own* root and points to them via
    // `"$ref": "#/$defs/Name"`. A `$ref` with no `$id` in scope resolves
    // against the whole document root by JSON Pointer — not against wherever
    // it's nested — so `$defs` must be hoisted again, up to *this* tool
    // schema's root, for those refs to resolve once `frame_schema` is
    // embedded under `properties.frame` below.
    let defs = frame_schema
        .as_object_mut()
        .and_then(|obj| obj.remove("$defs"));
    if let Some(obj) = frame_schema.as_object_mut() {
        // Meaningful only at a schema's own root; embedded under
        // `properties.frame` they'd just be misleading (a `$schema` dialect
        // marker / `title` that both describe the outer tool schema, not the
        // `frame` field).
        obj.remove("$schema");
        obj.remove("title");
    }
    let mut schema = json!({
        "type": "object",
        "required": ["frame"],
        "properties": { "frame": frame_schema },
    });
    if let Some(defs) = defs {
        schema
            .as_object_mut()
            .expect("schema is freshly built as an object")
            .insert("$defs".to_string(), defs);
    }
    schema
}

#[async_trait]
impl Tool for UiPresentTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "ui_present",
            "1.0.0",
            "interaction",
            "Present a generative-UI frame to the human and wait for their decision. The \
             `frame` parameter must conform to this tool's input schema — the real UiFrame/ \
             UiNode component vocabulary — not free-form HTML/markup. Returns the human's \
             decision as `{action, values}`.",
        )
        // GUI-501: this tool's schema *is* the protocol contract, not a loose
        // suggestion — a model that drifts from it produces a frame
        // `UiFrame::from_value` will reject outright. Worth the vendor
        // schema-normalization pass (PRV-202/203) where the resolved provider
        // supports constrained decoding against it.
        .with_strict(true)
    }

    fn input_schema(&self) -> Value {
        ui_frame_tool_schema()
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

/// A minimal JSON-Schema-subset evaluator, scoped to exactly the keywords
/// `ui_frame_tool_schema()` emits (`$ref`/`$defs`, `oneOf`, `type`, `const`,
/// `enum`, `properties`/`required`/`additionalProperties`, `items`) — not a
/// general-purpose validator. It exists to prove the derived schema is a
/// real, mechanically-checkable contract (GUI-501's acceptance criterion),
/// without pulling in an external JSON-Schema crate for one test module.
#[cfg(test)]
fn schema_validates(defs: &Value, schema: &Value, instance: &Value) -> bool {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let name = r
            .strip_prefix("#/$defs/")
            .expect("this schema only ever emits local $defs refs");
        return schema_validates(defs, &defs[name], instance);
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        return one_of.iter().any(|s| schema_validates(defs, s, instance));
    }
    if let Some(ty) = schema.get("type") {
        let types: Vec<&str> = match ty {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let ok = types.iter().any(|t| match *t {
            "object" => instance.is_object(),
            "array" => instance.is_array(),
            "string" => instance.is_string(),
            "number" | "integer" => instance.is_number(),
            "boolean" => instance.is_boolean(),
            "null" => instance.is_null(),
            _ => false,
        });
        if !ok {
            return false;
        }
    }
    if let Some(c) = schema.get("const")
        && instance != c
    {
        return false;
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(instance)
    {
        return false;
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        let Some(obj) = instance.as_object() else {
            return false;
        };
        if schema.get("additionalProperties") == Some(&Value::Bool(false))
            && obj.keys().any(|k| !props.contains_key(k))
        {
            return false;
        }
        for (name, subschema) in props {
            if let Some(v) = obj.get(name)
                && !schema_validates(defs, subschema, v)
            {
                return false;
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let Some(obj) = instance.as_object() else {
            return false;
        };
        if !required
            .iter()
            .filter_map(Value::as_str)
            .all(|k| obj.contains_key(k))
        {
            return false;
        }
    }
    if let Some(items) = schema.get("items")
        && let Some(arr) = instance.as_array()
        && !arr.iter().all(|item| schema_validates(defs, items, item))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolRequest;
    use serde_json::json;
    use wovyr_ui_guard::PolicyRules;

    struct FixedInteraction(UiDecision);

    #[async_trait]
    impl UiInteraction for FixedInteraction {
        async fn present(&self, _frame: &UiFrame) -> wovyr_common::Result<UiDecision> {
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

    // --- GUI-501: the tool schema must teach a real model the UiFrame vocabulary ---

    #[test]
    fn metadata_requests_strict_schema_constrained_tool_calling() {
        let tool = UiPresentTool::new(Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: Default::default(),
        })));
        assert!(
            tool.metadata().strict,
            "ui_present should opt into strict/PRV-202 tool calling"
        );
    }

    #[test]
    fn input_schema_is_derived_from_the_real_uiframe_vocabulary_not_a_hand_written_stub() {
        let tool = UiPresentTool::new(Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: Default::default(),
        })));
        let schema = tool.input_schema();

        // The old stub was `{"type": "object", "properties": {"frame": {"type":
        // "object", "description": "..."}}}` — no `$defs`, no nested
        // `properties`/`required` at all. Any of these presence checks alone
        // is the concrete regression guard.
        assert!(
            schema.get("$defs").is_some(),
            "$defs must be hoisted to the tool schema root"
        );
        let frame_schema = &schema["properties"]["frame"];
        assert_eq!(frame_schema["required"], json!(["schema_version", "root"]));

        let ui_node_variants = schema["$defs"]["UiNode"]["oneOf"]
            .as_array()
            .expect("UiNode is a oneOf over the component vocabulary");
        let button = ui_node_variants
            .iter()
            .find(|v| v["properties"]["type"]["const"] == json!("button"))
            .expect("the button variant must be present in the derived schema");
        assert_eq!(
            button["required"],
            json!(["type", "action", "label"]),
            "a schema-aware model is told `action` is required on a button"
        );
    }

    #[test]
    fn derived_schema_accepts_a_valid_frame_and_rejects_pt19s_button_missing_action() {
        let tool = UiPresentTool::new(Arc::new(FixedInteraction(UiDecision {
            action: "ok".into(),
            values: Default::default(),
        })));
        let schema = tool.input_schema();
        let defs = &schema["$defs"];
        let frame_schema = &schema["properties"]["frame"];

        assert!(
            schema_validates(defs, frame_schema, &confirm_frame()),
            "a real, protocol-valid frame must validate against the derived schema"
        );

        // PT-19's concrete invalid shape: a `button` node with no `action`.
        let button_missing_action = json!({
            "schema_version": "1.0.0",
            "root": { "type": "button", "label": "Go" }
        });
        assert!(
            !schema_validates(defs, frame_schema, &button_missing_action),
            "a button missing `action` must fail as a schema-shape mismatch"
        );
    }
}
