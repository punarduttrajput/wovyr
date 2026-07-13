//! A real, local, offline LLM backend using [mistral.rs](https://github.com/EricLBuehler/mistral.rs).
//!
//! Unlike [`crate::mock::MockProvider`] (a canned template) and
//! [`crate::openai::OpenAiProvider`] (a real model, but over the network via a
//! vendor API), this backend loads actual model weights in-process and runs
//! real inference. `chat` sets no sampling parameters, so this was assumed to
//! be the first genuinely non-deterministic provider in this workspace's own
//! tests — **empirically corrected**: `apex-eval`'s
//! `real_model_comparison_variance_over_n_runs`
//! (`crates/apex-eval/tests/real_model_comparison.rs`) ran the identical
//! prompt against the default `Qwen/Qwen2.5-0.5B-Instruct-GGUF` config 4 times
//! and got byte-identical output every time (same token counts, same text) —
//! this configuration behaves deterministically in practice, most likely
//! because mistral.rs defaults to greedy decoding when no sampler is set.
//! Whether that holds for other models/prompts, or would change if a sampler
//! were configured, is untested. Loads a GGUF-quantized model straight from a
//! HuggingFace repo (mistral.rs handles the download via `hf-hub`); default
//! configuration points at `Qwen/Qwen2.5-0.5B-Instruct-GGUF`, a small,
//! ungated, Apache-2.0 model — enough to prove the integration end to end
//! without a large download or GPU.
//!
//! **Deliberately not wired into [`crate::Gateway::from_env`]'s fallback
//! chain** — that path is synchronous and meant to stay a cheap, safe default
//! (OpenAI if a key is set, else the zero-cost mock); loading a local model is
//! async and downloads real weight files on first use. A caller opts in
//! explicitly via [`MistralRsProvider::from_env`].
//!
//! Tool calling goes through mistral.rs's low-level `RequestBuilder`/`Tool`
//! API, not its own `AgentBuilder` — the tool-calling *loop* stays owned by
//! `apex_agent::run_agent`, so there is only ever one agent loop in the
//! platform, not two competing ones.

use crate::provider::AIProvider;
use crate::types::{
    ChatRequest, ChatResponse, Message, ResponseFormat, Role, ToolCall, ToolChoice, ToolSpec,
};
use apex_common::{Error, Result, Usage};
use async_trait::async_trait;
use mistralrs::{
    CalledFunction, Constraint, Function, GgufModelBuilder, Model, RequestBuilder, TextMessageRole,
    Tool, ToolCallResponse, ToolCallType, ToolChoice as MrsToolChoice, ToolType,
};
use serde_json::Value;
use std::collections::HashMap;

/// Default model: small, ungated, Apache-2.0 — see the module doc.
const DEFAULT_GGUF_REPO: &str = "Qwen/Qwen2.5-0.5B-Instruct-GGUF";
const DEFAULT_GGUF_FILE: &str = "qwen2.5-0.5b-instruct-q4_k_m.gguf";
const DEFAULT_TOK_MODEL_ID: &str = "Qwen/Qwen2.5-0.5B-Instruct";

/// A provider backed by a locally-loaded GGUF model via mistral.rs.
pub struct MistralRsProvider {
    model: Model,
    /// The GGUF filename, used as the `model` label on responses (mistral.rs
    /// has exactly one model per `Model` instance — there is no selection to
    /// honor from `ChatRequest.model`).
    model_name: String,
}

impl MistralRsProvider {
    /// Build from the environment: `APEX_MISTRALRS_GGUF_REPO` / `_GGUF_FILE` /
    /// `_TOK_MODEL_ID`, each defaulting to Qwen2.5-0.5B-Instruct. Downloads the
    /// GGUF file (and tokenizer/chat-template source) on first use if not
    /// already cached by `hf-hub` — real network access, real disk usage.
    pub async fn from_env() -> Result<Self> {
        let repo =
            std::env::var("APEX_MISTRALRS_GGUF_REPO").unwrap_or_else(|_| DEFAULT_GGUF_REPO.into());
        let file =
            std::env::var("APEX_MISTRALRS_GGUF_FILE").unwrap_or_else(|_| DEFAULT_GGUF_FILE.into());
        let tok_model_id = std::env::var("APEX_MISTRALRS_TOK_MODEL_ID")
            .unwrap_or_else(|_| DEFAULT_TOK_MODEL_ID.into());

        let model = GgufModelBuilder::new(repo.clone(), vec![file.clone()])
            .with_tok_model_id(tok_model_id)
            .with_logging()
            .build()
            .await
            .map_err(|e| {
                Error::provider(format!("failed to load mistralrs model {repo}/{file}: {e}"))
            })?;

        Ok(Self {
            model,
            model_name: file,
        })
    }
}

/// A tool spec, normalized to mistral.rs's `Tool`/`Function` shape. JSON
/// Schema `parameters` map straight onto `Function.parameters`'s
/// `HashMap<String, Value>` since a JSON Schema object's top-level fields
/// (`type`, `properties`, `required`, …) are themselves a flat object.
fn to_mistralrs_tool(spec: &ToolSpec) -> Tool {
    let parameters = match &spec.parameters {
        Value::Object(map) => Some(map.clone().into_iter().collect::<HashMap<String, Value>>()),
        _ => None,
    };
    Tool {
        tp: ToolType::Function,
        function: Function {
            name: spec.name.clone(),
            description: Some(spec.description.clone()),
            parameters,
        },
    }
}

/// Replay a previously-issued [`ToolCall`] (from an earlier assistant turn in
/// the conversation history) back into mistral.rs's response-shaped call
/// type — needed because `add_message_with_tool_call` takes the same type the
/// model's own responses use, not the request-side [`ToolCall`].
fn to_tool_call_response(call: &ToolCall, index: usize) -> ToolCallResponse {
    ToolCallResponse {
        index,
        id: call.id.clone(),
        tp: ToolCallType::Function,
        function: CalledFunction {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        },
    }
}

