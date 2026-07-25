//! Vendor-neutral data models for chat completion.
//!
//! These mirror the normalized request/response shapes described in the
//! [Provider SDK spec](../../docs/04-agent-framework/provider-sdk.md) (§14
//! function calling, §13 streaming). Provider adapters translate to and from
//! their wire formats so the rest of the platform stays provider-independent.

use serde::{Deserialize, Serialize};

/// The author of a [`Message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System / developer instructions.
    System,
    /// End-user input.
    User,
    /// Model output.
    Assistant,
    /// Result of a tool invocation, fed back to the model.
    Tool,
}

/// One typed part of a multimodal message (RM-AIM-P2 PRV-204).
///
/// Providers translate each part to their own wire blocks and fail closed
/// ([`wovyr_common::Error::Invalid`]) on parts they can't express (e.g. audio
/// on Anthropic, anything non-text on mistral.rs) rather than silently
/// dropping them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text, interleaved with the other parts in order.
    Text {
        /// The text.
        text: String,
    },
    /// An image the provider fetches from a URL.
    ImageUrl {
        /// Publicly fetchable image URL.
        url: String,
    },
    /// An inline base64-encoded image.
    Image {
        /// MIME type, e.g. `image/png`.
        media_type: String,
        /// Base64-encoded image bytes (no data-URI prefix).
        data: String,
    },
    /// Inline base64-encoded audio. Only OpenAI-compatible endpoints accept
    /// audio input today; other providers fail closed.
    Audio {
        /// MIME type, e.g. `audio/wav` or `audio/mp3`.
        media_type: String,
        /// Base64-encoded audio bytes.
        data: String,
    },
}

impl ContentPart {
    /// A text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// An image referenced by URL.
    pub fn image_url(url: impl Into<String>) -> Self {
        Self::ImageUrl { url: url.into() }
    }

    /// An inline base64 image.
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            media_type: media_type.into(),
            data: data.into(),
        }
    }

    /// Inline base64 audio.
    pub fn audio_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Audio {
            media_type: media_type.into(),
            data: data.into(),
        }
    }
}

/// A single conversation message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// Who produced the message.
    pub role: Role,
    /// Text content. `None` when an assistant turn is purely tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Additional typed multimodal parts (RM-AIM-P2 PRV-204), rendered *after*
    /// `content`'s text, in order. Only `Role::User` turns may carry parts —
    /// providers reject them elsewhere fail-closed. Empty (the default) keeps
    /// the plain-text wire shape, so text-only callers are untouched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<ContentPart>,
    /// Tool calls requested by the assistant (empty for other roles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Role::Tool` messages, the id of the call this responds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional tool name (used by some providers on tool result messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    /// A system message carrying instructions.
    pub fn system(content: impl Into<String>) -> Self {
        Self::text(Role::System, content)
    }

    /// A user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }

    /// An assistant message containing text.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }

    /// A tool-result message replying to `tool_call_id`.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            parts: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }

    /// A user message made of typed multimodal parts (RM-AIM-P2 PRV-204).
    pub fn user_with_parts(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: None,
            parts,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    /// Append one multimodal part (builder-style).
    pub fn with_part(mut self, part: ContentPart) -> Self {
        self.parts.push(part);
        self
    }

    fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            parts: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
}

/// A model's request to invoke a tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned call id (correlates with the tool-result message).
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// JSON-encoded arguments (kept as a string to match provider wire formats).
    pub arguments: String,
}

/// A tool advertised to the model, normalized to a JSON-Schema parameter object.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name the model calls.
    pub name: String,
    /// Human/model-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: serde_json::Value,
    /// Request vendor strict/guaranteed argument validation (RM-AIM-P2
    /// PRV-203). When set, providers normalize `parameters` into the
    /// strict-mode schema subset (unsupported keywords stripped, objects
    /// closed, every property required) and flag the tool `strict` on the
    /// wire; when unset (the default) the schema is forwarded verbatim.
    #[serde(default)]
    pub strict: bool,
}

