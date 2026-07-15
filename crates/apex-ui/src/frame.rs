//! The [`UiFrame`] tree and its component vocabulary (UIP-101/102/106).

use apex_common::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The frame-protocol schema version this build of the runtime speaks (UIP-106).
///
/// Frames carry the version that authored them; [`UiFrame::validate`] rejects a
/// frame whose version is *newer* than this — an old runtime must not
/// best-effort-render a shape it doesn't fully understand (the same
/// version-skew posture as the workflow store's MIG-A1 check).
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Protocol-level cap on total nodes in a frame. A hard bound on validation
/// and render work — policy (`apex-ui-guard`) may tighten it, never widen it.
pub const MAX_NODES: usize = 512;

/// Protocol-level cap on tree depth. Same tighten-only contract as [`MAX_NODES`].
pub const MAX_DEPTH: usize = 32;

/// Where a frame came from (UIP-102): stamped by the *runtime* when the frame
/// is presented, never trusted from the author. All fields optional — a frame
/// authored inline in a workflow definition starts with none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The workflow execution that presented the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// The `ui` activity within that execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
    /// The agent run that presented the frame (the HIL-304 path, P2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The model that generated the frame content, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// A pinned prompt-registry reference (SAF-202) that produced the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<String>,
}

/// Text emphasis for a [`UiNode::Text`] node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    /// Body copy (the default).
    #[default]
    Body,
    /// A section heading.
    Heading,
    /// De-emphasized fine print.
    Caption,
}

/// Semantic tone for a [`UiNode::Badge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// No particular signal (the default).
    #[default]
    Neutral,
    /// Informational.
    Info,
    /// Positive/complete.
    Success,
    /// Caution.
    Warning,
    /// Problem/destructive context.
    Danger,
}

/// The declared semantic class of a button's intent (UIP-103). This is the
/// machine-readable half of intent-consistency checking (GRD-203): policy can
/// gate whole classes (e.g. `destructive`), and a label that contradicts its
/// class is a blockable deception shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Affirmative, non-destructive ("OK", "Continue").
    Confirm,
    /// An approval decision ("Approve").
    Approve,
    /// Submits the frame's input values.
    Submit,
    /// Backs out without effect ("Cancel").
    Cancel,
    /// A rejection decision ("Reject").
    Reject,
    /// Irreversible/destructive effect ("Delete", "Terminate") — deny-by-default
    /// in policy (GRD-201).
    Destructive,
    /// None of the above (the default).
    #[default]
    Neutral,
}

impl ActionClass {
    /// Whether a decision taking this action affirms the frame's inputs —
    /// required-input enforcement (HIL-302) applies only to affirmative
    /// classes; a "Cancel"/"Reject" must never be blocked by an empty form.
    pub fn is_affirmative(self) -> bool {
        matches!(
            self,
            ActionClass::Confirm
                | ActionClass::Approve
                | ActionClass::Submit
                | ActionClass::Destructive
        )
    }
}

/// One `key: value` display row in a [`UiNode::KeyValue`] node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyValueEntry {
    /// The row's label.
    pub key: String,
    /// The row's display value.
    pub value: String,
}

/// One choice in a [`UiNode::Select`] input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    /// The machine value submitted when chosen.
    pub value: String,
    /// The human-readable label shown.
    pub label: String,
}

