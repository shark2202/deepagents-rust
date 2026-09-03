#!/usr/bin/env python3
"""Generate 19-crate workspace skeletons (lib.rs + Cargo.toml)."""
import os

WORKSPACE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRATES_DIR = os.path.join(WORKSPACE, "crates")

# (crate_name, description, Q_ref, deps_list)
# deps_list: list of (dep_name, dep_expr) tuples for [dependencies]
CRATES = [
    # 1. top-level library
    ("deepagents", "Top-level library: re-exports all sub-crates, provides DeepAgentBuilder", "Q1-Q26", [
        ("deepagents-core", "deepagents-core"),
        ("deepagents-config", "deepagents-config"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 2. core SDK
    ("deepagents-core", "SDK layer: rig AgentRun + serde state, DeepAgentBuilder, middleware, backends, permissions (Q1-Q8)", "Q1-Q8", [
        ("rig-core", "rig-core"),
        ("rig-agent", "rig-agent"),
        ("tokio", "tokio"),
        ("async-trait", "async-trait"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("schemars", "schemars"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("globset", "globset"),
        ("walkdir", "walkdir"),
        ("regex", "regex"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 3. config
    ("deepagents-config", "config.toml 6-layer ranked resolver, 105 options (Q11)", "Q11", [
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("toml", "toml"),
        ("toml_edit", "toml_edit"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 4. env
    ("deepagents-env", "55 env vars, dotenvy, 3-layer denylist (Q12)", "Q12", [
        ("serde", "serde"),
        ("dotenvy", "dotenvy"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 5. cli
    ("deepagents-cli", "clap, 13 subcommands (Q13)", "Q13", [
        ("clap", "clap"),
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-core", "deepagents-core"),
        ("deepagents-config", "deepagents-config"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 6. tui
    ("deepagents-tui", "ratatui, Screen/Modal, 47 modals, theme (Q14,Q24)", "Q14,Q24", [
        ("ratatui", "ratatui"),
        ("crossterm", "crossterm"),
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 7. hooks
    ("deepagents-hooks", "12 events, tokio async, Windows cmd/pwsh (Q15)", "Q15", [
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 8. plugins
    ("deepagents-plugins", "JSON-RPC stdio, gix, rust-embed adapter (Q16)", "Q16", [
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("gix", "gix"),
        ("rust-embed", "rust-embed"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 9. sessions
    ("deepagents-sessions", "rusqlite+bundled, 18-channel ResumeState (Q17)", "Q17", [
        ("rusqlite", "rusqlite"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 10. approval
    ("deepagents-approval", "Manual/Auto/YOLO, classifier, HITL checkpoint (Q18)", "Q18", [
        ("rig-core", "rig-core"),
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("schemars", "schemars"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 11. mcp
    ("deepagents-mcp", "rmcp, .mcp.json, trust lists, OAuth (Q19)", "Q19", [
        ("tokio", "tokio"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 12. skills
    ("deepagents-skills", "8-source discovery, SKILL.md, rust-embed (Q20)", "Q20", [
        ("serde", "serde"),
        ("serde_yaml", "serde_yaml"),
        ("rust-embed", "rust-embed"),
        ("walkdir", "walkdir"),
        ("regex", "regex"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 13. sandbox
    ("deepagents-sandbox", "trait provider, 6 providers, reqwest+rustls (Q21)", "Q21", [
        ("async-trait", "async-trait"),
        ("reqwest", "reqwest"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 14. cost
    ("deepagents-cost", "bundled JSON catalog, calc_price, CostState (Q22)", "Q22", [
        ("rust-embed", "rust-embed"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("reqwest", "reqwest"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 15. goal
    ("deepagents-goal", "GoalStatus, GraderResponse, self-grading loop (Q23)", "Q23", [
        ("rig-core", "rig-core"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("schemars", "schemars"),
        ("uuid", "uuid"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 16. onboarding
    ("deepagents-onboarding", "marker files, name memory (Q25)", "Q25", [
        ("serde", "serde"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 17. update
    ("deepagents-update", "GitHub Releases, 5 install methods, auto-update (Q25)", "Q25", [
        ("reqwest", "reqwest"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("semver", "semver"),
        ("chrono", "chrono"),
        ("sha2", "sha2"),
        ("tempfile", "tempfile"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 18. doctor
    ("deepagents-doctor", "4 sections + context-doctor (Q25)", "Q25", [
        ("serde", "serde"),
        ("serde_json", "serde_json"),
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("deepagents-errors", "deepagents-errors"),
    ]),
    # 19. errors
    ("deepagents-errors", "~40 error types, structured diagnostics (Q26)", "Q26", [
        ("thiserror", "thiserror"),
        ("tracing", "tracing"),
        ("serde", "serde"),
        ("serde_json", "serde_json"),
    ]),
]


def gen_lib_rs(name, desc, q_ref):
    return f"""//! {desc}
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
"""


def gen_cargo_toml(name, desc, q_ref, deps):
    lines = []
    lines.append(f"[package]")
    lines.append(f'name = "{name}"')
    lines.append(f'description = "{desc}"')
    lines.append(f"version.workspace = true")
    lines.append(f"edition.workspace = true")
    lines.append(f"rust-version.workspace = true")
    lines.append(f"license.workspace = true")
    lines.append(f"authors.workspace = true")
    lines.append(f"repository.workspace = true")
    lines.append("")
    lines.append(f"[lib]")
    lines.append(f'path = "src/lib.rs"')
    lines.append("")
    if deps:
        lines.append(f"[dependencies]")
        for dep_name, dep_expr in deps:
            lines.append(f"{dep_name} = {{ workspace = true }}")
        lines.append("")
    lines.append(f"# Q-ref: {q_ref}")
    return "\n".join(lines)


for name, desc, q_ref, deps in CRATES:
    crate_dir = os.path.join(CRATES_DIR, name)
    src_dir = os.path.join(crate_dir, "src")
    os.makedirs(src_dir, exist_ok=True)

    lib_path = os.path.join(src_dir, "lib.rs")
    with open(lib_path, "w") as f:
        f.write(gen_lib_rs(name, desc, q_ref))

    cargo_path = os.path.join(crate_dir, "Cargo.toml")
    with open(cargo_path, "w") as f:
        f.write(gen_cargo_toml(name, desc, q_ref, deps))

    print(f"  {name}: lib.rs + Cargo.toml")

print(f"\nTotal: {len(CRATES)} crates generated")