/// Constraint on the model's tool selection for a turn (RM-AIM-P2 PRV-202).
///
/// Providers translate this to their own wire shapes (OpenAI `tool_choice`,
/// Anthropic `tool_choice`, mistral.rs `ToolChoice`). Unset means the
/// provider's default (the model decides).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides whether to call a tool (every provider's default).
    Auto,
    /// The model must not call any tool this turn.
    None,
    /// The model must call at least one tool (any of the advertised ones).
    /// Not every backend supports it (mistral.rs doesn't) — unsupported
    /// providers fail closed with [`wovyr_common::Error::Invalid`].
    Required,
    /// The model must call the named tool.
    Tool(String),
}

/// Constraint on the shape of the model's final answer (RM-AIM-P2 PRV-202).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Any syntactically valid JSON object (OpenAI "JSON mode"). Providers
    /// without a schema-less JSON mode (Anthropic, mistral.rs) fail closed
    /// with [`wovyr_common::Error::Invalid`] — prefer
    /// [`ResponseFormat::JsonSchema`], which every backend supports.
    JsonObject,
    /// JSON validating against the given schema (OpenAI structured outputs,
    /// Anthropic `output_config.format`, mistral.rs grammar constraint).
    JsonSchema {
        /// A short identifier for the schema (OpenAI requires one; others
        /// ignore it).
        name: String,
        /// The JSON Schema the answer must validate against.
        schema: serde_json::Value,
    },
}

/// A normalized chat completion request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Concrete model id (already resolved by the [`crate::Gateway`]).
    pub model: String,
    /// Conversation so far.
    pub messages: Vec<Message>,
    /// Sampling temperature, if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max output tokens, if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Tools the model may call this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Constraint on tool selection, if any (unset = provider default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Constraint on the final answer's shape, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

impl ChatRequest {
    /// Start a request for `model` with the given message history.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
        }
    }

    /// Constrain tool selection (builder-style).
    pub fn with_tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// Constrain the answer's shape (builder-style).
    pub fn with_response_format(mut self, format: ResponseFormat) -> Self {
        self.response_format = Some(format);
        self
    }
}

/// A normalized chat completion response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The assistant's reply (text and/or tool calls).
    pub message: Message,
    /// Concrete model that served the request.
    pub model: String,
    /// Token/cost accounting for this call.
    pub usage: wovyr_common::Usage,
    /// Provider-reported stop reason (e.g. `stop`, `tool_calls`).
    pub finish_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_parts_serialize_internally_tagged() {
        assert_eq!(
            serde_json::to_value(ContentPart::text("hi")).unwrap(),
            json!({ "type": "text", "text": "hi" })
        );
        assert_eq!(
            serde_json::to_value(ContentPart::image_url("https://x/img.png")).unwrap(),
            json!({ "type": "image_url", "url": "https://x/img.png" })
        );
        assert_eq!(
            serde_json::to_value(ContentPart::image_base64("image/png", "AAAA")).unwrap(),
            json!({ "type": "image", "media_type": "image/png", "data": "AAAA" })
        );
        assert_eq!(
            serde_json::to_value(ContentPart::audio_base64("audio/wav", "BBBB")).unwrap(),
            json!({ "type": "audio", "media_type": "audio/wav", "data": "BBBB" })
        );
    }

    #[test]
    fn message_without_parts_keeps_its_old_wire_shape_and_deserializes_back() {
        // Backward compatibility (PRV-204): a text-only message serializes with
        // no `parts` field at all, and a pre-parts JSON message deserializes.
        let wire = serde_json::to_value(Message::user("hi")).unwrap();
        assert_eq!(wire, json!({ "role": "user", "content": "hi" }));

        let old: Message =
            serde_json::from_value(json!({ "role": "assistant", "content": "yo" })).unwrap();
        assert!(old.parts.is_empty());
        assert_eq!(old.content.as_deref(), Some("yo"));
    }

    #[test]
    fn message_with_parts_round_trips() {
        let msg =
            Message::user("look at this").with_part(ContentPart::image_base64("image/png", "AAAA"));
        let wire = serde_json::to_value(&msg).unwrap();
        assert_eq!(wire["parts"][0]["type"], "image");
        let back: Message = serde_json::from_value(wire).unwrap();
        assert_eq!(back.parts, msg.parts);
        assert_eq!(back.content.as_deref(), Some("look at this"));
    }
}
