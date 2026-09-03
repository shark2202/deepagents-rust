//! SDK layer: rig AgentRun + serde state, DeepAgentBuilder, middleware, backends, permissions (Q1-Q8)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
