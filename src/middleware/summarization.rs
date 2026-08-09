//! `SummarizationMiddleware` —— 超过阈值时把旧消息折叠成一条 AI 摘要，保留最近窗口。
//!
//! 对应 deepagents `SummarizationMiddleware`：每次 LLM 调用前（`wrap_model_call`）
//! 检查 `state.messages.len() > max_messages`，若超出：
//! 1. split 成 `old = messages[..len-max]`（待摘要）/ `recent = messages[len-max..]`（原样保留）
//! 2. 用单独的 `ChatModel` 调用把 `old` 摘要成一段文本
//! 3. `state.messages = [Message::ai(summary)] ++ recent`（直接整体替换）
//!
//! `messages` 永远从 `state` 读（见 `ModelRequest` 注释），故本中间件不碰 `req`，
//! 只改 `state.messages`。
//!
//! # 摘要失败的 fallback
//!
//! 摘要模型调用失败时不阻断 agent 主循环：回退到一条占位 AI 消息
//! `"[Summary unavailable: <error>]"`，结构（摘要 + recent）保持不变，上下文仍被折叠。
//! deepagents 的 `ContextOverflow` 回退 / backend markdown offload 已 defer。
//!
//! # 跨文件协调点（需主会话改 `src/graph.rs`）
//!
//! `wrap_model_call` 在本地 `state` clone 上整体替换了 `state.messages`，但当前
//! `graph.rs` 的 agent node 只返回 `DeepAgentStateUpdate { messages: Some(vec![response]) }`
//! （靠 `messages_reducer` 追加 response）——**不反映** summarization 的替换，被折叠的
//! 旧消息在真实图状态里仍然存在。主会话需改 agent node：检测 wrap 前后 `state.messages`
//! 的变化，变化时返回 `remove_all()` 哨兵 + 全量新消息以整体替换（见本文件末尾返回说明）。

use std::fmt::{self, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use juncture::llm::{ChatModel, Message};
use juncture::state::messages::Role;

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 默认阈值：消息数超过此值即触发摘要（对齐 deepagents `max_messages` 默认 50）。
pub const DEFAULT_MAX_MESSAGES: usize = 50;

/// 默认摘要提示（对应 deepagents `DEEPAGENTS_DEFAULT_SUMMARY_PROMPT` 风格）。
pub const DEFAULT_SUMMARY_PROMPT: &str =
    "Summarize the conversation so far. Distill the prior messages into a concise recap that \
     preserves key context, decisions, unresolved questions, and any in-flight work, so the agent \
     can continue effectively.";

/// 摘要中间件：超阈值时折叠旧消息为一条 AI 摘要，保留最近窗口。
///
/// 泛型 `M: ChatModel` 用于内部摘要调用；实现的是非泛型 `Middleware` trait
/// （`impl<M: ChatModel> Middleware for SummarizationMiddleware<M>`）。
/// 持 `Arc<M>` 而非 `M`，避免要求 `M: Clone`（`ChatModel` 虽有 `Clone` supertrait，
/// 但 `Arc` 共享一份更省）。
pub struct SummarizationMiddleware<M: ChatModel> {
    model: Arc<M>,
    max_messages: usize,
    summarize_prompt: String,
}

impl<M: ChatModel> SummarizationMiddleware<M> {
    /// 以摘要模型 + 触发阈值构造，摘要提示取 [`DEFAULT_SUMMARY_PROMPT`]。
    #[must_use]
    pub fn new(model: Arc<M>, max_messages: usize) -> Self {
        Self {
            model,
            max_messages,
            summarize_prompt: DEFAULT_SUMMARY_PROMPT.to_string(),
        }
    }

    /// 覆盖摘要提示（builder 风格）。
    #[must_use]
    pub fn with_summarize_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.summarize_prompt = prompt.into();
        self
    }

    /// 触发阈值（只读访问）。
    #[must_use]
    pub const fn threshold(&self) -> usize {
        self.max_messages
    }
}

