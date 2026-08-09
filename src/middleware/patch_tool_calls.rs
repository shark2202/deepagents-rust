//! `PatchToolCallsMiddleware` —— 修复 dangling tool calls（AI 消息含 tool_calls 但无对应
//! Tool-role 结果），避免 Pregel `interrupt!` 后 resume 时 ToolNode 重复执行或 LLM 校验失败。
//!
//! 对应 deepagents `PatchToolCallsMiddleware`：
//! - `before_agent`：检查 `state.messages` 末尾 AI 消息是否含未应答 tool_calls，
//!   为每个 dangling tool_call 追加一条 synthetic Tool 结果消息
//!   `"Error: tool call was interrupted"`（对齐 deepagents 默认 error message）。
//!
//! # 选型：追加 synthetic Tool result
//!
//! deepagents 允许两种修法：移除 AI 消息的 tool_calls（让 loop 正常终止），或为每个
//! dangling tool_call 追加一条 synthetic Tool result。本实现选后者——更安全，使
//! `ToolNode` 不会重复执行该 tool_call，并让对话历史满足 "每条 tool_call 必有对应
//! Tool 结果" 的不变量（部分 provider/校验逻辑强依赖此不变量）。
//!
//! # 简化
//!
//! 仅处理**末尾** dangling（最后一条消息为 AI 且含未应答 tool_calls），不扫描全历史
//! 修复中段 dangling（主会话接受）。构建 "已应答 id" 集合时仍遍历全历史，以正确判断
//! 末尾 AI 消息中哪些 tool_call 已有结果、哪些真正 dangling。

use std::collections::HashSet;

use async_trait::async_trait;
use juncture::llm::{Message, Role};

use crate::middleware::{Middleware, MiddlewareError};
use crate::state::DeepAgentState;

/// 修复 dangling tool calls 的中间件（单元 struct，无字段无配置）。
///
/// 在 agent run 开始时（`before_agent`）检查 `state.messages` 末尾 AI 消息是否含
/// 未应答 tool_calls（典型场景：Pregel `interrupt!` 后 resume，末尾 AI 消息的
/// tool_calls 尚未执行）。为每个 dangling tool_call 追加一条 Tool-role synthetic
/// 结果 `"Error: tool call was interrupted"`，使 `ToolNode` 不重复执行该调用并让
/// 历史满足 "每条 tool_call 必有对应 Tool 结果" 的不变量。
#[derive(Debug, Default, Clone, Copy)]
pub struct PatchToolCallsMiddleware;

