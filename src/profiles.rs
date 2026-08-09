//! Harness/provider profile registry —— 对应 deepagents `profiles/harness/harness_profiles.py`。
//!
//! `HarnessProfile` 描述 `create_deep_agent` 在模型已构造后如何塑造 agent 运行时：
//! prompt 拼装（`base_system_prompt` / `system_prompt_suffix`）、工具可见性
//! （`excluded_tools`）、中间件裁剪（`excluded_middleware`）、工具描述覆写
//! （`tool_description_overrides`）。它**不是** `Middleware`——是纯配置层，
//! 由 `create_deep_agent` 消费（见本文件末尾「整合点」）。
//!
//! # 与 deepagents 的对齐与简化
//!
//! - `HarnessProfile` 字段对齐 deepagents `@dataclass(frozen=True) HarnessProfile`
//!   的声明式子集（不含 `extra_middleware` 运行时字段、`general_purpose_subagent`
//!   子配置——defer 到后续阶段）。
//! - `provider` / `model`：deepagents 用 `provider:model` 复合 key 注册，我们拆成
//!   两字段便于 Rust 直接构造模型实例；`ProfileRegistry` 以独立 `name` 为 key。
//! - `apply_profile_prompt`：对齐 deepagents 主 agent 的拼装顺序 `USER + BASE + SUFFIX`
//!   （空段跳过）。deepagents 的 `_apply_profile_prompt` 中 `BASE` 会**替换** caller
//!   base prompt（用于子 agent authored prompt 场景）；此处采用主 agent 的拼接语义
//!   （USER 在前、BASE 随后、SUFFIX 最后），是 task 指定的简化。子 agent 的替换语义
//!   defer 到 subagent profile 落地时再补。
//! - `register`：last-write-wins 覆盖（Rust `HashMap` 自然语义）。deepagents 的
//!   additive-merge（`_merge_profiles`：标量取新值、set 取并集、map 逐 key 覆盖）
//!   defer 到需要 layering 时再实现。
//! - profile key 语法校验（deepagents `validate_profile_key`：空 / 多 `:` / 半空段
//!   拒绝）defer——Rust 侧 key 即 `&str` name，校验留待主会话决定是否加。
//! - 内置 profile 的 `system_prompt_suffix` 用占位文本标签示例（model 名亦占位），
//!   不必精确对齐 deepagents 发布的最新 prompt。

use std::collections::HashMap;
use std::fmt;

/// Harness profile：provider/model 维度的运行时配置。
///
/// 纯配置对象，由 `create_deep_agent` 消费——不实现 `Middleware` trait。
/// 见 [`apply_profile_prompt`] 做 prompt 拼装，`excluded_tools` 做工具过滤。
#[derive(Clone, PartialEq, Eq)]
pub struct HarnessProfile {
    /// provider 名（如 `"anthropic"` / `"openai"`）。用于路由与 provider 级 fallback。
    pub provider: String,
    /// 模型名占位（如 `"claude-opus-4-7"` / `"gpt-5.1"`）。deepagents 用最新，此处标签示例。
    pub model: String,
    /// `BASE` 槽：拼装在 USER 之后、SUFFIX 之前。`None` 跳过。
    pub base_system_prompt: Option<String>,
    /// `SUFFIX` 槽：拼装在最后（USER + BASE 之后）。`None` 跳过。
    pub system_prompt_suffix: Option<String>,
    /// 需从工具集移除的工具名（caller tools + middleware 提供的工具都受影响）。
    pub excluded_tools: Vec<String>,
    /// 需从中间件链剥离的中间件名。deepagents 在此拒绝对 scaffolding 中间件
    /// （`FilesystemMiddleware`/`SubAgentMiddleware`）的排除——本 Rust 移植暂不强制，
    /// 由消费方（`create_deep_agent`）决定是否校验。
    pub excluded_middleware: Vec<String>,
    /// 按工具名覆写工具描述。key = 工具名，value = 新描述。
    pub tool_description_overrides: HashMap<String, String>,
}

impl fmt::Debug for HarnessProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessProfile")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("has_base", &self.base_system_prompt.is_some())
            .field("has_suffix", &self.system_prompt_suffix.is_some())
            .field("excluded_tools", &self.excluded_tools.len())
            .field("excluded_middleware", &self.excluded_middleware.len())
            .field("tool_overrides", &self.tool_description_overrides.len())
            .finish()
    }
}