/// The constrained component vocabulary (UIP-101). Every renderable element is
/// one of these variants — an unknown `type` tag fails deserialization
/// (fail-closed), and `deny_unknown_fields` rejects smuggled extra fields.
///
/// There is deliberately **no credential-input component** (no password, card,
/// or OTP field): frames that need one cannot be expressed, which is the
/// structural half of GRD-204's deception defense. Free-text inputs whose
/// *naming* suggests credentials are the policy engine's job (GRD-201).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiNode {
    /// Vertical layout container.
    Column {
        /// Children, rendered top-to-bottom.
        children: Vec<UiNode>,
    },
    /// Horizontal layout container.
    Row {
        /// Children, rendered left-to-right.
        children: Vec<UiNode>,
    },
    /// A visually grouped section.
    Card {
        /// Optional card heading.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Card contents.
        children: Vec<UiNode>,
    },
    /// A horizontal rule.
    Divider {},
    /// A run of text.
    Text {
        /// The text content (plain text — never interpreted as markup).
        text: String,
        /// Emphasis.
        #[serde(default)]
        style: TextStyle,
    },
    /// A small status label.
    Badge {
        /// The badge text.
        text: String,
        /// Semantic tone.
        #[serde(default)]
        tone: Tone,
    },
    /// A label/value table.
    KeyValue {
        /// The rows.
        entries: Vec<KeyValueEntry>,
    },
    /// An image **by reference** — subject to the policy media-origin
    /// allow-list (GRD-201); there is no inline-bytes variant in v1.
    Image {
        /// The image URL.
        url: String,
        /// Required alternative text (the RDR-405 accessibility floor starts
        /// at the protocol: an image without alt text is invalid, not just
        /// unstyled).
        alt: String,
    },
    /// A single- or multi-line free-text input.
    TextInput {
        /// Unique machine name the submitted value is keyed by.
        name: String,
        /// Visible label.
        label: String,
        /// Placeholder hint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// Whether an affirmative decision must supply a non-empty value.
        #[serde(default)]
        required: bool,
        /// Render as a multi-line textarea.
        #[serde(default)]
        multiline: bool,
    },
    /// A numeric input.
    NumberInput {
        /// Unique machine name the submitted value is keyed by.
        name: String,
        /// Visible label.
        label: String,
        /// Inclusive minimum, when bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Inclusive maximum, when bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Whether an affirmative decision must supply a value.
        #[serde(default)]
        required: bool,
    },
    /// A closed set of choices.
    Select {
        /// Unique machine name the submitted value is keyed by.
        name: String,
        /// Visible label.
        label: String,
        /// The choices (must be non-empty, values unique).
        options: Vec<SelectOption>,
        /// Whether an affirmative decision must choose one.
        #[serde(default)]
        required: bool,
    },
    /// A boolean toggle.
    Checkbox {
        /// Unique machine name the submitted value is keyed by.
        name: String,
        /// Visible label.
        label: String,
        /// Initial state.
        #[serde(default)]
        checked: bool,
    },
    /// An action the human can take. Every button **declares** its intent
    /// (UIP-103): the decision it produces is keyed by `action`, and its
    /// semantic `class` is what policy gates and consistency-checks — there
    /// are no anonymous actions in the protocol.
    Button {
        /// Unique action id; a [`UiDecision`](crate::UiDecision) must name one
        /// of the frame's declared actions.
        action: String,
        /// Visible label.
        label: String,
        /// Declared semantic class.
        #[serde(default)]
        class: ActionClass,
    },
}

/// A complete agent-generated interface: the unit that is policy-checked,
/// audited, presented, and decided on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiFrame {
    /// The protocol version that authored this frame (UIP-106).
    pub schema_version: String,
    /// Optional frame title, shown as the surface heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Runtime-stamped origin metadata (UIP-102).
    #[serde(default, skip_serializing_if = "provenance_is_empty")]
    pub provenance: Provenance,
    /// The component tree.
    pub root: UiNode,
}

fn provenance_is_empty(p: &Provenance) -> bool {
    *p == Provenance::default()
}

impl UiFrame {
    /// Parse a frame from JSON, fail-closed (UIP-101/106): unknown node types,
    /// unknown fields, and a schema version newer than [`SCHEMA_VERSION`] are
    /// all hard `Invalid` errors. The returned frame has passed
    /// [`validate`](Self::validate).
    pub fn from_value(value: &Value) -> Result<UiFrame> {
        let frame: UiFrame = serde_json::from_value(value.clone())
            .map_err(|e| Error::Invalid(format!("invalid ui frame: {e}")))?;
        frame.validate()?;
        Ok(frame)
    }

