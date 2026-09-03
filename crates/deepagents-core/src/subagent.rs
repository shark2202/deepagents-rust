//! SubAgent specs, harness profiles, and the builder configuration types (Q6).
//!
//! This module provides the Rust equivalents of the Python SDK's
//! `create_deep_agent` configuration surface:
//! - [`SubAgentSpec`] — declarative sub-agent definition (serde-serializable)
//! - [`CompiledSubAgent`] — pre-compiled `AgentRunner` trait object
//! - [`HarnessProfile`] — prompt assembly + tool/middleware filtering profile
//! - [`GeneralPurposeSubagentProfile`] — controls the auto-injected GP sub-agent
//!
//! See `docs/SPEC.md` §Q6 for the design rationale.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hitl::InterruptMap;
use crate::permission::FilesystemPermission;

// ── Forward declarations from rig ─────────────────────────────────────

// We use `rig_core::completion::CompletionModel` as a trait object for the
// model field. Since `CompletionModel` is not object-safe by default (it has
// `Self` in return positions via `stream`), we store model specs as strings
// ("provider:model") in the declarative form and resolve them at build time.

/// A model specification string in `"provider:model"` format.
///
/// Example: `"openai:gpt-4o"`, `"anthropic:claude-sonnet-4-20250514"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec(pub String);

impl ModelSpec {
    /// Create a new model spec from a string.
    pub fn new(spec: impl Into<String>) -> Self {
        Self(spec.into())
    }

    /// Get the provider name (before the `:`).
    pub fn provider(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    /// Get the model name (after the `:`).
    pub fn model_name(&self) -> &str {
        self.0.split(':').nth(1).unwrap_or("")
    }
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self("openai:gpt-4o".into())
    }
}

impl std::fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Tool specification ────────────────────────────────────────────────

/// A serializable tool specification.
///
/// In v0, tools are identified by name and JSON schema. The builder resolves
/// these to rig `Tool` implementations at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name (must match the rig tool's `name()`).
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON schema for the tool's parameters.
    pub parameters: serde_json::Value,
}

// ── SubAgentSpec (declarative) ─────────────────────────────────────────

/// A declarative sub-agent specification.
///
/// Maps to the Python SDK's `SubAgent` TypedDict. At build time, the
/// `DeepAgentBuilder` recursively constructs a child agent from this spec.
///
/// **interrupt_on inheritance**: if `interrupt_on` is `None`, the sub-agent
/// inherits the parent's `interrupt_on`. If `Some`, it **replaces** the
/// parent's (not merged).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentSpec {
    /// Sub-agent name (required).
    pub name: String,
    /// Human-readable description shown to the parent agent (required).
    pub description: String,
    /// System prompt for the sub-agent (required).
    pub system_prompt: String,
    /// Optional tool specifications. If `None`, inherits parent's tools.
    pub tools: Option<Vec<ToolSpec>>,
    /// Optional model spec (`"provider:model"`). If `None`, inherits parent's.
    pub model: Option<ModelSpec>,
    /// Optional interrupt map. If `None`, inherits parent's. If `Some`,
    /// replaces (not merges with) the parent's.
    pub interrupt_on: Option<InterruptMap>,
    /// Optional skill file paths.
    pub skills: Option<Vec<PathBuf>>,
    /// Optional filesystem permissions.
    pub permissions: Option<Vec<FilesystemPermission>>,
    /// Optional response format schema.
    pub response_format: Option<serde_json::Value>,
}

impl SubAgentSpec {
    /// Create a new sub-agent spec with the required fields.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            system_prompt: system_prompt.into(),
            tools: None,
            model: None,
            interrupt_on: None,
            skills: None,
            permissions: None,
            response_format: None,
        }
    }

    /// Set the tools.
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the model spec.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(ModelSpec::new(model));
        self
    }

    /// Set the interrupt map (replaces parent's, not merged).
    pub fn with_interrupt_on(mut self, interrupt_on: InterruptMap) -> Self {
        self.interrupt_on = Some(interrupt_on);
        self
    }

    /// Set the skills.
    pub fn with_skills(mut self, skills: Vec<PathBuf>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Set the permissions.
    pub fn with_permissions(mut self, permissions: Vec<FilesystemPermission>) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// Set the response format schema.
    pub fn with_response_format(mut self, schema: serde_json::Value) -> Self {
        self.response_format = Some(schema);
        self
    }

    /// Returns `true` if this spec inherits the parent's interrupt_on.
    pub fn inherits_interrupt_on(&self) -> bool {
        self.interrupt_on.is_none()
    }
}

// ── CompiledSubAgent (pre-compiled) ───────────────────────────────────

/// A pre-compiled sub-agent that wraps a `Box<dyn AgentRunner>`.
///
/// Maps to the Python SDK's `CompiledSubAgent`. Unlike [`SubAgentSpec`],
/// this is already constructed and ready to run. It does **not** inherit
/// the parent's `interrupt_on`.
///
/// Note: in v0, we store the pre-compiled agent as an opaque serde-serializable
/// handle. The actual `AgentRunner` trait object is resolved at build time
/// from the spec, so the builder can accept either form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledSubAgentSpec {
    /// Sub-agent name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Serialized runner state (opaque). The actual agent is reconstructed
    /// from this at build time.
    pub runner_state: serde_json::Value,
}

