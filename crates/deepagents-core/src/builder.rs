//! DeepAgentBuilder — the 18-parameter builder mapping `create_deep_agent` → Rust (Q6).
//!
//! This module provides [`DeepAgentBuilder`], a fluent builder that maps the
//! Python SDK's `create_deep_agent` 18 parameters to the rig-agent
//! [`AgentBuilder`] API. The builder assembles the harness profile, middleware
//! stack, sub-agents, and filesystem tools into a final [`Agent`] (and
//! optionally an [`AgentRunner`]).
//!
//! See `docs/SPEC.md` §Q6 for the full 18-parameter mapping table.

use std::path::PathBuf;
use std::sync::Arc;

use rig_agent::agent::{
    Agent, AgentBuilder, AgentHook, AgentRunner, HookStack,
};
use rig_core::completion::CompletionModel;
use rig_core::wasm_compat::WasmCompatSend;

use crate::backend::Backend;
use crate::hitl::InterruptMap;
use crate::middleware::{FilesystemMiddleware, SubAgentMiddleware, SummarizationMiddleware};
use crate::permission::FilesystemPermission;
use crate::subagent::{
    GeneralPurposeSubagentProfile, HarnessProfile, McpServerConfig, ModelSpec, SubAgentSpec,
};

// ── Placeholder traits for v0 (forward declarations) ──────────────────

/// A checkpointer trait (v0: stub, maps to parameter #14).
///
/// **v0 status: not connected.** The `checkpointer` field is accepted by
/// [`DeepAgentBuilder`](crate::DeepAgentBuilder) and stored, but `build()`
/// does not yet use it. True checkpointing requires a custom runner wrapper
/// that serializes the `AgentRunner` state between turns — this will be
/// implemented in `deepagents-sessions` (v1), alongside true HITL
/// pause/resume (see `docs/adr/0001-rig-as-base.md`).
pub trait Checkpointer: Send + Sync {
    /// Save a checkpoint.
    fn save(&self, id: &str, state: &serde_json::Value) -> Result<(), String>;
    /// Load a checkpoint.
    fn load(&self, id: &str) -> Result<Option<serde_json::Value>, String>;
}

