# 子系统无损迁移验证报告

> **日期**: 2026-09-01
> **范围**: hooks / MCP / 插件-extension / md-agent-memory / subagent / remote-agent / skill
> **方法**: 7 个 sub-agent 并行勘测原版 Python 源码，逐行比对 Rust 迁移可行性

---

## 一、总览

| # | 子系统 | 评估 | 一句话结论 |
|---|---|---|---|
| 1 | **hooks** (Q15) | ✅ 完全无损 | 纯外部子进程协议，语言无关，所有 Python 机制有 Rust 等价物 |
| 2 | **md agent / AGENTS.md memory** | ✅ 完全无损 | 文件发现→读 markdown→拼进 system prompt，无 Python 黑魔法 |
| 3 | **MCP** (Q19) | ⚠️ 有缺口 | 协议层 rmcp 覆盖，但遗留 SSE / GitHub device flow / 中间层需自补 |
| 4 | **插件 / extension** (Q16) | ⚠️ 有缺口 | 声明式 plugins 无损；Python 原生 extensions **不可迁移** |
| 5 | **subagent** (Q6) | ⚠️ 有缺口 | 核心闭环可迁移，但 Command 回写 / ToolRuntime / interrupt_on / GP 需补 |
| 6 | **remote-agent (AsyncSubAgent)** | ⚠️ 有大缺口 | 需重写 Agent Protocol 客户端 + 状态层，当前是降级实现 |
| 7 | **skill** (Q20) | ⚠️ 有缺口 | 核心数据流无损，但 middleware 生命周期 / symlink 语义 / trust 模态需重写 |

**"完全兼容、无损迁移"对 7 项中的 2 项成立，4 项部分成立，1 项子集不成立。**

---

## 二、完全无损（2 项）

### 2.1 hooks (Q15) — ✅ 完全无损

**原版位置**: 产品层 `libs/code/deepagents_code/hooks/`（24 文件，~9400 行）。SDK 层无 hooks 子系统。

**架构**: 语言无关的外部子进程系统。Handler 唯一类型是 `type: "command"`（OS 子进程），无 Python callable、无 in-process handler、无 `@hook` 装饰器、无 importlib 动态加载。

**12 事件 / 3 scope / 配置 JSON / wire 协议 Claude 兼容 JSON**。

**Python 依赖 → Rust 等价**:

| Python | Rust 等价 | 无损 |
|---|---|---|
| `functools.singledispatch` | `match event { ... }` | ✅ |
| Pydantic `BaseModel` + discriminator | `serde` + `#[serde(tag = "...")]` | ✅ |
| `asyncio.create_subprocess_*` | `tokio::process::Command` | ✅ |
| `ctypes.WinDLL("kernel32")` | `windows-sys` crate | ✅ |
| `re` 模块 | `regex` crate | ✅ |
| `__pydantic_extra__` 未知字段报告 | `#[serde(flatten)] extra: HashMap<String, Value>` | ✅ |

**5 个实现差异点（均有直接等价方案，非缺口）**:
1. `sys.executable` 遗留迁移适配器 → 内联实现或随 legacy 一并移除
2. `singledispatch` 事件分发 → `match`
3. Pydantic 未知字段报告 → `serde(flatten)`
4. LangGraph 运行时耦合（server_middleware.py 1468 行）→ 胶水层重写（hooks 核心不依赖 LangGraph）
5. `ctypes` Win32 FFI → `windows-sys`

### 2.2 md agent / AGENTS.md memory — ✅ 完全无损

**原版位置**: SDK 层 `libs/deepagents/deepagents/middleware/memory.py`（核心）+ 产品层 `project_utils.py` / `onboarding.py` / `memory_guard.py`。

**架构**: 发现 AGENTS.md → 读取纯 markdown → 剥离 HTML 注释 → 格式化 → 拼到 system message 末尾 → 幂等加载。