// ── GeneralPurposeSubagentProfile ──────────────────────────────────────

/// Controls whether and how the default `general-purpose` sub-agent is
/// auto-injected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralPurposeSubagentProfile {
    /// Whether to auto-inject the `general-purpose` sub-agent.
    /// Default: `true`. Set to `false` to disable.
    pub enabled: bool,
    /// If `Some`, use this as the model spec for the GP sub-agent.
    /// If `None`, inherits the parent's model.
    pub model: Option<ModelSpec>,
    /// If `Some`, overrides the default GP system prompt.
    pub system_prompt: Option<String>,
    /// If `Some`, overrides the default GP description.
    pub description: Option<String>,
}

impl Default for GeneralPurposeSubagentProfile {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            system_prompt: None,
            description: None,
        }
    }
}

// ── HarnessProfile ─────────────────────────────────────────────────────

/// A harness profile that controls prompt assembly, tool filtering, and
/// middleware filtering for a deep agent.
///
/// Prompt assembly order (per SPEC Q6):
/// 1. `USER` — the user-supplied `system_prompt`
/// 2. `BASE` — `profile.base_system_prompt`
/// 3. `SUFFIX` — `profile.system_prompt_suffix`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessProfile {
    /// Base system prompt, inserted between USER and SUFFIX.
    pub base_system_prompt: Option<String>,
    /// Suffix appended after the base prompt.
    pub system_prompt_suffix: Option<String>,
    /// Tool description overrides (by tool name).
    pub tool_description_overrides: HashMap<String, String>,
    /// Tool names to exclude from the agent's tool set.
    pub excluded_tools: HashSet<String>,
    /// Middleware hook names to exclude.
    ///
    /// **Protected set**: `Filesystem` and `SubAgent` hooks cannot be
    /// excluded; attempts to do so are silently ignored at build time.
    pub excluded_middleware: HashSet<String>,
    /// Extra middleware hooks to add (beyond the standard four).
    pub extra_middleware: Vec<String>,
    /// Configuration for the auto-injected general-purpose sub-agent.
    pub general_purpose_subagent: GeneralPurposeSubagentProfile,
}

impl Default for HarnessProfile {
    fn default() -> Self {
        Self {
            base_system_prompt: None,
            system_prompt_suffix: None,
            tool_description_overrides: HashMap::new(),
            excluded_tools: HashSet::new(),
            excluded_middleware: HashSet::new(),
            extra_middleware: Vec::new(),
            general_purpose_subagent: GeneralPurposeSubagentProfile::default(),
        }
    }
}

impl HarnessProfile {
    /// Create a new empty profile with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base system prompt.
    pub fn with_base_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.base_system_prompt = Some(prompt.into());
        self
    }

    /// Set the system prompt suffix.
    pub fn with_system_prompt_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.system_prompt_suffix = Some(suffix.into());
        self
    }

    /// Add a tool description override.
    pub fn with_tool_description_override(
        mut self,
        tool_name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.tool_description_overrides
            .insert(tool_name.into(), description.into());
        self
    }

    /// Exclude a tool by name.
    pub fn exclude_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.excluded_tools.insert(tool_name.into());
        self
    }

    /// Exclude a middleware hook by name.
    ///
    /// Note: `Filesystem` and `SubAgent` are protected and cannot be excluded.
    pub fn exclude_middleware(mut self, hook_name: impl Into<String>) -> Self {
        self.excluded_middleware.insert(hook_name.into());
        self
    }

    /// Add an extra middleware hook name.
    pub fn with_extra_middleware(mut self, hook_name: impl Into<String>) -> Self {
        self.extra_middleware.push(hook_name.into());
        self
    }

    /// Set the general-purpose sub-agent profile.
    pub fn with_gp_subagent(mut self, profile: GeneralPurposeSubagentProfile) -> Self {
        self.general_purpose_subagent = profile;
        self
    }

    /// Returns `true` if the given hook name is excluded.
    /// Protected hooks (`Filesystem`, `SubAgent`) are never excluded.
    pub fn is_middleware_excluded(&self, hook_name: &str) -> bool {
        match hook_name {
            "Filesystem" | "SubAgent" => false,
            _ => self.excluded_middleware.contains(hook_name),
        }
    }

    /// Returns `true` if the given tool name is excluded.
    pub fn is_tool_excluded(&self, tool_name: &str) -> bool {
        self.excluded_tools.contains(tool_name)
    }

    /// Returns the override description for a tool, if any.
    pub fn tool_description(&self, tool_name: &str) -> Option<&str> {
        self.tool_description_overrides.get(tool_name).map(|s| s.as_str())
    }

    /// Assemble the full system prompt from USER + BASE + SUFFIX.
    ///
    /// The order is:
    /// 1. USER (the user-supplied `system_prompt`)
    /// 2. BASE (`profile.base_system_prompt`)
    /// 3. SUFFIX (`profile.system_prompt_suffix`)
    pub fn assemble_prompt(&self, user_prompt: &str) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(3);
        parts.push(user_prompt);
        if let Some(base) = &self.base_system_prompt {
            parts.push(base);
        }
        if let Some(suffix) = &self.system_prompt_suffix {
            parts.push(suffix);
        }
        parts.join("\n\n")
    }
}

