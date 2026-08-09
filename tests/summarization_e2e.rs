//! SummarizationMiddleware 端到端集成测试 —— 验证 graph.rs agent node step 7 的
//! `remove_all()` 哨兵 + `messages_reducer` 整体替换在真实 ReAct loop 里生效。
//!
//! # 被测整合点
//!
//! `SummarizationMiddleware.wrap_model_call` 折叠旧消息时只改了 agent node 的本地
//! `state` clone（`state.messages = [ai(summary)] ++ recent`）。只有 graph.rs step 7
//! 返回 `[remove_all(), ...state.messages, response]`，`messages_reducer` 命中
//! `REMOVE_ALL_MESSAGES` 先 clear 再 append 全集，才能把折叠结果传播到图状态。
//!
//! 若 `remove_all` 哨兵不工作（step 7 只返回 `[response]`），reducer 仅追加 response：
//! - 旧消息仍在（未清除）
//! - summary 丢失（只在本地 clone，未传播）
//! - messages 无限增长
//!
//! 本测试用真实 `DeepAgentBuilder` + `SummarizationMiddleware` + juncture `StateGraph`
//! runtime 驱动，证明折叠生效。
//!
//! # ScriptedModel 摘要轮 vs 主对话轮区分
//!
//! `SummarizationMiddleware` 构造摘要请求 `[system(summary_prompt), human(old_text)]`，
//! 首条消息是 System 且 content 含 "Summarize"。主 agent 调用（graph.rs step 4-5）不设
//! system_prompt 时无 system 前缀，首条消息来自 `state.messages[0]`（Ai/Human/Tool）。
//! 模型据此区分两种调用：摘要轮返回 `"[summary]"`，主对话轮返回预设脚本。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{DeepAgentBuilder, DeepAgentState, SummarizationMiddleware};
use juncture::llm::{CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition};
use juncture::state::messages::ToolCall;
use juncture::tools::{Tool, ToolError};
use juncture::RunnableConfig;
use serde_json::{json, Value};

// ============================================================================
// ScriptedModel —— 计数 invoke + 区分摘要轮 vs 主对话轮
// ============================================================================

/// 测试模型：所有 clone（主 agent + 摘要中间件）共享 `Arc` 计数器。
///
/// - 摘要轮（首条消息 = System 且含 "Summarize"）→ 返回 `Message::ai("[summary]")`
/// - 主对话轮 → 按序返回 `main_turns` 脚本（content + tool_calls），越界取最后一条
#[derive(Clone)]
struct SummarizationScriptedModel {
    /// 主对话轮脚本：(content, tool_calls)。越界返回最后一条（防 panic）。
    main_turns: Arc<Vec<(String, Vec<ToolCall>)>>,
    /// 主对话轮 index（仅主对话轮递增，摘要轮不递增）。
    main_index: Arc<AtomicUsize>,
    /// 总 invoke 调用数。
    invoke_count: Arc<AtomicUsize>,
    /// 摘要调用数。
    summary_count: Arc<AtomicUsize>,
    /// 每次 invoke 收到的首条消息 role（断言调用顺序用）。
    first_roles: Arc<Mutex<Vec<String>>>,
}

