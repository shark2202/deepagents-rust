//! `create_deep_agent` 工厂 —— deepagents-rust 的公共入口。
//!
//! 自建 `agent` 节点（`DeepAgentNode` 闭包式）+ `tools` 节点 + 路由器，组装成 juncture
//! `StateGraph`。这是 B 路径的核心：不依赖 juncture 的 `create_agent_with_middleware`
//! （其 `before_model` 只读、tools 预绑死、`CallOptions` 无 tools 字段，无法承载 deepagents
//! 拦截语义），而是借 juncture 的 `StateGraph`/Pregel/`Command`/`Node`/`ToolNode` runtime。

use std::sync::Arc;

use futures::future::FutureExt;
use juncture::checkpoint::CheckpointSaver;
use juncture::edge::{END, PathMap, RouteResult, Router};
use juncture::graph::{CompiledGraph, StateGraph, TopologyError};
use juncture::llm::{CallOptions, ChatModel, Message, ToolDefinition as LlmToolDefinition};
use juncture::node::NodeFnUpdate;
use juncture::tools::{Tool, ToolDefinition as ToolsToolDefinition, ToolNode};
use juncture::wasm_send::force_send;
use juncture::JunctureError;

use crate::middleware::{MiddlewareChain, MiddlewareError, ModelRequest};
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
        checkpointer: _,
    } = config;

    // 合并 caller tools + 各 middleware 提供的工具（如 FilesystemMiddleware 的 8 个 fs 工具）。
    // 加性合并，对齐 deepagents：中间件提供工具 + caller 自带工具共存。
    let all_tools: Vec<Box<dyn Tool>> = tools
        .into_iter()
        .chain(middleware.iter().flat_map(|m| m.tools()))
        .collect();

    // 全量工具定义（llm 格式）。中间件 wrap_model_call 在此基础上 per-call 过滤。
    let all_tool_defs: Vec<ToolsToolDefinition> = all_tools.iter().map(|t| t.definition()).collect();
    let all_llm_tool_defs = convert_tool_defs(&all_tool_defs);

    let model = Arc::new(model);
    let tool_defs_for_closure = all_llm_tool_defs.clone();
    let system_prompt_for_closure = system_prompt;
    let middleware_for_agent = middleware.clone();

    // agent 节点：闭包式。每次调用 clone 必要状态（Future 需 'static）。
    let agent_node = NodeFnUpdate(move |state: &DeepAgentState| {
        let model = Arc::clone(&model);
        let tool_defs = tool_defs_for_closure.clone();
        let system_prompt = system_prompt_for_closure.clone();
        let middleware = middleware_for_agent.clone();
        let state = state.clone();

        async move {
            let mut state = state;

            // 1. before_agent（修 dangling tool calls 等）
            middleware.before_agent(&mut state).await.map_err(mw_err)?;

            // 2. 构造 ModelRequest（初始：全量 tools + 用户 prompt + 默认 options）
            let mut req = ModelRequest {
                tools: tool_defs.clone(),
                system_message: system_prompt.clone().unwrap_or_default(),
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

            // 7. 追加 response 到 messages
            Ok(DeepAgentStateUpdate {
                messages: Some(vec![response]),
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
                .map_err(|e| JunctureError::execution(e.to_string()))?;
            Ok(DeepAgentStateUpdate {
                messages: Some(results),
            })
        }
        .boxed()
    });

    // 组装图
    let mut graph = StateGraph::<DeepAgentState>::new();
    graph.add_node_simple("agent", agent_node)?;
    graph.add_node_simple("tools", tools_node)?;
    graph.set_entry_point("agent");

    let path_map = PathMap::from(&[("tools", "tools"), (END, END)][..]);
    graph.add_conditional_edges("agent", Arc::new(DeepAgentRouter), path_map);
    graph.add_edge("tools", "agent");

    // TODO Phase 2: checkpointer 支持（compile_with_checkpointer）
    graph.compile()
}

/// 路由器：末消息有 tool_calls → `tools`，否则 END。
struct DeepAgentRouter;

impl Router<DeepAgentState> for DeepAgentRouter {
    #[allow(clippy::type_complexity, reason = "签名由 juncture Router trait 决定")]
    fn route(
        &self,
        state: &DeepAgentState,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<RouteResult, JunctureError>> + Send + '_,
        >,
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
    use juncture::llm::{Message, MockChatModel};

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
}