// ── MCP server config (pure additive, 19th capability) ───────────────

/// Configuration for an MCP (Model Context Protocol) server connection.
///
/// This is a pure additive capability — the original Python SDK has no MCP
/// equivalent. It is implemented by rig's MCP client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport")]
pub enum McpServerConfig {
    /// stdio-based MCP server.
    #[serde(rename = "stdio")]
    Stdio {
        /// Command to launch the MCP server.
        command: String,
        /// Arguments for the command.
        args: Vec<String>,
        /// Optional environment variables.
        env: Option<HashMap<String, String>>,
    },
    /// SSE-based MCP server.
    #[serde(rename = "sse")]
    Sse {
        /// URL of the SSE endpoint.
        url: String,
    },
    /// HTTP-based MCP server.
    #[serde(rename = "http")]
    Http {
        /// URL of the HTTP endpoint.
        url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_spec_parsing() {
        let spec = ModelSpec::new("openai:gpt-4o");
        assert_eq!(spec.provider(), "openai");
        assert_eq!(spec.model_name(), "gpt-4o");
    }

    #[test]
    fn test_subagent_spec_builder() {
        let spec = SubAgentSpec::new("researcher", "Research agent", "You are a researcher")
            .with_model("openai:gpt-4o")
            .with_tools(vec![ToolSpec {
                name: "search".into(),
                description: "Search the web".into(),
                parameters: serde_json::json!({}),
            }]);

        assert_eq!(spec.name, "researcher");
        assert_eq!(spec.description, "Research agent");
        assert!(spec.model.is_some());
        assert!(spec.tools.is_some());
        assert!(spec.inherits_interrupt_on());
    }

    #[test]
    fn test_subagent_spec_with_interrupt_on() {
        let mut map = InterruptMap::new();
        map.insert_simple("write_file", true);
        let spec = SubAgentSpec::new("agent", "desc", "prompt")
            .with_interrupt_on(map);
        assert!(!spec.inherits_interrupt_on());
    }

    #[test]
    fn test_harness_profile_prompt_assembly() {
        let profile = HarnessProfile::new()
            .with_base_system_prompt("BASE INSTRUCTIONS")
            .with_system_prompt_suffix("SUFFIX");

        let prompt = profile.assemble_prompt("USER PROMPT");
        assert_eq!(prompt, "USER PROMPT\n\nBASE INSTRUCTIONS\n\nSUFFIX");
    }

    #[test]
    fn test_harness_profile_prompt_user_only() {
        let profile = HarnessProfile::new();
        let prompt = profile.assemble_prompt("USER PROMPT");
        assert_eq!(prompt, "USER PROMPT");
    }

    #[test]
    fn test_harness_profile_tool_exclusion() {
        let profile = HarnessProfile::new().exclude_tool("dangerous_tool");
        assert!(profile.is_tool_excluded("dangerous_tool"));
        assert!(!profile.is_tool_excluded("safe_tool"));
    }

    #[test]
    fn test_harness_profile_middleware_exclusion_protected() {
        let profile = HarnessProfile::new()
            .exclude_middleware("Filesystem")
            .exclude_middleware("SubAgent")
            .exclude_middleware("Summarization");

        // Protected hooks are never excluded
        assert!(!profile.is_middleware_excluded("Filesystem"));
        assert!(!profile.is_middleware_excluded("SubAgent"));
        // Non-protected hooks can be excluded
        assert!(profile.is_middleware_excluded("Summarization"));
    }

    #[test]
    fn test_harness_profile_tool_description_override() {
        let profile = HarnessProfile::new()
            .with_tool_description_override("search", "Custom search tool");
        assert_eq!(
            profile.tool_description("search"),
            Some("Custom search tool")
        );
        assert_eq!(profile.tool_description("write"), None);
    }

    #[test]
    fn test_gp_profile_default() {
        let gp = GeneralPurposeSubagentProfile::default();
        assert!(gp.enabled);
        assert!(gp.model.is_none());
        assert!(gp.system_prompt.is_none());
    }

    #[test]
    fn test_mcp_stdio_config() {
        let config = McpServerConfig::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@mcp/server".into()],
            env: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("stdio"));
        assert!(json.contains("npx"));
    }

    #[test]
    fn test_mcp_sse_config() {
        let config = McpServerConfig::Sse {
            url: "http://localhost:3001/sse".into(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("sse"));
        assert!(json.contains("localhost"));
    }
}