impl SummarizationScriptedModel {
    fn new(main_turns: Vec<(String, Vec<ToolCall>)>) -> Self {
        Self {
            main_turns: Arc::new(main_turns),
            main_index: Arc::new(AtomicUsize::new(0)),
            invoke_count: Arc::new(AtomicUsize::new(0)),
            summary_count: Arc::new(AtomicUsize::new(0)),
            first_roles: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn invoke_count(&self) -> usize {
        self.invoke_count.load(Ordering::Relaxed)
    }

    fn summary_count(&self) -> usize {
        self.summary_count.load(Ordering::Relaxed)
    }

    fn first_roles(&self) -> Vec<String> {
        self.first_roles.lock().expect("lock").clone()
    }

    /// 判定是否为摘要调用：首条消息是 System 且 content 含 "Summarize"
    /// （对齐 `DEFAULT_SUMMARY_PROMPT` 开头 "Summarize the conversation so far."）。
    fn is_summary_call(messages: &[Message]) -> bool {
        matches!(
            messages.first(),
            Some(m) if matches!(m.role, Role::System) && m.content_text().contains("Summarize")
        )
    }

    /// 取下一轮主对话脚本（越界取最后一条）。
    fn next_main_turn(&self) -> (String, Vec<ToolCall>) {
        let i = self.main_index.fetch_add(1, Ordering::Relaxed);
        self.main_turns
            .get(i)
            .or_else(|| self.main_turns.last())
            .cloned()
            .unwrap_or_default()
    }

    fn first_role_str(msg: Option<&Message>) -> &'static str {
        match msg {
            Some(m) => match m.role {
                Role::System => "system",
                Role::Human => "human",
                Role::Ai => "ai",
                Role::Tool => "tool",
            },
            None => "none",
        }
    }
}

#[async_trait]
impl ChatModel for SummarizationScriptedModel {
    async fn invoke(
        &self,
        messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        self.invoke_count.fetch_add(1, Ordering::Relaxed);

        // 记录首条消息 role（断言调用顺序）。
        let role = Self::first_role_str(messages.first());
        self.first_roles.lock().expect("lock").push(role.to_string());

        if Self::is_summary_call(messages) {
            // 摘要轮：返回 "[summary]"。（与 recent 拼装成 [ai("[summary]"), ...recent]）
            self.summary_count.fetch_add(1, Ordering::Relaxed);
            Ok(Message::ai("[summary]"))
        } else {
            // 主对话轮：按序返回脚本。
            let (content, calls) = self.next_main_turn();
            Ok(Message::ai_with_tool_calls(&content, calls))
        }
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<juncture::llm::BoxStream<'_, Result<MessageChunk, LlmError>>, LlmError> {
        let chunk = MessageChunk {
            content: String::new(),
            tool_call_chunks: Vec::new(),
            usage_delta: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    fn bind_tools(&self, _tools: Vec<ToolDefinition>) -> Self {
        // 测试模型不依赖 tool definitions（脚本化返回），直接 clone。
        self.clone()
    }

    fn model_name(&self) -> &str {
        "summarization-scripted"
    }
}

// ============================================================================
// EchoTool —— 多轮 ReAct loop 测试用的桩工具
// ============================================================================

/// 桩工具：回显 `text` 参数，用于多轮 loop 累积 messages 直到触发摘要。
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echo back the text argument"
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"text":{"type":"string"}}})
    }

    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("echo");
        Ok(format!("echoed: {text}"))
    }
}

// ============================================================================
// Test 1: 预种 messages —— 直接验证 remove_all + summarization 整合
// ============================================================================

