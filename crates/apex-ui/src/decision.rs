//! Typed human decisions over a presented frame (HIL-302).
//!
//! A [`UiDecision`] is only ever accepted against the frame it answers:
//! the action must be one the frame declared, every supplied value must
//! correspond to a declared input and satisfy its constraints, and unknown
//! value keys are rejected — fail-closed **at the API boundary**, so an
//! out-of-vocabulary decision is never delivered to a workflow.

use crate::frame::{UiFrame, UiNode};
use apex_common::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A human's answer to a presented [`UiFrame`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiDecision {
    /// The declared action taken — must match a `button.action` in the frame.
    pub action: String,
    /// Submitted input values, keyed by input `name`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, Value>,
}

/// Validate `decision` against the `frame` it answers (HIL-302), fail-closed:
///
/// - the action must be declared by one of the frame's buttons;
/// - every supplied value must name a declared input and match its type and
///   constraints (select membership, numeric bounds);
/// - unknown value keys are rejected;
/// - inputs marked `required` must be supplied — but only when the action's
///   class is affirmative ([`ActionClass::is_affirmative`](crate::ActionClass::is_affirmative)):
///   a cancel/reject must never be blocked by an empty form.
pub fn validate_decision(frame: &UiFrame, decision: &UiDecision) -> Result<()> {
    let actions = frame.actions();
    let Some((_, _, class)) = actions
        .iter()
        .find(|(action, _, _)| *action == decision.action)
        .copied()
    else {
        return Err(Error::Invalid(format!(
            "decision action `{}` is not declared by the frame (declared: {})",
            decision.action,
            if actions.is_empty() {
                "none".to_string()
            } else {
                actions
                    .iter()
                    .map(|(a, _, _)| format!("`{a}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )));
    };

    // Every supplied value must map to a declared input and satisfy it.
    for (name, value) in &decision.values {
        let Some(input) = find_input(frame, name) else {
            return Err(Error::Invalid(format!(
                "decision supplies a value for undeclared input `{name}`"
            )));
        };
        check_value(input, name, value)?;
    }

    // Affirmative actions must satisfy every required input.
    if class.is_affirmative() {
        for node in frame.nodes() {
            let (name, required) = match node {
                UiNode::TextInput { name, required, .. }
                | UiNode::NumberInput { name, required, .. }
                | UiNode::Select { name, required, .. } => (name, *required),
                _ => continue,
            };
            if required && !decision.values.contains_key(name) {
                return Err(Error::Invalid(format!(
                    "decision `{}` requires a value for input `{name}`",
                    decision.action
                )));
            }
        }
    }
    Ok(())
}

fn find_input<'a>(frame: &'a UiFrame, name: &str) -> Option<&'a UiNode> {
    frame.nodes().into_iter().find(|n| match n {
        UiNode::TextInput { name: n, .. }
        | UiNode::NumberInput { name: n, .. }
        | UiNode::Select { name: n, .. }
        | UiNode::Checkbox { name: n, .. } => n == name,
        _ => false,
    })
}

fn check_value(input: &UiNode, name: &str, value: &Value) -> Result<()> {
    match input {
        UiNode::TextInput { required, .. } => {
            let Some(text) = value.as_str() else {
                return Err(Error::Invalid(format!(
                    "input `{name}` expects a string value"
                )));
            };
            if *required && text.is_empty() {
                return Err(Error::Invalid(format!(
                    "required input `{name}` must be non-empty"
                )));
            }
        }
        UiNode::NumberInput { min, max, .. } => {
            let Some(n) = value.as_f64() else {
                return Err(Error::Invalid(format!(
                    "input `{name}` expects a numeric value"
                )));
            };
            if let Some(min) = min
                && n < *min
            {
                return Err(Error::Invalid(format!(
                    "input `{name}` value {n} is below the minimum {min}"
                )));
            }
            if let Some(max) = max
                && n > *max
            {
                return Err(Error::Invalid(format!(
                    "input `{name}` value {n} is above the maximum {max}"
                )));
            }
        }
        UiNode::Select { options, .. } => {
            let Some(chosen) = value.as_str() else {
                return Err(Error::Invalid(format!(
                    "input `{name}` expects a string option value"
                )));
            };
            if !options.iter().any(|o| o.value == chosen) {
                return Err(Error::Invalid(format!(
                    "input `{name}` value `{chosen}` is not one of the declared options"
                )));
            }
        }
        UiNode::Checkbox { .. } => {
            if !value.is_boolean() {
                return Err(Error::Invalid(format!(
                    "input `{name}` expects a boolean value"
                )));
            }
        }
        _ => {
            return Err(Error::Invalid(format!(
                "`{name}` does not accept a submitted value"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::SCHEMA_VERSION;
    use serde_json::json;

    fn frame() -> UiFrame {
        UiFrame::from_value(&json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "column", "children": [
                { "type": "text_input", "name": "po_number", "label": "PO number", "required": true },
                { "type": "number_input", "name": "qty", "label": "Quantity", "min": 1.0, "max": 10.0 },
                { "type": "select", "name": "speed", "label": "Shipping", "options": [
                    { "value": "ground", "label": "Ground" },
                    { "value": "air", "label": "Air" }
                ]},
                { "type": "checkbox", "name": "gift", "label": "Gift wrap" },
                { "type": "row", "children": [
                    { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
                    { "type": "button", "action": "cancel", "label": "Cancel", "class": "cancel" }
                ]}
            ]}
        }))
        .expect("valid frame")
    }

    fn decision(action: &str, values: Value) -> UiDecision {
        serde_json::from_value(json!({ "action": action, "values": values })).unwrap()
    }

    #[test]
    fn a_well_formed_affirmative_decision_validates() {
        let d = decision(
            "approve",
            json!({ "po_number": "PO-777", "qty": 3, "speed": "air", "gift": true }),
        );
        validate_decision(&frame(), &d).expect("valid decision");
    }

    #[test]
    fn undeclared_actions_and_unknown_value_keys_fail_closed() {
        let d = decision("launch_missiles", json!({}));
        assert!(validate_decision(&frame(), &d).is_err());

        let d = decision("approve", json!({ "po_number": "x", "smuggled": 1 }));
        let err = validate_decision(&frame(), &d).expect_err("unknown key");
        assert!(err.to_string().contains("undeclared input `smuggled`"));
    }

    #[test]
    fn required_inputs_bind_affirmative_actions_but_never_cancel() {
        // Approve without the required PO number: rejected.
        let d = decision("approve", json!({}));
        assert!(validate_decision(&frame(), &d).is_err());

        // Cancel with an empty form: always allowed.
        let d = decision("cancel", json!({}));
        validate_decision(&frame(), &d).expect("cancel never blocked by empty form");
    }

    #[test]
    fn type_and_constraint_mismatches_fail_closed() {
        let base = json!({ "po_number": "PO-1" });

        let mut wrong_type = base.clone();
        wrong_type["qty"] = json!("three");
        assert!(validate_decision(&frame(), &decision("approve", wrong_type)).is_err());

        let mut out_of_range = base.clone();
        out_of_range["qty"] = json!(99);
        assert!(validate_decision(&frame(), &decision("approve", out_of_range)).is_err());

        let mut bad_option = base.clone();
        bad_option["speed"] = json!("teleport");
        assert!(validate_decision(&frame(), &decision("approve", bad_option)).is_err());

        let mut bad_bool = base;
        bad_bool["gift"] = json!("yes");
        assert!(validate_decision(&frame(), &decision("approve", bad_bool)).is_err());
    }
}