// 手动 Debug：`ChatModel` 无 `Debug` supertrait，不能 derive。用 `model_name()` 展示模型。
impl<M: ChatModel> fmt::Debug for SummarizationMiddleware<M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SummarizationMiddleware")
            .field("model", &self.model.model_name())
            .field("max_messages", &self.max_messages)
            .field("summarize_prompt_len", &self.summarize_prompt.len())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<M: ChatModel> Middleware for SummarizationMiddleware<M> {
    /// 每次 LLM 调用前：超阈值则把旧消息折叠为一条 AI 摘要，整体替换 `state.messages`。
    /// 不改 `req`（messages 永远从 `state` 读）。
    async fn wrap_model_call(
        &self,
        _req: &mut ModelRequest,
        state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 未超阈值：no-op。
        if state.messages.len() <= self.max_messages {
            return Ok(());
        }

        // 1. split：recent 保留最后 max_messages 条，old 为待摘要的前缀。
        let split = state.messages.len() - self.max_messages;
        let old = &state.messages[..split];
        let recent: Vec<Message> = state.messages[split..].to_vec();

        // 2. 构造摘要请求：system(摘要指令) + human(逐条 "{role}: {content}" 拼接的旧对话)。
        let old_text = old
            .iter()
            .map(message_to_line)
            .collect::<Vec<_>>()
            .join("\n");
        let summary_messages = vec![
            Message::system(self.summarize_prompt.as_str()),
            Message::human(old_text),
        ];

        // 3. 调摘要模型；失败时回退占位摘要（不阻断 agent 主循环，结构保持摘要 + recent）。
        let summary_text = match self.model.invoke(&summary_messages, None).await {
            Ok(m) => m.content_text().to_string(),
            Err(e) => format!("[Summary unavailable: {e}]"),
        };

        // 4. 整体替换 state.messages = [ai(summary)] ++ recent。
        let mut new_messages = Vec::with_capacity(recent.len() + 1);
        new_messages.push(Message::ai(summary_text));
        new_messages.extend(recent);
        state.messages = new_messages;

        Ok(())
    }
}

/// 把一条消息格式化为摘要输入的一行 `"{role}: {content_text}"`。
///
/// `content_text()` 对 `MultiPart` 取首个文本 part（无则空串）——MVP 简化，
/// 深度多模态对话的更细致拼装 defer。
fn message_to_line(msg: &Message) -> String {
    let role = match &msg.role {
        Role::System => "system",
        Role::Human => "human",
        Role::Ai => "ai",
        Role::Tool => "tool",
    };
    format!("{role}: {}", msg.content_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use juncture::llm::MockChatModel;

    fn make_messages(n: usize) -> Vec<Message> {
        (0..n).map(|i| Message::human(format!("msg {i}"))).collect()
    }

    #[test]
    fn debug_smoke() {
        let model = MockChatModel::new("gpt-4");
        let mw = SummarizationMiddleware::new(Arc::new(model), 10);
        let s = format!("{mw:?}");
        assert!(s.contains("SummarizationMiddleware"));
        assert!(s.contains("max_messages"));
    }

    #[test]
    fn threshold_and_builder() {
        let model = MockChatModel::new("gpt-4");
        let mw = SummarizationMiddleware::new(Arc::new(model), 7)
            .with_summarize_prompt("custom prompt");
        assert_eq!(mw.threshold(), 7);
        assert_eq!(mw.summarize_prompt, "custom prompt");
    }

    #[tokio::test]
    async fn no_op_under_threshold() {
        let model = MockChatModel::new("gpt-4").with_response("summary");
        let mw = SummarizationMiddleware::new(Arc::new(model), 5);
        let mut state = DeepAgentState {
            messages: make_messages(5),
        };
        let mut req = ModelRequest {
            tools: vec![],
            system_message: String::new(),
            options: juncture::llm::CallOptions::default(),
        };
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        // 未超阈值：消息不变。
        assert_eq!(state.messages.len(), 5);
    }

    #[tokio::test]
    async fn summarizes_over_threshold() {
        let model = MockChatModel::new("gpt-4").with_response("-condensed summary-");
        let mw = SummarizationMiddleware::new(Arc::new(model), 3);
        let mut state = DeepAgentState {
            messages: make_messages(6),
        };
        let mut req = ModelRequest {
            tools: vec![],
            system_message: String::new(),
            options: juncture::llm::CallOptions::default(),
        };
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        // 结果 = 1 条 AI 摘要 + recent(3) = 4。
        assert_eq!(state.messages.len(), 4);
        assert_eq!(state.messages[0].content_text(), "-condensed summary-");
        // recent 是最后 3 条原消息（msg 3/4/5）。
        assert_eq!(state.messages[1].content_text(), "msg 3");
        assert_eq!(state.messages[3].content_text(), "msg 5");
    }
}