/// 预种 6 条 human 消息，max_messages=3。首轮 wrap_model_call 即触发摘要。
///
/// 预期流：
/// 1. wrap_model_call: 6 > 3 → 摘要调用(#1, system首条) → "[summary]"
///    state.messages = [ai("[summary]"), h3, h4, h5] (1+3=4)
/// 2. 主调用(#2, ai首条) → "done"（无 tool_calls）→ END
/// 3. step 7 返回 [remove_all, ai("[summary]"), h3, h4, h5, ai("done")]
/// 4. reducer: clear → append 全集 → [ai("[summary]"), h3, h4, h5, ai("done")] = 5
///
/// 若 remove_all 不工作（step 7 只返回 [response]）：
/// reducer 仅追加 → [h0..h5, ai("done")] = 7；旧消息 h0/h1/h2 仍在，summary 丢失。
#[tokio::test]
async fn summarization_folds_old_messages_via_remove_all_sentinel() {
    let model = SummarizationScriptedModel::new(vec![
        // 唯一的主对话轮：done，无 tool_calls → 路由 END。
        ("done".to_string(), vec![]),
    ]);

    // 主 agent 与摘要中间件共用同一 model（clone 共享 Arc 计数器）。
    // 不设 system_prompt → 主对话调用无 system 前缀 → 首条消息非 System → 与摘要轮可区分。
    let agent = DeepAgentBuilder::new(model.clone())
        .middleware_one(SummarizationMiddleware::new(
            Arc::new(model.clone()),
            3,
        ))
        .build()
        .expect("agent builds");

    // 预种 6 条 human 消息（msg 0..5），max_messages=3 → 首轮即触发摘要。
    let state = DeepAgentState {
        messages: (0..6).map(|i| Message::human(format!("msg {i}"))).collect(),
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let msgs = &out.value.messages;

    // 断言1：invoke 计数 = 1 摘要 + 1 主对话 = 2。
    assert_eq!(
        model.invoke_count(),
        2,
        "expected 1 summary + 1 main invoke, got {}",
        model.invoke_count()
    );
    assert_eq!(model.summary_count(), 1, "exactly 1 summary call");

    // 断言2：调用顺序 —— 首次 system（摘要），第二次 ai（主对话，state.messages[0]=summary）。
    let roles = model.first_roles();
    assert_eq!(
        roles,
        vec!["system", "ai"],
        "invoke order should be [summary(system), main(ai)]: {roles:?}"
    );

    // 断言3（核心）：summary 消息在最终 state。
    // 若 remove_all 不工作，summary 只在本地 clone，不会出现在图状态。
    let has_summary = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Ai) && m.content_text() == "[summary]");
    assert!(
        has_summary,
        "summary message should be in final state (remove_all propagated it): {:?}",
        debug_msgs(msgs)
    );

    // 断言4（核心）：旧消息（msg 0/1/2）已被 remove_all 清除。
    // 若 remove_all 不工作，它们仍在（reducer 仅追加）。
    for i in 0..3 {
        let old = format!("msg {i}");
        let present = msgs.iter().any(|m| m.content_text() == old);
        assert!(
            !present,
            "old message '{old}' should be removed by remove_all sentinel: {:?}",
            debug_msgs(msgs)
        );
    }

    // 断言5：recent 窗口（msg 3/4/5）保留。
    for i in 3..6 {
        let recent = format!("msg {i}");
        let present = msgs.iter().any(|m| m.content_text() == recent);
        assert!(
            present,
            "recent message '{recent}' should be preserved: {:?}",
            debug_msgs(msgs)
        );
    }

    // 断言6：最终回复 "done" 在 state。
    let has_done = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Ai) && m.content_text() == "done");
    assert!(
        has_done,
        "final 'done' response should be present: {:?}",
        debug_msgs(msgs)
    );

    // 断言7（核心）：最终长度 = summary(1) + recent(3) + response(1) = 5。
    // 若 remove_all 不工作 → 6 原始 + 1 response = 7。
    assert_eq!(
        msgs.len(),
        5,
        "final length should be 5 (summary + 3 recent + done), got {}: {:?}",
        msgs.len(),
        debug_msgs(msgs)
    );

    // 断言8：首条消息 = summary（remove_all 清空后 summary 是新的第一条）。
    assert!(
        matches!(msgs[0].role, Role::Ai),
        "first message should be AI summary: {:?}",
        debug_msgs(msgs)
    );
    assert_eq!(msgs[0].content_text(), "[summary]");
}

// ============================================================================
// Test 2: 多轮 ReAct loop —— 摘要在 loop 中途触发
// ============================================================================

