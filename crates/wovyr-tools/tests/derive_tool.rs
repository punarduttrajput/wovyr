//! SBX-303 acceptance test: a tool defined via `#[derive(Tool)]` round-trips a
//! real JSON Schema (via `schemars`) and typed parameter parsing (via `serde`)
//! with **no hand-written JSON** anywhere — no `json!({...})` schema literal, no
//! `request.parameters.get("x").and_then(Value::as_str)` chain. Compare this file
//! against `wovyr_tools::builtin`'s hand-written tools (e.g. `FsReadTool`) to see
//! exactly what the derive eliminates.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use wovyr_tool_macros::Tool;
use wovyr_tools::{ToolContext, ToolError, ToolRegistry, ToolRequest, ToolResponse};

/// Typed, schema-derived parameters — the derive's `input_schema()` comes from
/// this struct's `JsonSchema` impl, not a hand-written literal.
#[derive(Deserialize, JsonSchema)]
struct GreetParams {
    /// Name to greet.
    name: String,
    /// Number of times to repeat the greeting (default 1).
    #[serde(default = "default_count")]
    count: u32,
}

fn default_count() -> u32 {
    1
}

#[derive(Tool)]
#[tool(
    id = "greet",
    version = "1.0.0",
    category = "utility",
    description = "Greet someone by name.",
    params = GreetParams,
    permissions = ["greet.run"],
)]
struct GreetTool;

#[async_trait::async_trait]
impl wovyr_tools::Tool for GreetTool {
    fn metadata(&self) -> wovyr_tools::ToolMetadata {
        Self::__tool_metadata()
    }

    fn input_schema(&self) -> serde_json::Value {
        Self::__tool_input_schema()
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        // Typed, not `request.parameters.get("name").and_then(Value::as_str)`.
        let params = Self::__tool_parse_params(&request)?;
        let greeting =
            std::iter::repeat_n(format!("Hello, {}!", params.name), params.count as usize)
                .collect::<Vec<_>>()
                .join(" ");
        Ok(ToolResponse::success(json!({ "greeting": greeting })))
    }
}

#[test]
fn generated_metadata_matches_the_tool_attribute() {
    use wovyr_tools::Tool as _;
    let t = GreetTool;
    let meta = t.metadata();
    assert_eq!(meta.id, "greet");
    assert_eq!(meta.version, "1.0.0");
    assert_eq!(meta.category, "utility");
    assert_eq!(meta.description, "Greet someone by name.");
    assert_eq!(meta.permissions, vec!["greet.run".to_string()]);
}

#[test]
fn generated_schema_is_a_real_json_schema_derived_from_the_params_struct() {
    use wovyr_tools::Tool as _;
    let schema = GreetTool.input_schema();
    // A real, non-trivial object schema — not a hand-authored stand-in.
    assert_eq!(schema["type"], "object");
    let properties = schema["properties"]
        .as_object()
        .expect("schema must declare properties");
    assert!(properties.contains_key("name"));
    assert!(properties.contains_key("count"));
    assert_eq!(properties["name"]["type"], "string");
    // `name` has no default, so it's required; `count` has one, so it isn't.
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema must declare required fields")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"name"));
    assert!(!required.contains(&"count"));
}

#[tokio::test]
async fn execute_round_trips_typed_parameters_end_to_end() {
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(GreetTool));

    let ctx = ToolContext::default();
    let resp = registry
        .execute(
            "greet",
            &ctx,
            ToolRequest::new(json!({"name": "Wovyr", "count": 2})),
        )
        .await
        .unwrap();
    assert!(resp.success);
    assert_eq!(resp.payload["greeting"], "Hello, Wovyr! Hello, Wovyr!");

    // The `count` default (via serde, not the derive) applies when omitted.
    let resp = registry
        .execute("greet", &ctx, ToolRequest::new(json!({"name": "Wovyr"})))
        .await
        .unwrap();
    assert_eq!(resp.payload["greeting"], "Hello, Wovyr!");
}

#[tokio::test]
async fn malformed_parameters_are_a_validation_error_not_a_panic() {
    let t = GreetTool;
    use wovyr_tools::Tool as _;
    // `name` is required (no default) and must be a string — both a missing
    // field and a wrong-typed one are rejected before `execute`'s own logic runs.
    let err = t
        .execute(&ToolContext::default(), ToolRequest::new(json!({})))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Validation(_)), "{err:?}");

    let err = t
        .execute(
            &ToolContext::default(),
            ToolRequest::new(json!({"name": 42})),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Validation(_)), "{err:?}");
}
