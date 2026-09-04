//! bundled JSON catalog, `calc_price`, `CostState` (Q22).
//!
//! Rust port of the `genai_prices` Python package used by the LangChain Deep
//! Agents SDK. This crate embeds a pricing catalog via [`rust_embed`], exposes
//! a [`calc_price`]-style [`CostCalculator`], and tracks session-level cost
//! state (an additive reducer + subagent cost transfers) through a
//! process-wide [`SessionCostRecorder`].
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rust_embed::Embed;
use serde::{Deserialize, Serialize};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bundled pricing catalog embedded into the binary at compile time.
///
/// In debug builds (without the `debug-embed` feature) the file is read from
/// disk at runtime; in release builds it is embedded directly. This is the
/// primary source of pricing data and mirrors the `genai_prices` JSON catalog.
#[derive(Embed)]
#[folder = "assets/"]
struct CatalogAsset;

// ────────────────────────────────────────────────────────────────────────────
// Usage buckets
// ────────────────────────────────────────────────────────────────────────────

/// Bucketed prompt-caching writes, split by cache lifetime.
///
/// Different providers (notably Anthropic) charge differently for a cache
/// write that lives for ~5 minutes versus ~1 hour. The `generic` bucket is
/// used when a provider exposes only a single cache-write rate.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
pub struct CacheWrites {
    /// Provider-agnostic / single-rate cache writes.
    pub generic: u64,
    /// ~5 minute cache writes (e.g. Anthropic `cache_creation_input_tokens`
    /// priced at the 5m ephemeral rate).
    pub five_minute: u64,
    /// ~1 hour cache writes (Anthropic 1h ephemeral cache tier).
    pub one_hour: u64,
}

/// Token-usage metadata for a single model call, aligned to
/// `genai-prices` semantics.
///
/// Every field is a *token count* (not a cost). Costs are derived from the
/// corresponding [`ModelPricing`] rates via [`CostCalculator::calculate`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
pub struct Usage {
    /// Standard input / prompt tokens.
    pub input_tokens: u64,
    /// Standard output / completion tokens.
    pub output_tokens: u64,
    /// Tokens read from a prompt cache (discounted input).
    pub cache_read_tokens: u64,
    /// Tokens written to a prompt cache, split by lifetime.
    pub cache_write_tokens: CacheWrites,
    /// Audio tokens consumed on the input side.
    pub input_audio_tokens: u64,
    /// Audio tokens produced on the output side.
    pub output_audio_tokens: u64,
    /// Hidden reasoning tokens (e.g. OpenAI o1/o3 `output_reasoning`).
    pub output_reasoning_tokens: u64,
}

// ────────────────────────────────────────────────────────────────────────────
// Pricing catalog
// ────────────────────────────────────────────────────────────────────────────

/// Per-token pricing for a single `(provider, model)` pair.
///
/// All fields are USD per single token (i.e. the model list price divided by
/// one million). A field set to `0.0` means the provider does not charge for
/// that token bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Catalog provider name (e.g. `openai`, `anthropic`, `google`).
    pub provider: String,
    /// Model identifier as the provider names it.
    pub model: String,
    /// USD per input token.
    pub input_per_token: f64,
    /// USD per output token.
    pub output_per_token: f64,
    /// USD per cache-read token.
    pub cache_read_per_token: f64,
    /// USD per generic cache-write token.
    pub cache_write_generic_per_token: f64,
    /// USD per ~5m cache-write token.
    pub cache_write_5m_per_token: f64,
    /// USD per ~1h cache-write token.
    pub cache_write_1h_per_token: f64,
    /// USD per input audio token.
    pub input_audio_per_token: f64,
    /// USD per output audio token.
    pub output_audio_per_token: f64,
    /// USD per hidden reasoning token.
    pub reasoning_per_token: f64,
}

/// A collection of [`ModelPricing`] entries, the in-memory form of the bundled
/// catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PricingCatalog {
    /// All known model pricing rows.
    pub models: Vec<ModelPricing>,
}