/// 从 1 条 human 消息起步，用 EchoTool 累积 2 轮 tool_call，当 messages > 3 时摘要触发。
///
/// 预期流：
/// - Turn 1: 1 msg ≤ 3 → 主调用#1(human首条) → tool_call echo
///   tools 执行 → [h, ai(tc1), tool1] = 3
/// - Turn 2: 3 ≤ 3 → 主调用#2(human首条) → tool_call echo
///   tools 执行 → [h, ai(tc1), tool1, ai(tc2), tool2] = 5
/// - Turn 3: 5 > 3 → 摘要调用#3(system首条) → "[summary]"
///   state = [ai("[summary]"), tool1, ai(tc2), tool2] = 4
///   主调用#4(ai首条) → "done" → END
///   step 7: [remove_all, ai(summary), tool1, ai(tc2), tool2, ai("done")]
///   reducer → 5 条
///
/// invoke 顺序: [main(human), main(human), summary(system), main(ai)] = 4 次
#[tokio::test]
async fn summarization_triggers_during_multi_turn_react_loop() {
    let echo: Box<dyn Tool> = Box::new(EchoTool);

    let model = SummarizationScriptedModel::new(vec![
        // 主对话轮1: tool_call echo("alpha")
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: json!({"text":"alpha"}),
            }],
        ),
        // 主对话轮2: tool_call echo("beta")
        (
            String::new(),
            vec![ToolCall {
                id: "c2".into(),
                name: "echo".into(),
                arguments: json!({"text":"beta"}),
            }],
        ),
        // 主对话轮3: done（摘要后）→ END
        ("done".to_string(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model.clone())
        .tool(echo)
        .middleware_one(SummarizationMiddleware::new(
            Arc::new(model.clone()),
            3,
        ))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("start")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let msgs = &out.value.messages;

    // 断言1：invoke = 3 主对话 + 1 摘要 = 4。
    assert_eq!(
        model.invoke_count(),
        4,
        "expected 3 main + 1 summary invoke, got {}",
        model.invoke_count()
    );
    assert_eq!(model.summary_count(), 1, "exactly 1 summary call");

    // 断言2：调用顺序 [main(human), main(human), summary(system), main(ai)]。
    let roles = model.first_roles();
    assert_eq!(
        roles,
        vec!["human", "human", "system", "ai"],
        "invoke order should be [main, main, summary, main]: {roles:?}"
    );

    // 断言3（核心）：summary 在最终 state。
    let has_summary = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Ai) && m.content_text() == "[summary]");
    assert!(
        has_summary,
        "summary should be in final state: {:?}",
        debug_msgs(msgs)
    );

    // 断言4（核心）：原始 "start" 消息已被折叠进 summary（不在最终 state）。
    let has_start = msgs.iter().any(|m| m.content_text() == "start");
    assert!(
        !has_start,
        "original 'start' should be folded into summary (removed by remove_all): {:?}",
        debug_msgs(msgs)
    );

    // 断言5：第一轮 AI tool_call 消息(ai(tc1)) 也被折叠（不在最终 state）。
    // old = [human("start"), ai(tc1)] → 折叠进 summary。
    let has_tc1 = msgs
        .iter()
        .any(|m| m.tool_calls.iter().any(|tc| tc.id == "c1"));
    assert!(
        !has_tc1,
        "first tool_call (c1) should be folded into summary: {:?}",
        debug_msgs(msgs)
    );

    // 断言6：recent 窗口保留 —— tool1, ai(tc2), tool2 在最终 state。
    let has_tool1 = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("alpha"));
    assert!(has_tool1, "tool1 result (alpha) should be preserved");
    let has_tc2 = msgs
        .iter()
        .any(|m| m.tool_calls.iter().any(|tc| tc.id == "c2"));
    assert!(has_tc2, "second tool_call (c2) should be preserved");
    let has_tool2 = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("beta"));
    assert!(has_tool2, "tool2 result (beta) should be preserved");

    // 断言7："done" 在最终 state。
    let has_done = msgs
        .iter()
        .any(|m| matches!(m.role, Role::Ai) && m.content_text() == "done");
    assert!(
        has_done,
        "final 'done' should be present: {:?}",
        debug_msgs(msgs)
    );

    // 断言8（核心）：最终长度 = summary(1) + recent(3) + done(1) = 5。
    // 若 remove_all 不工作 → start + ai(tc1) + tool1 + ai(tc2) + tool2 + done = 6。
    assert_eq!(
        msgs.len(),
        5,
        "final length should be 5 (summary + 3 recent + done), got {}: {:?}",
        msgs.len(),
        debug_msgs(msgs)
    );
}