/// A store trait (v0: stub, maps to parameter #15).
/// The real implementation lives in `deepagents-state`.
pub trait Store: Send + Sync {
    /// Get a value by key.
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    /// Set a value by key.
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    /// Delete a value by key.
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// A cache trait (v0: stub, maps to parameter #18).
/// The real implementation lives in `deepagents-runtime`.
pub trait Cache: Send + Sync {
    /// Get a cached value by key.
    fn get(&self, key: &str) -> Option<String>;
    /// Set a cached value by key.
    fn set(&self, key: &str, value: &str);
}

// ── DeepAgentBuilder ───────────────────────────────────────────────────

/// The 18-parameter builder mapping `create_deep_agent` → Rust.
///
/// Each setter corresponds to one of the 18 Python parameters (see SPEC Q6
/// mapping table). The builder accumulates configuration and produces an
/// [`Agent`] (or [`AgentRunner`]) via [`build`](Self::build).
///
/// # Parameter Index
///
/// | # | Method | Python param |
/// |---|--------|---------------|
/// | 1 | `model` / `model_spec` | `model` |
/// | 2 | `tool` / `tools` | `tools` |
/// | 3 | `system_prompt` | `system_prompt` |
/// | 4 | `hook` / `hooks` | `middleware` |
/// | 5 | `subagent` / `subagents` | `subagents` |
/// | 6 | `skills` | `skills` |
/// | 7 | `memory` | `memory` |
/// | 8 | `permissions` | `permissions` |
/// | 9 | `backend` | `backend` |
/// | 10 | `interrupt_on` | `interrupt_on` |
/// | 11 | `response_format` | `response_format` |
/// | 12 | *(deleted — no TypedDict in Rust)* | `state_schema` |
/// | 13 | `context` (generic) | `context_schema` |
/// | 14 | `checkpointer` | `checkpointer` |
/// | 15 | `store` | `store` |
/// | 16 | `debug` | `debug` |
/// | 17 | `name` | `name` |
/// | 18 | `cache` | `cache` |
/// | 19 | `mcp_server` (pure additive) | *(no Python equivalent)* |
pub struct DeepAgentBuilder<M = MockModelPlaceholder>
where
    M: CompletionModel + 'static,
{
    // #1 model
    model: M,
    model_spec: Option<ModelSpec>,
    // #2 tools (accumulated as tool specs; resolved at build)
    tool_specs: Vec<crate::subagent::ToolSpec>,
    // #3 system prompt (USER slot)
    system_prompt: Option<String>,
    // #4 extra hooks (beyond the standard 4, stored as HookStack)
    extra_hook_stack: HookStack,
    extra_hook_count: usize,
    // #5 subagents
    subagents: Vec<SubAgentSpec>,
    // #6 skills
    skills: Vec<PathBuf>,
    // #7 memory files (AGENTS.md paths)
    memory_files: Vec<PathBuf>,
    // #8 permissions
    permissions: Vec<FilesystemPermission>,
    // #9 backend
    backend: Option<Arc<dyn Backend>>,
    // #10 interrupt_on
    interrupt_on: Option<InterruptMap>,
    // #11 response_format
    response_format: Option<serde_json::Value>,
    // #13 context (generic, stored as opaque JSON for v0)
    context: Option<serde_json::Value>,
    // #14 checkpointer
    checkpointer: Option<Arc<dyn Checkpointer>>,
    // #15 store
    store: Option<Arc<dyn Store>>,
    // #16 debug
    debug: bool,
    // #17 name
    name: Option<String>,
    // #18 cache
    cache: Option<Arc<dyn Cache>>,
    // #19 MCP servers (pure additive)
    mcp_servers: Vec<McpServerConfig>,
    // harness profile
    profile: HarnessProfile,
}

/// Placeholder model type for when no model is configured yet.
/// Users must call `.model(...)` or `.model_spec(...)` before `.build()`.
#[derive(Debug, Clone)]
pub struct MockModelPlaceholder;

impl CompletionModel for MockModelPlaceholder {
    fn completion(
        &self,
        _request: rig_core::completion::CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<
            rig_core::completion::CompletionResponse,
            rig_core::completion::CompletionError,
        >,
    > + WasmCompatSend {
        async {
            Err(rig_core::completion::CompletionError::ResponseError(
                "DeepAgentBuilder: no model configured — call .model(...) before .build()"
                    .to_string(),
            ))
        }
    }

    fn stream(
        &self,
        _request: rig_core::completion::CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<
            rig_core::streaming::StreamingCompletionResponse,
            rig_core::completion::CompletionError,
        >,
    > + WasmCompatSend {
        async {
            Err(rig_core::completion::CompletionError::ResponseError(
                "DeepAgentBuilder: no model configured — call .model(...) before .build()"
                    .to_string(),
            ))
        }
    }
}

// ── Default ────────────────────────────────────────────────────────────

impl Default for DeepAgentBuilder<MockModelPlaceholder> {
    fn default() -> Self {
        Self::new()
    }
}

impl DeepAgentBuilder<MockModelPlaceholder> {
    /// Create a new `DeepAgentBuilder` with no model configured.
    ///
    /// You **must** call `.model(...)` before `.build()`.
    pub fn new() -> Self {
        Self {
            model: MockModelPlaceholder,
            model_spec: None,
            tool_specs: Vec::new(),
            system_prompt: None,
            extra_hook_stack: HookStack::new(),
            extra_hook_count: 0,
            subagents: Vec::new(),
            skills: Vec::new(),
            memory_files: Vec::new(),
            permissions: Vec::new(),
            backend: None,
            interrupt_on: None,
            response_format: None,
            context: None,
            checkpointer: None,
            store: None,
            debug: false,
            name: None,
            cache: None,
            mcp_servers: Vec::new(),
            profile: HarnessProfile::default(),
        }
    }
}

impl<M> DeepAgentBuilder<M>
where
    M: CompletionModel + 'static,
{
    // ── #1: model ───────────────────────────────────────────────────────

    /// Set the completion model (typed).
    ///
    /// Maps to parameter #1: `model`.
    pub fn model<NewM>(self, model: NewM) -> DeepAgentBuilder<NewM>
    where
        NewM: CompletionModel + 'static,
    {
        DeepAgentBuilder {
            model,
            model_spec: self.model_spec,
            tool_specs: self.tool_specs,
            system_prompt: self.system_prompt,
            extra_hook_stack: self.extra_hook_stack,
            extra_hook_count: self.extra_hook_count,
            subagents: self.subagents,
            skills: self.skills,
            memory_files: self.memory_files,
            permissions: self.permissions,
            backend: self.backend,
            interrupt_on: self.interrupt_on,
            response_format: self.response_format,
            context: self.context,
            checkpointer: self.checkpointer,
            store: self.store,
            debug: self.debug,
            name: self.name,
            cache: self.cache,
            mcp_servers: self.mcp_servers,
            profile: self.profile,
        }
    }

    /// Set the model spec string (`"provider:model"`).
    ///
    /// Maps to parameter #1: `model` (alternative form).
    pub fn model_spec(mut self, spec: impl Into<String>) -> Self {
        self.model_spec = Some(ModelSpec::new(spec));
        self
    }

    // ── #2: tools ───────────────────────────────────────────────────────

    /// Add a tool specification.
    ///
    /// Maps to parameter #2: `tools`.
    pub fn tool(mut self, tool: crate::subagent::ToolSpec) -> Self {
        self.tool_specs.push(tool);
        self
    }

    /// Set multiple tool specifications.
    ///
    /// Maps to parameter #2: `tools`.
    pub fn tools(mut self, tools: Vec<crate::subagent::ToolSpec>) -> Self {
        self.tool_specs.extend(tools);
        self
    }

    // ── #3: system_prompt ──────────────────────────────────────────────

    /// Set the system prompt (USER slot in the prompt assembly).
    ///
    /// Maps to parameter #3: `system_prompt`.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    // ── #4: middleware ─────────────────────────────────────────────────

    /// Add an extra middleware hook.
    ///
    /// Maps to parameter #4: `middleware`.
    pub fn hook<H>(mut self, hook: H) -> Self
    where
        H: AgentHook + 'static,
    {
        self.extra_hook_stack.push(hook);
        self.extra_hook_count += 1;
        self
    }

    // ── #5: subagents ──────────────────────────────────────────────────

    /// Add a sub-agent specification.
    ///
    /// Maps to parameter #5: `subagents`.
    pub fn subagent(mut self, spec: SubAgentSpec) -> Self {
        self.subagents.push(spec);
        self
    }

    /// Set multiple sub-agent specifications.
    ///
    /// Maps to parameter #5: `subagents`.
    pub fn subagents(mut self, specs: Vec<SubAgentSpec>) -> Self {
        self.subagents.extend(specs);
        self
    }

    // ── #6: skills ─────────────────────────────────────────────────────

    /// Set skill file paths (SKILL.md discovery).
    ///
    /// Maps to parameter #6: `skills`.
    pub fn skills(mut self, paths: Vec<PathBuf>) -> Self {
        self.skills = paths;
        self
    }

    // ── #7: memory ─────────────────────────────────────────────────────

    /// Set memory file paths (AGENTS.md loading into system prompt).
    ///
    /// Maps to parameter #7: `memory`.
    pub fn memory(mut self, paths: Vec<PathBuf>) -> Self {
        self.memory_files = paths;
        self
    }

    // ── #8: permissions ────────────────────────────────────────────────

    /// Set filesystem permissions.
    ///
    /// Maps to parameter #8: `permissions`.
    pub fn permissions(mut self, perms: Vec<FilesystemPermission>) -> Self {
        self.permissions = perms;
        self
    }

    // ── #9: backend ─────────────────────────────────────────────────────

    /// Set the backend (filesystem provider).
    ///
    /// Maps to parameter #9: `backend`.
    pub fn backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = Some(backend);
        self
    }

    // ── #10: interrupt_on ───────────────────────────────────────────────

    /// Set the interrupt map for HITL.
    ///
    /// Maps to parameter #10: `interrupt_on`.
    pub fn interrupt_on(mut self, map: InterruptMap) -> Self {
        self.interrupt_on = Some(map);
        self
    }

    // ── #11: response_format ────────────────────────────────────────────

    /// Set the response format schema.
    ///
    /// Maps to parameter #11: `response_format`.
    pub fn response_format(mut self, schema: serde_json::Value) -> Self {
        self.response_format = Some(schema);
        self
    }

    // ── #13: context ────────────────────────────────────────────────────

    /// Set the context (generic, stored as opaque JSON for v0).
    ///
    /// Maps to parameter #13: `context_schema`.
    pub fn context(mut self, ctx: serde_json::Value) -> Self {
        self.context = Some(ctx);
        self
    }

    // ── #14: checkpointer ───────────────────────────────────────────────

    /// Set the checkpointer.
    ///
    /// Maps to parameter #14: `checkpointer`.
    ///
    /// **v0: not connected.** The checkpointer is stored but `build()` does
    /// not wire it into the agent. Checkpointing requires a custom runner
    /// wrapper (planned for `deepagents-sessions`, v1).
    pub fn checkpointer(mut self, cp: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(cp);
        self
    }

    // ── #15: store ──────────────────────────────────────────────────────

    /// Set the store.
    ///
    /// Maps to parameter #15: `store`.
    pub fn store(mut self, store: Arc<dyn Store>) -> Self {
        self.store = Some(store);
        self
    }

    // ── #16: debug ──────────────────────────────────────────────────────

    /// Enable or disable debug mode.
    ///
    /// Maps to parameter #16: `debug`.
    pub fn debug(mut self, enabled: bool) -> Self {
        self.debug = enabled;
        self
    }

    // ── #17: name ───────────────────────────────────────────────────────

    /// Set the agent name.
    ///
    /// Maps to parameter #17: `name`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    // ── #18: cache ──────────────────────────────────────────────────────

    /// Set the cache.
    ///
    /// Maps to parameter #18: `cache`.
    pub fn cache(mut self, cache: Arc<dyn Cache>) -> Self {
        self.cache = Some(cache);
        self
    }

    // ── #19: MCP servers (pure additive) ───────────────────────────────

    /// Add an MCP server configuration.
    ///
    /// Pure additive capability #19 — no Python equivalent.
    pub fn mcp_server(mut self, config: McpServerConfig) -> Self {
        self.mcp_servers.push(config);
        self
    }

    // ── HarnessProfile configuration ────────────────────────────────────

    /// Set the harness profile.
    pub fn profile(mut self, profile: HarnessProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Set the base system prompt (BASE slot).
    pub fn base_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.profile.base_system_prompt = Some(prompt.into());
        self
    }

    /// Set the system prompt suffix (SUFFIX slot).
    pub fn system_prompt_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.profile.system_prompt_suffix = Some(suffix.into());
        self
    }