**发现路径**: `$DEEPAGENTS_HOME/<profile>/<agent>/AGENTS.md` → `<project>/.deepagents/AGENTS.md` → `<project>/AGENTS.md`（两个都加载，非择一）。

**安全**: symlink 循环检测（`Path.resolve(strict=True)`），解析后必须 `relative_to(project_root)`。

**写入机制**: agent 通过 `edit_file`/`write_file` 工具改写 AGENTS.md 文件本身（非专用 API）。

**Python 依赖 → Rust 等价**:

| Python | Rust 等价 | 无损 |
|---|---|---|
| LangChain `AgentMiddleware` / `wrap_model_call` | 自建中间件 trait + 显式调度 | ✅ |
| `PrivateStateAttr` + `AgentState` TypedDict | Rust struct + 私有字段 | ✅ |
| `difflib.SequenceMatcher` (行级 diff) | `similar` crate (Myers diff) | ✅ |
| `Path.resolve(strict=True)` + symlink loop | `std::fs::canonicalize` + 手动 ELOOP 校验 | ✅ |
| `asyncio.to_thread` | `tokio::task::spawn_blocking` | ✅ |
| `re.compile(r"<!--.*?-->")` | `regex` crate | ✅ |
| `os.O_NOFOLLOW` | `OpenOptions::symlink(false)` | ✅ |

**7 个实现细节对齐点（非阻断，需测试覆盖）**: symlink 循环检测跨版本差异、SequenceMatcher opcode 对齐、受管块恢复语义、`add_cache_control` 仅 ChatAnthropic、加载幂等性表达、项目级双文件都加载、onboarding 块 HTML 注释 marker 剥离顺序。原版有 `tests/unit_tests/test_memory_guard.py` / `test_onboarding.py` 可直接作为迁移验收基准。

---

## 三、有缺口（3 项，可达成功能等价但非 1:1 无损）

### 3.1 MCP (Q19) — ⚠️ 有缺口

**原版位置**: 产品层 `libs/code/deepagents_code/mcp_tools.py`（3359 行）+ `mcp_auth.py`（2250 行）。SDK 层零 MCP 引用。

**3 个缺口**:

| # | 缺口 | 原版位置 | rmcp 覆盖 | 自补量 |
|---|---|---|---|---|
| 1 | 遗留 HTTP+SSE transport (`type: "sse"`) | `mcp_tools.py:2502` `SSEConnection(transport="sse")` | ❌ rmcp 明确"intentionally not provided" | 需自建或前置 proxy |
| 2 | GitHub Device Flow (RFC 8628) | `mcp_auth.py:1863` `_run_device_flow` | ❌ rmcp 无 device flow | ~100 行 reqwest 自写轮询 |
| 3 | langchain_mcp_adapters 中间层 | `mcp_tools.py:501,2060-2061,2190,2432` | ❌ 含私有 API + LangChain StructuredTool + AnyIO cancel-scope | 整套重写 tool 包装层 |

**rmcp 覆盖的功能**（✅）: stdio transport、streamable HTTP、SSE 响应、`list_tools(cursor)` 分页、`call_tool`、initialize 握手、OAuth Authorization Code + PKCE(S256)、Dynamic Client Registration (RFC 7591)、Protected Resource Metadata (RFC 9728)、AS Metadata (RFC 8414)、token 刷新、401 自动检测。

**次要缺口**: `FileTokenStorage`（rmcp 仅 `InMemoryCredentialStore`，需自实现 trait ~200 行）、loopback callback server（rmcp 无内置，需 axum/hyper 自建 ~100 行）、provider 注册表（纯逻辑可直接移植）、`.mcp.json` 环境变量扩展（纯字符串逻辑可直接移植）。

**判定**: 协议层完全可迁移到 rmcp；3 个缺口约 ~500 行自补代码即可达成功能等价。但非"无损 1:1 移植"。

### 3.2 subagent (Q6) — ⚠️ 有缺口

