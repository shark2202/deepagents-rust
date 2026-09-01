# deepagents-rust 设计规格

> **状态**: spec 先行，实现前共识已达成（Q1–Q26 全量勘测完成）
> **底座**: [rig](https://github.com/0xPlaygrounds/rig)（`rig-core` + `rig-agent`）
> **原版**: [LangChain Deep Agents Python SDK](https://github.com/langchain-ai/deepagents) (`libs/deepagents/`)
> **版本**: v0.1.0（pre-1.0，不承诺 semver）
> **许可**: Apache-2.0

---

## 一、项目定位

将 LangChain Deep Agents Python SDK（~27k 行）移植到 Rust，作为 `deepagents-rust`，使用 `rig`（`rig-core` + `rig-agent`）作为运行时基础。

- **纯库 SDK**：发布到 crates.io，非产品外壳
- **兼容对象**：deepagents Python SDK 自己的配置机制（`create_deep_agent` API 参数、skills/memory 路径、subagent 声明式 spec、permissions、backend、interrupt_on），不是 opcteam 的 `~/.opcteam/`
- **不自造平行机制**：复用 rig 的 CompletionModel / Tool / AgentHook / HookStack / AgentRun / MCP client

---

## 二、核心架构决策

### Q1 — 底座选择：juncture 废弃，rig 为底座

juncture 依赖路径不存在，`cargo check` 失败，旧 12 commits 全部不可复用。基于 rig 完全重写。

### Q2 — Scope：MVP 子集，分阶段（B）

v0 先交付可运行的最小子集，后续迭代扩展。

### Q2b — v0 范围

- **4 件套中间件**：Filesystem / SubAgent / Summarization / HITL
- **完整 tools/agent/subagent/mcp/skill 能力**
- **兼容 deepagents 自己的配置**
- **纯库 SDK**（非产品外壳）

### Q3 — 旧代码处理：完全删除，从零开始

完全删除旧 `src/`，从空仓库起步。push/force 等破坏性 git 操作仅用户手动执行。

---

## 三、关键技术决策

### Q4 — HITL（Human-in-the-Loop）：pause+resume 全链路

**机制**：

1. `on_tool_call` hook 检测需审批工具 → `ToolCallAction::Stop("awaiting approval")` 挂起
2. `AgentRun` serde 序列化存 checkpoint（rig 原生设计意图——`AgentRun` impl `Serialize + Deserialize`，sans-IO 状态机，官方文档明示"反序列化挂起的 run 后重导出 resolution context"）
3. caller 人工决策（approve / reject / edit）
4. 反序列化重建 `AgentRun` → `tool_results()` 喂回人工结果 → `next_step()` 续跑

**InterruptPolicy 形状（中等）**：

```rust
enum InterruptPolicy {
    Simple(bool),                    // approve/reject 二元
    WithResolver { auto: Resolver }, // 白名单路径自动批准，危险操作强制审批
}
type Resolver = Arc<dyn Fn(&ToolCall) -> ApprovalDecision + Send + Sync>;
enum ApprovalDecision { Approve, Reject, RequireHuman }
```

**原版 `interrupt_on` dict 映射**：

```rust
// 原版: interrupt_on = {"write_file": True, "execute": {"resolver": ...}}
// Rust:
HashMap<String, InterruptPolicy>
```

**rig HITL 能力依据**：

| rig 原语 | 能力 | HITL 映射 |
|---|---|---|
| `ToolCallAction::Stop(String)` | 停止 run | pause 挂起 |
| `AgentRun` serde | `impl Serialize + Deserialize` | checkpoint 持久化 |
| `AgentRun::pending_invalid_tool_call()` doc | "re-derive resolution context after deserializing a suspended run" | 官方设计意图 = 可序列化挂起续跑 |
| `AgentRun` sans-IO 步进 | `next_step()→CallModel/CallTools/Done` + `model_response()`/`tool_results()` 喂回 | pause→存→重建→喂回→续 |

### Q5 — 中间件 trait 形状：impl AgentHook，无包装 trait

**映射表**：

| deepagents hook | 签名 | rig 映射 |
|---|---|---|
| `before_agent(state, runtime, config)` | run 开始时执行一次 | ❌ 无直接对应 → **builder 初始化吸收** |
| `wrap_model_call(request, handler)` 前半 | 改 req（system prompt / 工具 / 消息） | `on_completion_call` → `RequestPatch` |
| `wrap_model_call(request, handler)` 后半 | 改 resp | `on_model_turn_finished` → `ModelTurnAction` |
| `wrap_tool_call(request, handler)` 前半 | 拦截/改参数 | `on_tool_call` → `ToolCallAction` |
| `wrap_tool_call(request, handler)` 后半 | 改 result | `on_tool_result` → `ToolResultAction` |
| `after_agent(state, runtime)` | run 结束，可 loop-back | `on_model_turn_finished` → `Retry` |

**`before_agent` GAP 处理**：原版 `before_agent` 的三件事在 rig 下被自然吸收：
- Memory/Skills 加载 → `AgentBuilder` / `AgentRun::with_history()` 构造时完成
- PatchToolCalls（dangling tool calls）→ `AgentRun::pending_invalid_tool_call()` 在反序列化续跑时处理

**组件模式**：原版一个 middleware = Rust 一个组件 struct，同时贡献 (a) 工具集 + (b) `impl AgentHook`：

```rust
struct FilesystemMiddleware { /* state */ }
impl FilesystemMiddleware {
    fn tools(&self) -> Vec<Tool> { /* 贡献工具 → AgentBuilder */ }
}
impl AgentHook for FilesystemMiddleware {
    // on_completion_call: 注入 system prompt + 动态过滤工具
    // on_tool_result: 大结果驱逐到文件
}
```

**v0 四件套中间件映射**：

| 中间件 | 工具供给 | `on_completion_call` | `on_tool_call` | `on_tool_result` | `on_model_turn_finished` |
|---|---|---|---|---|---|
| **Filesystem** | write_file/read_file/edit_file/ls/glob/grep/execute | 注入 fs 指令 + 动态过滤 | — | 大结果驱逐到文件 | — |
| **SubAgent** | task tool | 注入 subagent 使用说明 | — | — | — |
| **Summarization** | — | 截断消息/历史 offload/arg truncation | — | — | — |
| **HITL** | — | — | `Stop("awaiting approval")` if tool in interrupt_on | — | — |

### Q6 — 配置规范：create_deep_agent → Rust builder

**18 参数映射**：

| # | 原版参数 | 原版类型 | Rust 映射 |
|---|---|---|---|
| 1 | `model` | `str \| BaseChatModel \| None` | `.model(impl CompletionModel)` 或 `.model_spec("provider:model")` |
| 2 | `tools` | `Sequence[BaseTool \| Callable \| dict]` | `.tool(Tool)` / `.tools(Vec<Tool>)` |
| 3 | `system_prompt` | `str \| SystemMessage \| None` | `.system_prompt(String)` — USER slot |
| 4 | `middleware` | `Sequence[AgentMiddleware]` | `.hook(impl AgentHook)` / `.hooks(...)` |
| 5 | `subagents` | `Sequence[SubAgent \| CompiledSubAgent \| AsyncSubAgent]` | `.subagent(SubAgentSpec)` |
| 6 | `skills` | `list[str]` | `.skills(Vec<PathBuf>)` — SKILL.md 发现 |
| 7 | `memory` | `list[str]` | `.memory(Vec<PathBuf>)` — AGENTS.md 加载进 system prompt |
| 8 | `permissions` | `list[FilesystemPermission]` | `.permissions(Vec<FilesystemPermission>)` |
| 9 | `backend` | `BackendProtocol \| None` | `.backend(Box<dyn Backend>)` |
| 10 | `interrupt_on` | `dict[str, bool \| InterruptOnConfig]` | `.interrupt_on(HashMap<String, InterruptPolicy>)` |
| 11 | `response_format` | `ResponseFormat \| type \| dict` | `.response_format(Schema)` |
| 12 | `state_schema` | `type[DeepAgentState]` | ~~删除~~ — Rust 无 TypedDict 反射，middleware 各持状态 |
| 13 | `context_schema` | `type[ContextT]` | `.context(Context)` 泛型 |
| 14 | `checkpointer` | `Checkpointer \| None` | `.checkpointer(Box<dyn Checkpointer>)` |
| 15 | `store` | `BaseStore \| None` | `.store(Box<dyn Store>)` |
| 16 | `debug` | `bool` | `.debug(bool)` |
| 17 | `name` | `str \| None` | `.name(String)` |
| 18 | `cache` | `BaseCache \| None` | `.cache(Box<dyn Cache>)` |

**SubAgent 三态**：

| 原版形式 | Rust 映射 | v0？ |
|---|---|---|
| `SubAgent`（声明式 TypedDict） | `SubAgentSpec` struct（serde）→ 递归构建子 agent | ✅ |
| `CompiledSubAgent`（预编译 runnable） | `Box<dyn AgentRunner>` trait object | ✅ |
| `AsyncSubAgent`（远程 background） | `AsyncSubAgentSpec` struct | ❌ 推迟 |

**SubAgentSpec 字段**：

```rust
struct SubAgentSpec {
    name: String,           // required
    description: String,    // required
    system_prompt: String,  // required
    tools: Option<Vec<Tool>>,
    model: Option<String>,  // "provider:model"
    hooks: Option<Vec<Box<dyn AgentHook>>>,
    interrupt_on: Option<HashMap<String, InterruptPolicy>>,
    skills: Option<Vec<PathBuf>>,
    permissions: Option<Vec<FilesystemPermission>>,
    response_format: Option<Schema>,
}
```

**interrupt_on 继承规则**：
- 声明式 `SubAgentSpec` 默认继承顶层 `interrupt_on`
- 若 `SubAgentSpec.interrupt_on` 存在 → **整体替换**（非合并）
- `CompiledSubAgent` / `AsyncSubAgent` 不继承

**HarnessProfile**：

```rust
struct HarnessProfile {
    base_system_prompt: Option<String>,
    system_prompt_suffix: Option<String>,
    tool_description_overrides: HashMap<String, String>,
    excluded_tools: HashSet<String>,
    excluded_middleware: HashSet<String>,  // by hook name
    extra_middleware: Vec<Box<dyn AgentHook>>,
    general_purpose_subagent: GeneralPurposeSubagentProfile,
}
```

- v0：单一默认 profile 起步，注册表 API 留扩展位
- prompt 装配序：`USER (system_prompt) → BASE (profile.base_system_prompt) → SUFFIX (profile.system_prompt_suffix)`
- `excluded_middleware` 按 hook 名匹配，受保护集（Filesystem/SubAgent hook）不可排除
- `general-purpose` 默认子 agent 自动注入（除非 profile 禁用或已提供同名）

**MCP = 纯增量第 19 能力**：

```rust
DeepAgentBuilder::mcp_server(McpServerConfig)  // stdio/SSE/HTTP
```

- 原版无 MCP（grep 全仓零匹配）
- 由 rig MCP client 实现，不对标原版任何参数
- 纯增量，不破坏兼容性

### Q7 — Backend / Permission

**Backend trait 形状（对齐原版两层协议）**：

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    async fn ls(&self, path: &str) -> Result<LsResult>;
    async fn read(&self, path: &str, offset: usize, limit: usize) -> Result<ReadResult>;
    async fn grep(&self, pattern: &str, path: Option<&str>, glob: Option<&str>, max_count: Option<usize>) -> Result<GrepResult>;
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult>;
    async fn write(&self, path: &str, content: &str) -> Result<WriteResult>;
    async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult>;
    async fn delete(&self, path: &str) -> Result<DeleteResult>;
    async fn info(&self, path: &str) -> Result<FileInfo>;
    async fn list_files(&self) -> Result<Vec<FileInfo>>;
}

#[async_trait]
pub trait SandboxBackend: Backend {
    async fn execute(&self, command: &str, timeout: Option<u64>) -> Result<ExecuteResponse>;
    fn id(&self) -> &str;
}
```

- 原版同步/异步两版 → Rust 统一 `async`
- `SandboxBackend: Backend`（supertrait），对齐原版 `SandboxBackendProtocol(BackendProtocol)`
- `FilesystemMiddleware` 的 `on_completion_call` 检查 backend 是否 `impl SandboxBackend` → 决定是否暴露 `execute` 工具

**v0 backend 实现范围：4 个**

| Rust 实现 | 对应原版 | 说明 |
|---|---|---|
| `StateBackend` | StateBackend | 内存 `HashMap<String, FileData>`，ephemeral，默认 |
| `FilesystemBackend` | FilesystemBackend | std::fs，强制 root_dir 沙箱 |
| `LocalShellBackend` | LocalShellBackend | 磁盘 + `std::process::Command` |
| `CompositeBackend` | CompositeBackend | 路径前缀最长匹配路由 |

推迟 2 个：
- `StoreBackend`：依赖 LangGraph BaseStore 持久化机制，Rust 无对应物
- `BaseSandbox`：抽象基类，需具体 sandbox 实现（Docker/VM/远程）

**Permission 执行（工具层，非 backend 层）**：

```rust
enum FilesystemOperation { Read, Write }
struct FilesystemPermission {
    operations: Vec<FilesystemOperation>,
    paths: Vec<String>,  // glob patterns, 必须 "/" 开头，禁 ".." 和 "~"
    mode: PermissionMode,
}
enum PermissionMode { Allow, Deny, Interrupt }

impl FilesystemMiddleware {
    fn check_permission(&self, op: FilesystemOperation, path: &str) -> PermissionMode {
        for rule in &self.permissions {
            if rule.operations.contains(&op) && rule.matches(path) {
                return rule.mode;
            }
        }
        PermissionMode::Allow  // no match = allow
    }
}
```

- `deny` → 工具返回 permission-denied 错误
- `interrupt` → `on_tool_call` 返回 `ToolCallAction::Stop`（触发 HITL pause+resume）
- `allow` → 正常执行
- glob 匹配用 `globset` crate
- 路径验证：强制 `/` 开头，禁 `..` 和 `~`
- **Permission 与 HITL 联动**：permissions 中有 interrupt-mode 规则时，自动生成 InterruptPolicy + 注册 HitlHook（与用户显式 interrupt_on 合并，用户优先）
- **子 agent permission 继承**：不指定 → 继承父；指定 → 整体替换；CompiledSubAgent → 自管理

**grep 实现**：纯 Rust `grep` crate（ripgrep 底层库），零外部进程依赖

### Q8 — 测试 / CI / 分发

| 维度 | 决策 |
|---|---|
| 测试金字塔 | 单元（内联 `#[tokio::test]`）+ 集成（mock model）+ doc tests；v0 无 E2E |
| Mock model | `MockCompletionModel: impl CompletionModel`，队列消费预设响应 |
| CI | GitHub Actions，三平台（ubuntu/macos/windows），fmt + clippy(-D warnings) + test + doc |
| 分发 | crates.io 纯库，`default` 全开 + `minimal` opt-out（reqwest/tokio 模式） |
| 版本 | 0.1.0 重置，pre-1.0 不承诺 semver |
| 路径系统 | POSIX 虚拟路径（平台无关），仅落盘时转 `std::path::Path` |
| Windows 兼容 | 虚拟路径层完全平台无关；LocalShell `#[cfg(windows)]` 选 shell |

**Feature gate**：

```toml
[features]
default = ["filesystem", "composite", "mcp"]   # 全开
minimal  = []                                     # 仅 StateBackend + 核心中间件
filesystem = ["dep:grep", "dep:walkdir"]
composite = []
mcp = ["rig-core/mcp"]
```

**Cargo.toml 修正**：

```toml
[package]
name = "deepagents"
version = "0.1.0"
edition = "2024"
rust-version = "1.88"
license = "Apache-2.0"
description = "Rust port of the LangChain Deep Agents SDK, built on rig"

[dependencies]
rig-core = "..."
rig-agent = "..."
# ...
```

---

## 四、设计原则贯穿

| 原则 | 体现 |
|---|---|
| 稳定可靠 | rig 原生 AgentRun serde（官方设计意图），不自造 interrupt runtime |
| 方便维护 | 无平行机制，middleware = impl AgentHook，复用 HookStack |
| 安全高效 | Stop 在工具执行前触发；root_dir 强制沙箱；permission 工具层拦截 |
| 模块化/插件化 | 每个组件独立装卸，注册到 builder + HookStack |
| spec 先行 | Q1-Q9 全部决策先行，实现后做 |
| 重复利用 rig | CompletionModel/Tool/AgentHook/HookStack/AgentRun/MCP client |
| 不承诺未验证能力 | v0 无 E2E 骨架；rig serde 已 docs.rs 验证 |
| 全平台兼容 | POSIX 虚拟路径 + 三平台 CI |

---

## 五、rig 关键 API 参考

| rig 原语 | 用途 |
|---|---|
| `CompletionModel` trait | 模型抽象（provider 20+） |
| `Message` / `Tool` / `PortableTool` | 消息与工具 |
| `Agent` / `AgentBuilder` | agent 构建 |
| `AgentRun` | sans-IO 状态机，`impl Serialize + Deserialize`，可步进 |
| `AgentRunner` | 驱动 AgentRun |
| `AgentHook` trait | 拦截面 |
| `HookStack` / `HookContext` | hook 组合 |
| `Scratchpad` | hook 间共享状态 |
| `ToolCallAction` | `Run` / `Rewrite(Value)` / `Skip(String)` / `Stop(String)` |
| `ToolResultAction` | `Keep` / `Rewrite(ToolOutput)` / `Stop(String)` |
| `ModelTurnAction` | `Continue` / `Retry` / `Stop` |
| `RequestPatch` | `on_completion_call` 返回值 |

---

## 六、原版参考结构

- **原 Python 源码**: `libs/deepagents/deepagents/`（graph.py + middleware/* + backends/* + profiles/*）
- **原版默认栈 14 层**: Skills/Filesystem/SubAgent/Summarization/PatchToolCalls/AsyncSubAgent/(user)/profile.extra/ToolExclusion/AnthropicPromptCaching/BedrockCaching/FireworksCaching/Memory/HumanInTheLoop
- **原版 `_REQUIRED_MIDDLEWARE`**: FilesystemMiddleware + SubAgentMiddleware（不可被 excluded）
- **原版中间件 4 个 hook**: `before_agent` / `wrap_model_call` / `wrap_tool_call` / `after_agent`
- **原版 backend 6 个**: StateBackend / FilesystemBackend / LocalShellBackend / CompositeBackend / StoreBackend / BaseSandbox

---

## 七、产品架构（Q10）

### 产品形态：axum HTTP/SSE server + 19-crate workspace

原版 `deepagents-code` 是 Python CLI/TUI 产品（~130k 行），运行 LangGraph agent 在子进程 server 中。Rust 版移植为 axum HTTP/SSE server + ratatui TUI。

### 19-crate workspace

```
deepagents-code/
├── crates/
│   ├── deepagents-core        # SDK 层: rig AgentRun + serde state (Q1-Q8)
│   ├── deepagents-config      # config.toml 6-layer resolver, 105 options (Q11)
│   ├── deepagents-env         # 55 env vars, dotenvy, 3-layer denylist (Q12)
│   ├── deepagents-cli         # clap, 13 subcommands (Q13)
│   ├── deepagents-tui         # ratatui, Screen/Modal, 47 modals, theme (Q14,Q24)
│   ├── deepagents-hooks       # 12 events, tokio async, Windows cmd/pwsh (Q15)
│   ├── deepagents-plugins     # JSON-RPC stdio, gix, rust-embed adapter (Q16)
│   ├── deepagents-sessions    # rusqlite+bundled, 18-channel ResumeState (Q17)
│   ├── deepagents-approval    # Manual/Auto/YOLO, classifier, HITL checkpoint (Q18)
│   ├── deepagents-mcp         # rmcp, .mcp.json, trust lists, OAuth (Q19)
│   ├── deepagents-skills      # 8-source discovery, SKILL.md, rust-embed (Q20)
│   ├── deepagents-sandbox     # trait provider, 6 providers, reqwest+rustls (Q21)
│   ├── deepagents-cost        # bundled JSON catalog, calc_price, CostState (Q22)
│   ├── deepagents-goal        # GoalStatus, GraderResponse, self-grading loop (Q23)
│   ├── deepagents-onboarding   # marker files, name memory (Q25)
│   ├── deepagents-update       # GitHub Releases, 5 install methods, auto-update (Q25)
│   ├── deepagents-doctor       # 4 sections + context-doctor (Q25)
│   └── deepagents-errors      # ~40 error types, structured diagnostics (Q26)
├── data/
│   └── catalog.json           # bundled pricing catalog (Q22)
└── skills/                    # built-in skills (rust-embed, Q20)
```

### HTTP/SSE server（axum + hyper + tokio）

- server 在 ephemeral port 启动，TUI 连接 SSE 流
- 端点：`POST /invoke`、`GET /events`（SSE）、`POST /approve`、`POST /interrupt`
- 原版用 subprocess + LangGraph checkpoint 协议 → Rust 用 axum 直接 in-process 或同机 HTTP

### 交叉编译基线（锁死）

**所有 crate 必须纯 Rust，零 C 依赖**。以下决策基于此约束：

| 原版依赖 | Rust 替代 | 理由 |
|---|---|---|
| LangGraph | rig AgentRun (sans-IO, serde) | 无 Python 运行时依赖 |
| Textual | ratatui + crossterm | 纯 Rust TUI 框架 |
| genai-prices (Python 包) | bundled JSON + calc_price Rust 重写 | 无 Python 包依赖 |
| git2 (libgit2 C) | gix (纯 Rust) | 零 C 依赖 |
| SQLite C | rusqlite + bundled | SQLite C 源码编译进二进制 |
| OpenSSL | reqwest with rustls | 纯 Rust TLS |

### 状态目录 `~/.deepagents/.state/`

```
sessions.db                         # SQLite (Q17)
approval.json                       # HITL 持久化 (Q18)
marketplaces.json                   # 插件市场
installed_plugins.json              # 已安装插件
plugin_enabled.json                 # 插件启用状态
conversation_history/               # 对话历史 archive (Q17)
mcp-tokens/                         # MCP OAuth token cache (Q19)
skill_trust.json                    # 技能信任列表 (Q20)
extension_trust.json                 # 扩展信任列表
plugins/                            # 插件安装目录
latest_version.json                  # 更新缓存 (Q25)
onboarding_complete                  # onboarding marker (Q25)
goal_auto_accept_prompt_shown        # goal prompt marker (Q25)
```

---

## 八、配置系统（Q11-Q12）

### Q11 — config.toml：6-layer ranked resolver

**6 层配置源**（rank 从高到低）：

| Rank | Source | 持久性 |
|---|---|---|
| 0 | CLI flags | ephemeral |
| 1 | Environment variables | ephemeral |
| 2 | Managed config (admin) | durable |
| 3 | Project config (`./deepagents/config.toml`) | semi-durable |
| 4 | User config (`~/.deepagents/config.toml`) | durable |
| 5 | Built-in defaults | immutable |

- **105 个 ConfigOption**，每个声明：key、type、default、env_name、cli_flag、merge_strategy、description
- **MergeStrategy**：`Replace`（高 rank 完全覆盖）/ `Merge`（深度合并）
- **tier_diagnostics**：每层拒绝原因记录在 `HashMap<Rank, Vec<String>>`
- **3 种 file-wide 诊断**：`SHADOWED_TABLE` / `UNUSABLE_SOURCE` / `RETAINED_SOURCE`（每代只发射一次）
- masked CLI flag 警告：`--yolo` 被 managed `manual` 覆盖时警告

**crate 选型**：`toml`（解析）+ `toml_edit`（保留注释的修改）

### Q12 — 环境变量：55 个 DEEPAGENTS_CODE_*

- **55 个环境变量**，全部 `DEEPAGENTS_CODE_` 前缀
- **3 层 denylist**：env var → config option 名映射时，逐层检查 denylist
  1. 全局 denylist（安全敏感，永不从 env 接受）
  2. managed policy denylist（admin禁止的选项）
  3. option-level denylist（单个 option 可标记 `env_deny = true`）
- **crate 选型**：`dotenvy`（`.env` 文件加载，纯 Rust）

---

## 九、CLI 与 TUI 命令（Q13-Q14）

### Q13 — CLI：clap derive, 13 subcommands

**13 个子命令**（排除 `--acp`）：

```
deepagents           # 默认 TUI 模式
deepagents -p <prompt>  # 单次 prompt 模式
deepagents --print   # 打印模式
deepagents serve     # server 模式
deepagents resume    # 恢复会话
deepagents config    # 配置管理
deepagents doctor    # 诊断
deepagents context-doctor  # 上下文诊断
deepagents mcp       # MCP 管理
deepagents plugins   # 插件管理
deepagents skills    # 技能管理
deepagents hooks     # hooks 管理
deepagents update    # 自更新
```

- **BooleanOptionalAction 等价**：`--flag` / `--no-flag` / 不指定 → clap `Option<bool>` + `#[arg(long)]` + `negatable` 自定义
- **crate 选型**：`clap`（derive 宏，纯 Rust）

### Q14 — TUI slash commands：47 全量

**47 个 slash 命令**（45 public + 2 hidden）：

| 分类 | 命令 |
|---|---|
| 会话 | `/compact`, `/clear`, `/resume`, `/save`, `/load` |
| 模型 | `/model`, `/effort` |
| 审批 | `/yolo`, `/auto`, `/manual`, `/approve`, `/reject` |
| 工具 | `/tools`, `/mcp`, `/sandbox` |
| 上下文 | `/cost`, `/context`, `/tokens`, `/timestamps`, `/scrollbar` |
| 目标 | `/goal`, `/rubric`, `/accept`, `/amend` |
| 技能 | `/skills`, `/skill` |
| 主题 | `/theme`, `/light`, `/dark` |
| 调试 | `/debug`, `/console` |
| 其他 | `/help`, `/quit`, `/restart`, `/cwd`, `/editor`, `/notifications`, `/subagents`, `/clipboard`, `/install`, `/update` |

**BypassTier 5 级**：
1. `None` — 无 bypass
2. `AutoApprove` — 自动审批（YOLO 模式）
3. `BypassHooks` — 绕过 hooks
4. `BypassApproval` — 绕过审批
5. `BypassAll` — 绕过全部（仅 hidden 命令可达）

---

## 十、扩展与集成（Q15-Q21）

### Q15 — hooks.json：12 events, 4 enums, 3 scope

**12 个 hook 事件**：

| 事件 | 触发时机 |
|---|---|
| `before_agent` | agent run 开始 |
| `after_agent` | agent run 结束 |
| `before_tool_call` | 工具调用前 |
| `after_tool_call` | 工具调用后 |
| `before_model_call` | 模型调用前 |
| `after_model_call` | 模型调用后 |
| `on_approval` | 审批请求 |
| `on_reject` | 拒绝操作 |
| `on_error` | 错误发生 |
| `on_session_start` | 会话开始 |
| `on_session_end` | 会话结束 |
| `on_compaction` | 上下文压缩 |

**4 个枚举**：`HookEvent`, `HookScope`, `HookAction`, `HookResult`
**3 个 scope**：`session` / `thread` / `global`

- 异步执行：`tokio` spawn
- Windows 支持：`cmd` + `pwsh`（5+7 shell 组合）
- **crate 选型**：`tokio` + `serde_json`

### Q16 — Plugin/extension：JSON-RPC over stdio

- **传输协议**：JSON-RPC 2.0 over stdio（与 LSP/MCP 一致）
- **Python adapter**：`rust-embed` 内嵌 Python adapter 脚本，运行时解压到临时目录
- **Git 操作**：`gix`（纯 Rust git 实现，零 C 依赖）
- **插件清单**：`plugin_manifest.json`（name, version, entry_point, permissions, tools）
- **信任列表**：`extension_trust.json`，手动 trust/untrust
- **crate 选型**：`rust-embed` + `gix` + `serde_json`
- **第三方沙箱 provider**：同样通过 JSON-RPC over stdio adapter

### Q17 — Sessions/resume：rusqlite + bundled

- **SQLite driver**：`rusqlite` + `bundled` feature（SQLite C 源码编译进二进制，交叉编译安全）
- **18-channel ResumeState**：会话状态 18 个 channel（messages, tool_calls, cost, goal, rubric, approval, compaction, model_spec, 等）
- **conversation_history archive**：压缩后存 `conversation_history/` 目录，按 thread_id 组织
- **crate 选型**：`rusqlite`（bundled feature）+ `serde`

### Q18 — Approval modes：Manual/Auto/YOLO + classifier

**3 种审批模式**：

| 模式 | 行为 |
|---|---|
| Manual | 每个危险操作需人工审批 |
| Auto | classifier 自动判定（allow/deny/require_human） |
| YOLO | 全部自动批准（BypassTier=AutoApprove） |

**Classifier 双层策略**：
1. **Primary**：rig `with_structured_output<AutoDecisionBatch>`（schemars::JsonSchema derive）
2. **Fallback**：prompt-based JSON extraction（providers 不支持 schema mode 时）

- **9 个 AutoDecisionCategory**：scope_escalation, destructive_action, credential_access, external_sharing, security_bypass, persistence, protected_resource, trust_boundary, other_policy
- **5 个 DecisionDisposition**：deterministic_allow, classifier_allow, policy_deny, classifier_unavailable, require_human
- **CLASSIFIER_POLICY prompt**：~120 行，必须 verbatim 移植
- **Fallback 阈值**：`_CONSECUTIVE_DENIAL_FALLBACK = 3`，`_CONSECUTIVE_UNAVAILABLE_FALLBACK = 2`
- **HITL checkpoint interrupt**：AgentRun 在 tool 执行边界 pause，SSE 推送 approval request，用户响应后 resume

### Q19 — MCP client：rmcp

- **crate 选型**：`rmcp`（official Rust MCP SDK）
- **配置**：`.mcp.json`（servers, trust lists, OAuth config）
- **Trust lists**：reject-wins（任一 reject 则 reject）
- **OAuth 3 种 flow**：loopback redirect / device code / paste-back
- **环境变量扩展**：`${VAR}` / `${VAR:-default}`
- **Token cache**：`mcp-tokens/` 目录

### Q20 — Skills：8-source discovery

**8 个发现源**：
1. Built-in skills（rust-embed 内嵌）
2. `~/.deepagents/skills/`
3. `./.deepagents/skills/`
4. `~/.agents/skills/`（跨 agent 共享）
5. Project `skills/` 目录
6. Plugin 贡献的 skills
7. MCP 贡献的 skills
8. Marketplaces 安装的 skills

- **SKILL.md frontmatter**：YAML frontmatter（name, description, triggers, triggers_as_regex, containment）
- **containment allowlist**：skill 只能调用 allowlist 内的工具
- **skill_trust.json**：手动 trust/untrust
- **crate 选型**：`rust-embed` + `serde_yaml`

### Q21 — Sandbox：trait-based provider

**6 个 provider**（v0: 3 完整 + 3 stub）：

| Provider | v0 状态 | 实现 |
|---|---|---|
| LangSmith | ✅ 完整 | reqwest + rustls |
| Daytona | ✅ 完整 | reqwest + rustls |
| Vercel | ✅ 完整 | reqwest + rustls |
| AgentCore | stub | trait 占位 |
| Modal | stub | trait 占位 |
| Runloop | stub | trait 占位 |

- **trait-based**：`SandboxProvider` trait，每个 provider 独立实现
- **crate 选型**：`reqwest`（rustls，非 native-tls）

---

## 十一、成本与目标（Q22-Q23）

### Q22 — Cost tracking：bundled JSON catalog + calc_price Rust 重写

原版依赖 `genai_prices` Python 包。Rust 版替代方案：

- **bundled JSON pricing catalog**：`rust-embed` 内嵌 catalog.json
- **calc_price Rust 重写**：token × rate 分桶计算（input/output/cache/reasoning/audio）
- **auto-update**：`reqwest` 定期 fetch 最新 catalog（可选，`prices_auto_update` 控制）

**Usage struct**（严格对齐 genai-prices 语义）：

```rust
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: CacheWrites,  // generic/5m/1h
    pub input_audio_tokens: u64,
    pub output_audio_tokens: u64,
    pub output_reasoning_tokens: u64,
}
```

**CostState**（checkpoint channel）：
- `session_cost_usd: f64` — additive reducer
- `session_cost_transfers: HashMap<String, CostTransfer>` — subagent 成本转移

**SessionCostRecorder**：process-wide singleton，记录每个 model call 的 usage metadata，`drain` 是 destructive

**Provider name mapping**：`_LC_TO_GENAI_PROVIDER` — LangChain provider → catalog provider

### Q23 — Goal/Rubric：self-grading loop

**GoalStatus 4 种**：`active` / `paused` / `blocked` / `complete`

**字符限制**：`GOAL_APPLICATION_CHAR_LIMIT`（objective + criteria 总和）、`GOAL_STATUS_NOTE_CHAR_LIMIT = 4_000`

**GraderResponse**（structured output）：

```rust
pub struct GraderResponse {
    pub result: GraderVerdict,      // satisfied / needs_revision / failed
    pub explanation: String,
    pub criteria: Vec<CriterionEval>,
}

pub enum CriterionEval {
    Pass { name: String },
    Fail { name: String, gap: String },
}
```

**Self-grading loop**：
```
Agent 执行 → grader 评估 → GraderResponse
  ├─ satisfied → 结束
  ├─ needs_revision → 注入 feedback → agent 继续
  ├─ failed → 结束
  └─ grader_error → 结束
重复直到 satisfied/failed/max_iterations_reached(=3)/grader_error
```

**RubricResult 5 种**：satisfied / needs_revision / failed / max_iterations_reached / grader_error

**ReliableRubricMiddleware**：继承 SDK RubricMiddleware，扩展 dcode private channels

**4 budget middleware**：ContextBudget / ContextToolCallBudget / RepositoryToolBudget / WebSearchBudget（按 operation_id 隔离）

**INHERIT_RUBRIC_MODEL sentinel**：`_rubric_model_spec` 三态（absent / INHERIT / model spec）

---

## 十二、TUI 架构（Q24）

### 框架：ratatui + crossterm

原版用 Textual（retained-mode, CSS-driven, DOM-based）。ratatui 是 immediate-mode, programmatic。

**Screen/Modal 抽象层**（自建）：

```rust
pub trait Screen: Send {
    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme);
    fn handle_key(&mut self, key: KeyEvent, ctx: &mut AppContext) -> KeyResult;
    fn is_modal(&self) -> bool { false }
}

pub trait Modal: Screen {
    type Result;
    fn result(&self) -> Option<Self::Result>;
}
```

**主屏幕布局**：
```
┌─────────────────────────────────────────┐
│ _StaticHeader (optional)                 │
├─────────────────────────────────────────┤
│ ChatScroll (#chat)                       │
│   ├─ WelcomeBanner                        │
│   └─ Messages (O(1) append, 滑动窗口)      │
├─────────────────────────────────────────┤
│ BottomChrome                              │
│   ├─ SubagentPanel                        │
│   ├─ StartupTip (conditional)             │
│   ├─ GoalStatusPanel                      │
│   └─ ChatInput                            │
├─────────────────────────────────────────┤
│ StatusBar                                 │
└─────────────────────────────────────────┘
```

**~20 个 App 级键绑定**：escape=interrupt, ctrl+c=quit_or_interrupt, ctrl+d=quit_app, ctrl+t=toggle_subagent_panel, shift+tab=toggle_auto_approve, ctrl+o=toggle_tool_output, ctrl+g=open_editor, ctrl+r=open_prompt_clipboard, ctrl+n=open_notifications, ctrl+backslash=toggle_debug_console, approval keys (up/k, down/j, enter, y, 1/2/3, a, n, tab)

**47 个 ModalScreen 子类**：AgentSelector, AuthConfirm/Manager/Prompt, NotificationCenter/Detail/Settings, ThemeSelector, ModelSwitch/Selector, ContextUsage, EffortSelector, RestartPrompt, CwdSwitch/HookTrust, SkillTrust, DebugConsole, MCPLogin/Reconnect/Viewer, PromptClipboard, ResumeCompact, ColdCache, PluginManager, ThreadSelector/AgentSwitch, InstallConfirm, LaunchInit(3), Update(3), YoloModeNotice, AutoModeNotice, CodexAuth 等

**主题系统**：
- **ThemeColors**（~30 个 hex 字段）：brand palette（dark/light）、surface、border、text、primary、success、warning、error、muted、panel、skill/tool accent、mode colors
- **DEFAULT_THEME = "langchain"**（dark, tokyonight-inspired + LangChain blue）
- **11 个内置主题**：langchain, langchain-light, textual-dark, textual-light, ansi-dark, ansi-light, catppuccin-frappe, rose-pine, rose-pine-dawn, rose-pine-moon, tokyo-night
- **用户主题**：config.toml `[themes.<name>]` 覆盖颜色
- **4 层解析**：managed → env（`DEEPAGENTS_CODE_THEME`）→ user config.toml → DEFAULT_THEME
- **terminal_themes**：`[ui.terminal_themes][TERM_PROGRAM]` 按 terminal 映射

**CSS → Style 映射**：Textual `.tcss` → ratatui 程序化 `Style` + Theme 令牌传递，集中在 `style.rs`

**Messages O(1) append**：原版用 Textual "stream" layout，ratatui 自建滑动窗口 + 渲染缓存

**crate 选型**：`ratatui` + `crossterm` + `tokio`（全部纯 Rust）

---

## 十三、Onboarding 与自更新（Q25）

### Onboarding

**4 个 marker 文件**（`~/.deepagents/.state/`）：
- `onboarding_complete` — onboarding 完成
- `goal_auto_accept_prompt_shown` — goal auto-accept 提示已展示

**Onboarding flow**（TUI modals）：
1. `LaunchGoalCriteriaPreferenceScreen` — 是否启用 goal criteria
2. `LaunchNameScreen` — 询问用户名
3. `LaunchDependenciesScreen` — 依赖安装
4. 写入 memory name block → `mark_onboarding_complete()`

**Name memory**：`write_onboarding_name_memory(name)` 将用户名写入 AI 会话记忆，`extract_onboarding_name_block` / `strip_onboarding_name_markers` 解析/清理

### Self-update

原版是 Python 包，通过 uv/brew 升级。Rust 版是编译二进制，自更新机制不同。

**InstallMethod 5 种**：`CargoBinstall` / `Homebrew` / `Scoop` / `Standalone` / `Unknown`

- `detect_install_method()` — 通过可执行文件路径检测
- GitHub Releases 二进制下载（Standalone）：找匹配 target_triple 的 asset → 下载 → 验证 checksum → 原子替换

**版本检查**：
- `CACHE_TTL = 86_400`（24 小时）
- `latest_version.json` 缓存在 `~/.deepagents/.state/`
- `get_cached_update_available()` — 启动时快速路径（不联系上游）

**启动自更新冷却**：
- `STARTUP_AUTO_UPDATE_FAILURE_COOLDOWN = 24h`（失败后冷却）
- `RESUME_AUTO_UPDATE_GRACE_PERIOD = 7 天`（resume 时延迟自更新）

**crate 选型**：`reqwest`（rustls）+ `semver` + `chrono` + `sha2` + `tempfile`（全部纯 Rust）

### Doctor

**4 个 DiagnosticSection**（按显示顺序）：
1. **Diagnostics** — SDK 版本、commit hash、platform tag、build commit
2. **Updates** — 当前版本、最新版本、上次检查时间、自动更新状态
3. **Tracing** — tracing endpoint、gateway state、project
4. **Configuration** — managed config、user config、path 状态、fallback locations

**DiagnosticItem**：`label`, `value`, `ok`
**DiagnosticSection**：`title`, `items`, `ok`
**渲染**：树形渲染（tree connectors, status glyph, color coding, commit hash → GitHub 链接）

### Context doctor

**ContextDoctorReport**：
- `rows: Vec<ContextDoctorRow>` — 上下文审计行
- `injected_tokens` — 注入总 token 数
- `conversation_tokens` / `provider_tokens` — 对账

**审计项**：System prompt, AGENTS.md memory, Skills index, Built-in tool schemas, MCP servers, 三方对账

---

## 十四、错误与诊断系统（Q26）

### ~40 个错误类，7 个域

| 域 | 错误类型 |
|---|---|
| 启动 | `DeepAgentsHomeError`, `StartupError`（`STARTUP_ERROR_MARKER` stderr 标记） |
| 配置 | `ManagedConfigError`/`ManagedPolicyError`, `ConfigLoadError`, `ModelConfigError`（+ 5 个子类）, `UnknownProviderError`, provider 远程策略错误(3), extras 错误(5) |
| MCP | `MCPConfigError`, `MCPLoginCancelledError`, `MCPReauthRequiredError`, `ConfigErrorKind`(5 值), `ConfigResolutionError` |
| Approval/Classifier | `ClassifierDeadlineExceeded`, `ClassifierModelUnavailable`, `HITLIterationLimitError`, `ClientHookStopError`, `HookTransportInterruptError` |
| Session/Goal | `GoalStateSizeError`, `AutoCompactionBlockedError` |
| Sandbox/Plugin | `SandboxError`/`SandboxNotFoundError`, `PluginManifestError`/`PluginStateError`, `ExtensionError`, `MarketplaceError`, `ChecksumMismatchError` |
| Update | `NoWritableBinDirError` |
| Hook | `HookDiagnostic`（handler_id, code, message） |
| Tracing | `LangSmithLookupError` → `LangSmithImportError`/`LangSmithLookupTimeoutError`/`LangSmithApiError` → `LangSmithProjectNotFoundError` |
| Offload | `OffloadConflictError`/`OffloadUnavailableError`/`OffloadIndeterminateError` |
| TUI | `TextualAppError`, `ExternalEditorError` |

### `deepagents-errors` crate

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)] Home(#[from] HomeError),
    #[error(transparent)] Startup(#[from] StartupError),
    #[error(transparent)] Config(#[from] ConfigError),
    #[error(transparent)] Mcp(#[from] McpError),
    #[error(transparent)] Approval(#[from] ApprovalError),
    #[error(transparent)] Session(#[from] SessionError),
    #[error(transparent)] Goal(#[from] GoalError),
    #[error(transparent)] Sandbox(#[from] SandboxError),
    #[error(transparent)] Plugin(#[from] PluginError),
    #[error(transparent)] Update(#[from] UpdateError),
    #[error(transparent)] Hook(#[from] HookError),
    #[error(transparent)] Tracing(#[from] TracingError),
    #[error(transparent)] Offload(#[from] OffloadError),
    #[error(transparent)] Tui(#[from] TuiError),
    #[error(transparent)] Io(#[from] std::io::Error),
}
```

### 结构化诊断

```rust
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,  // Error / Warning / Info
    pub domain: DiagnosticDomain,       // Config / Mcp / Approval / ...
    pub code: String,
    pub message: String,
    pub source: Option<String>,
    pub detail: Option<String>,
}
```

### StartupErrorMarker

```rust
pub const STARTUP_ERROR_MARKER: &str = "DEEPAGENTS_STARTUP_ERROR:";
// parent 进程扫描 stderr 此标记，升级不透明退出码为可操作摘要
pub fn emit_startup_failure(e: &Error) { ... }
```

**crate 选型**：`thiserror`（derive 错误类型）+ `tracing`（结构化日志）

---

## 十五、Crate 选型汇总

| Q | 主题 | crate | 交叉编译 |
|---|---|---|---|
| Q1-8 | SDK 层 | rig-core + rig-agent + serde + schemars + tokio | ✅ |
| Q10 | 产品架构 | axum + hyper + tokio | ✅ |
| Q11 | config.toml | toml + toml_edit | ✅ |
| Q12 | 环境变量 | dotenvy | ✅ |
| Q13 | CLI | clap | ✅ |
| Q14 | TUI 命令 | clap + ratatui | ✅ |
| Q15 | hooks | tokio + serde_json | ✅ |
| Q16 | 插件 | rust-embed + gix + serde_json | ✅ |
| Q17 | sessions | rusqlite (bundled) + serde | ✅ |
| Q18 | 审批 | rig + schemars + tokio | ✅ |
| Q19 | MCP | rmcp + reqwest (rustls) | ✅ |
| Q20 | 技能 | rust-embed + serde_yaml | ✅ |
| Q21 | 沙箱 | reqwest (rustls) | ✅ |
| Q22 | 成本 | rust-embed + reqwest | ✅ |
| Q23 | goal/rubric | rig + schemars + uuid | ✅ |
| Q24 | TUI | ratatui + crossterm + tokio | ✅ |
| Q25 | onboarding/update/doctor | reqwest + semver + chrono + sha2 + tempfile | ✅ |
| Q26 | 错误/诊断 | thiserror + tracing | ✅ |

**全部 crate 纯 Rust，零 C 依赖，交叉编译 Windows/Linux/macOS 安全。**

---

## 十六、待验证项

### Q10-POC：rig AgentRun serde 兼容性验证

- **目标**：验证 rig `AgentRun` serde 序列化能否支撑 LangGraph state format 兼容
- **状态**：pending，不阻塞设计工作，可在实现前并行验证
- **验证内容**：`AgentRun` impl `Serialize + Deserialize`，序列化挂起的 run → 反序列化重建 → `tool_results()` 喂回 → `next_step()` 续跑