impl HarnessProfile {
    /// 以 provider + model 构造（其余字段为空）。
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            base_system_prompt: None,
            system_prompt_suffix: None,
            excluded_tools: Vec::new(),
            excluded_middleware: Vec::new(),
            tool_description_overrides: HashMap::new(),
        }
    }

    /// 设置 `base_system_prompt`（`BASE` 槽）。
    #[must_use]
    pub fn base_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.base_system_prompt = Some(prompt.into());
        self
    }

    /// 设置 `system_prompt_suffix`（`SUFFIX` 槽）。
    #[must_use]
    pub fn system_prompt_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.system_prompt_suffix = Some(suffix.into());
        self
    }

    /// 设置 `excluded_tools`（替换）。
    #[must_use]
    pub fn excluded_tools(mut self, tools: Vec<String>) -> Self {
        self.excluded_tools = tools;
        self
    }

    /// 追加单个 excluded tool。
    #[must_use]
    pub fn exclude_tool(mut self, tool: impl Into<String>) -> Self {
        self.excluded_tools.push(tool.into());
        self
    }

    /// 设置 `excluded_middleware`（替换）。
    #[must_use]
    pub fn excluded_middleware(mut self, middleware: Vec<String>) -> Self {
        self.excluded_middleware = middleware;
        self
    }

    /// 追加单个 excluded middleware。
    #[must_use]
    pub fn exclude_middleware(mut self, middleware: impl Into<String>) -> Self {
        self.excluded_middleware.push(middleware.into());
        self
    }

    /// 设置 `tool_description_overrides`（替换）。
    #[must_use]
    pub fn tool_description_overrides(mut self, overrides: HashMap<String, String>) -> Self {
        self.tool_description_overrides = overrides;
        self
    }

    /// 追加单条工具描述覆写。
    #[must_use]
    pub fn override_tool_description(
        mut self,
        tool: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.tool_description_overrides
            .insert(tool.into(), description.into());
        self
    }

    /// 是否排除某工具名。
    #[must_use]
    pub fn excludes_tool(&self, name: &str) -> bool {
        self.excluded_tools.iter().any(|t| t == name)
    }

    /// 是否排除某中间件名。
    #[must_use]
    pub fn excludes_middleware(&self, name: &str) -> bool {
        self.excluded_middleware.iter().any(|m| m == name)
    }
}

/// Profile 注册表：name → `HarnessProfile`。
///
/// last-write-wins（后注册覆盖同名）。additive-merge（deepagents `_merge_profiles`）
/// defer。线程安全：`ProfileRegistry` 非 `Send`/`Sync` 特化——持 `HashMap`，
/// 由调用方决定共享方式（Rust 侧通常在构造期一次性 `register_builtin` 后以值或
/// `Arc` 传递只读视图）。
#[derive(Clone, Debug, Default)]
pub struct ProfileRegistry {
    profiles: HashMap<String, HarnessProfile>,
}

impl ProfileRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    /// 注册（覆盖同名）。
    pub fn register(&mut self, name: &str, profile: HarnessProfile) {
        self.profiles.insert(name.to_string(), profile);
    }

    /// 查询（不存在返回 `None`）。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&HarnessProfile> {
        self.profiles.get(name)
    }

    /// 列出已注册 profile 名（插入序不保证；`HashMap` 迭代序）。便于诊断与 listing。
    #[must_use]
    pub fn list(&self) -> Vec<&str> {
        self.profiles.keys().map(|k| k.as_str()).collect()
    }

    /// 已注册 profile 数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// 是否空注册表。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// 注册内置示例 profile（`anthropic-opus` / `anthropic-sonnet` / `openai-gpt`）。
    ///
    /// model 名与 suffix 为占位标签示例，不必精确对齐 deepagents 发布的最新值。
    /// 调用方通常在启动时 `let mut reg = ProfileRegistry::new(); reg.register_builtin();`
    /// 一次性填充。
    pub fn register_builtin(&mut self) {
        self.register(
            "anthropic-opus",
            HarnessProfile::new("anthropic", "claude-opus-4-7").system_prompt_suffix(
                "Use parallel tool calls when independent. Read files before \
                     describing them. Reflect on tool results before proceeding.",
            ),
        );
        self.register(
            "anthropic-sonnet",
            HarnessProfile::new("anthropic", "claude-sonnet-4-6").system_prompt_suffix(
                "Prefer parallel tool calls for independent reads. Ground \
                     answers in observed tool output; do not speculate.",
            ),
        );
        self.register(
            "openai-gpt",
            HarnessProfile::new("openai", "gpt-5.1").system_prompt_suffix(
                "Bias to action: implement with reasonable assumptions rather \
                     than asking for clarification. Batch independent tool calls.",
            ),
        );
    }
}

