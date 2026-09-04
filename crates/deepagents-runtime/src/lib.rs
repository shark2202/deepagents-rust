//! Runtime: provider resolution, model factory, real LLM wiring (Q9-Q10).
//!
//! This crate bridges [`deepagents-core`]'s `DeepAgentBuilder` to real LLM
//! providers via [`rig_core`]. Because `CompletionModel` uses RPITIT
//! (return-position `impl Trait`) it is not object-safe — `dyn
//! CompletionModel` is impossible. Instead, [`ProviderModel`] is an enum
//! wrapping each supported provider's concrete model type, and it implements
//! `CompletionModel` by delegating to the inner variant.
//!
//! # Quick start
//!
//! ```no_run
//! use deepagents_runtime::ProviderModel;
//!
//! // Resolve provider + model from environment (OPENAI_API_KEY, etc.)
//! let model = ProviderModel::new("openai", "gpt-4o")?;
//! // Drive the async runtime yourself (tokio, etc.), then:
//! // let output = deepagents_runtime::run_prompt(model, "Say hello.").await?;
//! let _ = model;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::env;
use std::fmt;

use deepagents_core::builder::DeepAgentBuilder;
use rig_agent::agent::AgentRunner;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::completion::{
    CompletionModel, CompletionRequest, CompletionResponse, CompletionError,
};
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;

// ── Provider model enum (object-safe workaround via enum dispatch) ─────────

/// A concrete completion model from one of the supported providers.
///
/// Because `CompletionModel` is not object-safe (RPITIT), this enum wraps each
/// provider's concrete model type and implements `CompletionModel` by
/// delegation.
#[derive(Clone)]
pub enum ProviderModel {
    /// OpenAI (GPT-4o, GPT-4-turbo, …) via `rig_core::providers::openai`.
    Openai(rig_core::providers::openai::CompletionModel),
    /// Anthropic (Claude Sonnet, Haiku, …) via `rig_core::providers::anthropic`.
    Anthropic(rig_core::providers::anthropic::completion::CompletionModel),
    /// Ollama (local Llama, Qwen, …) via `rig_core::providers::ollama`.
    Ollama(rig_core::providers::ollama::CompletionModel),
}

/// Manual `Debug` impl — rig's concrete model types don't all implement
/// `Debug`, so we render a descriptive variant tag instead.
impl fmt::Debug for ProviderModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Openai(_) => f.write_str("ProviderModel::Openai(..)"),
            Self::Anthropic(_) => f.write_str("ProviderModel::Anthropic(..)"),
            Self::Ollama(_) => f.write_str("ProviderModel::Ollama(..)"),
        }
    }
}

impl ProviderModel {
    /// Build a provider model from a provider name and model identifier.
    ///
    /// Supported providers: `"openai"`, `"anthropic"`, `"ollama"`.
    /// API keys are read from the standard env vars (`OPENAI_API_KEY`,
    /// `ANTHROPIC_API_KEY`, `OLLAMA_API_KEY` / `OLLAMA_API_BASE_URL`).
    pub fn new(provider: &str, model: &str) -> Result<Self, ProviderError> {
        match provider.to_ascii_lowercase().as_str() {
            "openai" => {
                let client = rig_core::providers::openai::Client::from_env()
                    .map_err(ProviderError::client)?;
                // The default Client uses the Responses API; switch to the
                // Chat Completions API whose model type matches our enum.
                let completions_client = client.completions_api();
                Ok(Self::Openai(completions_client.completion_model(model)))
            }
            "anthropic" | "claude" => {
                let client = rig_core::providers::anthropic::Client::from_env()
                    .map_err(ProviderError::client)?;
                Ok(Self::Anthropic(client.completion_model(model)))
            }
            "ollama" => {
                let client = rig_core::providers::ollama::Client::from_env()
                    .map_err(ProviderError::client)?;
                Ok(Self::Ollama(client.completion_model(model)))
            }
            _ => Err(ProviderError::UnsupportedProvider(provider.to_string())),
        }
    }

    /// Build a provider model from the environment, using sensible defaults.
    ///
    /// Resolution order:
    /// 1. `DEEPAGENTS_CODE_MODEL` env var (format: `"provider:model"`, e.g.
    ///    `"openai:gpt-4o"`)
    /// 2. `DEEPAGENTS_CODE_PROVIDER` + a default model per provider
    /// 3. `OPENAI_API_KEY` → OpenAI + `gpt-4o`
    /// 4. `ANTHROPIC_API_KEY` → Anthropic + `claude-sonnet-4-5`
    /// 5. `OLLAMA_API_BASE_URL` or `OLLAMA_API_KEY` → Ollama + `llama3.2`
    /// 6. Error
    pub fn from_env_default() -> Result<Self, ProviderError> {
        // 1. DEEPAGENTS_CODE_MODEL = "provider:model"
        if let Ok(spec) = env::var("DEEPAGENTS_CODE_MODEL") {
            if let Some((provider, model)) = spec.split_once(':') {
                return Self::new(provider, model);
            }
            // Whole spec is the model, infer provider from available keys
            return Self::new(&infer_provider()?, &spec);
        }

        // 2. DEEPAGENTS_CODE_PROVIDER + default model
        if let Ok(provider) = env::var("DEEPAGENTS_CODE_PROVIDER") {
            let default_model = default_model_for(&provider);
            return Self::new(&provider, default_model);
        }

        // 3–6. Infer from available API keys
        let provider = infer_provider()?;
        let model = default_model_for(&provider);
        Self::new(&provider, model)
    }
}

