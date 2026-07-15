//! The generative-UI trust layer ([PRD-005](../../docs/01-product/prd-generative-ui-runtime.md)
//! workstream GRD-2xx): declarative [`UiPolicy`] evaluation over
//! [`apex_ui::UiFrame`]s, between generation and emission.
//!
//! **Fail-closed, deliberately** — the `Guardrail` stance (SAF-201) applied to
//! interfaces: a frame is [`Verdict::Allow`]ed, [`Verdict::Redact`]ed (the
//! caller emits the transformed frame only), or [`Verdict::Block`]ed with a
//! named rule; and a *missing* policy does not mean "anything goes" — the
//! hosted floor ([`hosted_floor`], GRD-207) denies interactive frames outright,
//! mirroring SEC-303's deny-all default for hosted tool permissions.
//!
//! Everything here is **pure and deterministic** (the house rule): no I/O, no
//! clock, no model call. The structural rules are the floor and are never
//! bypassed; judge-style checks (the GRD-203 LLM variant) are a later,
//! policy-opt-in *addition* — shadow-mode first — not a replacement.
//!
//! What the vocabulary already makes impossible (no raw HTML/script node, no
//! credential-input component — ADR-0011 §2.4) is not re-checked here; this
//! crate polices what remains expressible: free-text inputs *named* like
//! credential prompts (GRD-201), media from unapproved origins, destructive
//! action classes, labels that contradict their declared intent (GRD-203/204),
//! and size/depth beyond the tenant's tolerance.

use apex_common::{Error, Result};
use apex_ui::{ActionClass, UiFrame, UiNode};
use serde::{Deserialize, Serialize};

pub mod conformance;

/// Rule identifiers, stable for audit records (GRD-205) and tests.
pub mod rules {
    /// Frame exceeds the policy's node budget.
    pub const MAX_NODES: &str = "max_nodes";
    /// Frame exceeds the policy's depth budget.
    pub const MAX_DEPTH: &str = "max_depth";
    /// Image from an origin outside the allow-list.
    pub const MEDIA_ORIGIN: &str = "media_origin";
    /// A free-text input named/labelled like a credential or PII prompt.
    pub const SENSITIVE_INPUT: &str = "sensitive_input";
    /// A destructive action class without policy opt-in.
    pub const DESTRUCTIVE_ACTION: &str = "destructive_action";
    /// A button label that contradicts its declared action class.
    pub const INTENT_MISMATCH: &str = "intent_mismatch";
    /// Text matched a redaction pattern (a redact rule, not a block).
    pub const REDACT_TEXT: &str = "redact_text";
    /// The no-policy hosted floor (GRD-207).
    pub const HOSTED_FLOOR: &str = "hosted_floor";
}

/// Input-name/label tokens treated as credential/PII prompts by default
/// (GRD-201, deny-by-default). Matched against lowercase alphanumeric *tokens*
/// of the input's `name` and `label` — token matching, not substring, so
/// "discard" never trips "card" while "card_number" does.
pub const DEFAULT_SENSITIVE_INPUT_TOKENS: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "card",
    "cvv",
    "cvc",
    "ssn",
    "secret",
    "token",
    "iban",
    "passport",
    "otp",
    "pin",
    "tan",
    "mfa",
    "2fa",
    "credit",
    "apikey",
];

/// Label tokens that read as backing out — contradictory on an affirmative or
/// destructive button (GRD-203).
const NEGATIVE_LABEL_TOKENS: &[&str] = &[
    "cancel", "no", "stop", "back", "reject", "decline", "abort", "dismiss", "never",
];

/// Label tokens that read as destructive — contradictory on a button whose
/// declared class hides that (GRD-204: mislabeling destructiveness is a
/// deception shape).
const DESTRUCTIVE_LABEL_TOKENS: &[&str] = &[
    "delete",
    "remove",
    "terminate",
    "destroy",
    "drop",
    "wipe",
    "erase",
    "purge",
    "revoke",
];

