//! 55 env vars, dotenvy, 3-layer denylist (Q12)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
