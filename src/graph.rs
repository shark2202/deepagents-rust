//! `create_deep_agent` 工厂 —— deepagents-rust 的公共入口。
//!
//! 自建 `agent` 节点（`DeepAgentNode` 闭包式）+ `tools` 节点 + 路由器，组装成 juncture
//! `StateGraph`。这是 B 路径的核心：不依赖 juncture 的 `create_agent_with_middleware`
//! （其 `before_model` 只读、tools 预绑死、`CallOptions` 无 tools 字段，无法承载 deepagents
//! 拦截语义），而是借 juncture 的 `StateGraph`/Pregel/`Command`/`Node`/`ToolNode` runtime。

use std::sync::Arc;

use futures::future::FutureExt;
use juncture::JunctureError;
use juncture::checkpoint::CheckpointSaver;
use juncture::edge::{END, PathMap, RouteResult, Router};
use juncture::graph::{CompiledGraph, RetryPolicy, StateGraph, TopologyError};
use juncture::llm::{CallOptions, ChatModel, Message, ToolDefinition as LlmToolDefinition};
use juncture::node::NodeFnUpdate;
use juncture::tools::{Tool, ToolDefinition as ToolsToolDefinition, ToolNode};
use juncture::wasm_send::force_send;

use crate::middleware::{
    AnthropicPromptCachingMiddleware, MiddlewareChain, MiddlewareError, ModelRequest,
    PatchToolCallsMiddleware, SummarizationMiddleware,
};
use crate::profiles::{HarnessProfile, apply_profile_prompt};
use crate::state::{DeepAgentState, DeepAgentStateUpdate};

/// 把 `tools::ToolDefinition` 转为 `llm::ToolDefinition`（同字段，不同类型，见 juncture `react.rs`）。
fn convert_tool_defs(defs: &[ToolsToolDefinition]) -> Vec<LlmToolDefinition> {
    defs.iter()
        .map(|d| LlmToolDefinition {
            name: d.name.clone(),
            description: d.description.clone(),
            parameters: d.parameters.clone(),
        })
        .collect()
}

/// 把 `MiddlewareError` 转 `JunctureError`（Pregel 的 node 返回 `JunctureError`）。
fn mw_err(e: MiddlewareError) -> JunctureError {
    JunctureError::execution(e.to_string())
}

/// DeepAgents 配置（builder 消费）。
pub struct DeepAgentConfig<M: ChatModel> {
    /// LLM 模型（实现 `ChatModel` trait）。
    pub model: M,
    /// 调用方自带工具（与中间件提供的工具加性合并）。
    pub tools: Vec<Box<dyn Tool>>,
    /// 用户系统提示（USER 段；中间件会追加 BASE/SUFFIX 及各特性段）。
    pub system_prompt: Option<String>,
    /// 中间件链。
    pub middleware: MiddlewareChain,
    /// Harness profile：provider/model 维度的运行时配置（prompt 拼装 / excluded_tools /
    /// tool_description_overrides）。`None` 不施加 profile 修饰，沿用旧语义。
    /// 消费见 [`create_deep_agent`]。
    pub profile: Option<HarnessProfile>,
    /// 可选 checkpointer（不提供则纯内存、无跨调用持久化）。
    pub checkpointer: Option<Arc<dyn CheckpointSaver>>,
}

impl<M: ChatModel> DeepAgentConfig<M> {
    /// 从模型构造默认配置（无工具、无 prompt、空中间件链、无 checkpointer）。
    #[must_use]
    pub fn new(model: M) -> Self {
        Self {
            model,
            tools: Vec::new(),
            system_prompt: None,
            middleware: MiddlewareChain::new(),
            profile: None,
            checkpointer: None,
        }
    }
}

/// Builder（Rust 惯用法，对应忠实度决策 2）。
pub struct DeepAgentBuilder<M: ChatModel> {
    config: DeepAgentConfig<M>,
}