    /// Exclude a tool by name.
    pub fn exclude_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.profile.excluded_tools.insert(tool_name.into());
        self
    }

    /// Exclude a middleware hook by name (protected: Filesystem, SubAgent).
    pub fn exclude_middleware(mut self, hook_name: impl Into<String>) -> Self {
        self.profile.excluded_middleware.insert(hook_name.into());
        self
    }

    /// Configure the general-purpose sub-agent.
    pub fn general_purpose_subagent(mut self, gp: GeneralPurposeSubagentProfile) -> Self {
        self.profile.general_purpose_subagent = gp;
        self
    }

    // ── Build ───────────────────────────────────────────────────────────

    /// Assemble the full system prompt from USER + BASE + SUFFIX.
    fn assemble_prompt(&self) -> String {
        let user = self.system_prompt.as_deref().unwrap_or("");
        self.profile.assemble_prompt(user)
    }

    /// Count how many standard middleware hooks would be included.
    #[cfg(test)]
    fn standard_hook_count(&self) -> usize {
        let mut count = 0;
        if !self.profile.is_middleware_excluded("Filesystem") && self.backend.is_some() {
            count += 1;
        }
        if !self.profile.is_middleware_excluded("SubAgent") {
            count += 1;
        }
        if !self.profile.is_middleware_excluded("Summarization") {
            count += 1;
        }
        if !self.profile.is_middleware_excluded("HITL") && self.interrupt_on.is_some() {
            count += 1;
        }
        count
    }