**原版位置**: SDK 层 `libs/deepagents/deepagents/middleware/subagents.py`（747 行）+ `async_subagents.py`（931 行）+ `graph.py`（943 行）。

**三态**: `SubAgent`（声明式同步）→ `create_sub_agent` 编译；`CompiledSubAgent`（预编译 runnable）；`AsyncSubAgent`（远端异步）。

**task 工具调用链**: `task(description, subagent_type, runtime)` → 取注册表子 agent → 构造全新 state（剥离 messages/todos/structured_response/private_state_keys）→ `subagent.invoke()` → 取末条非空文本 AIMessage 或 structured_response → 返回 `Command(update={**state_update, "messages": [ToolMessage]})`。

**4 个关键缺口**:

| # | 缺口 | 原版位置 | rig 现状 | 影响 |
|---|---|---|---|---|
| 1 | `Command(update=)` 状态回写 | `subagents.py:509-514,570` | `Tool::call` 只返回 `ToolOutput`，无状态回写通道 | 子 agent 无法向父回传自定义 state 字段 |
| 2 | `ToolRuntime` 父 state 传递 | `subagents.py:21,547` | `ToolContext` 不暴露父 state | 无法剥离父 state 构造子 agent 输入 |
| 3 | `interrupt_on` 继承 + permissions→interrupt 合并 | `graph.py:719-723`, `_fs_interrupt.py:156-183` | rig HITL `ToolCallAction` 与 LangChain `InterruptOnConfig(when)` 不同构 | `when` 谓词（path 匹配）需在 rig hook 层重新实现 |
| 4 | `general-purpose` 自动注入 + middleware 继承 | `graph.py:744-813` | 依赖中间件 `.name` 标识，rig middleware 无 `fn name()` | GP middleware 栈重建 + `_gp_inheritable` 按 name 匹配需自建 |

**rig 无原生子 agent 机制**——需在 deepagents-rust 层用 `Agent` + `Tool` + `HookStack` + `AgentRun` 自建完整 subagent 中间件。

**其他缺口**: `structured_response` 多态序列化（Pydantic/dataclass → serde）、动态 `response_format` per-call 注入、`resolve_model('provider:model')` 字符串解析、LangSmith tracing tag。

**判定**: 核心闭环（task 工具 + 注册表 + 隔离 invoke + 末消息提取）语言无关可迁移，但 4 个关键缺口需补齐才语义对等。约 ~1000 行桥接代码。

### 3.3 skill (Q20) — ⚠️ 有缺口

**原版位置**: SDK 层 `libs/deepagents/deepagents/middleware/skills.py`（~1000 行核心）+ 产品层 `skills/load.py` / `skills/trust.py` / `skills/merge.py`。

**架构**: 8 源 discovery → frontmatter 解析 → progressive disclosure（只注入 name+description+path 到 system prompt，模型按需 `read_file` 读全文）→ last-wins 合并。

**3 个关键缺口**:

| # | 缺口 | 原版位置 | 影响 |
|---|---|---|---|
| 1 | LangChain middleware 生命周期 | `skills.py:107-118,870-940` | `AgentMiddleware` 的 `before_agent`/`wrap_model_call`/`PrivateStateAttr`/`TracePolicy`/`request.override()` 无 Rust 等价物，需自建中间件 trait |
| 2 | `Path.resolve()` symlink 语义 | `load.py:184`, `trust.py:505-540` | containment 安全核心"批准存原样、读取时 resolve-to-self 复验"双层防 swap，Rust `canonicalize` 跨平台边界条件不同 |
| 3 | Textual `SkillTrustScreen` 信任模态 | `tui/widgets/skill_trust.py` | `_pushscreen_wait` 异步阻塞弹窗，ratatui 无等价模态栈，需自建 |