/// The tunable rule set of a [`UiPolicy`]. Every field has a safe default, so
/// a minimal policy document is `{name, version}` — and *tightening* is the
/// only direction that needs no opt-in flag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyRules {
    /// Node budget (protocol cap [`apex_ui::MAX_NODES`] still applies above).
    pub max_nodes: usize,
    /// Depth budget (protocol cap [`apex_ui::MAX_DEPTH`] still applies above).
    pub max_depth: usize,
    /// Whether `destructive`-class actions may render at all (GRD-201's
    /// approval-gate requirement arrives with ITS-603; until then the class is
    /// simply deniable, default-denied).
    pub allow_destructive_actions: bool,
    /// Escape hatch disabling the sensitive-input-name rule. Deliberately a
    /// coarse boolean, not a per-token override: a tenant that needs an input
    /// literally named "password" is a tenant that should not be using
    /// generated frames for it.
    pub allow_sensitive_input_names: bool,
    /// Tokens (lowercase) flagged by the sensitive-input rule. Replaces the
    /// default list when set — [`DEFAULT_SENSITIVE_INPUT_TOKENS`] otherwise.
    pub sensitive_input_tokens: Vec<String>,
    /// Origins images may load from, matched as exact host or `.suffix`
    /// (`"cdn.example.com"`, `".example.org"`). Empty = **no images** —
    /// deny-by-default like everything else.
    pub allowed_media_origins: Vec<String>,
    /// Case-insensitive substrings scrubbed from display text ([`Verdict::Redact`]):
    /// each occurrence in text/badge/key-value content is replaced with `█`.
    pub redact_text_patterns: Vec<String>,
}

impl Default for PolicyRules {
    fn default() -> Self {
        Self {
            max_nodes: 256,
            max_depth: 16,
            allow_destructive_actions: false,
            allow_sensitive_input_names: false,
            sensitive_input_tokens: Vec::new(),
            allowed_media_origins: Vec::new(),
            redact_text_patterns: Vec::new(),
        }
    }
}

/// A tenant-scoped, versioned UI policy (GRD-201/206). Versions are what the
/// audit trail records; treat a published `(name, version)` as immutable, the
/// SAF-202 pin discipline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPolicy {
    /// Policy name (e.g. `"default"`, `"acme-prod"`).
    pub name: String,
    /// Monotonic version, recorded with every verdict.
    pub version: u32,
    /// The rule set.
    #[serde(default)]
    pub rules: PolicyRules,
}

impl UiPolicy {
    /// Parse a policy from YAML, fail-closed: unknown fields are rejected, the
    /// name must be non-empty and the version non-zero.
    pub fn from_yaml(yaml: &str) -> Result<UiPolicy> {
        let policy: UiPolicy = serde_yaml::from_str(yaml)
            .map_err(|e| Error::Invalid(format!("invalid ui policy: {e}")))?;
        if policy.name.is_empty() {
            return Err(Error::Invalid("ui policy requires a non-empty name".into()));
        }
        if policy.version == 0 {
            return Err(Error::Invalid(
                "ui policy version must be >= 1 (versions are audited; 0 is reserved)".into(),
            ));
        }
        Ok(policy)
    }

    /// `name@vN` — the reference stamped into audit records and pending-frame
    /// metadata.
    pub fn reference(&self) -> String {
        format!("{}@v{}", self.name, self.version)
    }

    /// The effective sensitive-input token list (the policy's own, or the
    /// defaults when unset).
    fn sensitive_tokens(&self) -> Vec<String> {
        if self.rules.sensitive_input_tokens.is_empty() {
            DEFAULT_SENSITIVE_INPUT_TOKENS
                .iter()
                .map(|t| t.to_string())
                .collect()
        } else {
            self.rules
                .sensitive_input_tokens
                .iter()
                .map(|t| t.to_lowercase())
                .collect()
        }
    }
}

/// The outcome of evaluating a frame against a policy.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The frame renders as-is.
    Allow,
    /// The frame renders **transformed** — the caller must emit `frame`, never
    /// the original. `rules` names what fired (for the audit record).
    Redact {
        /// The transformed frame.
        frame: Box<UiFrame>,
        /// The rule ids that fired.
        rules: Vec<String>,
    },
    /// The frame must not render.
    Block {
        /// The rule id that fired ([`rules`]).
        rule: String,
        /// Human-readable detail for the audit record — never shown to the
        /// end user (the blocklist stance: don't echo the trigger back).
        reason: String,
    },
}

/// The no-policy hosted floor (GRD-207): display-only frames pass, anything
/// interactive is denied. Mirrors SEC-303 — a hosted deployment must not render
/// arbitrary generated *controls* just because nobody wrote a policy yet.
/// `APEX_UNRESTRICTED_UI=1` (resolved by the platform, not here) is the
/// documented trusted-first-party escape hatch.
pub fn hosted_floor(frame: &UiFrame) -> Verdict {
    if frame.is_interactive() {
        Verdict::Block {
            rule: rules::HOSTED_FLOOR.to_string(),
            reason: "no ui policy is configured for this tenant; interactive frames are denied \
                     by default (display-only frames pass)"
                .to_string(),
        }
    } else {
        Verdict::Allow
    }
}