    /// Structural validation (UIP-101): version supported, protocol caps
    /// respected, input names and action ids unique and non-empty, labels
    /// present, selects non-empty. Pure and deterministic.
    pub fn validate(&self) -> Result<()> {
        ensure_version_supported(&self.schema_version)?;

        let mut count = 0usize;
        let mut input_names: Vec<&str> = Vec::new();
        let mut action_ids: Vec<&str> = Vec::new();
        validate_node(&self.root, 1, &mut count, &mut input_names, &mut action_ids)?;
        // An input the human can never submit is a dead control — a frame with
        // inputs must declare at least one action, or the decision loop can
        // never complete (HIL-302: decisions are keyed by a declared action).
        if !input_names.is_empty() && action_ids.is_empty() {
            return Err(Error::Invalid(
                "ui frame declares input nodes but no button to submit them".to_string(),
            ));
        }
        Ok(())
    }

    /// Every node in the tree, depth-first. Used by validation, policy
    /// evaluation, and decision checking.
    pub fn nodes(&self) -> Vec<&UiNode> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    /// Whether the frame carries any input or action node. Display-only
    /// frames pass the hosted floor without a policy (GRD-207); interactive
    /// ones do not.
    pub fn is_interactive(&self) -> bool {
        self.nodes().iter().any(|n| {
            matches!(
                n,
                UiNode::TextInput { .. }
                    | UiNode::NumberInput { .. }
                    | UiNode::Select { .. }
                    | UiNode::Checkbox { .. }
                    | UiNode::Button { .. }
            )
        })
    }

    /// The frame's declared buttons, in tree order.
    pub fn actions(&self) -> Vec<(&str, &str, ActionClass)> {
        self.nodes()
            .into_iter()
            .filter_map(|n| match n {
                UiNode::Button {
                    action,
                    label,
                    class,
                } => Some((action.as_str(), label.as_str(), *class)),
                _ => None,
            })
            .collect()
    }