impl<M: ChatModel> DeepAgentBuilder<M> {
    /// 以模型起步。
    #[must_use]
    pub fn new(model: M) -> Self {
        Self {
            config: DeepAgentConfig::new(model),
        }
    }

    /// 设置工具（替换）。
    #[must_use]
    pub fn tools(mut self, tools: Vec<Box<dyn Tool>>) -> Self {
        self.config.tools = tools;
        self
    }

    /// 追加单个工具。
    #[must_use]
    pub fn tool(mut self, tool: Box<dyn Tool>) -> Self {
        self.config.tools.push(tool);
        self
    }

    /// 设置系统提示。
    #[must_use]
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(prompt.into());
        self
    }

    /// 设置中间件链（替换）。
    #[must_use]
    pub fn middleware(mut self, chain: MiddlewareChain) -> Self {
        self.config.middleware = chain;
        self
    }

    /// 追加单个中间件。
    #[must_use]
    pub fn middleware_one(mut self, m: impl crate::middleware::Middleware + 'static) -> Self {
        self.config.middleware = self.config.middleware.with(m);
        self
    }

    /// 套 deepagents 默认 always-on 中间件栈（`Summarization` + `PatchToolCalls` + `PromptCaching`）。
    ///
    /// 对齐 deepagents `create_deep_agent` 默认 14 层栈的 always-on 子集。`Filesystem` 需 backend，
    /// 用 `.middleware_one(FilesystemMiddleware::new(backend))` 单独加。deepagents 自动套；
    /// 此处用显式方法（Rust 惯用法——避免空 middleware 测试被意外破坏，让默认行为可见可控）。
    ///
    /// 顺序：Summarization（outer）→ PatchToolCalls → PromptCaching（inner）。
    /// wrap_model_call 正向执行：Summarization 先折叠 messages，再 Patch/Cache。
    #[must_use]
    pub fn with_default_middleware(mut self) -> Self {
        let model_arc = Arc::new(self.config.model.clone());
        self.config.middleware = self
            .config
            .middleware
            .with(SummarizationMiddleware::new(model_arc, 50))
            .with(PatchToolCallsMiddleware)
            .with(AnthropicPromptCachingMiddleware);
        self
    }

    /// 设置 harness profile（provider/model 维度的 prompt 拼装 / 工具过滤 / 描述覆写）。
    /// 对应 deepagents `create_deep_agent` 的 `profile` 参数。
    #[must_use]
    pub fn profile(mut self, p: HarnessProfile) -> Self {
        self.config.profile = Some(p);
        self
    }

    /// 设置 checkpointer。
    #[must_use]
    pub fn checkpointer(mut self, saver: Arc<dyn CheckpointSaver>) -> Self {
        self.config.checkpointer = Some(saver);
        self
    }

    /// 组装为 juncture `CompiledGraph`。
    ///
    /// # Errors
    ///
    /// 返回 `TopologyError` 若图无法编译（如节点名冲突）。
    pub fn build(self) -> Result<CompiledGraph<DeepAgentState>, TopologyError> {
        create_deep_agent(self.config)
    }
}

