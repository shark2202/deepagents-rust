//! `AsyncSubAgentMiddleware` —— 把任务委派给远程（Agent Protocol）子 agent，异步轮询。
//!
//! 对应 deepagents `AsyncSubAgentMiddleware`：
//! - 持 `AsyncAgentRegistry`（`Arc<HashMap<String, RemoteGraph>>`）：`subagent_type` → 远程图引用
//! - `tools()`：返回 `start_async_task` / `check_async_task` / `cancel_async_task` /
//!   `list_async_tasks` 四个工具
//! - `wrap_model_call`：把可用 async 子 agent 名单注入 `system_message`，引导模型用
//!   `start_async_task` 委派、`check_async_task` 取回结果
//!
//! # 协议适配（juncture RemoteGraph）
//!
//! deepagents 原实现基于 langgraph_sdk 的 `AsyncClient`：`runs.create()` 启动后台 run 返回
//! `run_id`、`runs.get()` 轮询状态、`runs.cancel()` 取消。juncture 的 `RemoteGraph`/
//! `GraphClient` **没有**原生后台 run 模型——`invoke()` 是同步阻塞调用（HTTP 请求等到远程
//! 图跑完才返回最终 state）。
//!
//! 故本实现用 `tokio::spawn` 在后台跑 `RemoteGraph::invoke`，用一个进程内 `AsyncTaskStore`
//! 跟踪每个 `task_id` 的状态（running / done / error / cancelled）。行为对齐 deepagents 的
//! start/poll/cancel 三件套，但 `task_id` 是本中间件自分配的（非服务端 `run_id`）。
//!
//! # 已知简化 / defer
//!
//! - **鉴权**：`RemoteGraph::new` 固定 `AuthConfig::None`；需鉴权的远程服务器要求 juncture 上游
//!   支持带 auth 的 RemoteGraph 构造（或经 `JunctureClient`），defer 到上游增强。
//! - **取消**：本地 `JoinHandle::abort()` 中断后台 future；若远端 run 已在执行，其取消需服务端
//!   `run_id`（`GraphClient::cancel(thread_id, run_id)` 需要 `run_id`，本中间件无 `run_id`），
//!   当前不传递，defer。
//! - **远端 schema 耦合**：`invoke::<DeepAgentState>` 假设远端图返回 `DeepAgentState` 兼容 JSON
//!   （含 `messages` 字段，与 [`crate::state::DeepAgentState`] 同构）；反序列化失败则 task 标
//!   `Error`。跨 schema 远端需改用 `serde_json::Value` 反序列化，defer。
//! - **`task_id` 唯一性**：进程内原子计数器，重启不复用；跨进程去重需调用方协调。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use juncture::graph::RemoteGraph;
use juncture::llm::{Message, Role};
use juncture::tools::{Tool, ToolError};
use juncture::{ClientError, InvokeConfig};
use serde_json::{Value, json};

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 远程子 agent 注册表：`subagent_type` → 远程图引用。`Arc` 包裹使中间件与各工具各持一份
/// 廉价 clone（注册表本身不可变；构造时一次性建好）。
///
/// 每个 [`RemoteGraph`] 由调用方经 `RemoteGraph::new(endpoint, graph_id)` 构造——指向一台
/// 部署了该子 agent 的 juncture 远程服务器。
pub type AsyncAgentRegistry = Arc<HashMap<String, RemoteGraph>>;

/// 远程子 agent 中间件：向主 agent 暴露 4 个 async task 工具 + 注入可用 async 子 agent 列表。
///
/// 子 agent 以远程图（`RemoteGraph`）形式部署；`start_async_task` 在后台 `tokio::spawn`
/// 调远程 `invoke`，立即返回 `task_id`，主 agent 后续用 `check_async_task` 轮询取结果。
pub struct AsyncSubAgentMiddleware {
    registry: AsyncAgentRegistry,
    tasks: AsyncTaskStore,
}

impl std::fmt::Debug for AsyncSubAgentMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let task_count = self.tasks.lock().map_or(0, |g| g.tasks.len());
        f.debug_struct("AsyncSubAgentMiddleware")
            .field("agent_count", &self.registry.len())
            .field("active_tasks", &task_count)
            .finish_non_exhaustive()
    }
}

impl AsyncSubAgentMiddleware {
    /// 以给定远程子 agent 注册表构造。任务跟踪表内部新建（空）。
    #[must_use]
    pub fn new(registry: AsyncAgentRegistry) -> Self {
        Self {
            registry,
            tasks: Arc::new(Mutex::new(AsyncTaskStoreInner {
                tasks: HashMap::new(),
                next_id: 1,
            })),
        }
    }

    /// 任务跟踪表引用（供外部诊断 / 测试）。
    #[must_use]
    pub fn tasks(&self) -> &AsyncTaskStore {
        &self.tasks
    }
}

