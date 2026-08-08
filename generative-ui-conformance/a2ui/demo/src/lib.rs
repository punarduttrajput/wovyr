//! The generative-UI trust layer, compiled to wasm, evaluating real A2UI
//! surfaces. No wasm-bindgen: the module has zero imports and is driven through
//! a ~20-line JS shim (see index.html).
//!
//! ABI: call `alloc(n)`, write UTF-8 JSON into the buffer, call
//! `evaluate_a2ui(ptr, len)`, read the packed `u64` (`ptr << 32 | len`) result,
//! then `dealloc` both.

mod a2ui;

use std::alloc::{alloc as rust_alloc, dealloc as rust_dealloc, Layout};

#[no_mangle]
pub extern "C" fn alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }
    unsafe { rust_alloc(Layout::from_size_align(size, 1).unwrap()) }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe { rust_dealloc(ptr, Layout::from_size_align(size, 1).unwrap()) }
    }
}

fn pack(s: String) -> u64 {
    let bytes = s.into_bytes();
    let len = bytes.len();
    let ptr = Box::into_raw(bytes.into_boxed_slice()) as *mut u8;
    ((ptr as u64) << 32) | (len as u64)
}

fn err(stage: &str, msg: String) -> u64 {
    pack(serde_json::json!({ "verdict": "error", "stage": stage, "reason": msg }).to_string())
}

/// Adapt an A2UI message stream and evaluate it against the default
/// deny-by-default policy. Input: `{"messages": [...]}`.
#[no_mangle]
pub extern "C" fn evaluate_a2ui(ptr: *const u8, len: usize) -> u64 {
    let text = match std::str::from_utf8(unsafe { std::slice::from_raw_parts(ptr, len) }) {
        Ok(t) => t,
        Err(e) => return err("utf8", e.to_string()),
    };
    let doc: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return err("json", e.to_string()),
    };
    let messages = match doc.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m.clone(),
        None => return err("input", "expected {\"messages\": [...]}".into()),
    };

    let adapted = match a2ui::adapt(&messages) {
        Ok(a) => a,
        Err(e) => return err("adapt", e),
    };

    // "floor" selects the no-policy hosted floor (GRD-207), a separate code path
    // from a configured policy: display-only passes, anything interactive is denied.
    let use_floor = doc.get("policy").and_then(|p| p.as_str()) == Some("floor");
    let raw = if use_floor {
        wovyr_ui_guard::hosted_floor(&adapted.frame)
    } else {
        let policy = wovyr_ui_guard::UiPolicy {
            name: "demo".to_string(),
            version: 1,
            rules: wovyr_ui_guard::PolicyRules::default(),
        };
        wovyr_ui_guard::evaluate(&policy, &adapted.frame)
    };

    let verdict = match raw {
        wovyr_ui_guard::Verdict::Allow => serde_json::json!({ "verdict": "allow" }),
        wovyr_ui_guard::Verdict::Redact { rules, .. } => {
            serde_json::json!({ "verdict": "redact", "rules": rules })
        }
        wovyr_ui_guard::Verdict::Block { rule, reason } => {
            serde_json::json!({ "verdict": "block", "rule": rule, "reason": reason })
        }
    };

    pack(
        serde_json::json!({
            "verdict": verdict["verdict"],
            "rule": verdict.get("rule"),
            "reason": verdict.get("reason"),
            "adapter_notes": adapted.notes,
            "derived_frame": adapted.frame,
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(messages: serde_json::Value) -> serde_json::Value {
        let adapted = a2ui::adapt(messages.as_array().unwrap()).expect("adapt");
        let policy = wovyr_ui_guard::UiPolicy {
            name: "t".into(),
            version: 1,
            rules: wovyr_ui_guard::PolicyRules::default(),
        };
        let v = match wovyr_ui_guard::evaluate(&policy, &adapted.frame) {
            wovyr_ui_guard::Verdict::Allow => json!({"verdict": "allow"}),
            wovyr_ui_guard::Verdict::Redact { .. } => json!({"verdict": "redact"}),
            wovyr_ui_guard::Verdict::Block { rule, .. } => {
                json!({"verdict": "block", "rule": rule})
            }
        };
        json!({ "v": v, "notes": adapted.notes })
    }

    fn surface(components: serde_json::Value) -> serde_json::Value {
        json!([{ "version": "v0.9",
                 "updateComponents": { "surfaceId": "s", "components": components } }])
    }

    #[test]
    fn credential_field_is_blocked_through_the_adapter() {
        let out = run(surface(json!([
            {"id":"root","component":"Column","children":["f"]},
            {"id":"f","component":"TextField","label":"Card number",
             "value":{"path":"/payment/cardNumber"}}
        ])));
        assert_eq!(out["v"]["verdict"], "block");
        assert_eq!(out["v"]["rule"], "sensitive_input");
    }

    #[test]
    fn a_data_bound_label_is_resolved_not_skipped() {
        // The credential-shaped label lives in the data model, not the component.
        // A policy inspecting components only would see nothing.
        let msgs = json!([
            {"version":"v0.9","updateComponents":{"surfaceId":"s","components":[
                {"id":"root","component":"Column","children":["f"]},
                {"id":"f","component":"TextField","label":{"path":"/labels/pw"},
                 "value":{"path":"/auth/x"}}]}},
            {"version":"v0.9","updateDataModel":{"surfaceId":"s",
                "value":{"labels":{"pw":"Password"}}}}
        ]);
        let out = run(msgs);
        assert_eq!(out["v"]["verdict"], "block");
        assert_eq!(out["v"]["rule"], "sensitive_input");
    }

    #[test]
    fn button_label_resolves_through_the_child_reference() {
        let out = run(surface(json!([
            {"id":"root","component":"Column","children":["b"]},
            {"id":"b","component":"Button","child":"l",
             "action":{"event":{"name":"go"}}},
            {"id":"l","component":"Text","text":"Delete account"}
        ])));
        // Caught only because the label reads destructive — not because A2UI said so.
        assert_eq!(out["v"]["verdict"], "block");
        assert_eq!(out["v"]["rule"], "intent_mismatch");
    }

    #[test]
    fn cancel_label_deception_is_undetectable_without_an_action_class() {
        // In wovyr's own protocol this blocks (affirmative class + negative label).
        // Through A2UI there is no class, so nothing contradicts the label.
        let out = run(surface(json!([
            {"id":"root","component":"Column","children":["b"]},
            {"id":"b","component":"Button","child":"l",
             "action":{"event":{"name":"confirm_purchase"}}},
            {"id":"l","component":"Text","text":"Cancel"}
        ])));
        assert_eq!(out["v"]["verdict"], "allow");
    }

    #[test]
    fn every_button_surface_reports_the_missing_action_class() {
        let out = run(surface(json!([
            {"id":"root","component":"Column","children":["b"]},
            {"id":"b","component":"Button","child":"l","action":{"event":{"name":"ok"}}},
            {"id":"l","component":"Text","text":"OK"}
        ])));
        let notes = out["notes"].as_array().unwrap();
        assert!(
            notes.iter().any(|n| n.as_str().unwrap().contains("no semantic action class")),
            "expected the forced-neutral note, got {notes:?}"
        );
    }
}