/// 构造一个 Deep Agent（ReAct loop：`agent → [router] → tools → agent`）。
///
/// `agent` 节点：跑中间件链的 `before_agent`/`wrap_model_call`/`after_model_call`，
/// 每轮用过滤后的 `req.tools` 重新 `model.bind_tools()`，messages 取 `state.messages` 最新。
///
/// `tools` 节点：复用 juncture `ToolNode` 执行工具调用。
///
/// 路由：末消息有 tool_calls → `tools`，否则 END。
///
/// # Errors
///
/// 返回 `TopologyError` 若图无法编译。
#[allow(clippy::needless_pass_by_value, reason = "config 所有权转入图")]
pub fn create_deep_agent<M: ChatModel>(
    config: DeepAgentConfig<M>,
) -> Result<CompiledGraph<DeepAgentState>, TopologyError> {
    let DeepAgentConfig {
        model,
        tools,
        system_prompt,
        middleware,
        profile,
        checkpointer,
    } = config;

    // 合并 caller tools + 各 middleware 提供的工具（如 FilesystemMiddleware 的 8 个 fs 工具）。
    // 加性合并，对齐 deepagents：中间件提供工具 + caller 自带工具共存。
    // 若 profile 设了 excluded_tools，按 name 过滤掉排除项（caller 与 middleware 工具都受影响）。
    let all_tools: Vec<Box<dyn Tool>> = tools
        .into_iter()
        .chain(middleware.iter().flat_map(|m| m.tools()))
        .filter(|t| !profile.as_ref().is_some_and(|p| p.excludes_tool(t.name())))
        .collect();

    // 全量工具定义（llm 格式）。中间件 wrap_model_call 在此基础上 per-call 过滤。
    let all_tool_defs: Vec<ToolsToolDefinition> =
        all_tools.iter().map(|t| t.definition()).collect();
    // 转为 llm 格式，并按 profile.tool_description_overrides 覆写 description（仅影响 LLM 可见描述）。
    let mut all_llm_tool_defs = convert_tool_defs(&all_tool_defs);
    if let Some(p) = &profile {
        for d in &mut all_llm_tool_defs {
            if let Some(desc) = p.tool_description_overrides.get(d.name.as_str()) {
                d.description = desc.clone();
            }
        }
    }
    // 注：profile.excluded_middleware 过滤 defer —— 需 `Middleware` trait 暴露稳定 name
    // （当前 trait 无 `fn name()`），留待主会话决定命名方案后再接。

    let model = Arc::new(model);
    let tool_defs_for_closure = all_llm_tool_defs.clone();
    let system_prompt_for_closure = system_prompt;
    // profile 进 agent node 闭包：构造 ModelRequest.system_message 初值（USER+BASE+SUFFIX 拼接）。
    let profile_for_closure = profile;
    let middleware_for_agent = middleware.clone();

    // agent 节点：闭包式。每次调用 clone 必要状态（Future 需 'static）。
    let agent_node = NodeFnUpdate(move |state: &DeepAgentState| {
        let model = Arc::clone(&model);
        let tool_defs = tool_defs_for_closure.clone();
        let system_prompt = system_prompt_for_closure.clone();
        let profile = profile_for_closure.clone();
        let middleware = middleware_for_agent.clone();
        let state = state.clone();

        async move {
            let mut state = state;

            // 1. before_agent（修 dangling tool calls 等）
            middleware.before_agent(&mut state).await.map_err(mw_err)?;

            // 2. 构造 ModelRequest（初始：过滤后 tools + profile 拼装 system + 默认 options）。
            //    有 profile 时 system_message = apply_profile_prompt(USER+BASE+SUFFIX)；
            //    无 profile 回退到 caller system_prompt（旧语义）。后续中间件 wrap_model_call
            //    仍可 push_str 追加段。
            let initial_system = match &profile {
                Some(p) => apply_profile_prompt(p, system_prompt.as_deref().unwrap_or_default()),
                None => system_prompt.clone().unwrap_or_default(),
            };
            let mut req = ModelRequest {
                tools: tool_defs.clone(),
                system_message: initial_system,
                options: CallOptions::default(),
            };

            // 3. wrap_model_call（核心拦截：过滤 tools / 注入 system / 改 messages via state）
            middleware
                .wrap_model_call(&mut req, &mut state)
                .await
                .map_err(mw_err)?;

            // 4. 构造传给模型的 messages：system + state.messages（中间件可能已改 state.messages）
            let mut messages = Vec::with_capacity(state.messages.len() + 1);
            if !req.system_message.is_empty() {
                messages.push(Message::system(&req.system_message));
            }
            messages.extend(state.messages.iter().cloned());

            // 5. 每轮重新 bind 过滤后的 tools，调模型
            let model_with_tools = model.bind_tools(req.tools);
            let response = force_send(model_with_tools.invoke(&messages, Some(&req.options)))
                .await
                .map_err(|e| JunctureError::execution(e.to_string()))?;

            // 6. after_model_call
            let mut response = response;
            middleware
                .after_model_call(&mut response, &mut state)
                .await
                .map_err(mw_err)?;

            // 7. 反映中间件对 state.messages 的改动（SummarizationMiddleware 折叠 / PatchToolCallsMiddleware
            //    追加 synthetic tool results / before_agent 等）：
            //    用 `Message::remove_all()` 哨兵清空旧 messages + 追加 state.messages（含中间件改动）
            //    + response。`messages_reducer` 命中 `REMOVE_ALL_MESSAGES` 先 clear，再 append 新全集 → 替换语义。
            let mut new_messages = vec![Message::remove_all()];
            new_messages.extend(state.messages.iter().cloned());
            new_messages.push(response);
            Ok(DeepAgentStateUpdate {
                messages: Some(new_messages),
            })
        }
        .boxed()
    });

    // tools 节点：复用 juncture ToolNode（持全量工具：caller + middleware 提供）
    let tool_node = Arc::new(ToolNode::new(all_tools));
    let tools_node = NodeFnUpdate(move |state: &DeepAgentState| {
        let tool_node = Arc::clone(&tool_node);
        let messages = state.messages.clone();
        let state_owned = state.clone();
        async move {
            let results = tool_node
                .execute_with_state(&messages, Some(&state_owned))
                .await
                .map_err(|e| {
                    let msg = e.to_string();
                    // HITL interrupt propagate：工具 check_interruptible 返 Err(ToolError) 含
                    // "HITL interrupt"。转 JunctureError::interrupted（非 execution）——
                    // RetryPolicy 不 retry interrupt → 不消耗 signal；interrupt_with_ctx! 已发
                    // signal 到 channel，Pregel after_tick drain 检测 → InterruptAfter pause。
                    if msg.contains("HITL interrupt") {
                        JunctureError::interrupted(0)
                    } else {
                        JunctureError::execution(msg)
                    }
                })?;
            Ok(DeepAgentStateUpdate {
                messages: Some(results),
            })
        }
        .boxed()
    });

    // 组装图
    let mut graph = StateGraph::<DeepAgentState>::new();
    // agent 节点走默认（单 task inline fast-path 足够；无 interrupt 需求）。
    graph.add_node_simple("agent", agent_node)?;
    // tools 节点配 retry —— 让 runner 走非 inline 路径（`INTERRUPT_CONTEXT.scope`），
    // 使工具内 `interrupt_with_ctx!` 能取到 task-local 上下文、发出 interrupt signal。
    // RetryPolicy 默认不 retry interrupt 错误，走非 inline 即达 scope 目的。
    graph.add_node_with_retry("tools", tools_node, RetryPolicy::default())?;
    graph.set_entry_point("agent");

    let path_map = PathMap::from(&[("tools", "tools"), (END, END)][..]);
    graph.add_conditional_edges("agent", Arc::new(DeepAgentRouter), path_map);
    graph.add_edge("tools", "agent");

    // TODO Phase 2: checkpointer 支持（compile_with_checkpointer）
    graph.compile_with_checkpointer(checkpointer)
}