    /// Content hash over the canonical serialized form (UIP-102): SHA-256 hex
    /// of the frame's JSON. `serde_json` maps are key-sorted, so serialization
    /// is canonical for equal frames. The provenance block is *included* — the
    /// hash identifies exactly what was presented, origin stamp and all.
    pub fn content_hash(&self) -> String {
        // A UiFrame always serializes (no non-string map keys, no NaN floats
        // survive validation of authored input; and this is a value we built).
        let canonical = serde_json::to_string(self).unwrap_or_else(|_| format!("{self:?}"));
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        hex(&hasher.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reject a frame authored by a *newer* protocol than this runtime speaks
/// (UIP-106). Older 1.x versions are accepted; anything above
/// [`SCHEMA_VERSION`] — or a different major — fails closed.
fn ensure_version_supported(version: &str) -> Result<()> {
    let ours = semver::Version::parse(SCHEMA_VERSION)
        .map_err(|e| Error::Runtime(format!("bad built-in schema version: {e}")))?;
    let theirs = semver::Version::parse(version)
        .map_err(|e| Error::Invalid(format!("invalid ui frame schema_version `{version}`: {e}")))?;
    if theirs.major != ours.major || theirs > ours {
        return Err(Error::Invalid(format!(
            "ui frame schema_version `{version}` is newer than this runtime understands \
             (`{SCHEMA_VERSION}`); refusing to render a shape it may not fully validate"
        )));
    }
    Ok(())
}

fn validate_node<'a>(
    node: &'a UiNode,
    depth: usize,
    count: &mut usize,
    input_names: &mut Vec<&'a str>,
    action_ids: &mut Vec<&'a str>,
) -> Result<()> {
    *count += 1;
    if *count > MAX_NODES {
        return Err(Error::Invalid(format!(
            "ui frame exceeds the protocol cap of {MAX_NODES} nodes"
        )));
    }
    if depth > MAX_DEPTH {
        return Err(Error::Invalid(format!(
            "ui frame exceeds the protocol depth cap of {MAX_DEPTH}"
        )));
    }

    let mut register_input = |name: &'a str, label: &str| -> Result<()> {
        if name.is_empty() || label.is_empty() {
            return Err(Error::Invalid(
                "ui input nodes require a non-empty `name` and `label`".to_string(),
            ));
        }
        if input_names.contains(&name) {
            return Err(Error::Invalid(format!(
                "duplicate ui input name `{name}` — decision values must be unambiguous"
            )));
        }
        input_names.push(name);
        Ok(())
    };

    match node {
        UiNode::Column { children } | UiNode::Row { children } => {
            for child in children {
                validate_node(child, depth + 1, count, input_names, action_ids)?;
            }
        }
        UiNode::Card { children, .. } => {
            for child in children {
                validate_node(child, depth + 1, count, input_names, action_ids)?;
            }
        }
        UiNode::Divider {} => {}
        UiNode::Text { text, .. } => {
            if text.is_empty() {
                return Err(Error::Invalid("ui text node with empty text".to_string()));
            }
        }
        UiNode::Badge { text, .. } => {
            if text.is_empty() {
                return Err(Error::Invalid("ui badge node with empty text".to_string()));
            }
        }
        UiNode::KeyValue { entries } => {
            if entries.is_empty() {
                return Err(Error::Invalid(
                    "ui key_value node with no entries".to_string(),
                ));
            }
        }
        UiNode::Image { url, alt } => {
            if url.is_empty() {
                return Err(Error::Invalid("ui image node with empty url".to_string()));
            }
            if alt.is_empty() {
                return Err(Error::Invalid(
                    "ui image node requires non-empty alt text".to_string(),
                ));
            }
        }
        UiNode::TextInput { name, label, .. } => register_input(name, label)?,
        UiNode::NumberInput {
            name,
            label,
            min,
            max,
            ..
        } => {
            register_input(name, label)?;
            if let (Some(min), Some(max)) = (min, max)
                && min > max
            {
                return Err(Error::Invalid(format!(
                    "ui number_input `{name}` has min > max"
                )));
            }
        }
        UiNode::Select {
            name,
            label,
            options,
            ..
        } => {
            register_input(name, label)?;
            if options.is_empty() {
                return Err(Error::Invalid(format!("ui select `{name}` has no options")));
            }
            let mut seen: Vec<&str> = Vec::new();
            for opt in options {
                if opt.value.is_empty() || opt.label.is_empty() {
                    return Err(Error::Invalid(format!(
                        "ui select `{name}` has an option with an empty value or label"
                    )));
                }
                if seen.contains(&opt.value.as_str()) {
                    return Err(Error::Invalid(format!(
                        "ui select `{name}` has duplicate option value `{}`",
                        opt.value
                    )));
                }
                seen.push(&opt.value);
            }
        }
        UiNode::Checkbox { name, label, .. } => register_input(name, label)?,
        UiNode::Button { action, label, .. } => {
            if action.is_empty() || label.is_empty() {
                return Err(Error::Invalid(
                    "ui button nodes require a non-empty `action` and `label`".to_string(),
                ));
            }
            if action_ids.contains(&action.as_str()) {
                return Err(Error::Invalid(format!(
                    "duplicate ui action id `{action}` — decisions must be unambiguous"
                )));
            }
            action_ids.push(action);
        }
    }
    Ok(())
}

fn collect<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    match node {
        UiNode::Column { children } | UiNode::Row { children } => {
            for child in children {
                collect(child, out);
            }
        }
        UiNode::Card { children, .. } => {
            for child in children {
                collect(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn confirm_frame() -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "title": "Confirm order",
            "root": {
                "type": "column",
                "children": [
                    { "type": "text", "text": "Reorder 3 boxes of pipette tips?" },
                    { "type": "key_value", "entries": [
                        { "key": "Vendor", "value": "LabSupply Co" },
                        { "key": "Total", "value": "$412.80" }
                    ]},
                    { "type": "text_input", "name": "po_number", "label": "PO number", "required": true },
                    { "type": "row", "children": [
                        { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
                        { "type": "button", "action": "cancel", "label": "Cancel", "class": "cancel" }
                    ]}
                ]
            }
        })
    }

    #[test]
    fn a_valid_frame_parses_and_reports_interactivity_and_actions() {
        let frame = UiFrame::from_value(&confirm_frame()).expect("valid frame");
        assert!(frame.is_interactive());
        let actions = frame.actions();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], ("approve", "Approve", ActionClass::Approve));
    }

