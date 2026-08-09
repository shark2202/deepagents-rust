//! `AnthropicPromptCachingMiddleware` —— Anthropic prompt caching（`cache_control` marker）中间件。
//!
//! 对应 deepagents `AnthropicPromptCachingMiddleware`：在每次 LLM 调用前给 `system_message`
//! 末尾与长消息加 `cache_control: { type: "ephemeral" }` break，标记可缓存前缀，让 Anthropic
//! 按 prompt 前缀复用 KV cache，降低长上下文重复调用的成本与延迟。对非 Anthropic provider no-op。
//!
//! # Fact 查证结论（2026-08）：juncture `ChatAnthropic` 不支持 `cache_control`
//!
//! 已查 `D:\juncture\crates\juncture\src\llm\anthropic.rs` 与全 `D:\juncture\crates` 树：
//! - 字面量 `cache_control` 在 juncture 全 crate 树**零命中**（`rg 'cache_control'`）。
//! - `AnthropicRequest.system` 字段类型为 `Option<String>`（anthropic.rs:684）——序列化为
//!   裸 JSON 字符串，**非** Anthropic prompt caching 所需的 block 数组形式
//!   `[{"type":"text","text":"...","cache_control":{"type":"ephemeral"}}]`，无处挂 `cache_control`。
//! - `ContentBlock` 枚举（anthropic.rs:718-734）的 `Text`/`Image`/`ToolUse`/`ToolResult` 各变体
//!   **均无** `cache_control` 字段；`AnthropicMessage`（anthropic.rs:700-704）亦无。
//! - `juncture::llm` 里出现的 `cache*` 符号（`try_llm_cache_lookup`/`try_llm_cache_store`/
//!   `CacheKeyInput`，design 09-observability §4.4）是 juncture 自有的 **LLM 响应缓存**
//!   （按输入哈希缓存整条响应），与 Anthropic prompt-prefix caching 是两回事。
//! - `juncture_core::state::messages` 里的 `ephemeral` 命中是 State trait 的 `reset_ephemeral()`
//!   （checkpoint 时重置 untracked 字段），与 `cache_control: {type:"ephemeral"}` 无关。
//!
//! # 为何无法在中间件层“绕过”
//!
//! 即便本中间件想注入 marker，也无路径可达：`ModelRequest.system_message` 是 `String`，
//! 只能 `push_str` 追加文本；而 `cache_control` 必须作为结构化字段挂在 HTTP 请求的 content
//! block 上，这些 block 由 `ChatAnthropic::invoke` 内部从 `Message`/`Content` 构造，中间件
//! 拿不到该模型句柄、也改不了其内部序列化。`Message`/`Content`/`ContentPart`（re-export 自
//! `juncture_core::state::messages`）本身也不带 `cache_control` 字段，且 `convert_content`
//! 会把它们重建为无 `cache_control` 的 `ContentBlock`。
//!
//! # 当前实现：no-op（defer 到 juncture 上游支持后）
//!
//! 据上述查证，**prompt caching defer 到 juncture 上游 `ChatAnthropic` 支持 `cache_control`
//! 后**再实现真实 marker 注入。当前为本中间件提供 no-op 占位：
//! - `wrap_model_call` 空实现（不改 `req` / `state`），保持链序与 deepagents API 同构。
//! - 非 Anthropic provider 时的 no-op 行为（deepagents 原行为）天然由“恒 no-op”覆盖。
//!
//! 上游落地所需改动（供后续追踪）：juncture `ChatAnthropic` 需 (a) 把 `system` 序列化为
//! block 数组并支持 `cache_control`，(b) 给 `ContentBlock`/`Message` 加可选 `cache_control`
//! 字段并在 `convert_content` 透传，(c) 暴露给中间件层（或由 `ModelRequest` 携带 cache 断点
//! 配置）。届时本中间件即可在 `wrap_model_call` 给 `system_message` 末尾 + 长消息加 break。

use async_trait::async_trait;

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// Anthropic prompt caching 中间件（当前 no-op，详见模块级文档的 fact 查证结论）。
///
/// 对应 deepagents `AnthropicPromptCachingMiddleware`。因 juncture `ChatAnthropic` 当前
/// 不支持 `cache_control` marker（模块级文档已查证），本中间件的 `wrap_model_call` 为
/// 空实现，不改 `req` / `state`。待 juncture 上游支持后，此处将给 `system_message` 末尾
/// 与长消息加 `cache_control: { type: "ephemeral" }` break。
///
/// 对非 Anthropic provider no-op（deepagents 原行为）；当前恒 no-op 已覆盖该语义。
///
/// 单元 struct：无字段无配置。真实实现落地时若需 cache 断点阈值等配置，届时再加字段——
/// 现在加只会是 dead code（clippy `field_reassigned_with_no_reason` / 无用字段告警）。
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicPromptCachingMiddleware;

impl AnthropicPromptCachingMiddleware {
    /// 构造（单元 struct，无参数）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Middleware for AnthropicPromptCachingMiddleware {
    /// no-op：juncture `ChatAnthropic` 不支持 `cache_control`，prompt caching defer 到上游支持后。
    ///
    /// 真实实现将给 `req.system_message` 末尾 + `state.messages` 中的长消息加 `cache_control`
    /// break（标记可缓存前缀）。当前 `ModelRequest.system_message` 是 `String`、`Message`/
    /// `Content` 不带 `cache_control` 字段，且 `ChatAnthropic::invoke` 内部序列化不透传该
    /// marker——中间件层无路径注入，故空实现。详见模块级文档。
    async fn wrap_model_call(
        &self,
        _req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_smoke() {
        let mw = AnthropicPromptCachingMiddleware::new();
        let s = format!("{mw:?}");
        assert!(s.contains("AnthropicPromptCachingMiddleware"));
    }

    #[tokio::test]
    async fn wrap_model_call_is_noop() {
        let mw = AnthropicPromptCachingMiddleware::new();
        let mut state = DeepAgentState {
            messages: vec![juncture::Message::human("hello")],
        };
        let mut req = ModelRequest {
            tools: vec![],
            system_message: "system".to_string(),
            options: juncture::llm::CallOptions::default(),
        };
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        // no-op：system_message / messages 原样不变。
        assert_eq!(req.system_message, "system");
        assert_eq!(state.messages.len(), 1);
    }
}
