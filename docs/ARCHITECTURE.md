# deepagents-rust 架构文档

> 本文档描述**实际落地**的架构，基于代码实现而非设计意图。
> 上游 survey 见 `docs/SPEC.md`（Q1-Q26），决策记录见 `docs/adr/`。

## 1. 项目定位

`deepagents-rust` 是 Python `deepagents` SDK 的 Rust 移植，基于 `rig-core` + `rig-agent` 构建。目标是一个纯 Rust、零 C 依赖、可交叉编译的 AI agent SDK + 产品。

- **底座**: `rig-agent 0.42.0` + `rig-core 0.42.0`（替代 Python LangGraph + juncture）
- **范式**: sans-IO state machine (`AgentRun`) + serde 可序列化状态
- **安全**: `#![forbid(unsafe_code)]` 全 crate 生效
- **文档**: `#![warn(missing_docs)]` 全 crate 生效

## 2. Workspace 布局

19-crate workspace，按 SPEC §七 (Q10) 布局：

```
deepagents-rust/
├── crates/
│   ├── deepagents-core       # ← 已实现: SDK 核心层 (Q4-Q8)
│   ├── deepagents-errors     # ← 已实现: ~40 错误类型 (Q26)
│   ├── deepagents-config     # config.toml 6-layer resolver (Q11)
│   ├── deepagents-env        # 55 env vars (Q12)
│   ├── deepagents-cli        # clap, 13 subcommands (Q13)
│   ├── deepagents-tui        # ratatui TUI (Q14, Q24)
│   ├── deepagents-hooks      # 12 events, async (Q15)
│   ├── deepagents-plugins    # JSON-RPC stdio (Q16)
│   ├── deepagents-sessions   # rusqlite+bundled, resume (Q17)
│   ├── deepagents-approval   # Manual/Auto/YOLO (Q18)
│   ├── deepagents-mcp        # rmcp client (Q19)
│   ├── deepagents-skills     # 8-source discovery (Q20)
│   ├── deepagents-sandbox    # trait provider, 6 providers (Q21)
│   ├── deepagents-cost       # bundled JSON catalog (Q22)
│   ├── deepagents-goal       # self-grading loop (Q23)
│   ├── deepagents-onboarding # marker files (Q25)
│   ├── deepagents-update      # GitHub Releases (Q25)
│   ├── deepagents-doctor      # diagnostics (Q25)
│   └── deepagents             # 顶层 facade crate
├── docs/
│   ├── SPEC.md               # Q1-Q26 全量勘测 survey
│   ├── ARCHITECTURE.md       # ← 本文件: 落地架构
│   ├── MIGRATION-VERIFICATION.md
│   └── adr/                  # 架构决策记录
└── Cargo.toml                # workspace root
```

### 实现进度

| Crate | 状态 | 测试 | Commit |
|-------|------|------|--------|
| `deepagents-errors` | ✅ 完成 | — | `a57bdba` |
| `deepagents-core` | ✅ 完成 | 51 + 1 doctest | `59d32ff` |
| 其他 17 crates | ⬜ 骨架 | — | `3783c3e` |

## 3. deepagents-core 模块架构

`deepagents-core` 是 SDK 核心层，包含 8 个模块：

```
┌─────────────────────────────────────────────────────────┐
│                    DeepAgentBuilder                      │
│                                                          │
│  model ──→ AgentBuilder::new(model).preamble(prompt)     │
│  system_prompt ──→ assemble_prompt() ──────────┘         │
│  backend ──→ FilesystemMiddleware                        │
│  subagents ──→ SubAgentMiddleware                        │
│  interrupt_on ──→ HitlMiddleware                         │
│  (always) ──→ SummarizationMiddleware                    │
│  extra hooks ──→ HookStack.push(hook)                    │
│                                                          │
│  build() ──→ Agent                                       │
│  build_runner(prompt) ──→ AgentRunner                    │
└─────────────────────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────────────────────────────────────┐
│                    HookStack (rig-agent)                  │
│                                                          │
│  FilesystemMiddleware  →  fs tools + preamble patch     │
│  SubAgentMiddleware    →  task tool + subagent prompt   │
│  SummarizationMiddleware →  history truncation          │
│  HitlMiddleware        →  ToolCallAction::Stop on match │
│  [user extra hooks]    →  any H: AgentHook              │
└─────────────────────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────────────────────────────────────┐
│              Agent / AgentRunner (rig-agent)              │
│                                                          │
│  AgentRunner::run() → AgentRun state machine             │
│  hooks fire at each lifecycle event                      │
│  CompletionModel drives the LLM call                     │
└─────────────────────────────────────────────────────────┘
```

