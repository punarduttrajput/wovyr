//! Per-model price table and cost computation.
//!
//! Every model response carries an estimated `cost_usd` in its
//! [`apex_common::Usage`] ([token-management](../../docs/05-llm-gateway/token-management.md)),
//! which the gateway turns into [`crate::CostEvent`]s and the server's per-project
//! budget accounting consumes. Before this module existed, `OpenAiProvider` (and
//! mistral.rs) hardcoded `cost_usd = 0.0`, so every cost/quota number in production
//! was fiction and `llm_cost_per_day_usd` quota enforcement was a silent no-op
//! (PRD-004 R-B.1 / RM-AIM-P1 PRV-101).
//!
//! A [`PriceBook`] maps a model name to a [`ModelPrice`] (USD per 1M input/output
//! tokens). It ships with built-in defaults for common OpenAI/Anthropic models and is
//! overridable via `APEX_MODEL_PRICES` (inline JSON) or `APEX_PRICEBOOK_FILE` (a JSON
//! file path), so an operator can price models this table doesn't know without a code
//! change. Lookup is exact-match-then-longest-prefix (so a date-suffixed variant like
//! `gpt-4o-mini-2024-07-18` resolves to the `gpt-4o-mini` entry), then a configured
//! default, and finally a loud one-time warn returning `$0` for a genuinely unknown
//! model — never a panic ([coding standards §7](../../docs/19-implementation-guide/coding-standards.md):
//! the computation itself is pure/deterministic; only construction reads config).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use apex_common::Usage;

/// Environment variable holding an inline JSON price map (highest precedence).
const ENV_INLINE: &str = "APEX_MODEL_PRICES";
/// Environment variable holding a path to a JSON price-map file.
const ENV_FILE: &str = "APEX_PRICEBOOK_FILE";

/// Price for one model, in USD per **one million** tokens.
///
/// Per-1M is the unit vendors publish, so a built-in default reads the same as the
/// price sheet it was copied from; cost is divided back down per token at compute time.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ModelPrice {
    /// USD per 1,000,000 input (prompt) tokens.
    pub input_per_1m: f64,
    /// USD per 1,000,000 output (completion) tokens.
    pub output_per_1m: f64,
}

impl ModelPrice {
    /// Construct a price from per-1M-token figures.
    pub const fn per_1m(input_per_1m: f64, output_per_1m: f64) -> Self {
        Self {
            input_per_1m,
            output_per_1m,
        }
    }

    /// Cost in USD for the given prompt/completion token counts.
    pub fn cost(&self, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        (prompt_tokens as f64 * self.input_per_1m + completion_tokens as f64 * self.output_per_1m)
            / 1_000_000.0
    }
}

/// A resolvable map of model name → [`ModelPrice`].
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    prices: BTreeMap<String, ModelPrice>,
    /// Fallback price applied to any model with no exact/prefix match.
    default: Option<ModelPrice>,
    /// Set once we've warned about an unknown, undefaulted model, so the warning
    /// isn't emitted on every single call for the same model.
    warned: std::sync::Arc<AtomicBool>,
}

impl PriceBook {
    /// An empty price book (every model costs $0). Rarely wanted directly — prefer
    /// [`PriceBook::with_defaults`] or [`PriceBook::from_env`].
    pub fn empty() -> Self {
        Self::default()
    }