impl PatchToolCallsMiddleware {
    /// 构造（单元 struct，无参数）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// 追加给 dangling tool_call 的 synthetic 错误结果内容（对齐 deepagents 默认）。
const INTERRUPTED_MSG: &str = "Error: tool call was interrupted";

#[async_trait]
impl Middleware for PatchToolCallsMiddleware {
    /// 修末尾 dangling tool calls：为末尾 AI 消息中未应答的 tool_call 追加 synthetic
    /// Tool result 消息。
    async fn before_agent(&self, state: &mut DeepAgentState) -> Result<(), MiddlewareError> {
        patch_dangling_tool_calls(&mut state.messages);
        Ok(())
    }
}

/// 修复末尾 dangling tool calls。
///
/// 遍历全历史构建 "已应答 tool_call id" 集合（所有 Tool-role 消息的 `tool_call_id`），
/// 然后仅检查最后一条消息：若为 AI 且含 tool_calls，对其每个 id 不在已应答集合中的
/// tool_call 追加 `Message::tool_result(id, INTERRUPTED_MSG)`。
///
/// # 简化
///
/// 不扫描中段 dangling——若最后一条消息非 AI（或无 tool_calls），即便历史中段存在
/// 未应答 tool_calls 也不处理。覆盖最常见场景（resume 时末尾 AI 消息的 tool_calls
/// 尚未执行）；中段修复 defer。
fn patch_dangling_tool_calls(messages: &mut Vec<Message>) {
    if messages.is_empty() {
        return;
    }

    // 收集所有已有 Tool 结果的 tool_call id（全历史扫描，正确判断 "已应答"）。
    let answered: HashSet<&str> = messages
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();

    // 仅处理末尾消息（简化：不扫描中段 dangling）。
    let last = messages.last().expect("non-empty checked above");
    let dangling: Vec<String> = if matches!(last.role, Role::Ai) && !last.tool_calls.is_empty() {
        last.tool_calls
            .iter()
            .filter(|tc| !answered.contains(tc.id.as_str()))
            .map(|tc| tc.id.clone())
            .collect()
    } else {
        return;
    };

    if dangling.is_empty() {
        return;
    }

    // 为每个 dangling id 追加 synthetic Tool 结果（在历史末尾，按 tool_call 出现序）。
    for id in dangling {
        messages.push(Message::tool_result(id, INTERRUPTED_MSG));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use juncture::llm::Message;
    use juncture::state::messages::ToolCall;
    use serde_json::json;

    /// 构造含给定 tool_call id 的空内容 AI 消息。
    fn ai_with_calls(ids: &[&str]) -> Message {
        let calls = ids
            .iter()
            .map(|id| ToolCall {
                id: (*id).into(),
                name: "some_tool".into(),
                arguments: json!({}),
            })
            .collect();
        Message::ai_with_tool_calls("", calls)
    }

    #[tokio::test]
    async fn appends_synthetic_results_for_trailing_dangling() {
        let mut state = DeepAgentState {
            messages: vec![Message::human("hi"), ai_with_calls(&["c1", "c2"])],
        };
        PatchToolCallsMiddleware::new()
            .before_agent(&mut state)
            .await
            .expect("before_agent ok");

        // 末尾追加两条 Tool 结果，id 对应 c1/c2，内容含 "interrupted"。
        assert_eq!(state.messages.len(), 4, "two synthetic results appended");
        assert!(matches!(state.messages[2].role, Role::Tool));
        assert_eq!(state.messages[2].tool_call_id.as_deref(), Some("c1"));
        assert!(matches!(state.messages[3].role, Role::Tool));
        assert_eq!(state.messages[3].tool_call_id.as_deref(), Some("c2"));
        assert!(
            state.messages[2].content_text().contains("interrupted"),
            "synthetic result is an interrupted-error message"
        );
    }

    #[tokio::test]
    async fn no_op_when_tool_results_already_present() {
        let mut state = DeepAgentState {
            messages: vec![
                Message::human("hi"),
                ai_with_calls(&["c1"]),
                Message::tool_result("c1", "real result"),
            ],
        };
        let len_before = state.messages.len();
        PatchToolCallsMiddleware::new()
            .before_agent(&mut state)
            .await
            .expect("before_agent ok");
        assert_eq!(state.messages.len(), len_before, "nothing appended");
    }

    #[tokio::test]
    async fn no_op_when_no_tool_calls() {
        let mut state = DeepAgentState {
            messages: vec![Message::human("hi"), Message::ai("hello")],
        };
        let len_before = state.messages.len();
        PatchToolCallsMiddleware::new()
            .before_agent(&mut state)
            .await
            .expect("before_agent ok");
        assert_eq!(state.messages.len(), len_before, "nothing appended");
    }

    #[tokio::test]
    async fn trailing_partial_dangling_only_appends_missing() {
        // 末尾 AI 消息含 c1（已有 result）+ c2（dangling）。
        let mut state = DeepAgentState {
            messages: vec![
                Message::human("hi"),
                Message::tool_result("c1", "earlier result"),
                ai_with_calls(&["c1", "c2"]),
            ],
        };
        PatchToolCallsMiddleware::new()
            .before_agent(&mut state)
            .await
            .expect("before_agent ok");

        // 仅追加 c2（c1 已应答，不重复）。
        assert_eq!(state.messages.len(), 4, "only c2 appended");
        assert!(matches!(state.messages[3].role, Role::Tool));
        assert_eq!(state.messages[3].tool_call_id.as_deref(), Some("c2"));
    }

    #[tokio::test]
    async fn no_op_when_last_message_is_not_ai() {
        // 中段 dangling（AI 非末尾）——简化下不处理，验证此 defer 边界。
        let mut state = DeepAgentState {
            messages: vec![
                Message::human("hi"),
                ai_with_calls(&["c1"]),
                Message::human("wait"),
            ],
        };
        let len_before = state.messages.len();
        PatchToolCallsMiddleware::new()
            .before_agent(&mut state)
            .await
            .expect("before_agent ok");
        assert_eq!(state.messages.len(), len_before, "mid-history dangling untouched");
    }
}