#[async_trait]
impl Middleware for AsyncSubAgentMiddleware {
    /// 提供 4 个 async task 工具（各持所需 `Arc` clone）。
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(StartAsyncTaskTool::new(
                Arc::clone(&self.registry),
                Arc::clone(&self.tasks),
            )),
            Box::new(CheckAsyncTaskTool::new(Arc::clone(&self.tasks))),
            Box::new(CancelAsyncTaskTool::new(Arc::clone(&self.tasks))),
            Box::new(ListAsyncTasksTool::new(Arc::clone(&self.tasks))),
        ]
    }

    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 空注册表不注入（无可用 async 子 agent 时 prompt 段无意义）。
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
            "\n\nAvailable async sub-agents: {list}. Use the start_async_task tool to delegate a \
             task to an async sub-agent running on a remote server; it returns a task_id \
             immediately. Poll with check_async_task until status is 'done' (returns the \
             sub-agent's final response) or 'error'. Use cancel_async_task to abandon a task, and \
             list_async_tasks to see all in-flight tasks.",
        ));
        Ok(())
    }
}

// ===== task store =====

/// 异步任务状态。
///
/// `Cancelled` 优先：后台 `invoke` 完成时若任务已被取消，不覆盖为 `Done`/`Error`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncTaskStatus {
    /// 后台 `invoke` 进行中。
    Running,
    /// 远程图跑完，结果已就绪。
    Done,
    /// 远程调用失败（网络 / 服务端 / 反序列化错误）。
    Error,
    /// 被主 agent 取消。
    Cancelled,
}

impl AsyncTaskStatus {
    /// 状态字面量（对齐 deepagents run status 文本）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 单个 async task 跟踪条目。
///
/// `handle` 仅供本地取消（`JoinHandle::abort`）；后台 `invoke` 完成时由 spawned 任务写
/// `status`/`result`，不碰 `handle`。
pub struct AsyncTaskEntry {
    subagent_type: String,
    task: String,
    status: AsyncTaskStatus,
    /// `Done`/`Error` 时的最终文本（Done = 子 agent 末条 AI 回复；Error = `Error: ...`）。
    result: Option<String>,
    /// 后台 `JoinHandle`，取消时 `take()` 出来 `abort()`。
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// 任务跟踪表内部状态（`Mutex` 保护）。
pub struct AsyncTaskStoreInner {
    tasks: HashMap<String, AsyncTaskEntry>,
    /// 自增计数器，生成 `task-{n}` id。
    next_id: u64,
}

/// 共享任务跟踪表：`Arc<Mutex<AsyncTaskStoreInner>>`。
///
/// 用 `std::sync::Mutex` 而非 `tokio::sync::Mutex`——所有临界区都是无 `await` 的快速读写
/// （insert / status 更新 / lookup），不阻塞 runtime；且无需为 `tokio::sync` 加 Cargo feature。
/// 中毒恢复：后台任务不会 panic（只做 `Result` 驱动的字段赋值），但用 [`lock_store`] 兜底。
pub type AsyncTaskStore = Arc<Mutex<AsyncTaskStoreInner>>;

/// 锁任务表，中毒时取回内部数据（不让一个后台任务的 panic 卡死所有后续工具调用）。
fn lock_store(store: &AsyncTaskStore) -> MutexGuard<'_, AsyncTaskStoreInner> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 从远程图返回的 state 中提取子 agent 的最终回复：末条无 `tool_calls` 的 AI 消息。
///
/// 与 [`crate::middleware::subagent::TaskTool`] 同一规则，对齐 deepagents 子 agent 隔离语义。
fn final_response(state: &DeepAgentState) -> String {
    state
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Ai && m.tool_calls.is_empty())
        .map(|m| m.content_text().to_string())
        .unwrap_or_else(|| "Error: subagent returned no response".to_string())
}

// ===== tools =====

/// 启动一个 async 子 agent 任务，立即返回 `task_id`。
pub struct StartAsyncTaskTool {
    registry: AsyncAgentRegistry,
    tasks: AsyncTaskStore,
}

impl StartAsyncTaskTool {
    #[must_use]
    pub fn new(registry: AsyncAgentRegistry, tasks: AsyncTaskStore) -> Self {
        Self { registry, tasks }
    }
}

#[async_trait]
impl Tool for StartAsyncTaskTool {
    fn name(&self) -> &'static str {
        "start_async_task"
    }
    fn description(&self) -> &'static str {
        "Delegate a task to an async sub-agent running on a remote server. Returns a task_id \
         immediately; the sub-agent runs autonomously in the background. Poll with \
         check_async_task to retrieve its result."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "Name of the registered async sub-agent to invoke."
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
        let remote = match self.registry.get(&subagent_type) {
            Some(r) => r.clone(),
            None => return Ok(format!("Error: unknown async subagent: {subagent_type}")),
        };