    /// Built-in defaults for common OpenAI/Anthropic models, as of 2026-07.
    ///
    /// Prices are USD per 1M tokens and are deliberately conservative starting
    /// points; an operator overrides them via [`PriceBook::from_env`] rather than
    /// waiting on a code change when a vendor reprices.
    pub fn with_defaults() -> Self {
        let mut prices = BTreeMap::new();
        // OpenAI (public list prices).
        prices.insert("gpt-4o".to_string(), ModelPrice::per_1m(2.50, 10.00));
        prices.insert("gpt-4o-mini".to_string(), ModelPrice::per_1m(0.15, 0.60));
        prices.insert("gpt-4-turbo".to_string(), ModelPrice::per_1m(10.00, 30.00));
        prices.insert("gpt-4".to_string(), ModelPrice::per_1m(30.00, 60.00));
        prices.insert("gpt-3.5-turbo".to_string(), ModelPrice::per_1m(0.50, 1.50));
        prices.insert("o1".to_string(), ModelPrice::per_1m(15.00, 60.00));
        prices.insert("o1-mini".to_string(), ModelPrice::per_1m(1.10, 4.40));
        prices.insert("o3-mini".to_string(), ModelPrice::per_1m(1.10, 4.40));
        // Anthropic (public list prices). Current generation first — the
        // "claude-opus-4"/"claude-sonnet-4" keys price every dotted variant
        // (4-6/4-7/4-8, 4-5/4-6) via the longest-prefix match.
        prices.insert(
            "claude-fable-5".to_string(),
            ModelPrice::per_1m(10.00, 50.00),
        );
        prices.insert("claude-opus-4".to_string(), ModelPrice::per_1m(5.00, 25.00));
        prices.insert(
            "claude-sonnet-5".to_string(),
            ModelPrice::per_1m(3.00, 15.00),
        );
        prices.insert(
            "claude-sonnet-4".to_string(),
            ModelPrice::per_1m(3.00, 15.00),
        );
        prices.insert(
            "claude-haiku-4-5".to_string(),
            ModelPrice::per_1m(1.00, 5.00),
        );
        prices.insert(
            "claude-3-5-sonnet".to_string(),
            ModelPrice::per_1m(3.00, 15.00),
        );
        prices.insert(
            "claude-3-5-haiku".to_string(),
            ModelPrice::per_1m(0.80, 4.00),
        );
        prices.insert(
            "claude-3-opus".to_string(),
            ModelPrice::per_1m(15.00, 75.00),
        );
        prices.insert("claude-3-haiku".to_string(), ModelPrice::per_1m(0.25, 1.25));
        Self {
            prices,
            default: None,
            warned: Default::default(),
        }
    }

    /// [`PriceBook::with_defaults`] with any `APEX_MODEL_PRICES` /
    /// `APEX_PRICEBOOK_FILE` overrides merged on top (overrides win per model).
    ///
    /// A malformed override is a loud warn + ignore, not a hard error, so a typo in
    /// an operator's price file can't take the whole platform down; the built-in
    /// defaults still apply.
    pub fn from_env() -> Self {
        let mut book = Self::with_defaults();
        if let Ok(inline) = std::env::var(ENV_INLINE) {
            book.merge_json(&inline, ENV_INLINE);
        } else if let Ok(path) = std::env::var(ENV_FILE) {
            match std::fs::read_to_string(&path) {
                Ok(contents) => book.merge_json(&contents, &path),
                Err(e) => {
                    tracing::warn!(target: "apex.pricing", path = %path, error = %e,
                        "could not read {ENV_FILE}; using built-in default prices");
                }
            }
        }
        book
    }