    /// Returns total hook count (standard + extra).
    #[cfg(test)]
    fn total_hook_count(&self) -> usize {
        self.standard_hook_count() + self.extra_hook_count
    }

    /// Build the agent, producing a configured [`Agent`].
    ///
    /// This assembles:
    /// 1. The system prompt (USER + BASE + SUFFIX)
    /// 2. The middleware hook stack in correct execution order:
    ///    Filesystem → SubAgent → Summarization → HITL → [extra hooks]
    /// 3. The rig [`AgentBuilder`] with all configured parameters
    ///
    /// Hook ordering rationale: `HookStack` dispatches hooks in registration
    /// order (first registered = first executed). For `on_tool_call`, the
    /// first non-`Run` action wins, so HITL must run *after* permission checks
    /// to allow `Deny` to take precedence over `Interrupt`. For
    /// `on_completion_call`, patches accumulate in registration order, so
    /// Filesystem (which injects fs tools) must run before SubAgent.
    pub fn build(self) -> Agent {
        let prompt = self.assemble_prompt();

        let mut builder = AgentBuilder::new(self.model).preamble(&prompt);

        if let Some(ref name) = self.name {
            builder = builder.name(name);
        }

        // Build the hook stack in correct execution order.
        // Standard middleware first (in priority order), then extra hooks
        // as a nested HookStack (HookStack itself implements AgentHook).
        let mut hook_stack = HookStack::new();

        // 1. Filesystem middleware (protected — always included if backend exists)
        if !self.profile.is_middleware_excluded("Filesystem") {
            if let Some(ref backend) = self.backend {
                let fs_mw = FilesystemMiddleware::new(backend.clone(), self.permissions.clone());
                hook_stack.push(fs_mw);
            }
        }

        // 2. SubAgent middleware (protected — always included)
        if !self.profile.is_middleware_excluded("SubAgent") {
            let names: Vec<String> = self.subagents.iter().map(|s| s.name.clone()).collect();
            let mut all_names = names;
            if self.profile.general_purpose_subagent.enabled {
                all_names.push("general-purpose".to_string());
            }
            hook_stack.push(SubAgentMiddleware::new(all_names));
        }

        // 3. Summarization middleware (excludable)
        if !self.profile.is_middleware_excluded("Summarization") {
            hook_stack.push(SummarizationMiddleware::new());
        }

        // 4. HITL middleware (excludable, runs last among standard middleware
        //    so permission Deny takes precedence over HITL Interrupt)
        if !self.profile.is_middleware_excluded("HITL") {
            if let Some(ref interrupt_map) = self.interrupt_on {
                hook_stack.push(crate::hitl::HitlMiddleware::new(interrupt_map.clone()));
            }
        }

        // 5. Extra hooks (user-registered, run after standard middleware)
        if self.extra_hook_count > 0 {
            hook_stack.push(self.extra_hook_stack);
        }

        if !hook_stack.is_empty() {
            builder = builder.add_hook(hook_stack);
        }

        builder.build()
    }