/// The inverse: a tool call the model just made, normalized back to
/// [`ToolCall`].
fn from_tool_call_response(call: ToolCallResponse) -> ToolCall {
    ToolCall {
        id: call.id,
        name: call.function.name,
        arguments: call.function.arguments,
    }
}

#[async_trait]
impl AIProvider for MistralRsProvider {
    fn name(&self) -> &str {
        "mistralrs"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let mut builder = RequestBuilder::new();
        for msg in &request.messages {
            let text = msg.content.clone().unwrap_or_default();
            builder = match msg.role {
                Role::System => builder.add_message(TextMessageRole::System, text),
                Role::User => builder.add_message(TextMessageRole::User, text),
                Role::Assistant if msg.tool_calls.is_empty() => {
                    builder.add_message(TextMessageRole::Assistant, text)
                }
                Role::Assistant => {
                    let calls: Vec<ToolCallResponse> = msg
                        .tool_calls
                        .iter()
                        .enumerate()
                        .map(|(i, tc)| to_tool_call_response(tc, i))
                        .collect();
                    builder.add_message_with_tool_call(TextMessageRole::Assistant, text, calls)
                }
                Role::Tool => {
                    let call_id = msg.tool_call_id.clone().unwrap_or_default();
                    builder.add_tool_message(text, call_id)
                }
            };
        }

        if !request.tools.is_empty() {
            let tools: Vec<Tool> = request.tools.iter().map(to_mistralrs_tool).collect();
            // Translate the normalized tool-choice constraint (RM-AIM-P2
            // PRV-202). mistral.rs has no "must call *some* tool" variant, so
            // `Required` fails closed rather than silently degrading to Auto.
            let choice = match &request.tool_choice {
                None | Some(ToolChoice::Auto) => MrsToolChoice::Auto,
                Some(ToolChoice::None) => MrsToolChoice::None,
                Some(ToolChoice::Tool(name)) => {
                    let spec =
                        request.tools.iter().find(|t| &t.name == name).ok_or_else(|| {
                            Error::invalid(format!(
                                "tool_choice names `{name}`, which is not among the advertised tools"
                            ))
                        })?;
                    MrsToolChoice::Tool(to_mistralrs_tool(spec))
                }
                Some(ToolChoice::Required) => {
                    return Err(Error::invalid(
                        "mistralrs does not support tool_choice `required`; pin a specific tool",
                    ));
                }
            };
            builder = builder.set_tools(tools).set_tool_choice(choice);
        }
        // Structured output via mistral.rs's grammar constraint (PRV-202) —
        // real constrained decoding, so the schema is enforced at generation.
        if let Some(rf) = &request.response_format {
            match rf {
                ResponseFormat::JsonSchema { schema, .. } => {
                    builder = builder.set_constraint(Constraint::JsonSchema(schema.clone()));
                }
                ResponseFormat::JsonObject => {
                    return Err(Error::invalid(
                        "mistralrs has no schema-less JSON mode; use ResponseFormat::JsonSchema",
                    ));
                }
            }
        }

        let response = self
            .model
            .send_chat_request(builder)
            .await
            .map_err(|e| Error::provider(format!("mistralrs chat request failed: {e}")))?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::provider("mistralrs returned no choices"))?;

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(from_tool_call_response)
            .collect();

        let message = Message {
            role: Role::Assistant,
            content: choice.message.content,
            tool_calls,
            tool_call_id: None,
            name: None,
        };

        // A local model has no per-token vendor price, so $0 is the *correct* cost
        // here — not the placeholder-zero PRV-101 removed from `OpenAiProvider`. The
        // machine's own compute/electricity isn't metered by this platform, and there
        // is no API bill; the `PriceBook` deliberately doesn't apply to this backend.
        let usage = Usage::new(
            response.usage.prompt_tokens as u32,
            response.usage.completion_tokens as u32,
            0.0,
        );

        Ok(ChatResponse {
            message,
            model: self.model_name.clone(),
            usage,
            finish_reason: choice.finish_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "get_weather".to_string(),
            description: "Get the weather for a place.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "place": { "type": "string" } },
                "required": ["place"],
            }),
            strict: false,
        }
    }

    #[test]
    fn tool_spec_converts_to_mistralrs_function_shape() {
        let tool = to_mistralrs_tool(&spec());
        assert!(matches!(tool.tp, ToolType::Function));
        assert_eq!(tool.function.name, "get_weather");
        assert_eq!(
            tool.function.description.as_deref(),
            Some("Get the weather for a place.")
        );
        let params = tool.function.parameters.expect("parameters set");
        assert_eq!(params.get("type").and_then(|v| v.as_str()), Some("object"));
        assert!(params.contains_key("properties"));
    }

    #[test]
    fn non_object_parameters_convert_to_none() {
        let mut spec = spec();
        spec.parameters = serde_json::json!("not an object");
        let tool = to_mistralrs_tool(&spec);
        assert!(tool.function.parameters.is_none());
    }

    #[test]
    fn tool_call_round_trips_through_the_mistralrs_response_shape() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            arguments: r#"{"place":"Boston"}"#.to_string(),
        };
        let response_shaped = to_tool_call_response(&call, 0);
        assert_eq!(response_shaped.index, 0);
        assert_eq!(response_shaped.tp, ToolCallType::Function);

        let round_tripped = from_tool_call_response(response_shaped);
        assert_eq!(round_tripped.id, call.id);
        assert_eq!(round_tripped.name, call.name);
        assert_eq!(round_tripped.arguments, call.arguments);
    }
}