    /// Merge a JSON `{ "model": { "input_per_1m": .., "output_per_1m": .. }, .. }`
    /// map into this book; a `"default"` key sets the fallback price.
    fn merge_json(&mut self, json: &str, source: &str) {
        match serde_json::from_str::<BTreeMap<String, ModelPrice>>(json) {
            Ok(map) => {
                for (model, price) in map {
                    if model == "default" {
                        self.default = Some(price);
                    } else {
                        self.prices.insert(model, price);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "apex.pricing", source = %source, error = %e,
                    "ignoring malformed price override; using built-in default prices");
            }
        }
    }

    /// Set an explicit fallback price for unknown models (builder-style).
    pub fn with_default_price(mut self, price: ModelPrice) -> Self {
        self.default = Some(price);
        self
    }

    /// Insert/override one model's price (builder-style).
    pub fn with_price(mut self, model: impl Into<String>, price: ModelPrice) -> Self {
        self.prices.insert(model.into(), price);
        self
    }

    /// Resolve a model to its price: exact match, then the longest known key that is
    /// a prefix of `model` (handles date-/size-suffixed variants), then the default.
    pub fn price(&self, model: &str) -> Option<ModelPrice> {
        if let Some(p) = self.prices.get(model) {
            return Some(*p);
        }
        let mut best: Option<(&str, ModelPrice)> = None;
        for (key, price) in &self.prices {
            if model.starts_with(key.as_str())
                && best.map(|(k, _)| key.len() > k.len()).unwrap_or(true)
            {
                best = Some((key.as_str(), *price));
            }
        }
        best.map(|(_, p)| p).or(self.default)
    }

    /// Estimated cost in USD for `usage`'s token counts under `model`.
    ///
    /// Falls back to `$0` (with a one-time warn) for a model that matches nothing and
    /// has no configured default — never panics.
    pub fn cost(&self, model: &str, usage: &Usage) -> f64 {
        match self.price(model) {
            Some(p) => p.cost(usage.prompt_tokens, usage.completion_tokens),
            None => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(target: "apex.pricing", model = %model,
                        "no price for model and no default configured; cost recorded as \
                         $0 (set {ENV_INLINE} or {ENV_FILE} to price it)");
                }
                0.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_token_count_times_price_is_expected_cost() {
        // gpt-4o-mini: $0.15 / 1M input, $0.60 / 1M output.
        let book = PriceBook::with_defaults();
        let usage = Usage::new(1_000_000, 1_000_000, 0.0);
        let cost = book.cost("gpt-4o-mini", &usage);
        assert!((cost - 0.75).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn small_counts_compute_proportionally() {
        let book = PriceBook::with_defaults();
        // 1000 in, 500 out on gpt-4o = 1000*2.5/1e6 + 500*10/1e6 = 0.0025 + 0.005.
        let cost = book.cost("gpt-4o", &Usage::new(1000, 500, 0.0));
        assert!((cost - 0.0075).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn date_suffixed_variant_matches_by_prefix() {
        let book = PriceBook::with_defaults();
        let exact = book.price("gpt-4o-mini").unwrap();
        let suffixed = book.price("gpt-4o-mini-2024-07-18").unwrap();
        assert_eq!(exact, suffixed);
    }

    #[test]
    fn longest_prefix_wins_over_shorter() {
        let book = PriceBook::with_defaults();
        // "gpt-4o-mini-..." must resolve to gpt-4o-mini, not the shorter "gpt-4o".
        assert_eq!(
            book.price("gpt-4o-mini-2024-07-18").unwrap(),
            ModelPrice::per_1m(0.15, 0.60)
        );
    }

    #[test]
    fn claude_4_family_variants_resolve_by_prefix() {
        let book = PriceBook::with_defaults();
        // Every dotted Opus 4.x variant resolves to the "claude-opus-4" entry.
        assert_eq!(
            book.price("claude-opus-4-8").unwrap(),
            ModelPrice::per_1m(5.00, 25.00)
        );
        // A date-suffixed Haiku snapshot resolves to its exact-family entry,
        // not a shorter prefix.
        assert_eq!(
            book.price("claude-haiku-4-5-20251001").unwrap(),
            ModelPrice::per_1m(1.00, 5.00)
        );
        assert_eq!(
            book.price("claude-sonnet-5").unwrap(),
            ModelPrice::per_1m(3.00, 15.00)
        );
    }

    #[test]
    fn unknown_model_without_default_is_free_not_a_panic() {
        let book = PriceBook::with_defaults();
        assert_eq!(
            book.cost("some-unheard-of-model", &Usage::new(10, 10, 0.0)),
            0.0
        );
    }

    #[test]
    fn configured_default_prices_unknown_models() {
        let book = PriceBook::empty().with_default_price(ModelPrice::per_1m(1.0, 2.0));
        let cost = book.cost("mystery", &Usage::new(1_000_000, 1_000_000, 0.0));
        assert!((cost - 3.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn inline_json_override_wins_and_adds_models() {
        let mut book = PriceBook::with_defaults();
        book.merge_json(
            r#"{"gpt-4o":{"input_per_1m":1.0,"output_per_1m":1.0},
                "custom-model":{"input_per_1m":5.0,"output_per_1m":5.0},
                "default":{"input_per_1m":9.0,"output_per_1m":9.0}}"#,
            "test",
        );
        // Override replaced the built-in gpt-4o price.
        assert_eq!(book.price("gpt-4o").unwrap(), ModelPrice::per_1m(1.0, 1.0));
        // New model added.
        assert_eq!(
            book.price("custom-model").unwrap(),
            ModelPrice::per_1m(5.0, 5.0)
        );
        // Default set for everything else.
        assert_eq!(
            book.price("anything").unwrap(),
            ModelPrice::per_1m(9.0, 9.0)
        );
    }

    #[test]
    fn malformed_override_is_ignored_and_defaults_survive() {
        let mut book = PriceBook::with_defaults();
        book.merge_json("{ not valid json", "test");
        // Built-in still intact.
        assert_eq!(
            book.price("gpt-4o-mini").unwrap(),
            ModelPrice::per_1m(0.15, 0.60)
        );
    }
}