/// Evaluate `frame` against `policy` (GRD-201/202/203/204). Pure and
/// deterministic; the first blocking rule wins, redaction applies only when
/// nothing blocked.
pub fn evaluate(policy: &UiPolicy, frame: &UiFrame) -> Verdict {
    // Size/depth budgets.
    let nodes = frame.nodes();
    if nodes.len() > policy.rules.max_nodes {
        return Verdict::Block {
            rule: rules::MAX_NODES.to_string(),
            reason: format!(
                "frame has {} nodes; policy `{}` allows {}",
                nodes.len(),
                policy.reference(),
                policy.rules.max_nodes
            ),
        };
    }
    if let Some(depth) = frame_depth(frame)
        && depth > policy.rules.max_depth
    {
        return Verdict::Block {
            rule: rules::MAX_DEPTH.to_string(),
            reason: format!(
                "frame depth {depth} exceeds policy `{}` budget {}",
                policy.reference(),
                policy.rules.max_depth
            ),
        };
    }

    // Media origins (deny-by-default).
    for node in &nodes {
        if let UiNode::Image { url, .. } = node
            && let Some(reason) = media_violation(url, &policy.rules.allowed_media_origins)
        {
            return Verdict::Block {
                rule: rules::MEDIA_ORIGIN.to_string(),
                reason,
            };
        }
    }

    // Sensitive input names (GRD-201) — the "generated phishing form" rule.
    if !policy.rules.allow_sensitive_input_names {
        let sensitive = policy.sensitive_tokens();
        for node in &nodes {
            let (name, label) = match node {
                UiNode::TextInput { name, label, .. }
                | UiNode::NumberInput { name, label, .. }
                | UiNode::Select { name, label, .. }
                | UiNode::Checkbox { name, label, .. } => (name, label),
                _ => continue,
            };
            for field in [name, label] {
                if let Some(token) = tokens(field).into_iter().find(|t| sensitive.contains(t)) {
                    return Verdict::Block {
                        rule: rules::SENSITIVE_INPUT.to_string(),
                        reason: format!(
                            "input `{name}` is named/labelled like a credential or PII prompt \
                             (matched token `{token}`); generated frames must never collect \
                             credentials (policy `{}`)",
                            policy.reference()
                        ),
                    };
                }
            }
        }
    }

    // Action classes + intent consistency (GRD-203/204).
    for (action, label, class) in frame.actions() {
        if class == ActionClass::Destructive && !policy.rules.allow_destructive_actions {
            return Verdict::Block {
                rule: rules::DESTRUCTIVE_ACTION.to_string(),
                reason: format!(
                    "action `{action}` declares class `destructive`, which policy `{}` \
                     does not allow",
                    policy.reference()
                ),
            };
        }
        let label_tokens = tokens(label);
        if class.is_affirmative()
            && label_tokens
                .iter()
                .any(|t| NEGATIVE_LABEL_TOKENS.contains(&t.as_str()))
        {
            return Verdict::Block {
                rule: rules::INTENT_MISMATCH.to_string(),
                reason: format!(
                    "action `{action}` declares affirmative class `{class:?}` but its label \
                     reads as backing out — a deceptive control shape"
                ),
            };
        }
        if class != ActionClass::Destructive
            && label_tokens
                .iter()
                .any(|t| DESTRUCTIVE_LABEL_TOKENS.contains(&t.as_str()))
        {
            return Verdict::Block {
                rule: rules::INTENT_MISMATCH.to_string(),
                reason: format!(
                    "action `{action}` has a destructive-reading label but declares class \
                     `{class:?}` — destructive effects must be declared as `destructive`"
                ),
            };
        }
    }

    // Redaction (GRD-202's Redact arm) — only reached when nothing blocked.
    if !policy.rules.redact_text_patterns.is_empty() {
        let mut redacted = frame.clone();
        if redact_node(&mut redacted.root, &policy.rules.redact_text_patterns) {
            return Verdict::Redact {
                frame: Box::new(redacted),
                rules: vec![rules::REDACT_TEXT.to_string()],
            };
        }
    }

    Verdict::Allow
}