### 3.1 模块清单

| 模块 | 文件 | 行数 | SPEC | 职责 |
|------|------|------|------|------|
| `backend` | `backend.rs` | 1014 | Q7 | Backend trait + 4 backends |
| `permission` | `permission.rs` | 317 | Q7 | FilesystemPermission + PermissionChecker |
| `mock` | `mock.rs` | 209 | Q8 | MockCompletionModel (FIFO 响应队列) |
| `hitl` | `hitl.rs` | 616 | Q4 | InterruptPolicy + InterruptMap + HitlMiddleware |
| `subagent` | `subagent.rs` | 509 | Q6 | SubAgentSpec / HarnessProfile / ModelSpec |
| `middleware` | `middleware.rs` | 643 | Q5 | Filesystem/SubAgent/Summarization 三件套 |
| `builder` | `builder.rs` | 807 | Q6 | DeepAgentBuilder (18 参数映射) |
| `lib` | `lib.rs` | 13 | — | 模块声明 + re-exports |

### 3.2 模块依赖关系

```
builder.rs
  ├── backend.rs      (Backend trait, Arc<dyn Backend>)
  ├── hitl.rs         (InterruptMap, HitlMiddleware)
  ├── middleware.rs   (FilesystemMiddleware, SubAgentMiddleware, SummarizationMiddleware)
  ├── permission.rs   (FilesystemPermission)
  └── subagent.rs    (SubAgentSpec, HarnessProfile, ModelSpec, McpServerConfig, ToolSpec)

middleware.rs
  ├── backend.rs       (Arc<dyn Backend>)
  └── permission.rs    (FilesystemPermission)

hitl.rs
  └── (standalone, only depends on rig-agent + serde)

subagent.rs
  ├── hitl.rs         (InterruptMap)
  └── permission.rs   (FilesystemPermission)

backend.rs
  └── deepagents-errors (Error enum)

mock.rs
  └── (standalone, only depends on rig-core)
```

关键设计：模块间无循环依赖。`builder` 是顶层组装点，依赖所有其他模块。

## 4. 核心抽象

### 4.1 Backend trait (Q7)

文件系统抽象层。所有路径是虚拟 POSIX 路径（`/`-分隔，绝对路径，禁止 `..`/`~`）。

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    async fn ls(&self, path: &str) -> Result<LsResult, Error>;
    async fn read(&self, path: &str, offset: usize, limit: usize) -> Result<ReadResult, Error>;
    async fn grep(&self, pattern: &str, path: Option<&str>, glob: Option<&str>, max_count: Option<usize>) -> Result<GrepResult, Error>;
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error>;
    async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error>;
    async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error>;
    async fn delete(&self, path: &str) -> Result<DeleteResult, Error>;
    async fn info(&self, path: &str) -> Result<FileInfo, Error>;
    async fn list_files(&self) -> Result<Vec<FileInfo>, Error>;
}

pub trait SandboxBackend: Backend {
    async fn execute(&self, command: &str, timeout: Option<u64>) -> Result<ExecuteResponse, Error>;
    fn id(&self) -> &str;
}
```

四个实现：
- **StateBackend** — `HashMap<String, String>`，内存中，默认，WASM 安全
- **FilesystemBackend** — `std::fs`，沙箱化到 `root_dir`（feature `filesystem`）
- **LocalShellBackend** — FilesystemBackend + `tokio::process::Command`（feature `filesystem`）
- **CompositeBackend** — 最长前缀路由（feature `composite`）

### 4.2 Permission 模型 (Q7)

权限在工具层强制执行，不在 backend 层。

```
FilesystemPermission { operations, paths (glob), mode }
                                ↓
PermissionChecker::check(op, path) → PermissionMode
```

三种模式：
- `Allow` — 正常执行
- `Deny` — 返回 permission-denied 错误
- `Interrupt` — 触发 HITL（`ToolCallAction::Stop`）

规则按顺序匹配，**首条匹配规则获胜**。无规则匹配时默认 `Allow`。

路径验证：必须以 `/` 开头，禁止 `..` 和 `~`。

### 4.3 HITL 中断系统 (Q4)

```
interrupt_on: HashMap<tool_name, InterruptPolicy>
                              ↓
