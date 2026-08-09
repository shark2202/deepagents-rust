//! `SubagentMiddleware` —— 注册预编译子 agent，向主 agent 暴露 `task` 工具用于委派。
//!
//! 对应 deepagents `SubAgentMiddleware`：
//! - 持 `AgentRegistry`（`Arc<HashMap<String, Arc<CompiledGraph<DeepAgentState>>>>`），子 agent
//!   由调用方用 `create_deep_agent` 预编译后注册进来——`SubagentMiddleware` 自身不递归构造子 agent。
//! - `tools()`：返回 `TaskTool`（持 registry clone）。
//! - `wrap_model_call`：把可用子 agent 名单注入 `system_message`，引导模型用 `task` 工具委派。
//!
//! `TaskTool.invoke`：解析 `subagent_type`/`task` → 从 registry 取子 agent → 以**全新** messages
//! （仅一条 `Message::human(&task)`）调用 `compiled.invoke_async`（不继承 caller state，
//! 对齐 deepagents 子 agent 隔离）→ 从末尾向前找首条无 `tool_calls` 的 `Role::Ai` 消息，
//! 返回其 `content_text()`。
//!
//! 工具失败返回 `Ok("Error: ...")`（对齐 deepagents `ToolMessage(status="error")` 与 fs_tools 约定，
//! 兼容 juncture `ToolErrorHandlingMiddleware` 的 "Error:" 前缀识别）。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use juncture::RunnableConfig;
use juncture::graph::CompiledGraph;
use juncture::llm::{Message, Role};
use juncture::tools::{Tool, ToolError};
use serde_json::{Value, json};

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 子 agent 注册表：名字 → 预编译图。`Arc` 包裹使 `SubagentMiddleware`/`TaskTool` 各持一份
/// 廉价 clone（注册表本身不可变；注册用 `AgentRegistry::build` 一次性构造）。
pub type AgentRegistry = Arc<HashMap<String, Arc<CompiledGraph<DeepAgentState>>>>;

/// 子 agent 中间件：向主 agent 暴露 `task` 工具 + 注入可用子 agent 列表。
///
/// 子 agent 预编译后经 `new` 注入；`TaskTool` 调用时起全新 state，不继承 caller messages。
pub struct SubagentMiddleware {
    registry: AgentRegistry,
}

impl std::fmt::Debug for SubagentMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentMiddleware")
            .field("agent_count", &self.registry.len())
            .finish()
    }
}

impl SubagentMiddleware {
    /// 以给定注册表构造。
    #[must_use]
    pub fn new(registry: AgentRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Middleware for SubagentMiddleware {
    /// 提供 `task` 工具（持 registry 的 `Arc` clone）。
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(TaskTool::new(Arc::clone(&self.registry)))]
    }

    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 空注册表不注入（无可用子 agent 时 prompt 段无意义）。
        if self.registry.is_empty() {
            return Ok(());
        }
        // 排序以保证 prompt 确定性。
        let mut names: Vec<&String> = self.registry.keys().collect();
        names.sort_unstable();
        let list = names
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        req.system_message.push_str(&format!(
            "\n\nAvailable sub-agents: {list}. Use the task tool to delegate a task to a sub-agent, \
             which runs autonomously and returns its final response.",
        ));
        Ok(())
    }
}

/// 委派工具：把 `task` 交给名为 `subagent_type` 的注册子 agent 执行并返回其最终回复。
pub struct TaskTool {
    registry: AgentRegistry,
}

impl TaskTool {
    #[must_use]
    pub fn new(registry: AgentRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        "Delegate a task to a registered sub-agent. The sub-agent runs autonomously with a fresh \
         conversation and returns its final response."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "Name of the registered sub-agent to invoke."
                },
                "task": {
                    "type": "string",
                    "description": "The task description to hand to the sub-agent."
                }
            },
            "required": ["subagent_type", "task"]
        })
    }

    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let subagent_type = get_str(&input, "subagent_type")?;
        let task = get_str(&input, "task")?;

        // 未知子 agent → "Error: ..."（对齐 deepagents 未知类型报错）。
        let Some(compiled) = self.registry.get(&subagent_type) else {
            return Ok(format!("Error: unknown subagent: {subagent_type}"));
        };

        // 全新 state：不继承 caller messages（deepagents 子 agent 隔离）。
        let fresh_state = DeepAgentState {
            messages: vec![Message::human(&task)],
        };

        // 用 `RunnableConfig::new()`（recursion_limit=25）；`Default` 的 0 会让 ReAct 多步循环
        // 立即触顶，故不用 `default()`。
        let out = match compiled
            .invoke_async(fresh_state, &RunnableConfig::new())
            .await
        {
            Ok(o) => o,
            Err(e) => return Ok(format!("Error: {e}")),
        };

        // 从末尾向前找首条无 tool_calls 的 AI 消息——即子 agent 的最终回复。
        let response = out
            .value
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Ai && m.tool_calls.is_empty());

        match response {
            Some(m) => Ok(m.content_text().to_string()),
            None => Ok("Error: subagent returned no response".to_string()),
        }
    }
}

/// 解析必填字符串字段（缺失或非字符串 → `ToolError::invalid_input`）。
fn get_str(input: &Value, field: &str) -> Result<String, ToolError> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::invalid_input(format!("missing or invalid '{field}'")))
}