/// Lowercase alphanumeric tokens of `text` (`"Card_Number"` → `["card", "number"]`).
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why `url` violates the media-origin allow-list, if it does. Only `https`
/// URLs from allow-listed hosts pass; an empty allow-list denies all images.
fn media_violation(url: &str, allowed: &[String]) -> Option<String> {
    if allowed.is_empty() {
        return Some("frame embeds an image but the policy allows no media origins".to_string());
    }
    let Some(without_scheme) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("HTTPS://"))
    else {
        return Some(format!(
            "image url `{url}` is not https — media references must be https"
        ));
    };
    let rest = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    // Strip any port and lowercase for comparison.
    let host = rest.split(':').next().unwrap_or("").to_lowercase();
    if host.is_empty() {
        return Some(format!("image url `{url}` has no host"));
    }
    let permitted = allowed.iter().any(|origin| {
        let origin = origin.to_lowercase();
        if let Some(suffix) = origin.strip_prefix('.') {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == origin
        }
    });
    if permitted {
        None
    } else {
        Some(format!(
            "image origin `{host}` is not in the policy's allowed media origins"
        ))
    }
}

fn frame_depth(frame: &UiFrame) -> Option<usize> {
    fn depth(node: &UiNode) -> usize {
        let children = match node {
            UiNode::Column { children } | UiNode::Row { children } => children,
            UiNode::Card { children, .. } => children,
            _ => return 1,
        };
        1 + children.iter().map(depth).max().unwrap_or(0)
    }
    Some(depth(&frame.root))
}