impl PricingCatalog {
    /// Look up a model by `(provider, model)`, matching case-insensitively on
    /// both components.
    ///
    /// # Examples
    /// ```
    /// use deepagents_cost::{PricingCatalog, default_catalog};
    ///
    /// let cat = default_catalog();
    /// assert!(cat.find("openai", "gpt-4o").is_some());
    /// assert!(cat.find("OpenAI", "GPT-4O").is_some());
    /// assert!(cat.find("openai", "nope").is_none());
    /// ```
    pub fn find(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        self.models.iter().find(|m| {
            m.provider.eq_ignore_ascii_case(provider)
                && m.model.eq_ignore_ascii_case(model)
        })
    }

    /// Parse a [`PricingCatalog`] from a JSON string (the catalog file format).
    ///
    /// Errors are propagated as [`deepagents_errors::Error::Json`].
    pub fn from_json(json: &str) -> Result<Self, deepagents_errors::Error> {
        let cat = serde_json::from_str(json)?;
        Ok(cat)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Cost calculator
// ────────────────────────────────────────────────────────────────────────────

/// Token × rate bucketed price calculation (`calc_price` rewrite).
///
/// Computes the total USD cost of a single model call by multiplying each
/// token bucket in [`Usage`] by its corresponding per-token rate in
/// [`ModelPricing`] and summing the result.
pub struct CostCalculator;

impl CostCalculator {
    /// Compute the total USD cost for `usage` at the given `pricing`.
    ///
    /// Sums every bucket: input, output, cache-read, the three cache-write
    /// tiers, input/output audio, and hidden reasoning tokens.
    ///
    /// # Examples
    /// ```
    /// use deepagents_cost::{CostCalculator, Usage, default_catalog};
    ///
    /// let cat = default_catalog();
    /// let p = cat.find("openai", "gpt-4o").unwrap();
    /// let usage = Usage {
    ///     input_tokens: 1_000,
    ///     output_tokens: 500,
    ///     ..Default::default()
    /// };
    /// // 1000 * 2.5e-6 + 500 * 1e-5 = 0.0025 + 0.005 = 0.0075
    /// let cost = CostCalculator::calculate(&usage, p);
    /// assert!((cost - 0.0075).abs() < 1e-9);
    /// ```
    pub fn calculate(usage: &Usage, pricing: &ModelPricing) -> f64 {
        let mut total = 0.0_f64;

        total += usage.input_tokens as f64 * pricing.input_per_token;
        total += usage.output_tokens as f64 * pricing.output_per_token;
        total += usage.cache_read_tokens as f64 * pricing.cache_read_per_token;

        let cw = &usage.cache_write_tokens;
        total += cw.generic as f64 * pricing.cache_write_generic_per_token;
        total += cw.five_minute as f64 * pricing.cache_write_5m_per_token;
        total += cw.one_hour as f64 * pricing.cache_write_1h_per_token;

        total += usage.input_audio_tokens as f64 * pricing.input_audio_per_token;
        total += usage.output_audio_tokens as f64 * pricing.output_audio_per_token;
        total += usage.output_reasoning_tokens as f64 * pricing.reasoning_per_token;

        total
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Cost transfers & state
// ────────────────────────────────────────────────────────────────────────────

/// A cost transfer from one agent to another (subagent accounting).
///
/// When a subagent performs a model call, its cost is recorded against the
/// subagent and then transferred to (charged against) the parent agent that
/// delegated the work. This struct captures both the USD amount and the
/// underlying [`Usage`] so the parent can attribute the spend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostTransfer {
    /// Agent incurring the cost (source).
    pub from_agent: String,
    /// Agent the cost is charged to (destination, typically the parent).
    pub to_agent: String,
    /// USD amount being transferred.
    pub cost_usd: f64,
    /// The usage that produced this cost.
    pub usage: Usage,
}

/// Checkpoint channel for session-level cost tracking.
///
/// `session_cost_usd` is an additive reducer that accumulates the cost of
/// every model call in the session. `session_cost_transfers` records
/// subagent-to-parent cost transfers so the spend can be attributed to the
/// correct agent at checkpoint time.
#[derive(Debug, Clone, Default)]
pub struct CostState {
    /// Additive reducer over all model-call costs in the session.
    pub session_cost_usd: f64,
    /// Subagent cost transfers, keyed by a stable transfer id.
    pub session_cost_transfers: HashMap<String, CostTransfer>,
}

impl CostState {
    /// Create an empty cost state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a model-call cost to the additive reducer.
    pub fn add_cost(&mut self, cost: f64) {
        self.session_cost_usd += cost;
    }

    /// Record a subagent cost transfer under `transfer.from_agent` as the key.
    pub fn add_transfer(&mut self, transfer: CostTransfer) {
        self.session_cost_transfers
            .insert(transfer.from_agent.clone(), transfer);
    }

    /// Total session cost: the additive reducer plus the sum of all
    /// transferred subagent costs.
    pub fn total(&self) -> f64 {
        let transfers: f64 = self
            .session_cost_transfers
            .values()
            .map(|t| t.cost_usd)
            .sum();
        self.session_cost_usd + transfers
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Process-wide recorder
// ────────────────────────────────────────────────────────────────────────────

/// Process-wide recorder of model-call usage metadata.
///
/// Each model call records `(model_name, usage, cost_usd)`. Recording uses
/// interior mutability (`Mutex`) so callers may push from any thread. Use
/// [`SessionCostRecorder::global`] to obtain the process-wide singleton and
/// [`SessionCostRecorder::drain`] to atomically take and clear the buffered
/// records (the drain is destructive).
pub struct SessionCostRecorder {
    records: Mutex<Vec<(String, Usage, f64)>>,
}

impl SessionCostRecorder {
    /// Obtain the process-wide singleton recorder.
    ///
    /// The singleton is lazily initialized on first use via [`OnceLock`].
    pub fn global() -> &'static Self {
        static RECORDER: OnceLock<SessionCostRecorder> = OnceLock::new();
        RECORDER.get_or_init(|| SessionCostRecorder {
            records: Mutex::new(Vec::new()),
        })
    }

    /// Record a model call's usage metadata and computed cost.
    pub fn record(&self, model: String, usage: Usage, cost: f64) {
        if let Ok(mut guard) = self.records.lock() {
            guard.push((model, usage, cost));
        }
    }

    /// Destructive drain: take all buffered records, leaving the buffer empty.
    ///
    /// Returns the records in insertion order. Safe to call concurrently; the
    /// take is atomic under the internal mutex.
    pub fn drain(&self) -> Vec<(String, Usage, f64)> {
        self.records
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

impl Default for SessionCostRecorder {
    fn default() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Provider name mapping & default catalog
// ────────────────────────────────────────────────────────────────────────────

/// Map a LangChain provider name to the catalog provider name.
///
/// LangChain exposes providers under names like `google_genai` and
/// `bedrock`, whereas the pricing catalog uses shorter provider keys
/// (`google`, `bedrock`). This function normalizes the former to the latter.
/// Unknown providers pass through unchanged.
///
/// # Examples
/// ```
/// use deepagents_cost::lc_to_genai_provider;
///
/// assert_eq!(lc_to_genai_provider("openai"), "openai");
/// assert_eq!(lc_to_genai_provider("google_genai"), "google");
/// assert_eq!(lc_to_genai_provider("anthropic"), "anthropic");
/// assert_eq!(lc_to_genai_provider("bedrock"), "bedrock");
/// assert_eq!(lc_to_genai_provider("unknown_xyz"), "unknown_xyz");
/// ```
pub fn lc_to_genai_provider(lc: &str) -> &str {
    match lc {
        "openai" => "openai",
        "anthropic" => "anthropic",
        "google_genai" | "google_vertex" | "google" => "google",
        "bedrock" => "bedrock",
        "mistral" => "mistral",
        "together" => "together",
        "fireworks" => "fireworks",
        "groq" => "groq",
        "cohere" => "cohere",
        other => other,
    }
}

/// Return the embedded pricing catalog, falling back to a small hardcoded
/// v0 catalog if the embedded JSON asset is missing or unparseable.
///
/// This is the primary entry point for obtaining pricing data at runtime. The
/// embedded catalog is compiled in via [`rust_embed`]; the v0 fallback is a
/// last-resort set of ~5 well-known models so the crate is always usable even
/// if the asset is stripped.
pub fn embedded_catalog() -> PricingCatalog {
    match CatalogAsset::get("catalog.json") {
        Some(file) => match PricingCatalog::from_json(
            &String::from_utf8_lossy(&file.data),
        ) {
            Ok(cat) => cat,
            Err(err) => {
                tracing::warn!(
                    "embedded catalog.json failed to parse; using v0 fallback: {err}"
                );
                default_catalog()
            }
        },
        None => {
            tracing::warn!("embedded catalog.json not found; using v0 fallback");
            default_catalog()
        }
    }
}

/// A small hardcoded v0 fallback catalog (~5 models) used when no embedded
/// JSON catalog is available.
///
/// These rates are illustrative snapshots of publicly listed model pricing and
/// must not be relied upon for billing. They exist only to keep the crate
/// functional in a stripped build.
pub fn default_catalog() -> PricingCatalog {
    PricingCatalog {
        models: vec![
            ModelPricing {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                input_per_token: 0.0000025,
                output_per_token: 0.00001,
                cache_read_per_token: 0.00000125,
                cache_write_generic_per_token: 0.0,
                cache_write_5m_per_token: 0.0,
                cache_write_1h_per_token: 0.0,
                input_audio_per_token: 0.0000007,
                output_audio_per_token: 0.000005,
                reasoning_per_token: 0.0,
            },
            ModelPricing {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                input_per_token: 0.00000015,
                output_per_token: 0.0000006,
                cache_read_per_token: 0.000000075,
                cache_write_generic_per_token: 0.0,
                cache_write_5m_per_token: 0.0,
                cache_write_1h_per_token: 0.0,
                input_audio_per_token: 0.0000001,
                output_audio_per_token: 0.0000012,
                reasoning_per_token: 0.0,
            },
            ModelPricing {
                provider: "anthropic".to_string(),
                model: "claude-3-5-sonnet".to_string(),
                input_per_token: 0.000003,
                output_per_token: 0.000015,
                cache_read_per_token: 0.0000003,
                cache_write_generic_per_token: 0.0,
                cache_write_5m_per_token: 0.00000375,
                cache_write_1h_per_token: 0.000003,
                input_audio_per_token: 0.0,
                output_audio_per_token: 0.0,
                reasoning_per_token: 0.0,
            },
            ModelPricing {
                provider: "google".to_string(),
                model: "gemini-1.5-pro".to_string(),
                input_per_token: 0.00000125,
                output_per_token: 0.000005,
                cache_read_per_token: 0.0,
                cache_write_generic_per_token: 0.0,
                cache_write_5m_per_token: 0.0,
                cache_write_1h_per_token: 0.0,
                input_audio_per_token: 0.0,
                output_audio_per_token: 0.0,
                reasoning_per_token: 0.0,
            },
            ModelPricing {
                provider: "bedrock".to_string(),
                model: "anthropic.claude-3-5-sonnet".to_string(),
                input_per_token: 0.000003,
                output_per_token: 0.000015,
                cache_read_per_token: 0.0000003,
                cache_write_generic_per_token: 0.0,
                cache_write_5m_per_token: 0.00000375,
                cache_write_1h_per_token: 0.0,
                input_audio_per_token: 0.0,
                output_audio_per_token: 0.0,
                reasoning_per_token: 0.0,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_default() {
        let u = Usage::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert_eq!(u.cache_read_tokens, 0);
        assert_eq!(u.cache_write_tokens, CacheWrites::default());
        assert_eq!(u.input_audio_tokens, 0);
        assert_eq!(u.output_audio_tokens, 0);
        assert_eq!(u.output_reasoning_tokens, 0);
    }

    #[test]
    fn test_cache_writes_serde() {
        let cw = CacheWrites {
            generic: 10,
            five_minute: 20,
            one_hour: 30,
        };
        let json = serde_json::to_string(&cw).unwrap();
        let back: CacheWrites = serde_json::from_str(&json).unwrap();
        assert_eq!(cw, back);

        // Verify JSON shape.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["generic"], 10);
        assert_eq!(v["five_minute"], 20);
        assert_eq!(v["one_hour"], 30);
    }

    #[test]
    fn test_pricing_catalog_find() {
        let cat = default_catalog();
        // Case-insensitive match.
        assert!(cat.find("openai", "gpt-4o").is_some());
        assert!(cat.find("OpenAI", "GPT-4O").is_some());
        assert!(cat.find("anthropic", "claude-3-5-sonnet").is_some());
        assert!(cat.find("google", "gemini-1.5-pro").is_some());
        assert!(cat.find("bedrock", "anthropic.claude-3-5-sonnet").is_some());
        // Missing.
        assert!(cat.find("openai", "nope").is_none());
        assert!(cat.find("nobody", "gpt-4o").is_none());
    }

    #[test]
    fn test_cost_calculator() {
        let cat = default_catalog();
        let p = cat.find("openai", "gpt-4o").unwrap();

        // input + output only.
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        // 1000 * 2.5e-6 + 500 * 1e-5 = 0.0025 + 0.005 = 0.0075
        let cost = CostCalculator::calculate(&usage, p);
        assert!((cost - 0.0075).abs() < 1e-9, "got {cost}");

        // With cache read + audio + reasoning.
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 200,
            input_audio_tokens: 100,
            output_audio_tokens: 50,
            ..Default::default()
        };
        // input 0.0025 + output 0.005 + cache_read 200*1.25e-6=0.00025
        // + in_audio 100*7e-7=0.00007 + out_audio 50*5e-6=0.00025
        // = 0.0025+0.005+0.00025+0.00007+0.00025 = 0.00807
        let cost = CostCalculator::calculate(&usage, p);
        assert!((cost - 0.00807).abs() < 1e-9, "got {cost}");

        // Zero usage -> zero cost.
        let cost = CostCalculator::calculate(&Usage::default(), p);
        assert!(cost.abs() < 1e-12);
    }

    #[test]
    fn test_cost_state_additive() {
        let mut state = CostState::new();
        assert_eq!(state.session_cost_usd, 0.0);
        assert!(state.session_cost_transfers.is_empty());
        assert!((state.total() - 0.0).abs() < 1e-12);

        state.add_cost(0.01);
        state.add_cost(0.02);
        assert!((state.session_cost_usd - 0.03).abs() < 1e-9);
        assert!((state.total() - 0.03).abs() < 1e-9);

        state.add_transfer(CostTransfer {
            from_agent: "sub_a".to_string(),
            to_agent: "root".to_string(),
            cost_usd: 0.05,
            usage: Usage::default(),
        });
        // total = reducer 0.03 + transfer 0.05 = 0.08
        assert!((state.total() - 0.08).abs() < 1e-9);
        assert_eq!(state.session_cost_transfers.len(), 1);
    }

    #[test]
    fn test_session_cost_recorder_drain() {
        // Use a fresh, non-global recorder so the test is hermetic.
        let recorder = SessionCostRecorder::default();
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        recorder.record("gpt-4o".to_string(), usage, 0.001);
        recorder.record("claude".to_string(), Usage::default(), 0.002);

        let drained = recorder.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].0, "gpt-4o");
        assert_eq!(drained[0].1, usage);
        assert!((drained[0].2 - 0.001).abs() < 1e-9);
        assert_eq!(drained[1].0, "claude");

        // Drain is destructive: second drain yields nothing.
        let again = recorder.drain();
        assert!(again.is_empty());
    }

    #[test]
    fn test_lc_to_genai_provider_mapping() {
        assert_eq!(lc_to_genai_provider("openai"), "openai");
        assert_eq!(lc_to_genai_provider("anthropic"), "anthropic");
        assert_eq!(lc_to_genai_provider("google_genai"), "google");
        assert_eq!(lc_to_genai_provider("google_vertex"), "google");
        assert_eq!(lc_to_genai_provider("bedrock"), "bedrock");
        // Unknown passes through.
        assert_eq!(lc_to_genai_provider("mystery"), "mystery");
    }

    #[test]
    fn test_pricing_catalog_from_json_roundtrip() {
        let cat = default_catalog();
        let json = serde_json::to_string(&cat).unwrap();
        let back = PricingCatalog::from_json(&json).unwrap();
        assert_eq!(cat.models.len(), back.models.len());
        assert!(back.find("openai", "gpt-4o").is_some());
    }

    #[test]
    fn test_embedded_catalog_loads() {
        // The embedded asset should always be present in a normal build.
        let cat = embedded_catalog();
        assert!(!cat.models.is_empty(), "embedded catalog should not be empty");
        assert!(cat.find("openai", "gpt-4o").is_some());
        assert!(cat.find("anthropic", "claude-3-5-sonnet").is_some());
    }
}