        // 1. 生成 task_id 并插入 Running 条目（先插条目再 spawn，确保后台完成时能查到）。
        let task_id = {
            let mut guard = lock_store(&self.tasks);
            let id = format!("task-{}", guard.next_id);
            guard.next_id += 1;
            guard.tasks.insert(
                id.clone(),
                AsyncTaskEntry {
                    subagent_type,
                    task: task.clone(),
                    status: AsyncTaskStatus::Running,
                    result: None,
                    handle: None,
                },
            );
            id
        };

        // 2. 后台 spawn 远程 invoke；完成时回写 status/result（Cancelled 优先）。
        let tasks = Arc::clone(&self.tasks);
        let task_id_for_spawn = task_id.clone();
        let handle = tokio::spawn(async move {
            // 全新 state：不继承 caller messages（deepagents 子 agent 隔离）。
            let fresh_state = DeepAgentState {
                messages: vec![Message::human(&task)],
            };
            // thread_id = task_id 便于远端 stateful 追踪；recursion_limit=25 对齐
            // SubagentMiddleware 的 RunnableConfig::new()。
            let config = InvokeConfig {
                thread_id: Some(task_id_for_spawn.clone()),
                recursion_limit: Some(25),
                ..Default::default()
            };
            let outcome: (AsyncTaskStatus, String) =
                match remote.invoke(&fresh_state, Some(config)).await {
                    Ok(state) => (AsyncTaskStatus::Done, final_response(&state)),
                    Err(e) => (AsyncTaskStatus::Error, client_error_text(&e)),
                };
            let mut guard = lock_store(&tasks);
            if let Some(entry) = guard.tasks.get_mut(&task_id_for_spawn)
                && !matches!(entry.status, AsyncTaskStatus::Cancelled)
            {
                entry.status = outcome.0;
                entry.result = Some(outcome.1);
            }
        });

        // 3. 回填 JoinHandle 供取消。spawn 与回填之间有极短窗口 handle=None——cancel 落在此时
        //    会直接标 Cancelled（见 CancelAsyncTaskTool），后台完成时 respect Cancelled。
        {
            let mut guard = lock_store(&self.tasks);
            if let Some(entry) = guard.tasks.get_mut(&task_id) {
                entry.handle = Some(handle);
            }
        }

        Ok(task_id)
    }
}

/// 查询 async 任务状态；`done` 时返回子 agent 最终回复。
pub struct CheckAsyncTaskTool {
    tasks: AsyncTaskStore,
}

impl CheckAsyncTaskTool {
    #[must_use]
    pub fn new(tasks: AsyncTaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for CheckAsyncTaskTool {
    fn name(&self) -> &'static str {
        "check_async_task"
    }
    fn description(&self) -> &'static str {
        "Check the status of an async task. Returns one of: 'running', 'done' (with the \
         sub-agent's final response), 'error' (with the error message), or 'cancelled'."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task_id returned by start_async_task."
                }
            },
            "required": ["task_id"]
        })
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let task_id = get_str(&input, "task_id")?;
        let guard = lock_store(&self.tasks);
        let Some(entry) = guard.tasks.get(&task_id) else {
            return Ok(format!("Error: unknown task id: {task_id}"));
        };
        let status = entry.status;
        let out = match status {
            AsyncTaskStatus::Running => format!("running: task {task_id} is still in progress"),
            AsyncTaskStatus::Done => {
                let result = entry.result.clone().unwrap_or_default();
                format!("done: {result}")
            }
            AsyncTaskStatus::Error => {
                let result = entry.result.clone().unwrap_or_default();
                format!("error: {result}")
            }
            AsyncTaskStatus::Cancelled => format!("cancelled: task {task_id} was cancelled"),
        };
        Ok(out)
    }
}

/// 取消一个 async 任务（中断后台 future）。
pub struct CancelAsyncTaskTool {
    tasks: AsyncTaskStore,
}

impl CancelAsyncTaskTool {
    #[must_use]
    pub fn new(tasks: AsyncTaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for CancelAsyncTaskTool {
    fn name(&self) -> &'static str {
        "cancel_async_task"
    }
    fn description(&self) -> &'static str {
        "Cancel an in-flight async task. No-op if the task already finished."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task_id returned by start_async_task."
                }
            },
            "required": ["task_id"]
        })
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let task_id = get_str(&input, "task_id")?;
        let mut guard = lock_store(&self.tasks);
        let Some(entry) = guard.tasks.get_mut(&task_id) else {
            return Ok(format!("Error: unknown task id: {task_id}"));
        };
        // 已终结：幂等返回当前状态（不重复 abort）。
        if !matches!(entry.status, AsyncTaskStatus::Running) {
            return Ok(format!(
                "{}: task {task_id} already {}",
                entry.status.as_str(),
                entry.status.as_str()
            ));
        }
        // take 出 JoinHandle 并 abort（中断后台 invoke future）。guard 在返回时自然释放
        // （返回值不借用 entry/guard），故无需显式 drop。
        if let Some(handle) = entry.handle.take() {
            handle.abort();
        }
        entry.status = AsyncTaskStatus::Cancelled;
        Ok(format!("cancelled: task {task_id}"))
    }
}