/// Infer which provider to use based on available env vars.
fn infer_provider() -> Result<String, ProviderError> {
    if env::var("OPENAI_API_KEY").is_ok() {
        return Ok("openai".into());
    }
    if env::var("ANTHROPIC_API_KEY").is_ok() {
        return Ok("anthropic".into());
    }
    if env::var("OLLAMA_API_BASE_URL").is_ok() || env::var("OLLAMA_API_KEY").is_ok() {
        return Ok("ollama".into());
    }
    Err(ProviderError::NoProvider)
}

/// Default model for a given provider.
fn default_model_for(provider: &str) -> &'static str {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => "claude-sonnet-4-5",
        "ollama" => "llama3.2",
        _ => "gpt-4o",
    }
}

// ── CompletionModel delegation ───────────────────────────────────────────

impl CompletionModel for ProviderModel {
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend
    {
        async move {
            match self {
                ProviderModel::Openai(m) => m.completion(request).await,
                ProviderModel::Anthropic(m) => m.completion(request).await,
                ProviderModel::Ollama(m) => m.completion(request).await,
            }
        }
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<StreamingCompletionResponse, CompletionError>,
    > + WasmCompatSend {
        async move {
            match self {
                ProviderModel::Openai(m) => m.stream(request).await,
                ProviderModel::Anthropic(m) => m.stream(request).await,
                ProviderModel::Ollama(m) => m.stream(request).await,
            }
        }
    }
}

// ── Run helpers ──────────────────────────────────────────────────────────

/// Run a single prompt through a [`DeepAgentBuilder`] configured with the
/// given model, returning the text output.
///
/// This assembles a minimal agent (system prompt + model, no tools/backend)
/// and drives it to completion via [`AgentRunner`].
pub async fn run_prompt(model: ProviderModel, prompt: &str) -> Result<String, RunError> {
    run_prompt_with(model, "You are a helpful assistant.", prompt).await
}

/// Run a single prompt with a custom system prompt.
pub async fn run_prompt_with(
    model: ProviderModel,
    system_prompt: &str,
    prompt: &str,
) -> Result<String, RunError> {
    let runner: AgentRunner = DeepAgentBuilder::new()
        .model(model)
        .system_prompt(system_prompt)
        .build_runner(prompt);

    let response = runner.run().await.map_err(RunError::from)?;
    Ok(response.output)
}

// ── Errors ──────────────────────────────────────────────────────────────

/// Errors from provider resolution and model construction.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The provider name is not one of the supported providers.
    #[error("unsupported provider: {0}. Supported: openai, anthropic, ollama")]
    UnsupportedProvider(String),

    /// No provider could be inferred from the environment.
    #[error("no LLM provider configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_API_BASE_URL, or set DEEPAGENTS_CODE_MODEL=provider:model")]
    NoProvider,

    /// The provider client failed to construct (missing API key, bad base URL, …).
    #[error("provider client construction failed: {0}")]
    Client(String),
}

impl ProviderError {
    fn from_client_error(e: rig_core::client::ProviderClientError) -> Self {
        Self::Client(e.to_string())
    }

    fn client<E: std::fmt::Display>(e: E) -> Self {
        Self::Client(e.to_string())
    }
}

impl From<rig_core::client::ProviderClientError> for ProviderError {
    fn from(e: rig_core::client::ProviderClientError) -> Self {
        Self::from_client_error(e)
    }
}

/// Errors from running a prompt through the agent loop.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The agent prompt loop failed.
    #[error("agent run failed: {0}")]
    Prompt(String),
}

impl From<rig_agent::completion::PromptError> for RunError {
    fn from(e: rig_agent::completion::PromptError) -> Self {
        Self::Prompt(e.to_string())
    }
}

// ── Re-exports ───────────────────────────────────────────────────────────

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_for_openai() {
        assert_eq!(default_model_for("openai"), "gpt-4o");
    }

    #[test]
    fn test_default_model_for_anthropic() {
        assert_eq!(default_model_for("anthropic"), "claude-sonnet-4-5");
        assert_eq!(default_model_for("claude"), "claude-sonnet-4-5");
    }

    #[test]
    fn test_default_model_for_ollama() {
        assert_eq!(default_model_for("ollama"), "llama3.2");
    }

    #[test]
    fn test_unsupported_provider() {
        let err = ProviderModel::new("google", "gemini").unwrap_err();
        assert!(err.to_string().contains("unsupported provider"));
    }

    #[test]
    fn test_no_provider_when_no_env() {
        // This test may fail if env has keys set; guard by checking
        if std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("ANTHROPIC_API_KEY").is_ok()
            || std::env::var("OLLAMA_API_BASE_URL").is_ok()
        {
            return; // skip in environments with keys
        }
        let err = infer_provider().unwrap_err();
        assert!(matches!(err, ProviderError::NoProvider));
    }

    #[test]
    fn test_provider_error_display() {
        let err = ProviderError::UnsupportedProvider("foo".into());
        assert!(err.to_string().contains("foo"));
    }

    #[test]
    fn test_version_nonempty() {
        assert!(!VERSION.is_empty());
    }
}
