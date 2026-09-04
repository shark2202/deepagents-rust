//! # deepagents — Rust port of the LangChain Deep Agents SDK
//!
//! Built on [`rig`](https://github.com/0xPlaygrounds/rig) (`rig-core` +
//! `rig-agent`). This is the top-level facade crate that re-exports all
//! sub-crates so consumers can depend on a single crate.
//!
//! ## Quick start
//!
//! ```toml
//! [dependencies]
//! deepagents = "0.1"
//! ```
//!
//! ```no_run
//! // Access the core builder:
//! use deepagents::core::builder::DeepAgentBuilder;
//! // Access error types:
//! use deepagents::errors::Error;
//! ```
//!
//! ## Feature flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `cli`   | ✅      | CLI sub-crate (clap derive + command dispatch) |
//! | `tui`   | ✅      | TUI sub-crate (ratatui + crossterm) |
//!
//! Disabling both yields a pure-library build with no CLI/TUI dependencies.
//!
//! ## Sub-crate map
//!
//! | Crate | SPEC | Responsibility |
//! |-------|------|----------------|
//! | `deepagents-core` | Q4-Q8 | rig AgentRun + serde state, builder, middleware, backend, HITL |
//! | `deepagents-config` | Q11 | config.toml 6-layer ranked resolver |
//! | `deepagents-env` | Q12 | 55 env vars, dotenvy, 3-layer denylist |
//! | `deepagents-cli` | Q13 | clap, 13 subcommands |
//! | `deepagents-tui` | Q14,Q24 | ratatui, Screen/Modal, 47 slash commands, theme |
//! | `deepagents-hooks` | Q15 | 12 events, tokio async, Windows cmd/pwsh |
//! | `deepagents-plugins` | Q16 | JSON-RPC stdio, gix, rust-embed adapter |
//! | `deepagents-sessions` | Q17 | rusqlite+bundled, 18-channel ResumeState |
//! | `deepagents-approval` | Q18 | Manual/Auto/YOLO, classifier, HITL checkpoint |
//! | `deepagents-mcp` | Q19 | .mcp.json, trust lists, OAuth, env expansion |
//! | `deepagents-skills` | Q20 | 8-source discovery, SKILL.md, rust-embed |
//! | `deepagents-sandbox` | Q21 | trait provider, 6 providers, reqwest+rustls |
//! | `deepagents-cost` | Q22 | bundled JSON catalog, calc_price, CostState |
//! | `deepagents-goal` | Q23 | GoalStatus, GraderResponse, self-grading loop |
//! | `deepagents-onboarding` | Q25 | marker files, name memory |
//! | `deepagents-update` | Q25 | GitHub Releases, 5 install methods, auto-update |
//! | `deepagents-doctor` | Q25 | 4 sections + context-doctor |
//! | `deepagents-errors` | Q26 | ~40 error types, structured diagnostics |
//!
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// ── Core SDK (always available) ──────────────────────────────────────────

/// Core SDK: rig AgentRun + serde state, builder, middleware, backend, HITL (Q4-Q8).
pub use deepagents_core as core;

/// Error + diagnostic system (~40 types, 7 domains) (Q26).
pub use deepagents_errors as errors;

// ── Configuration / environment ─────────────────────────────────────────

/// config.toml 6-layer ranked resolver, 105 options (Q11).
pub use deepagents_config as config;

/// 55 env vars, dotenvy, 3-layer denylist (Q12).
pub use deepagents_env as env;

// ── Extensions / integration ───────────────────────────────────────────

/// 12 hook events, tokio async, Windows cmd/pwsh (Q15).
pub use deepagents_hooks as hooks;

/// JSON-RPC stdio, gix, rust-embed adapter (Q16).
pub use deepagents_plugins as plugins;

/// rusqlite+bundled, 18-channel ResumeState (Q17).
pub use deepagents_sessions as sessions;

/// Manual/Auto/YOLO, classifier, HITL checkpoint (Q18).
pub use deepagents_approval as approval;

/// .mcp.json, trust lists, OAuth, env expansion (Q19).
pub use deepagents_mcp as mcp;

