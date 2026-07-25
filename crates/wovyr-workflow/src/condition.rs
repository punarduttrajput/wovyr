//! Guard-expression evaluation for conditional branching.
//!
//! Transitions may carry a `when` guard
//! ([workflow DSL §11](../../docs/03-workflow-engine/workflow-dsl.md#11-conditional-branching));
//! an edge is only followed when its guard evaluates true against the current
//! workflow variables. This is a deliberately small, side-effect-free expression
//! language (no function calls, ambient state, or I/O — keeping the engine
//! deterministic): boolean combinations of comparisons.
//!
//! Grammar (lowest → highest precedence):
//!
//! ```text
//! expr       := or
//! or         := and ( "||" and )*
//! and        := compare ( "&&" compare )*
//! compare    := operand ( ("=="|"!="|">="|"<="|">"|"<") operand )?
//! operand    := literal | path
//! literal    := quoted-string | number | "true" | "false"
//! path       := ident ( "." ident )*      ; resolved against variables (incl. `input.*`)
//! ```
//!
//! A bare `path` with no comparison is truthy when it resolves to a non-null,
//! non-false value.

use serde_json::Value;
use std::collections::BTreeMap;

/// Evaluate a guard expression against `vars`. An empty expression is always true
/// (an unconditional edge). Returns an error string on a malformed expression.
pub fn evaluate(expr: &str, vars: &BTreeMap<String, Value>) -> Result<bool, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Ok(true);
    }
    eval_or(expr, vars)
}

fn eval_or(expr: &str, vars: &BTreeMap<String, Value>) -> Result<bool, String> {
    for part in split_top(expr, "||") {
        if eval_and(&part, vars)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn eval_and(expr: &str, vars: &BTreeMap<String, Value>) -> Result<bool, String> {
    for part in split_top(expr, "&&") {
        if !eval_compare(&part, vars)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Operators ordered so two-character forms are matched before their prefixes.
const OPERATORS: &[&str] = &["==", "!=", ">=", "<=", ">", "<"];

fn eval_compare(expr: &str, vars: &BTreeMap<String, Value>) -> Result<bool, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("empty comparison".to_string());
    }
    for op in OPERATORS {
        if let Some(idx) = find_op(expr, op) {
            let lhs = resolve(expr[..idx].trim(), vars);
            let rhs = resolve(expr[idx + op.len()..].trim(), vars);
            return Ok(compare(&lhs, op, &rhs));
        }
    }
    // No operator: truthiness of the operand.
    Ok(truthy(&resolve(expr, vars)))
}

/// Resolve an operand to a JSON value: a quoted string, number, or bool literal,
/// otherwise a variable path (null if absent).
fn resolve(token: &str, vars: &BTreeMap<String, Value>) -> Value {
    let token = token.trim();
    if (token.starts_with('\'') && token.ends_with('\'') && token.len() >= 2)
        || (token.starts_with('"') && token.ends_with('"') && token.len() >= 2)
    {
        return Value::String(token[1..token.len() - 1].to_string());
    }
    match token {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if let Ok(n) = token.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return Value::Number(num);
        }
    }
    resolve_path(token, vars)
}

/// Resolve a dotted path (`a.b.c`) into the variables map, descending into objects.
fn resolve_path(path: &str, vars: &BTreeMap<String, Value>) -> Value {
    let mut parts = path.split('.');
    let Some(head) = parts.next() else {
        return Value::Null;
    };
    let Some(mut current) = vars.get(head).cloned() else {
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

/// Apply a comparison operator to two resolved values.
fn compare(lhs: &Value, op: &str, rhs: &Value) -> bool {
    match op {
        "==" => values_equal(lhs, rhs),
        "!=" => !values_equal(lhs, rhs),
        ">" | "<" | ">=" | "<=" => match (as_f64(lhs), as_f64(rhs)) {
            (Some(a), Some(b)) => match op {
                ">" => a > b,
                "<" => a < b,
                ">=" => a >= b,
                "<=" => a <= b,
                _ => unreachable!(),
            },
            _ => false,
        },
        _ => false,
    }
}

/// Equality with numeric coercion (so `1` and `1.0` match).
fn values_equal(a: &Value, b: &Value) -> bool {
    match (as_f64(a), as_f64(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// A value is truthy if it is not null, not `false`, not an empty string, and not 0.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        _ => true,
    }
}

/// Find `op` at the top level of `expr`, skipping anything inside quotes. Returns
/// the byte index of the operator, if present.
fn find_op(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i + op_bytes.len() <= bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                i += 1;
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                    i += 1;
                } else if bytes[i..i + op_bytes.len()] == *op_bytes {
                    return Some(i);
                } else {
                    i += 1;
                }
            }
        }
    }
    None
}

/// Split `expr` on a top-level `sep` (not inside quotes).
fn split_top(expr: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = expr;
    while let Some(idx) = find_op(rest, sep) {
        parts.push(rest[..idx].to_string());
        rest = &rest[idx + sep.len()..];
    }
    parts.push(rest.to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> BTreeMap<String, Value> {
        let mut v = BTreeMap::new();
        v.insert(
            "input".to_string(),
            json!({"intent": "refund", "amount": 150}),
        );
        v.insert("classify".to_string(), json!({"intent": "refund"}));
        v.insert("amount".to_string(), json!(150));
        v
    }

    #[test]
    fn empty_is_true() {
        assert!(evaluate("", &vars()).unwrap());
    }

    #[test]
    fn string_equality_and_inequality() {
        assert!(evaluate("classify.intent == 'refund'", &vars()).unwrap());
        assert!(!evaluate("classify.intent != 'refund'", &vars()).unwrap());
        assert!(evaluate("input.intent != 'question'", &vars()).unwrap());
    }

    #[test]
    fn numeric_comparison_with_input_path() {
        assert!(evaluate("input.amount > 100", &vars()).unwrap());
        assert!(!evaluate("input.amount <= 100", &vars()).unwrap());
        assert!(evaluate("amount >= 150", &vars()).unwrap());
    }

    #[test]
    fn conjunction_and_disjunction() {
        assert!(evaluate("input.intent == 'refund' && input.amount > 100", &vars()).unwrap());
        assert!(!evaluate("input.intent == 'refund' && input.amount > 200", &vars()).unwrap());
        assert!(evaluate("input.amount > 200 || input.intent == 'refund'", &vars()).unwrap());
    }

    #[test]
    fn missing_path_is_null_and_falsey() {
        assert!(!evaluate("missing.field == 'x'", &vars()).unwrap());
        assert!(!evaluate("missing", &vars()).unwrap());
    }

    #[test]
    fn quoted_operator_is_not_split() {
        let mut v = BTreeMap::new();
        v.insert("label".to_string(), json!("a && b"));
        assert!(evaluate("label == 'a && b'", &v).unwrap());
    }
}