/// 拼 profile 的 system prompt：`USER + BASE + SUFFIX`，空段跳过，`\n\n` 分隔。
///
/// 对齐 deepagents 主 agent 拼装顺序。`user_prompt`（USER）来自 caller `system_prompt`，
/// `base_system_prompt`（BASE）与 `system_prompt_suffix`（SUFFIX）来自 profile。三者均
/// 可空——空段跳过，非空段以 `\n\n` 连接。三者全空返回空串（由调用方决定是否注入
/// 空 system message）。
///
/// # 与 deepagents `_apply_profile_prompt` 的差异
///
/// deepagents 的 `_apply_profile_prompt(profile, base_prompt)` 中 `BASE` 会**替换**
/// `base_prompt`（用于子 agent authored prompt 场景）。本函数采用主 agent 拼接语义
/// （USER + BASE + SUFFIX），是 task 指定的简化；子 agent 替换语义 defer。
#[must_use]
pub fn apply_profile_prompt(profile: &HarnessProfile, user_prompt: &str) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    if !user_prompt.is_empty() {
        parts.push(user_prompt);
    }
    if let Some(base) = &profile.base_system_prompt
        && !base.is_empty()
    {
        parts.push(base.as_str());
    }
    if let Some(suffix) = &profile.system_prompt_suffix
        && !suffix.is_empty()
    {
        parts.push(suffix.as_str());
    }
    parts.join("\n\n")
}