**SPEC 过度声称修正**: `allowed_tools` **只在 system prompt 里打印提示，没有任何代码用它过滤工具集**。原版 `skills.py:879-880` 仅展示 `-> Allowed tools: ...`。这是"建议"非"限制"。

**原版 built-in skills 是文件系统读取**（`Path(__file__).parent / "built_in_skills"`），**不是二进制内嵌**。SPEC 称"rust-embed 内嵌"是移植设计选择而非原版机制。

**可无损迁移的部分**: frontmatter 正则与字段约束、name 校验规则、8 源优先级与 last-wins 合并、`SKILLS_SYSTEM_PROMPT` 模板与三槽注入、trust store JSON schema、`load_skill_content` allowlist 判定逻辑。

---

## 四、有大缺口 / 降级实现（1 项）

### 4.1 remote-agent / AsyncSubAgent — ⚠️ 有大缺口

**原版位置**: SDK 层 `libs/deepagents/deepagents/middleware/async_subagents.py`（931 行）。

**通信本质**: HTTP/REST + JSON（非 WebSocket/SSE）——对 Rust 友好，reqwest 即可复刻。

**5 工具**: `start_async_task` (POST /threads + POST /threads/{tid}/runs) / `check_async_task` (GET runs + GET thread values) / `update_async_task` (POST runs with `multitask_strategy=interrupt`) / `cancel_async_task` (POST cancel) / `list_async_tasks` (并发 GET 各 run)。

**状态持久化**: `async_tasks: Annotated[dict, _tasks_reducer]` 挂在 `AgentState`，survive context compaction。

**15 个缺口**（当前 Rust 实现是降级）:

| # | 缺口 | 严重度 |
|---|---|---|
| 1 | `update_async_task` 工具完全缺失 | 高 |
| 2 | `multitask_strategy="interrupt"` 语义无法表达 | 高 |
| 3 | `async_tasks` 状态持久化降级为进程内易失（重启即失） | 高 |
| 4 | `task_id` 用本地自增计数器（非远端 thread_id） | 高 |
| 5 | `run_id` 缺失 | 中 |
| 6 | 取消仅 `JoinHandle::abort`（无法取消远端 run） | 高 |
| 7 | 状态机值不一致（`Done` vs `success`，缺 `pending/timeout/interrupted`） | 中 |
| 8 | 3 个时间戳全缺（`created_at`/`last_checked_at`/`last_updated_at`） | 低 |
| 9 | list 无 live status fetch、无 `status_filter` 入参 | 中 |
| 10 | check 返回非结构化（扁平字符串 vs JSON） | 中 |
| 11 | 缺鉴权（`x-auth-scheme`/`headers`/API key） | 中 |
| 12 | 缺 ASGI in-process 传输 | 低 |
| 13 | 缺客户端缓存 `_ClientCache` | 低 |
| 14 | 远端 schema 耦合（强假设 `DeepAgentState` 同构） | 中 |
| 15 | 产品层 interrupt 策略挂载缺失 | 低 |

**判定**: 当前是把"远端后台 run 委派"降级成了"本地 spawn 的远端阻塞调用 + 进程内状态表"。要做到无损迁移，需用 reqwest 直接实现 Agent Protocol REST 客户端（threads/runs CRUD + multitask_strategy 参数），并把 `async_tasks` 接入 graph state reducer。约 ~1500 行新代码。

---

## 五、不可迁移（1 项子集）

### 5.1 插件 / extension 的 Python 原生 extensions — ❌ 不可迁移

**原版位置**: 产品层 `libs/code/deepagents_code/extensions/`（实验性，`DEEPAGENTS_CODE_EXPERIMENTAL=1` 时启用）。

**不可迁移原因**:

