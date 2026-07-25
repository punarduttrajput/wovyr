//! Tool JSON-Schema normalization for strict / structured-output modes
//! (RM-AIM-P2 PRV-203).
//!
//! Vendor strict validators (OpenAI structured outputs, Anthropic strict tool
//! use) accept only a JSON-Schema subset: no numeric/string/array bounds
//! keywords, `additionalProperties: false` on every object, and every declared
//! property listed in `required`. A schema that's perfectly valid for a
//! *non*-strict call gets a 400 from a strict one. [`normalize_strict`]
//! rewrites a tool's parameter schema into that subset so a caller can flip
//! [`crate::ToolSpec::strict`] without hand-editing schemas.
//!
//! Deliberately applied **only when `strict` is requested**: in non-strict
//! mode providers ignore unknown keywords but do honor bounds like `minimum`,
//! so stripping them unconditionally would silently discard real constraints.

use serde_json::{Map, Value, json};

/// Keywords the strict-mode validators reject. Stripped from every schema
/// node; the constraint they expressed is lost for the strict call (the tool
/// itself must still validate its inputs — [tool-framework §4](../../docs/04-agent-framework/tool-framework.md)).
const UNSUPPORTED_IN_STRICT: &[&str] = &[
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "patternProperties",
    "default",
];

/// Schema keys whose value is itself a schema (recurse into it).
const SUBSCHEMA_KEYS: &[&str] = &["items", "not", "if", "then", "else"];
/// Schema keys whose value is an *array* of schemas.
const SUBSCHEMA_LIST_KEYS: &[&str] = &["anyOf", "allOf", "oneOf"];
/// Schema keys whose value is a *map* of schemas.
const SUBSCHEMA_MAP_KEYS: &[&str] = &["properties", "$defs", "definitions"];

/// Rewrite `schema` into the strict-mode subset: strip unsupported keywords,
/// force `additionalProperties: false` on every object node, and require every
/// declared property (the OpenAI strict rule; Anthropic's is a compatible
/// subset). Pure and non-destructive — returns a new value.
pub(crate) fn normalize_strict(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                if UNSUPPORTED_IN_STRICT.contains(&key.as_str()) {
                    continue;
                }
                let normalized = if SUBSCHEMA_KEYS.contains(&key.as_str()) {
                    normalize_strict(value)
                } else if SUBSCHEMA_LIST_KEYS.contains(&key.as_str()) {
                    match value {
                        Value::Array(items) => {
                            Value::Array(items.iter().map(normalize_strict).collect())
                        }
                        other => other.clone(),
                    }
                } else if SUBSCHEMA_MAP_KEYS.contains(&key.as_str()) {
                    match value {
                        Value::Object(entries) => Value::Object(
                            entries
                                .iter()
                                .map(|(name, sub)| (name.clone(), normalize_strict(sub)))
                                .collect(),
                        ),
                        other => other.clone(),
                    }
                } else {
                    value.clone()
                };
                out.insert(key.clone(), normalized);
            }

            // An object node must close its property set and require every key.
            let is_object_schema = out.get("type").and_then(Value::as_str) == Some("object")
                || out.contains_key("properties");
            if is_object_schema {
                out.insert("additionalProperties".to_string(), json!(false));
                let all_keys: Vec<Value> = out
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|props| props.keys().map(|k| json!(k)).collect())
                    .unwrap_or_default();
                out.insert("required".to_string(), Value::Array(all_keys));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_unsupported_keywords_and_closes_objects() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 0, "maximum": 10 },
                "name": { "type": "string", "minLength": 1, "format": "email" }
            },
            "required": ["count"]
        });
        let out = normalize_strict(&schema);
        assert_eq!(out["properties"]["count"], json!({ "type": "integer" }));
        assert_eq!(out["properties"]["name"], json!({ "type": "string" }));
        assert_eq!(out["additionalProperties"], json!(false));
        // Strict mode requires *every* declared property, not just the original set.
        assert_eq!(out["required"], json!(["count", "name"]));
    }

    #[test]
    fn recurses_into_nested_objects_arrays_and_unions() {
        let schema = json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": { "id": { "type": "string", "pattern": "^x" } }
                    }
                },
                "mode": {
                    "anyOf": [
                        { "type": "string", "maxLength": 3 },
                        { "type": "integer", "multipleOf": 2 }
                    ]
                }
            }
        });
        let out = normalize_strict(&schema);
        let inner = &out["properties"]["items"]["items"];
        assert_eq!(inner["properties"]["id"], json!({ "type": "string" }));
        assert_eq!(inner["additionalProperties"], json!(false));
        assert_eq!(inner["required"], json!(["id"]));
        assert!(out["properties"]["items"].get("minItems").is_none());
        assert_eq!(
            out["properties"]["mode"]["anyOf"][0],
            json!({ "type": "string" })
        );
        assert_eq!(
            out["properties"]["mode"]["anyOf"][1],
            json!({ "type": "integer" })
        );
    }

    #[test]
    fn property_named_like_a_keyword_is_not_stripped() {
        // `format` here is a *property name* under `properties`, not a schema
        // keyword — it must survive (only its own keyword-position uses are
        // stripped).
        let schema = json!({
            "type": "object",
            "properties": { "format": { "type": "string" } }
        });
        let out = normalize_strict(&schema);
        assert_eq!(out["properties"]["format"], json!({ "type": "string" }));
        assert_eq!(out["required"], json!(["format"]));
    }

    #[test]
    fn propertyless_object_gets_empty_required() {
        let out = normalize_strict(&json!({ "type": "object" }));
        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(out["required"], json!([]));
    }

    #[test]
    fn non_object_schemas_pass_through() {
        assert_eq!(normalize_strict(&json!(true)), json!(true));
        assert_eq!(
            normalize_strict(&json!({ "type": "string", "maxLength": 5 })),
            json!({ "type": "string" })
        );
    }
}