HitlMiddleware::on_tool_call(event)
    ↓
    InterruptMap::check(ToolCallInfo)
    ↓
    ApprovalDecision::{Approve | Reject(reason) | RequireHuman}
    ↓
    ToolCallAction::{Run | skip(reason) | stop("awaiting approval")}
```

三种 `InterruptPolicy`：
- `Simple(bool)` — `true` = 永远要求人工，`false` = 从不中断
- `WithResolver { auto }` — 闭包 `Fn(&ToolCallInfo) -> ApprovalDecision`

`InterruptPolicy` 的 serde：`Simple(b)` ↔ JSON `bool`；`WithResolver` 序列化为 `true`（闭包不可序列化，反序列化后需运行时重新挂载）。

Pause + Resume：
- `PauseCheckpoint` — 序列化的 AgentRun 状态 + 待决 tool call 信息
- `ResumeInput::{Approve | Reject(String) | Edit { new_args }}` — 人工决策

### 4.4 Middleware 三件套 (Q5)

每个中间件是一个 struct，直接 `impl AgentHook`，无包装 trait。

| 中间件 | 贡献工具 | 拦截事件 | 可排除 |
|--------|---------|---------|--------|
| `FilesystemMiddleware` | write_file, read_file, edit_file, ls, glob, grep, (execute) | `on_completion_call` (preamble patch + active_tools), `on_tool_result` (eviction) | ❌ 受保护 |
| `SubAgentMiddleware` | task | `on_completion_call` (subagent prompt) | ❌ 受保护 |
| `SummarizationMiddleware` | — | `on_completion_call` (history truncation), `on_tool_result` (large result truncation) | ✅ 可排除 |
| `HitlMiddleware` (hitl.rs) | — | `on_tool_call` (approve/reject/stop) | ✅ 可排除 |

**受保护机制**：`HarnessProfile::is_middleware_excluded()` 对 `"Filesystem"` 和 `"SubAgent"` 硬编码返回 `false`。

### 4.5 DeepAgentBuilder (Q6)

18 参数 → Rust fluent builder：

```
DeepAgentBuilder<M = MockModelPlaceholder>
    .model::<NewM>(model)           // #1  (类型转换)
    .model_spec("openai:gpt-4o")    // #1  (字符串形式)
    .tool(ToolSpec)                 // #2
    .system_prompt("...")           // #3  (USER slot)
    .hook::<H: AgentHook>(hook)     // #4  (extra hooks)
    .subagent(SubAgentSpec)         // #5
    .skills(vec![PathBuf])          // #6
    .memory(vec![PathBuf])          // #7
    .permissions(vec![Perm])        // #8
    .backend(Arc<dyn Backend>)      // #9
    .interrupt_on(InterruptMap)     // #10
    .response_format(json)          // #11
    // #12 state_schema — 删除（Rust 无 TypedDict）
    .context(json)                  // #13
    .checkpointer(Arc<dyn CP>)      // #14
    .store(Arc<dyn Store>)          // #15
    .debug(true)                    // #16
    .name("agent-name")             // #17
    .cache(Arc<dyn Cache>)          // #18
    .mcp_server(McpServerConfig)    // #19 (纯增量, 无 Python 对应)
    .base_system_prompt("BASE")     // profile
    .system_prompt_suffix("SUFFIX") // profile
    .exclude_tool("dangerous")      // profile
    .exclude_middleware("Summ...")  // profile (protected: Filesystem, SubAgent)
    .build()                        // → Agent
    .build_runner(prompt)           // → AgentRunner
