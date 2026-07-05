//! `${path}` placeholder resolution for activity inputs.
//!
//! The engine hands each [`ActivityExecutor`](crate::ActivityExecutor) the *raw*
//! [`Definition`](crate::Definition) inputs — it does not interpolate them itself
//! ([execution model §14](../../docs/03-workflow-engine/execution-model.md) keeps the
//! core deterministic and free of this kind of policy). Resolving `${activity.field}`
//! references against the current workflow variables is therefore an executor
//! concern; [`resolve`] is the shared implementation so every executor (the CLI's
//! local runner, the server's) interpolates identically instead of drifting.
//!
//! Originally lived only in `apps/apex-cli/src/workflow.rs`'s `PlatformExecutor` —
//! extracted here after the server's `ServerExecutor` was found to have no
//! interpolation at all, so a workflow like `examples/workflows/agent-review.yaml`
//! (`inputs: { message: "${input.ticketText}" }`) passed the *literal* `${...}`
//! string to the agent when run through `apex-server`, never the referenced value.

use crate::ActivityContext;
use serde_json::Value;

/// Recursively replace `${path}` placeholders in string inputs with the resolved
/// workflow variable. A string that is exactly `${path}` adopts the value's JSON
/// type; embedded placeholders are stringified.
pub fn resolve(value: &Value, ctx: &ActivityContext) -> Value {
    match value {
        Value::String(s) => substitute(s, ctx),
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve(v, ctx)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve(v, ctx)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn substitute(s: &str, ctx: &ActivityContext) -> Value {
    let trimmed = s.trim();
    if let Some(path) = trimmed
        .strip_prefix("${")
        .and_then(|r| r.strip_suffix('}'))
        .filter(|_| trimmed.starts_with("${") && trimmed.ends_with('}'))
    {
        return lookup(path.trim(), ctx);
    }
    // Replace any embedded ${...} occurrences with their stringified values.
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let val = lookup(after[..end].trim(), ctx);
            out.push_str(&value_to_string(&val));
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    Value::String(out)
}

/// Resolve a dotted path against the activity's variables.
fn lookup(path: &str, ctx: &ActivityContext) -> Value {
    let mut parts = path.split('.');
    let Some(head) = parts.next() else {
        return Value::Null;
    };
    let Some(mut current) = ctx.variables.get(head).cloned() else {
        return Value::Null;
    };
    for part in parts {
        match current.get(part) {
            Some(v) => current = v.clone(),
            None => return Value::Null,
        }
    }
    current
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn ctx_with(variables: BTreeMap<String, Value>) -> ActivityContext {
        ActivityContext {
            id: "test".to_string(),
            activity_type: "function".to_string(),
            name: None,
            inputs: Value::Null,
            variables,
            attempt: 1,
        }
    }

    #[test]
    fn exact_placeholder_adopts_the_value_type() {
        let mut vars = BTreeMap::new();
        vars.insert("input".to_string(), json!({"topic": "rust"}));
        let ctx = ctx_with(vars);
        assert_eq!(
            resolve(&json!("${input.topic}"), &ctx),
            Value::String("rust".to_string())
        );
    }

    #[test]
    fn embedded_placeholder_is_stringified() {
        let mut vars = BTreeMap::new();
        vars.insert("draft".to_string(), json!({"message": "hi"}));
        let ctx = ctx_with(vars);
        assert_eq!(
            resolve(&json!("reply: ${draft.message}!"), &ctx),
            Value::String("reply: hi!".to_string())
        );
    }

    #[test]
    fn unresolved_path_becomes_null() {
        let ctx = ctx_with(BTreeMap::new());
        assert_eq!(resolve(&json!("${missing.field}"), &ctx), Value::Null);
    }

    #[test]
    fn nested_objects_and_arrays_are_resolved_recursively() {
        let mut vars = BTreeMap::new();
        vars.insert("a".to_string(), json!("x"));
        vars.insert("b".to_string(), json!("y"));
        let ctx = ctx_with(vars);
        let input = json!({"list": ["${a}", "${b}"], "nested": {"k": "${a}"}});
        assert_eq!(
            resolve(&input, &ctx),
            json!({"list": ["x", "y"], "nested": {"k": "x"}})
        );
    }
}