/// Scrub matching substrings in display text; returns whether anything matched.
fn redact_node(node: &mut UiNode, patterns: &[String]) -> bool {
    let mut hit = false;
    let mut scrub = |text: &mut String| {
        let lower = text.to_lowercase();
        for pattern in patterns {
            let pattern = pattern.to_lowercase();
            if pattern.is_empty() {
                continue;
            }
            let mut from = 0;
            while let Some(at) = lower[from..].find(&pattern) {
                let start = from + at;
                let end = start + pattern.len();
                text.replace_range(start..end, &"█".repeat(pattern.chars().count()));
                hit = true;
                from = end;
            }
        }
    };
    match node {
        UiNode::Text { text, .. } | UiNode::Badge { text, .. } => scrub(text),
        UiNode::KeyValue { entries } => {
            for entry in entries {
                scrub(&mut entry.value);
            }
        }
        UiNode::Column { children } | UiNode::Row { children } => {
            for child in children {
                hit |= redact_node(child, patterns);
            }
        }
        UiNode::Card { children, .. } => {
            for child in children {
                hit |= redact_node(child, patterns);
            }
        }
        _ => {}
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(root: serde_json::Value) -> UiFrame {
        UiFrame::from_value(&json!({
            "schema_version": apex_ui::SCHEMA_VERSION,
            "root": root
        }))
        .expect("test frame must be protocol-valid")
    }

    fn policy() -> UiPolicy {
        UiPolicy {
            name: "test".into(),
            version: 1,
            rules: PolicyRules::default(),
        }
    }

    #[test]
    fn a_benign_confirmation_frame_is_allowed() {
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text", "text": "Reorder 3 boxes?" },
            { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
            { "type": "button", "action": "cancel", "label": "Cancel", "class": "cancel" }
        ]}));
        assert_eq!(evaluate(&policy(), &f), Verdict::Allow);
    }

    #[test]
    fn credential_shaped_inputs_are_blocked_by_default_but_not_lookalike_words() {
        // The UC4 shape: a poisoned source steers the model into collecting a
        // card number. Blocked by token match on the name…
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text_input", "name": "card_number", "label": "Card number" },
            { "type": "button", "action": "pay", "label": "Continue", "class": "submit" }
        ]}));
        match evaluate(&policy(), &f) {
            Verdict::Block { rule, .. } => assert_eq!(rule, rules::SENSITIVE_INPUT),
            other => panic!("expected block, got {other:?}"),
        }

        // …and by the label alone even when the name is innocuous.
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text_input", "name": "field_1", "label": "CVV code" },
            { "type": "button", "action": "submit", "label": "Submit", "class": "submit" }
        ]}));
        assert!(matches!(evaluate(&policy(), &f), Verdict::Block { .. }));

        // Token matching, not substring: "discard" must not trip "card".
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text_input", "name": "discard_reason", "label": "Reason to discard" },
            { "type": "button", "action": "submit", "label": "Submit", "class": "submit" }
        ]}));
        assert_eq!(evaluate(&policy(), &f), Verdict::Allow);
    }

    #[test]
    fn destructive_actions_are_deny_by_default_and_opt_in() {
        let f = frame(json!({ "type": "column", "children": [
            { "type": "button", "action": "purge", "label": "Delete everything", "class": "destructive" }
        ]}));
        assert!(matches!(
            evaluate(&policy(), &f),
            Verdict::Block { rule, .. } if rule == rules::DESTRUCTIVE_ACTION
        ));

        let mut allowing = policy();
        allowing.rules.allow_destructive_actions = true;
        assert_eq!(evaluate(&allowing, &f), Verdict::Allow);
    }

    #[test]
    fn deceptive_labels_are_intent_mismatches() {
        // An affirmative class wearing a "Cancel" label.
        let f = frame(json!({ "type": "column", "children": [
            { "type": "button", "action": "sneaky", "label": "Cancel", "class": "confirm" }
        ]}));
        assert!(matches!(
            evaluate(&policy(), &f),
            Verdict::Block { rule, .. } if rule == rules::INTENT_MISMATCH
        ));

        // A destructive-reading label hiding behind a neutral class.
        let f = frame(json!({ "type": "column", "children": [
            { "type": "button", "action": "cleanup", "label": "Delete account", "class": "neutral" }
        ]}));
        assert!(matches!(
            evaluate(&policy(), &f),
            Verdict::Block { rule, .. } if rule == rules::INTENT_MISMATCH
        ));
    }

    #[test]
    fn media_origins_are_deny_by_default_and_suffix_matchable() {
        let img =
            json!({ "type": "image", "url": "https://cdn.example.com/x.png", "alt": "chart" });
        let f = frame(json!({ "type": "column", "children": [img] }));

        // No origins allowed → blocked.
        assert!(matches!(
            evaluate(&policy(), &f),
            Verdict::Block { rule, .. } if rule == rules::MEDIA_ORIGIN
        ));

        // Exact host allowed → passes.
        let mut allowing = policy();
        allowing.rules.allowed_media_origins = vec!["cdn.example.com".into()];
        assert_eq!(evaluate(&allowing, &f), Verdict::Allow);

        // `.suffix` form matches subdomains.
        allowing.rules.allowed_media_origins = vec![".example.com".into()];
        assert_eq!(evaluate(&allowing, &f), Verdict::Allow);

        // http (non-https) is never a valid media reference.
        let f = frame(json!({ "type": "column", "children": [
            { "type": "image", "url": "http://cdn.example.com/x.png", "alt": "chart" }
        ]}));
        assert!(matches!(evaluate(&allowing, &f), Verdict::Block { .. }));
    }

    #[test]
    fn redaction_transforms_display_text_and_names_the_rule() {
        let mut p = policy();
        p.rules.redact_text_patterns = vec!["ACME-INTERNAL".into()];
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text", "text": "Ref acme-internal in the notes" },
            { "type": "key_value", "entries": [ { "key": "Code", "value": "ACME-INTERNAL-7" } ] }
        ]}));
        match evaluate(&p, &f) {
            Verdict::Redact {
                frame,
                rules: fired,
            } => {
                assert_eq!(fired, vec![rules::REDACT_TEXT.to_string()]);
                let json = serde_json::to_string(&*frame).unwrap();
                assert!(!json.to_lowercase().contains("acme-internal"));
                assert!(json.contains('█'));
            }
            other => panic!("expected redact, got {other:?}"),
        }
    }

    #[test]
    fn policy_budgets_tighten_below_protocol_caps() {
        let mut p = policy();
        p.rules.max_nodes = 2;
        let f = frame(json!({ "type": "column", "children": [
            { "type": "text", "text": "one" },
            { "type": "text", "text": "two" }
        ]}));
        assert!(matches!(
            evaluate(&p, &f),
            Verdict::Block { rule, .. } if rule == rules::MAX_NODES
        ));
    }

    #[test]
    fn hosted_floor_passes_display_only_and_denies_interactive() {
        let display = frame(json!({ "type": "text", "text": "status: nominal" }));
        assert_eq!(hosted_floor(&display), Verdict::Allow);

        let interactive = frame(json!({ "type": "column", "children": [
            { "type": "button", "action": "go", "label": "Go" }
        ]}));
        assert!(matches!(
            hosted_floor(&interactive),
            Verdict::Block { rule, .. } if rule == rules::HOSTED_FLOOR
        ));
    }

    #[test]
    fn policies_parse_from_yaml_fail_closed() {
        let p = UiPolicy::from_yaml(
            "name: acme\nversion: 3\nrules:\n  allow_destructive_actions: true\n",
        )
        .expect("valid policy");
        assert_eq!(p.reference(), "acme@v3");
        assert!(p.rules.allow_destructive_actions);

        assert!(UiPolicy::from_yaml("name: acme\nversion: 0\n").is_err());
        assert!(UiPolicy::from_yaml("name: ''\nversion: 1\n").is_err());
        assert!(
            UiPolicy::from_yaml("name: a\nversion: 1\nrules:\n  no_such_rule: true\n").is_err(),
            "unknown rule fields must be rejected, not ignored"
        );
    }
}