```

Prompt 组装顺序：`USER + "\n\n" + BASE + "\n\n" + SUFFIX`

Hook 组装顺序：标准中间件（按 push 顺序追加到 extra hooks 之前）→ `HookStack` → `AgentBuilder::add_hook(hook_stack)`。

### 4.6 MockCompletionModel (Q8)

FIFO 响应队列，实现 `CompletionModel` trait。

```rust
let model = MockCompletionModel::from_responses(vec![
    "first response",
    "second response",
]);
// model.completion(req).await → "first response"
// model.completion(req).await → "second response"
// model.completion(req).await → Error (queue empty)
```

构造器：
- `new()` — 单个空响应占位（builder/config 测试用，不实际调用模型）
- `single(text)` — 单响应
- `empty()` — 空队列（所有调用报错）
- `from_responses(Vec<String>)` — 多文本响应
- `from_content(Vec<Vec<AssistantContent>>)` — 多内容块响应（含 tool calls）
- `with_provider(name)` — 设置 provider 名

## 5. 关键实现约束

以下约束是在实现过程中发现的，不在 SPEC survey 中，已固化到 ADR：

| 约束 | 影响 | ADR |
|------|------|-----|
| `AgentHook` trait 不 dyn-compatible | 不能用 `Arc<dyn AgentHook>` | [ADR-002](adr/0002-hookstack-over-dyn-agenthook.md) |
| `CompletionModel` 的 `completion()`/`stream()` 返回 `impl Future` | builder 必须泛型 `M: CompletionModel` | [ADR-003](adr/0003-builder-generic-placeholder-model.md) |
| rig 中间件用 `impl AgentHook` 而非包装 trait | 中间件直接实现 trait，无 dyn 分发 | [ADR-004](adr/0004-middleware-impl-agenthook-directly.md) |
| juncture HITL 有 2 个上游断点 | 弃 juncture 改用 rig | [ADR-001](adr/0001-rig-as-base.md) |

### 5.1 rig API 适配备忘

实现中发现的 rig 0.42 API 细节（非文档化）：

- `Agent.config` 字段是 **private** — 测试只能用 `agent.name()` 等公开方法
- `Message::from_text()` 不存在 — 用 `Message::user(text)` / `Message::assistant(text)`
- `Message` 不实现 `Display` — 用 `rag_text() -> Option<String>` 提取文本
- `HookStack::push()` 只是 append（无 `push_front`）— hook 执行顺序是追加序
- `HookStack::len()` 可用于判断是否为空
- `AgentRunner::from_agent(&agent, prompt)` 需要 `&Agent` 引用
- `RequestPatch::new().preamble(s).active_tools(names).history(msgs)` — 链式 patch
- `CompletionResponse::new(choice, usage, provider)` — 构造响应
- `CompletionRequestBuilder::new(model, Message::user("...")).build()` — 构造请求

## 6. 错误系统 (Q26)

`deepagents-errors` crate 提供 ~40 个错误类型，按 7 个域组织：

```
Error enum
├── Io(std::io::Error)
├── Config(ConfigError)
├── Sandbox(SandboxError)      ← backend/permission 使用
├── Agent(AgentError)
├── Session(SessionError)
├── Tool(ToolError)
└── Provider(ProviderError)
```

所有错误实现 `miette::Diagnostic`，提供结构化诊断信息。`StartupErrorMarker` 标记可启动 vs 不可启动错误。

## 7. 编译与测试

### 编译命令

```bash
cargo check -p deepagents-core              # 无警告编译
cargo test -p deepagents-core               # 51 tests + 1 doctest
cargo check --workspace                      # 全 workspace 编译
```

### Cargo features

`deepagents-core` 的 Cargo.toml：

```toml
[features]
default = []
filesystem = ["walkdir"]    # FilesystemBackend + LocalShellBackend
composite = []              # CompositeBackend
```

`StateBackend`（默认）无 feature gate，始终可用。

### 测试策略

- **单元测试**：内联在每个模块的 `#[cfg(test)] mod tests` 中
- **Mock 模型**：`MockCompletionModel` 提供 FIFO 响应队列，无网络依赖
- **集成测试**：builder 的 `build()` 端到端组装验证
- 当前无独立的 `tests/` 目录

## 8. 待实现清单

按 SPEC §七 的 19-crate 布局，剩余 17 个 crate 为骨架状态（`lib.rs` 空声明 + `Cargo.toml` 依赖声明）。

下一批优先级：
1. `deepagents-config` — config.toml 6-layer resolver (Q11)
2. `deepagents-sessions` — rusqlite checkpoint (Q17)
3. `deepagents-sandbox` — trait provider, 6 providers (Q21)
4. `deepagents-cli` — clap, 13 subcommands (Q13)
5. `deepagents-tui` — ratatui TUI (Q14, Q24)