    /// Build an [`AgentRunner`] from the configured agent, ready for a single
    /// prompt request.
    pub fn build_runner(self, prompt: impl Into<rig_core::completion::Message>) -> AgentRunner {
        let agent = self.build();
        AgentRunner::from_agent(&agent, prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::StateBackend;
    use crate::mock::MockCompletionModel;

    #[test]
    fn test_builder_prompt_assembly() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("USER PROMPT")
            .base_system_prompt("BASE")
            .system_prompt_suffix("SUFFIX");

        let prompt = builder.assemble_prompt();
        assert_eq!(prompt, "USER PROMPT\n\nBASE\n\nSUFFIX");
    }

    #[test]
    fn test_builder_default_prompt() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new().model(model);
        let prompt = builder.assemble_prompt();
        assert_eq!(prompt, "");
    }

    #[test]
    fn test_builder_with_backend() {
        let model = MockCompletionModel::new();
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .backend(backend);

        // Filesystem + SubAgent + Summarization = 3 standard hooks
        assert_eq!(builder.standard_hook_count(), 3);
        assert_eq!(builder.total_hook_count(), 3);
    }

    #[test]
    fn test_builder_without_backend() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test");

        // SubAgent + Summarization = 2 hooks (no Filesystem without backend)
        assert_eq!(builder.standard_hook_count(), 2);
    }

    #[test]
    fn test_builder_with_interrupt_on() {
        let model = MockCompletionModel::new();
        let mut map = InterruptMap::new();
        map.insert_simple("write_file", true);

        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .backend(backend)
            .interrupt_on(map);

        // Filesystem + SubAgent + Summarization + HITL = 4 hooks
        assert_eq!(builder.standard_hook_count(), 4);
    }

    #[test]
    fn test_builder_exclude_summarization() {
        let model = MockCompletionModel::new();
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .backend(backend)
            .exclude_middleware("Summarization");

        // Filesystem + SubAgent = 2 hooks (Summarization excluded)
        assert_eq!(builder.standard_hook_count(), 2);
    }

    #[test]
    fn test_builder_protected_middleware_cannot_exclude() {
        let model = MockCompletionModel::new();
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .backend(backend)
            .exclude_middleware("Filesystem")
            .exclude_middleware("SubAgent");

        // Filesystem + SubAgent + Summarization = 3 hooks (protected)
        assert_eq!(builder.standard_hook_count(), 3);
    }

    #[test]
    fn test_builder_gp_subagent_disabled() {
        let model = MockCompletionModel::new();
        let mut gp = GeneralPurposeSubagentProfile::default();
        gp.enabled = false;

        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .general_purpose_subagent(gp);

        // SubAgent + Summarization = 2 hooks
        assert_eq!(builder.standard_hook_count(), 2);
    }

    #[test]
    fn test_builder_build_agent() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .name("test-agent");

        let agent = builder.build();
        // Agent should be constructed successfully
        assert!(agent.name().is_some());
    }

    #[test]
    fn test_builder_build_agent_with_backend() {
        let model = MockCompletionModel::new();
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .name("test-agent")
            .backend(backend);

        let agent = builder.build();
        assert!(agent.name().is_some());
    }

    #[test]
    fn test_builder_chained_setters() {
        let model = MockCompletionModel::new();
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a helpful assistant")
            .name("my-agent")
            .debug(true)
            .backend(backend)
            .skills(vec![PathBuf::from("/path/to/skill")])
            .memory(vec![PathBuf::from("/path/to/AGENTS.md")]);

        assert_eq!(builder.name, Some("my-agent".into()));
        assert!(builder.debug);
        assert!(builder.backend.is_some());
        assert_eq!(builder.skills.len(), 1);
        assert_eq!(builder.memory_files.len(), 1);
    }

    #[test]
    fn test_builder_subagents() {
        let model = MockCompletionModel::new();
        let sub1 = SubAgentSpec::new("researcher", "Research", "You research");
        let sub2 = SubAgentSpec::new("coder", "Code", "You code");

        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .subagent(sub1)
            .subagent(sub2);

        assert_eq!(builder.subagents.len(), 2);
    }

    #[test]
    fn test_builder_model_spec() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new()
            .model(model)
            .model_spec("openai:gpt-4o");

        assert!(builder.model_spec.is_some());
        assert_eq!(builder.model_spec.as_ref().unwrap().provider(), "openai");
    }

    #[test]
    fn test_builder_extra_hook() {
        let model = MockCompletionModel::new();
        let builder = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("test")
            .hook(SummarizationMiddleware::new());

        // 2 standard + 1 extra = 3
        assert_eq!(builder.total_hook_count(), 3);
    }
}