/// 列出所有 async 任务及其状态。
pub struct ListAsyncTasksTool {
    tasks: AsyncTaskStore,
}

impl ListAsyncTasksTool {
    #[must_use]
    pub fn new(tasks: AsyncTaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for ListAsyncTasksTool {
    fn name(&self) -> &'static str {
        "list_async_tasks"
    }
    fn description(&self) -> &'static str {
        "List all async tasks (id, sub-agent type, task, status). Useful for tracking in-flight work."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn invoke(&self, _input: Value) -> Result<String, ToolError> {
        let guard = lock_store(&self.tasks);
        if guard.tasks.is_empty() {
            return Ok("No async tasks".to_string());
        }
        // 按 task_id 排序保证输出确定性。task 截断 60 字符避免输出过长。
        let mut entries: Vec<(&String, &AsyncTaskEntry)> = guard.tasks.iter().collect();
        entries.sort_unstable_by_key(|e| e.0);
        let out = entries
            .into_iter()
            .map(|(id, e)| {
                let task_preview = if e.task.len() > 60 {
                    format!("{}…", &e.task[..60])
                } else {
                    e.task.clone()
                };
                format!(
                    "{}\t{}\t{}\t{}",
                    id,
                    e.subagent_type,
                    task_preview,
                    e.status.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(out)
    }
}

// ===== helpers =====

/// 解析必填字符串字段（缺失或非字符串 → `ToolError::invalid_input`）。
fn get_str(input: &Value, field: &str) -> Result<String, ToolError> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::invalid_input(format!("missing or invalid '{field}'")))
}

/// 把 `ClientError` 格式为 `"Error: ..."` 文本（对齐 fs_tools / subagent 的 "Error:" 前缀约定，
/// 兼容 juncture `ToolErrorHandlingMiddleware`）。
fn client_error_text(e: &ClientError) -> String {
    format!("Error: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use juncture::llm::CallOptions;

    fn make_middleware_empty() -> AsyncSubAgentMiddleware {
        AsyncSubAgentMiddleware::new(Arc::new(HashMap::new()))
    }

    fn make_req() -> ModelRequest {
        ModelRequest {
            tools: vec![],
            system_message: String::new(),
            options: CallOptions::default(),
        }
    }

    #[test]
    fn debug_smoke() {
        let mw = make_middleware_empty();
        let s = format!("{mw:?}");
        assert!(s.contains("AsyncSubAgentMiddleware"));
        assert!(s.contains("agent_count"));
        assert!(s.contains("active_tasks"));
    }

    #[tokio::test]
    async fn empty_registry_no_injection() {
        let mw = make_middleware_empty();
        let mut state = DeepAgentState { messages: vec![] };
        let mut req = make_req();
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        // 空注册表：不注入，system_message 不变。
        assert!(req.system_message.is_empty());
    }

    #[tokio::test]
    async fn nonempty_registry_injects() {
        // RemoteGraph::new 仅本地构造（无网络），可安全用于测试 prompt 注入。
        let mut map = HashMap::new();
        map.insert(
            "researcher".to_string(),
            RemoteGraph::new("http://localhost:0", "researcher"),
        );
        let mw = AsyncSubAgentMiddleware::new(Arc::new(map));
        let mut state = DeepAgentState { messages: vec![] };
        let mut req = make_req();
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        assert!(
            req.system_message
                .contains("Available async sub-agents: researcher")
        );
        assert!(req.system_message.contains("start_async_task"));
        assert!(req.system_message.contains("check_async_task"));
    }

    #[test]
    fn tools_registered() {
        let mw = make_middleware_empty();
        let tools = mw.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [
                "start_async_task",
                "check_async_task",
                "cancel_async_task",
                "list_async_tasks",
            ]
        );
    }

    #[test]
    fn status_as_str() {
        assert_eq!(AsyncTaskStatus::Running.as_str(), "running");
        assert_eq!(AsyncTaskStatus::Done.as_str(), "done");
        assert_eq!(AsyncTaskStatus::Error.as_str(), "error");
        assert_eq!(AsyncTaskStatus::Cancelled.as_str(), "cancelled");
    }
}