// ============================================================================
// Test 3: 未超阈值 no-op —— remove_all 机制不破坏正常状态
// ============================================================================

/// 2 条消息，max_messages=5。未超阈值 → 摘要不触发。
/// 验证 step 7 的 remove_all + state.messages 在无摘要时仍正确传播
/// （clear 旧 → append 旧 + response，长度 = 原始 + 1）。
#[tokio::test]
async fn summarization_noop_under_threshold_preserves_all_messages() {
    let model = SummarizationScriptedModel::new(vec![
        // 唯一主对话轮：done，无 tool_calls → END。
        ("done".to_string(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model.clone())
        .middleware_one(SummarizationMiddleware::new(
            Arc::new(model.clone()),
            5,
        ))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hello"), Message::ai("hi there")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let msgs = &out.value.messages;

    // 未超阈值 → 无摘要调用，仅 1 次主对话 invoke。
    assert_eq!(model.invoke_count(), 1, "only 1 main invoke, no summary");
    assert_eq!(model.summary_count(), 0, "no summary call under threshold");

    // 调用首条 = human（state.messages[0]，无 system 前缀）。
    let roles = model.first_roles();
    assert_eq!(roles, vec!["human"], "single main call, human first: {roles:?}");

    // 原始消息全保留 + response。
    assert_eq!(msgs.len(), 3, "2 original + 1 response = 3");
    assert_eq!(msgs[0].content_text(), "hello");
    assert_eq!(msgs[1].content_text(), "hi there");
    assert_eq!(msgs[2].content_text(), "done");

    // 无 summary 消息。
    let has_summary = msgs.iter().any(|m| m.content_text() == "[summary]");
    assert!(
        !has_summary,
        "no summary message when under threshold: {:?}",
        debug_msgs(msgs)
    );
}

// ============================================================================
// Test 4: 反证 —— 无 SummarizationMiddleware 时 messages 不折叠
// ============================================================================

/// 对比测试：同样的 6 条预种消息，但不加 SummarizationMiddleware。
/// 证明 Test 1 的折叠效果来自中间件 + remove_all，而非其他机制。
/// 最终 state 应保留全部 6 条 + response = 7（无折叠）。
#[tokio::test]
async fn without_summarization_messages_grow_unbounded() {
    let model = SummarizationScriptedModel::new(vec![
        // 唯一主对话轮：done → END。
        ("done".to_string(), vec![]),
    ]);

    // 无 SummarizationMiddleware —— 纯裸 agent。
    let agent = DeepAgentBuilder::new(model.clone())
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: (0..6).map(|i| Message::human(format!("msg {i}"))).collect(),
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let msgs = &out.value.messages;

    // 无摘要中间件 → 0 摘要调用，1 主对话调用。
    assert_eq!(model.invoke_count(), 1, "only 1 main invoke, no summary");
    assert_eq!(model.summary_count(), 0);

    // 全部 6 条消息保留 + response = 7（无折叠）。
    assert_eq!(
        msgs.len(),
        7,
        "without summarization: 6 original + 1 response = 7, got {}",
        msgs.len()
    );

    // 旧消息仍存在（对比 Test 1 的断言4）。
    for i in 0..6 {
        let old = format!("msg {i}");
        let present = msgs.iter().any(|m| m.content_text() == old);
        assert!(present, "msg {i} should still be present without summarization");
    }

    // 无 summary 消息。
    let has_summary = msgs.iter().any(|m| m.content_text() == "[summary]");
    assert!(!has_summary, "no summary without SummarizationMiddleware");
}

// ============================================================================
// 辅助：调试输出
// ============================================================================

/// 格式化 messages 为 (role, content) 列表，用于断言失败时的 debug 输出。
fn debug_msgs(msgs: &[Message]) -> Vec<(String, String)> {
    msgs.iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::Human => "human",
                Role::Ai => "ai",
                Role::Tool => "tool",
            };
            (role.to_string(), m.content_text().to_string())
        })
        .collect()
}