/// 8-source discovery, SKILL.md, rust-embed (Q20).
pub use deepagents_skills as skills;

/// Trait-based sandbox provider, 6 providers (Q21).
pub use deepagents_sandbox as sandbox;

/// Bundled JSON pricing catalog, calc_price, CostState (Q22).
pub use deepagents_cost as cost;

/// GoalStatus, GraderResponse, self-grading loop (Q23).
pub use deepagents_goal as goal;

// ── Onboarding / update / doctor ────────────────────────────────────────

/// Marker files, name memory (Q25).
pub use deepagents_onboarding as onboarding;

/// GitHub Releases, 5 install methods, auto-update (Q25).
pub use deepagents_update as update;

/// 4 diagnostic sections + context-doctor (Q25).
pub use deepagents_doctor as doctor;

// ── Optional: CLI / TUI ────────────────────────────────────────────────

/// CLI: clap derive, 13 subcommands (Q13).
///
/// Requires feature `cli` (enabled by default).
#[cfg(feature = "cli")]
pub use deepagents_cli as cli;

/// TUI: ratatui, Screen/Modal, 47 slash commands, theme (Q14,Q24).
///
/// Requires feature `tui` (enabled by default).
#[cfg(feature = "tui")]
pub use deepagents_tui as tui;

// ── Crate version ───────────────────────────────────────────────────────

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Prelude ─────────────────────────────────────────────────────────────

/// Convenience re-exports for the most commonly used types.
///
/// ```no_run
/// use deepagents::prelude::*;
/// ```
pub mod prelude {
    /// The top-level error enum aggregating all domain errors.
    pub use deepagents_errors::Error;

    /// The agent builder (from core).
    pub use deepagents_core::builder::DeepAgentBuilder;

    /// The mock completion model for testing (from core).
    pub use deepagents_core::mock::MockCompletionModel;

    /// The Backend trait (from core).
    pub use deepagents_core::backend::Backend;

    /// The sandbox backend trait (from core).
    pub use deepagents_core::backend::SandboxBackend;

    /// StateBackend — in-memory, ephemeral, default (from core).
    pub use deepagents_core::backend::StateBackend;

    /// FilesystemPermission for permission rules (from core).
    pub use deepagents_core::permission::FilesystemPermission;

    /// InterruptPolicy for HITL (from core).
    pub use deepagents_core::hitl::InterruptPolicy;

    /// SubAgentSpec for declarative subagent config (from core).
    pub use deepagents_core::subagent::SubAgentSpec;
}

#[cfg(test)]
mod tests {
    use crate::VERSION;

    #[test]
    fn test_version_nonempty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn test_core_reexport() {
        // Verify the re-export chain works.
        let _ = deepagents_core::VERSION;
    }

    #[test]
    fn test_errors_reexport() {
        let _ = deepagents_errors::STARTUP_ERROR_MARKER;
    }

    #[test]
    fn test_config_reexport() {
        let _ = deepagents_config::VERSION;
    }

    #[test]
    fn test_env_reexport() {
        assert_eq!(deepagents_env::PREFIX, "DEEPAGENTS_CODE_");
    }

    #[test]
    fn test_all_subcrates_accessible() {
        // Smoke test: every sub-crate is reachable.
        let _ = deepagents_hooks::VERSION;
        let _ = deepagents_plugins::VERSION;
        let _ = deepagents_sessions::VERSION;
        let _ = deepagents_approval::VERSION;
        let _ = deepagents_mcp::VERSION;
        let _ = deepagents_skills::VERSION;
        let _ = deepagents_sandbox::VERSION;
        let _ = deepagents_cost::VERSION;
        let _ = deepagents_goal::VERSION;
        let _ = deepagents_onboarding::VERSION;
        let _ = deepagents_update::VERSION;
        let _ = deepagents_doctor::VERSION;
    }

    #[cfg(feature = "cli")]
    #[test]
    fn test_cli_reexport() {
        let _ = deepagents_cli::VERSION;
    }

    #[cfg(feature = "tui")]
    #[test]
    fn test_tui_reexport() {
        assert_eq!(deepagents_tui::DEFAULT_THEME, "langchain");
    }
}