    #[test]
    fn unknown_node_types_and_unknown_fields_fail_closed() {
        // An unknown `type` tag — the "raw HTML smuggle" shape.
        let bad_type = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "html", "content": "<script>alert(1)</script>" }
        });
        assert!(UiFrame::from_value(&bad_type).is_err());

        // A known node with a smuggled extra field.
        let extra_field = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "text", "text": "hi", "onclick": "steal()" }
        });
        assert!(UiFrame::from_value(&extra_field).is_err());
    }

    #[test]
    fn newer_schema_versions_are_rejected_older_same_major_accepted() {
        let mut frame = confirm_frame();
        frame["schema_version"] = json!("999.0.0");
        let err = UiFrame::from_value(&frame).expect_err("newer must be rejected");
        assert!(err.to_string().contains("newer than this runtime"));

        frame["schema_version"] = json!("2.0.0");
        assert!(
            UiFrame::from_value(&frame).is_err(),
            "different major rejected"
        );

        frame["schema_version"] = json!("0.9.0");
        assert!(
            UiFrame::from_value(&frame).is_err(),
            "older major is a different major — rejected"
        );

        frame["schema_version"] = json!("1.0.0");
        assert!(UiFrame::from_value(&frame).is_ok());
    }

    #[test]
    fn duplicate_input_names_and_action_ids_are_rejected() {
        let dup_input = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "column", "children": [
                { "type": "text_input", "name": "x", "label": "One" },
                { "type": "checkbox", "name": "x", "label": "Two" }
            ]}
        });
        assert!(UiFrame::from_value(&dup_input).is_err());

        let dup_action = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "row", "children": [
                { "type": "button", "action": "go", "label": "Go" },
                { "type": "button", "action": "go", "label": "Also go" }
            ]}
        });
        assert!(UiFrame::from_value(&dup_action).is_err());
    }

    #[test]
    fn images_require_alt_text_and_selects_require_options() {
        let no_alt = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "image", "url": "https://cdn.example/x.png", "alt": "" }
        });
        assert!(UiFrame::from_value(&no_alt).is_err());

        let empty_select = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "select", "name": "s", "label": "Pick", "options": [] }
        });
        assert!(UiFrame::from_value(&empty_select).is_err());
    }

    #[test]
    fn node_and_depth_caps_fail_closed() {
        // Depth: nest columns past MAX_DEPTH.
        let mut node = json!({ "type": "text", "text": "deep" });
        for _ in 0..MAX_DEPTH {
            node = json!({ "type": "column", "children": [node] });
        }
        let too_deep = json!({ "schema_version": SCHEMA_VERSION, "root": node });
        assert!(UiFrame::from_value(&too_deep).is_err());

        // Node count: a flat column with MAX_NODES children (plus the column
        // itself) exceeds the cap.
        let children: Vec<Value> = (0..MAX_NODES)
            .map(|i| json!({ "type": "text", "text": format!("t{i}") }))
            .collect();
        let too_many = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "column", "children": children }
        });
        assert!(UiFrame::from_value(&too_many).is_err());
    }

    #[test]
    fn content_hash_is_stable_for_equal_frames_and_differs_on_change() {
        let a = UiFrame::from_value(&confirm_frame()).unwrap();
        let b = UiFrame::from_value(&confirm_frame()).unwrap();
        assert_eq!(a.content_hash(), b.content_hash());

        let mut changed = confirm_frame();
        changed["title"] = json!("Confirm order — updated");
        let c = UiFrame::from_value(&changed).unwrap();
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn display_only_frames_are_not_interactive() {
        let display = json!({
            "schema_version": SCHEMA_VERSION,
            "root": { "type": "card", "title": "Status", "children": [
                { "type": "badge", "text": "healthy", "tone": "success" },
                { "type": "divider" },
                { "type": "text", "text": "All queues nominal." }
            ]}
        });
        let frame = UiFrame::from_value(&display).unwrap();
        assert!(!frame.is_interactive());
        assert!(frame.actions().is_empty());
    }
}