// ============================================================
// 整合点（供主会话）——本文件不实现，仅说明消费契约
// ============================================================
//
// `profiles` 是**配置层**，不是 Middleware。`create_deep_agent`（`src/graph.rs`）
// 需新增可选 `profile: Option<&HarnessProfile>` 参数（或 `DeepAgentConfig` 字段），
// 在 agent node 中消费：
//
// 1. prompt 拼装：把 `DeepAgentConfig.system_prompt`（USER）经
//    `apply_profile_prompt(profile, &user_system_prompt)` 拼出 USER+BASE+SUFFIX，
//    作为 `ModelRequest.system_message` 初值（替代当前
//    `system_prompt.clone().unwrap_or_default()`）。中间件仍在其后 `push_str` 追加。
//
// 2. excluded_tools 过滤：在 `create_deep_agent` 合并 `all_tools` 后、构造
//    `all_tool_defs` 前，按 `profile.excluded_tools` 过滤（`retain` 不匹配排除项），
//    模式参考 `FilesystemMiddleware::wrap_model_call` 的 `req.tools.retain(...)`。
//
// 3. excluded_middleware 过滤：从 `MiddlewareChain` 剥离匹配项——当前
//    `MiddlewareChain` 按插入序持 `Arc<dyn Middleware>`，中间件名匹配需中间件
//    暴露稳定名（deepagents 的 `AgentMiddleware.name`）。Rust 侧 trait 无 name 字段，
//    主会话需先决定命名方案（如 `Middleware` trait 加 `fn name()` 默认实现，
//    或用 `std::any::type_name::<T>()`）再接过滤。
//
// 4. tool_description_overrides：在构造 `ToolDefinition` 时按 name 覆写
//    `description` 字段（`convert_tool_defs` 或其上游加 override 步骤）。
//
// 主会话改动清单：
// - `src/graph.rs`：`DeepAgentConfig` 加 `profile: Option<HarnessProfile>` 字段 +
//   `DeepAgentBuilder::profile(...)` 方法；agent node 消费上述 1/2/4。
// - `src/lib.rs`：`pub mod profiles;` + `pub use profiles::{
//     apply_profile_prompt, HarnessProfile, ProfileRegistry
//   };`
// - `src/middleware/mod.rs`：若做 excluded_middleware 过滤，给 `Middleware` trait
//   加 `fn name() -> &str`（默认 `std::any::type_name::<Self>()` 的短名）。
// - 无 Cargo.toml 依赖新增（profiles.rs 仅用 `std::collections::HashMap`）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profile_defaults_empty() {
        let p = HarnessProfile::new("anthropic", "claude-opus-4-7");
        assert_eq!(p.provider, "anthropic");
        assert_eq!(p.model, "claude-opus-4-7");
        assert!(p.base_system_prompt.is_none());
        assert!(p.system_prompt_suffix.is_none());
        assert!(p.excluded_tools.is_empty());
        assert!(p.excluded_middleware.is_empty());
        assert!(p.tool_description_overrides.is_empty());
    }

    #[test]
    fn builder_chains() {
        let p = HarnessProfile::new("openai", "gpt-5.1")
            .base_system_prompt("base")
            .system_prompt_suffix("suffix")
            .exclude_tool("execute")
            .exclude_middleware("SummarizationMiddleware")
            .override_tool_description("ls", "list files");
        assert_eq!(p.base_system_prompt.as_deref(), Some("base"));
        assert_eq!(p.system_prompt_suffix.as_deref(), Some("suffix"));
        assert!(p.excludes_tool("execute"));
        assert!(p.excludes_middleware("SummarizationMiddleware"));
        assert_eq!(
            p.tool_description_overrides.get("ls").map(|s| s.as_str()),
            Some("list files"),
        );
    }

    #[test]
    fn registry_register_get_list() {
        let mut reg = ProfileRegistry::new();
        assert!(reg.is_empty());
        reg.register("mine", HarnessProfile::new("anthropic", "claude-opus-4-7"));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("mine").is_some());
        assert!(reg.get("missing").is_none());
        let names = reg.list();
        assert_eq!(names, vec!["mine"]);
    }

    #[test]
    fn register_overwrites_same_name() {
        let mut reg = ProfileRegistry::new();
        reg.register("k", HarnessProfile::new("a", "m1"));
        reg.register("k", HarnessProfile::new("b", "m2"));
        let p = reg.get("k").expect("present");
        assert_eq!(p.provider, "b");
        assert_eq!(p.model, "m2");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn register_builtin_populates_three() {
        let mut reg = ProfileRegistry::new();
        reg.register_builtin();
        assert!(reg.get("anthropic-opus").is_some());
        assert!(reg.get("anthropic-sonnet").is_some());
        assert!(reg.get("openai-gpt").is_some());
        assert_eq!(reg.len(), 3);
        let opus = reg.get("anthropic-opus").expect("opus present");
        assert_eq!(opus.provider, "anthropic");
        assert!(opus.system_prompt_suffix.is_some());
    }

    #[test]
    fn apply_prompt_user_only() {
        let p = HarnessProfile::new("a", "m");
        assert_eq!(apply_profile_prompt(&p, "hello"), "hello");
    }

    #[test]
    fn apply_prompt_all_three() {
        let p = HarnessProfile::new("a", "m")
            .base_system_prompt("BASE")
            .system_prompt_suffix("SUFFIX");
        assert_eq!(apply_profile_prompt(&p, "USER"), "USER\n\nBASE\n\nSUFFIX",);
    }

    #[test]
    fn apply_prompt_skips_empty_segments() {
        let p = HarnessProfile::new("a", "m").system_prompt_suffix("SUFFIX");
        // USER 空 → 跳过；BASE None → 跳过；只剩 SUFFIX。
        assert_eq!(apply_profile_prompt(&p, ""), "SUFFIX");
    }

    #[test]
    fn apply_prompt_all_empty_returns_empty() {
        let p = HarnessProfile::new("a", "m");
        assert_eq!(apply_profile_prompt(&p, ""), "");
    }

    #[test]
    fn apply_prompt_skips_empty_base_or_suffix() {
        let p = HarnessProfile::new("a", "m")
            .base_system_prompt("")
            .system_prompt_suffix("SUFFIX");
        // BASE 虽 Some 但为空串 → 跳过。
        assert_eq!(apply_profile_prompt(&p, "USER"), "USER\n\nSUFFIX");
    }

    #[test]
    fn debug_does_not_leak_prompt_bodies() {
        let p = HarnessProfile::new("a", "m")
            .base_system_prompt("secret base")
            .system_prompt_suffix("secret suffix");
        let s = format!("{p:?}");
        assert!(s.contains("has_base"));
        assert!(!s.contains("secret"));
    }

    #[test]
    fn clone_roundtrips() {
        let p = HarnessProfile::new("a", "m")
            .base_system_prompt("base")
            .exclude_tool("execute");
        let cloned = p.clone();
        assert_eq!(p, cloned);
    }
}
