//! Vendor-neutral text-to-image generation types.
//!
//! Mirrors [`crate::embeddings`]'s shape: a request/response pair plus a
//! default-unsupported [`crate::provider::AIProvider::generate_image`] method,
//! so providers that don't support image generation need not implement it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A text-to-image generation request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageGenRequest {
    /// Text description of the desired image.
    pub prompt: String,
    /// Image dimensions, e.g. `1024x1024`.
    pub size: String,
    /// Number of images to generate.
    pub n: u64,
}

impl ImageGenRequest {
    /// A request for one `1024x1024` image from `prompt`.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            size: "1024x1024".to_string(),
            n: 1,
        }
    }

    /// Override the image size.
    pub fn with_size(mut self, size: impl Into<String>) -> Self {
        self.size = size.into();
        self
    }

    /// Override the number of images.
    pub fn with_n(mut self, n: u64) -> Self {
        self.n = n;
        self
    }
}

/// The result of an image generation request: the provider's raw per-image
/// entries (each typically a URL or base64 payload). Passed through opaquely
/// rather than modeled field-by-field, since the shape is provider-specific.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageGenResponse {
    /// One entry per generated image, in the provider's own wire shape.
    pub images: Vec<Value>,
}
