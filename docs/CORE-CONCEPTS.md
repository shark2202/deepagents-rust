# deepagents-rust 核心要素、概念与流程

> 本文档描述 deepagents-rust 的核心要素、核心概念和核心流程。
> 基于已实现的 `deepagents-core` + `deepagents-errors` 两个 crate。
> 架构全貌见 `docs/ARCHITECTURE.md`，决策记录见 `docs/adr/`。

## 一、核心要素

核心要素是 **5 个 struct + 2 个 trait**，它们构成了 SDK 的全部运行时实体：

| 要素 | 类型 | 位置 | 作用 |
|------|------|------|------|
| `DeepAgentBuilder` | struct | `builder.rs` | 唯一入口，18 参数组装 agent |
| `HookStack` | struct (rig) | rig-agent | 聚合所有 hook，按追加序执行 |
| `MockCompletionModel` | struct | `mock.rs` | FIFO 响应队列，测试驱动 run-loop |
| `InterruptMap` | struct | `hitl.rs` | `HashMap<tool_name, InterruptPolicy>` |
| `HarnessProfile` | struct | `subagent.rs` | prompt 组装 + tool/middleware 过滤 |
| `Backend` trait | trait | `backend.rs` | 文件系统抽象（ls/read/write/edit/grep/glob/delete） |
| `AgentHook` trait | trait (rig) | rig-agent | 生命周期拦截（on_tool_call/on_tool_result/on_completion_call） |

## 二、核心概念

### 概念 1：sans-IO state machine（无 IO 状态机）

Agent 的运行不是函数调用，而是一个状态机 `AgentRun`。每次 LLM 调用是一步 tick，状态可序列化、可暂停、可恢复。这是 rig 带来的核心范式。

### 概念 2：Hook 拦截模型

所有定制行为（文件系统工具、子 agent 调度、历史摘要、人工审批）都不是 if-else 分支，而是 hook。rig 在 run-loop 的三个关键点回调：

```
LLM 调用前 → on_completion_call → RequestPatch (改 preamble / active_tools / history)
工具调用前 → on_tool_call      → ToolCallAction::{Run, skip, stop}
工具调用后 → on_tool_result     → (副作用：eviction / truncation)
```

### 概念 3：Permission 在工具层强制，不在 backend 层

Backend 是纯 IO，不关心权限。Permission 是独立的 `FilesystemPermission` 规则集，由 `PermissionChecker` 在工具调用前检查。三种模式：`Allow` / `Deny` / `Interrupt`（触发 HITL）。

### 概念 4：虚拟 POSIX 路径

所有文件路径是虚拟的 POSIX 路径（`/`-分隔，绝对路径，禁止 `..`/`~`）。这让 `StateBackend`（内存 HashMap）和 `FilesystemBackend`（真实磁盘）可以互换，也让 sandbox 和非 sandbox 模式统一。

### 概念 5：受保护中间件

`Filesystem` 和 `SubAgent` 两个中间件不可排除。`HarnessProfile::is_middleware_excluded()` 对这两个名字硬编码返回 `false`。这是设计约束——没有文件系统工具的 agent 无法工作，没有子 agent 调度的 agent 退化严重。

### 概念 6：Prompt 三段组装

系统 prompt 不是单一字符串，而是三段拼接：

```
USER (用户传入) + "\n\n" + BASE (profile.base_system_prompt) + "\n\n" + SUFFIX (profile.system_prompt_suffix)
```

中间件通过 `RequestPatch::preamble()` 在 `on_completion_call` 时注入额外指令（如 fs 工具说明、subagent 描述），叠加在这个三段之上。

## 三、核心流程

### 流程 1：Agent 组装（build 时）

```
用户创建 DeepAgentBuilder::new()
    │
    ├─ .system_prompt("...")          → 存 USER prompt
    ├─ .backend(Arc<dyn Backend>)      → 存 backend
    ├─ .interrupt_on(InterruptMap)    → 存中断规则
    ├─ .subagent(SubAgentSpec)        → 存子 agent 声明
    ├─ .permissions(vec![Perm])        → 存权限规则
    ├─ .model::<M>(model)             → 泛型类型转换
    │
    └─ .build()
         │
         ├─ HarnessProfile::assemble_prompt() → "USER\n\nBASE\n\nSUFFIX"
         │
         ├─ HookStack::new()
         │   ├─ push(FilesystemMiddleware)   [受保护，不可排除]
         │   ├─ push(SubAgentMiddleware)     [受保护，不可排除]
         │   ├─ push(SummarizationMiddleware) [可排除]
         │   ├─ push(HitlMiddleware)          [可排除]
         │   └─ push(用户自定义 hooks)
         │
         ├─ AgentBuilder::new(model)
         │   .preamble(组装后的 prompt)
         │   .add_hook(hook_stack)
         │
         └→ Agent<M>
```

### 流程 2：Agent 运行（run 时）

```
AgentRunner::run() 启动 AgentRun state machine
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 1: on_completion_call             │
│  ├─ FilesystemMiddleware  → 注入 fs tools + preamble │
│  ├─ SubAgentMiddleware    → 注入 task tool + subagent 描述 │
│  ├─ SummarizationMiddleware → 如果历史过长，截断 history │
│  └─ RequestPatch 合并 → 修改后的请求           │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 2: LLM 调用 (CompletionModel)     │
│  MockCompletionModel 出队一个预设响应    │
│  真实 model 发 HTTP 请求                 │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 3: LLM 返回 → 解析 tool calls      │
│  如果无 tool call → agent 结束，返回最终回复 │
│  如果有 tool call → 进入 Step 4         │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 4: 对每个 tool call:              │
│                                          │
│  on_tool_call (HITL 拦截点)              │
│  ├─ InterruptMap::check(tool_call_info)  │
│  ├─ Approve  → ToolCallAction::Run       │
│  ├─ Reject  → ToolCallAction::skip(reason) │
│  └─ RequireHuman → ToolCallAction::stop() │
│      → AgentRun 暂停，序列化 PauseCheckpoint │
│      → 等待 ResumeInput::{Approve,Reject,Edit} │
│                                          │
│  执行工具 (backend.read/write/edit/...)  │
│                                          │
│  on_tool_result (副作用阶段)             │
│  ├─ FilesystemMiddleware  → eviction (结果太大则驱逐) │
│  └─ SummarizationMiddleware → 截断大结果  │
└─────────────────────────────────────────┘
    │
    └──→ 回到 Step 1 (下一轮 tick)
```

### 流程 3：HITL pause + resume

```
正常运行
    │
    ▼ on_tool_call 匹配 InterruptMap
    │
ToolCallAction::stop("awaiting approval")
    │
    ▼
AgentRun 暂停 → 序列化 PauseCheckpoint
    │   (含: 当前 run state + 待决 tool call info)
    │
    ▼ 等待人工
    │
ResumeInput::Approve       → 继续执行该 tool call
ResumeInput::Reject("msg")  → skip 该 tool call
ResumeInput::Edit { new_args } → 用新参数执行
    │
    ▼
AgentRun 恢复，从暂停点继续 tick
```

## 四、一句话总结

`DeepAgentBuilder` 把 backend/permission/middleware/subagent 组装成一个 `Agent<M>`，`Agent` 内部的 `AgentRun` 状态机通过 `HookStack` 在三个生命周期点（LLM 调用前 / 工具调用前 / 工具调用后）回调各个中间件，HITL 通过 `ToolCallAction::Stop` 暂停状态机并序列化等待人工恢复。