// ── End-to-end integration tests ───────────────────────────────────────
//
// These tests exercise the full pipeline: DeepAgentBuilder → build() →
// AgentRunner::run() → PromptResponse. They use MockCompletionModel so no
// network access is required.

#[cfg(test)]
mod integration {
    use super::*;
    use crate::backend::StateBackend;
    use crate::mock::MockCompletionModel;

    /// Build a minimal agent (no backend, no tools) and run it with a single
    /// mock text response. The run should succeed and the output should match
    /// the mock's programmed response.
    #[tokio::test]
    async fn test_e2e_simple_text_response() {
        let model = MockCompletionModel::single("Hello from the agent!");
        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a test agent.")
            .name("e2e-simple")
            .build_runner("Say hello");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "Hello from the agent!");
    }

    /// Build an agent with a backend (Filesystem + SubAgent + Summarization
    /// middleware) and run it. The middleware hooks fire but the mock model
    /// just returns text — the run should still succeed.
    #[tokio::test]
    async fn test_e2e_with_backend_and_middleware() {
        let model = MockCompletionModel::single("I have filesystem access.");
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a file agent.")
            .name("e2e-backend")
            .backend(backend)
            .build_runner("List files");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "I have filesystem access.");
    }

    /// Build an agent with permissions that deny writes to /etc, then run.
    /// The mock model returns text (no tool call), so permissions are not
    /// triggered — the run should succeed normally.
    #[tokio::test]
    async fn test_e2e_permissions_not_triggered_without_tool_call() {
        let model = MockCompletionModel::single("Just chatting, no tools.");
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let perms = vec![crate::permission::FilesystemPermission::deny(
            vec![crate::permission::FilesystemOperation::Write],
            vec!["/etc/**".to_string()],
        )];

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a restricted agent.")
            .name("e2e-perms")
            .backend(backend)
            .permissions(perms)
            .build_runner("Write to /etc/passwd");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "Just chatting, no tools.");
    }

    /// Build an agent with HITL interrupt_on configured, then run with a
    /// mock text response. Since the model doesn't make a tool call, the HITL
    /// hook is never triggered — the run should succeed.
    #[tokio::test]
    async fn test_e2e_hitl_not_triggered_without_tool_call() {
        let model = MockCompletionModel::single("No tool calls here.");
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let mut interrupt_map = InterruptMap::new();
        interrupt_map.insert_simple("write_file", true);

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a HITL agent.")
            .name("e2e-hitl")
            .backend(backend)
            .interrupt_on(interrupt_map)
            .build_runner("Write a file");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "No tool calls here.");
    }

    /// Build an agent with an extra user-registered hook and run it.
    /// The extra hook (SummarizationMiddleware) fires on completion_call
    /// but doesn't block the run.
    #[tokio::test]
    async fn test_e2e_with_extra_hook() {
        let model = MockCompletionModel::single("Extra hook test.");
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a hooked agent.")
            .name("e2e-hook")
            .backend(backend)
            .hook(SummarizationMiddleware::new())
            .build_runner("Say something");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "Extra hook test.");
    }

    /// Build an agent with excluded Summarization middleware and run it.
    /// The run should succeed with only Filesystem + SubAgent middleware.
    #[tokio::test]
    async fn test_e2e_exclude_summarization() {
        let model = MockCompletionModel::single("No summarization needed.");
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a minimal agent.")
            .name("e2e-no-sum")
            .backend(backend)
            .exclude_middleware("Summarization")
            .build_runner("Say something");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "No summarization needed.");
    }

    /// Build an agent with a multi-response mock model. The first response
    /// is consumed by the run; the model should still have remaining
    /// responses in its queue.
    #[tokio::test]
    async fn test_e2e_multi_response_model() {
        let model = MockCompletionModel::from_responses(vec![
            "first response".to_string(),
            "second response".to_string(),
        ]);

        // Clone the model so we can inspect remaining() after the run.
        let model_for_check = model.clone();

        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a multi-response agent.")
            .name("e2e-multi")
            .build_runner("Say first");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "first response");
        // One response consumed, one remaining.
        assert_eq!(model_for_check.remaining(), 1);
    }

    /// Build an agent with a prompt assembled from USER + BASE + SUFFIX.
    /// The run should succeed and the mock model's response should come back.
    #[tokio::test]
    async fn test_e2e_prompt_assembly() {
        let model = MockCompletionModel::single("Assembled prompt works.");
        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("USER INSTRUCTIONS")
            .base_system_prompt("BASE CONTEXT")
            .system_prompt_suffix("SUFFIX NOTES")
            .name("e2e-prompt")
            .build_runner("Do something");

        let response = runner.run().await.expect("run should succeed");
        assert_eq!(response.output, "Assembled prompt works.");
    }

    /// Verify that the PromptResponse carries message history after a run.
    #[tokio::test]
    async fn test_e2e_response_has_messages() {
        let model = MockCompletionModel::single("History test.");
        let runner = DeepAgentBuilder::new()
            .model(model)
            .system_prompt("You are a history agent.")
            .name("e2e-history")
            .build_runner("Say something");

        let response = runner.run().await.expect("run should succeed");
        // The response should carry message history (at least the system
        // prompt + user prompt + assistant response).
        assert!(response.messages.is_some());
        let messages = response.messages.as_ref().unwrap();
        assert!(
            messages.len() >= 2,
            "expected at least 2 messages in history, got {}",
            messages.len()
        );
    }
}
