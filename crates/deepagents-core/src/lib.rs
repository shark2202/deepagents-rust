//! SDK layer: rig AgentRun + serde state, DeepAgentBuilder, middleware, backends, permissions (Q1-Q8)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod builder;
pub mod hitl;
pub mod middleware;
pub mod mock;
pub mod permission;
pub mod subagent;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