/// 路由器：末消息有 tool_calls → `tools`，否则 END。
struct DeepAgentRouter;

impl Router<DeepAgentState> for DeepAgentRouter {
    #[allow(clippy::type_complexity, reason = "签名由 juncture Router trait 决定")]
    fn route(
        &self,
        state: &DeepAgentState,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RouteResult, JunctureError>> + Send + '_>,
    > {
        let target = state
            .messages
            .last()
            .map_or(END, |m| if m.has_tool_calls() { "tools" } else { END });
        Box::pin(async move { Ok(RouteResult::One(target.to_string())) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::Middleware;
    use async_trait::async_trait;
    use juncture::llm::{Message, MockChatModel};
    use juncture::tools::{Tool, ToolError};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    #[test]
    fn build_compiles_empty() {
        let model = MockChatModel::new("gpt-4").with_response("hi");
        let _ = create_deep_agent(DeepAgentConfig::new(model)).expect("empty config compiles");
    }

    #[test]
    fn builder_compiles() {
        let model = MockChatModel::new("gpt-4").with_response("hi");
        let _ = DeepAgentBuilder::new(model)
            .system_prompt("You are helpful.")
            .build()
            .expect("builder compiles");
    }

    #[tokio::test]
    async fn agent_responds_without_tools() {
        // 无工具、无中间件：单轮，模型直接回复，路由到 END。
        let model = MockChatModel::new("gpt-4").with_response("hello!");
        let agent = create_deep_agent(DeepAgentConfig::new(model)).expect("compiles");
        let state = DeepAgentState {
            messages: vec![Message::human("hi")],
        };
        let out = agent
            .invoke_async(state, &juncture::RunnableConfig::new())
            .await
            .expect("agent runs");
        let last = out.value.messages.last().expect("has response");
        assert_eq!(last.content_text(), "hello!");
    }

    // ===== profile 接入测试 =====

    /// 捕获 wrap_model_call 收到的 ModelRequest 快照（system_message + 工具 name/desc）。
    #[derive(Debug, Default, Clone)]
    struct Captured {
        system_message: String,
        /// (name, description) 对。
        tools: Vec<(String, String)>,
    }

    impl Captured {
        fn tool_names(&self) -> Vec<&str> {
            self.tools.iter().map(|(n, _)| n.as_str()).collect()
        }
    }

    /// 间谍中间件：在 wrap_model_call 中快照 req（此时 system_message 已由 profile 拼装、
    /// tools 已由 excluded_tools 过滤），供测试断言。
    #[derive(Debug)]
    struct SpyMiddleware {
        captured: Arc<Mutex<Option<Captured>>>,
    }

    impl SpyMiddleware {
        /// 构造 spy + 返回共享捕获句柄（测试侧读取）。
        fn new() -> (Self, Arc<Mutex<Option<Captured>>>) {
            let captured: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
            let spy = Self {
                captured: Arc::clone(&captured),
            };
            (spy, captured)
        }
    }

    #[async_trait]
    impl Middleware for SpyMiddleware {
        async fn wrap_model_call(
            &self,
            req: &mut ModelRequest,
            _state: &mut DeepAgentState,
        ) -> Result<(), MiddlewareError> {
            let snap = Captured {
                system_message: req.system_message.clone(),
                tools: req
                    .tools
                    .iter()
                    .map(|t| (t.name.clone(), t.description.clone()))
                    .collect(),
            };
            // std::sync::Mutex 不跨 await 持有（块作用域内释放 guard）。
            {
                let mut guard = self.captured.lock().expect("mutex poisoned");
                *guard = Some(snap);
            }
            Ok(())
        }
    }

    /// 简单桩工具（持 name/desc）。
    struct StubTool {
        tool_name: &'static str,
        tool_desc: &'static str,
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.tool_name
        }
        fn description(&self) -> &str {
            self.tool_desc
        }
        fn schema(&self) -> Value {
            json!({"type":"object"})
        }
        async fn invoke(&self, _input: Value) -> Result<String, ToolError> {
            Ok("stub".to_string())
        }
    }

    #[tokio::test]
    async fn profile_concatenates_system_prompt() {
        // profile + user system_prompt → system_message = USER + BASE + SUFFIX
        let model = MockChatModel::new("gpt-4").with_response("ok");
        let (spy, captured) = SpyMiddleware::new();
        let profile = HarnessProfile::new("anthropic", "claude-opus")
            .base_system_prompt("BASE")
            .system_prompt_suffix("SUFFIX");
        let agent = DeepAgentBuilder::new(model)
            .system_prompt("USER")
            .profile(profile)
            .middleware_one(spy)
            .build()
            .expect("compiles");
        let state = DeepAgentState {
            messages: vec![Message::human("hi")],
        };
        let _ = agent
            .invoke_async(state, &juncture::RunnableConfig::new())
            .await
            .expect("runs");
        let snap = captured
            .lock()
            .expect("mutex lock")
            .as_ref()
            .expect("capture present")
            .clone();
        assert_eq!(snap.system_message, "USER\n\nBASE\n\nSUFFIX");
    }

    #[tokio::test]
    async fn profile_excludes_tools() {
        // excluded_tools 过滤：keep 保留、drop 移除
        let model = MockChatModel::new("gpt-4").with_response("ok");
        let (spy, captured) = SpyMiddleware::new();
        let profile = HarnessProfile::new("anthropic", "claude-opus").exclude_tool("drop");
        let agent = DeepAgentBuilder::new(model)
            .tool(Box::new(StubTool {
                tool_name: "keep",
                tool_desc: "kept",
            }))
            .tool(Box::new(StubTool {
                tool_name: "drop",
                tool_desc: "dropped",
            }))
            .profile(profile)
            .middleware_one(spy)
            .build()
            .expect("compiles");
        let state = DeepAgentState {
            messages: vec![Message::human("hi")],
        };
        let _ = agent
            .invoke_async(state, &juncture::RunnableConfig::new())
            .await
            .expect("runs");
        let snap = captured
            .lock()
            .expect("mutex lock")
            .as_ref()
            .expect("capture present")
            .clone();
        assert!(snap.tool_names().contains(&"keep"));
        assert!(!snap.tool_names().contains(&"drop"));
    }

    #[tokio::test]
    async fn profile_overrides_tool_description() {
        // tool_description_overrides 覆写 LLM 可见 description
        let model = MockChatModel::new("gpt-4").with_response("ok");
        let (spy, captured) = SpyMiddleware::new();
        let profile = HarnessProfile::new("anthropic", "claude-opus")
            .override_tool_description("keep", "OVERRIDDEN");
        let agent = DeepAgentBuilder::new(model)
            .tool(Box::new(StubTool {
                tool_name: "keep",
                tool_desc: "original",
            }))
            .profile(profile)
            .middleware_one(spy)
            .build()
            .expect("compiles");
        let state = DeepAgentState {
            messages: vec![Message::human("hi")],
        };
        let _ = agent
            .invoke_async(state, &juncture::RunnableConfig::new())
            .await
            .expect("runs");
        let snap = captured
            .lock()
            .expect("mutex lock")
            .as_ref()
            .expect("capture present")
            .clone();
        let desc = snap
            .tools
            .iter()
            .find(|(n, _)| n == "keep")
            .map(|(_, d)| d.as_str())
            .expect("keep tool present");
        assert_eq!(desc, "OVERRIDDEN");
    }

    #[tokio::test]
    async fn no_profile_preserves_user_system_prompt() {
        // 无 profile：system_message = caller system_prompt（旧语义）
        let model = MockChatModel::new("gpt-4").with_response("ok");
        let (spy, captured) = SpyMiddleware::new();
        let agent = DeepAgentBuilder::new(model)
            .system_prompt("just user")
            .middleware_one(spy)
            .build()
            .expect("compiles");
        let state = DeepAgentState {
            messages: vec![Message::human("hi")],
        };
        let _ = agent
            .invoke_async(state, &juncture::RunnableConfig::new())
            .await
            .expect("runs");
        let snap = captured
            .lock()
            .expect("mutex lock")
            .as_ref()
            .expect("capture present")
            .clone();
        assert_eq!(snap.system_message, "just user");
    }
}