| # | Python 特有机制 | 原版位置 |
|---|---|---|
| 1 | `importlib.util.spec_from_file_location` + `exec_module` | `loader.py:31-44` |
| 2 | `importlib.metadata.entry_points(group="dcode.extensions")` | `discovery.py:141` |
| 3 | `sys.modules` 注入/回滚 | `loader.py:41,46,57,98,103` |
| 4 | `inspect.iscoroutinefunction` 校验 `async def` 工厂 | `loader.py:60` |
| 5 | in-process `AgentMiddleware` 贡献 | `api.py:86-107` |
| 6 | in-process `BaseTool`/callable 工具贡献 | `api.py:110-128` |
| 7 | `BackendProtocol` 虚拟存储路由 | `api.py:131-160` |
| 8 | Python callable shutdown hook | `api.py:163-178` |
| 9 | plugin manifest `pythonExtensions` 耦合 | `manifest.py:27,216-247` |

**结论**: 无法用纯 Rust 复刻——需内嵌 CPython（PyO3/cpython），但这破坏"纯 Rust、零 C 依赖、交叉编译安全"约束。`rust-embed` 内嵌静态资产 OK，**不能**内嵌 Python 执行能力。

**声明式 `plugins/`（manifest + MCP/skills/hooks 贡献 + marketplace）：✅ 可无损迁移**。handler 是 OS 子进程（JSON-RPC over stdio），配置是 JSON，marketplace 是 git clone + 文件复制 + JSON 状态。

---

## 六、SPEC 修正建议

| SPEC 声明 | 实际情况 | 修正建议 |
|---|---|---|
| MCP "完全兼容" | 协议层兼容，3 个缺口需自补 | "协议层完全兼容，遗留 SSE/GitHub device flow/中间层需自补（~500 行）" |
| 插件 "完全兼容" | 声明式 plugins 无损，Python extensions 不可迁移 | "声明式 plugins 无损；Python 原生 extensions 不可迁移（需 PyO3 或放弃）" |
| subagent "v0 支持两种" | 核心闭环可做，4 个缺口需补齐 | "v0 核心闭环可做，Command 回写/interrupt_on/GP 注入需逐项补齐（~1000 行）" |
| AsyncSubAgent "v0 推迟" | 推迟合理，需明确当前是降级 | "v0 推迟；需新建 Agent Protocol REST 客户端 + state reducer 才能无损（~1500 行）" |
| skill "完全兼容" | 核心数据流无损，middleware/symlink/trust 需重写 | "核心数据流无损，middleware trait/symlink 语义/trust 模态需重写" |
| skill "containment allowlist 限制 skill 工具" | `allowed_tools` 只打印不强制 | "allowed_tools 仅提示不强制（原版行为）" |
| skill "rust-embed 内嵌" | 原版是文件系统读取 | "rust-embed 内嵌是移植设计选择，非原版机制" |

---

## 七、根因分析

主要原因是这些子系统都深度绑定 **LangChain/LangGraph Python 框架**:

- `AgentMiddleware` 生命周期（`before_agent`/`wrap_model_call`/`wrap_tool_call`/`after_agent`）
- `Command(update=)` 状态回写原语
- `ToolRuntime` 父 state 注入
- `PrivateStateAttr` + `AgentState` TypedDict 状态 schema
- `TracePolicy` + `request.override()`
- `langgraph_sdk` 远端 Agent Protocol 客户端
- `langchain_mcp_adapters` 工具包装中间层

**rig 不提供这些抽象**——rig 只提供积木（Agent、Tool、HookStack、AgentRun serde、CompletionModel）。需在 deepagents-rust 层自建整个中间件生命周期 + 状态管理 + subagent 委派机制。

这不是"翻译"，而是"用 rig 积木重新实现"，需补充约 **~3000-5000 行桥接代码**才能达到原版语义对等:

| 子系统 | 自补代码量（估计） |
|---|---|
| MCP（3 缺口 + 次要） | ~500 行 |
| subagent（4 关键缺口） | ~1000 行 |
| skill（3 关键缺口） | ~800 行 |
| remote-agent（Agent Protocol 客户端 + state reducer） | ~1500 行 |
| **合计** | **~3800 行** |
