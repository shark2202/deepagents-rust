# ADR-0003: DeepAgentBuilder 使用泛型参数 `<M = MockModelPlaceholder>`

- **状态**: Accepted
- **日期**: 2026-09-01
- **决策者**: deepagents-rust 团队
- **关联 SPEC**: Q6（DeepAgentBuilder 18 参数映射）

## 上下文

`DeepAgentBuilder` 需要组装一个 rig `Agent`，而 rig 的 `Agent` 和 `AgentBuilder` 都是泛型结构，泛型参数 `M` 是 `CompletionModel` trait 的具体实现类型。

```rust
// rig-agent 内部
pub struct Agent<M: CompletionModel> { ... }
pub struct AgentBuilder<M: CompletionModel> { ... }
```

### 问题

`CompletionModel` trait 的方法返回 `impl Future`（RPITIT — Return Position Impl Trait In Trait）：

```rust
pub trait CompletionModel: Clone + Send + Sync {
    fn completion(&self, req: CompletionRequest)
        -> impl Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend;
    fn stream(&self, req: CompletionRequest)
        -> impl Future<Output = Result<StreamingCompletionResponse, CompletionError>> + WasmCompatSend;
    fn capabilities(&self) -> ProviderCapabilities;
}
```

由于 `impl Future` 在 trait 方法返回位置中使用 `Self`（通过关联类型），`CompletionModel` **不 dyn-compatible**。因此：

- 不能用 `Box<dyn CompletionModel>` 存储 model
- `Agent` 和 `AgentBuilder` 必须在编译时知道具体 `M` 类型
- `DeepAgentBuilder` 也必须泛型化 `<M>`

### 候选方案

#### 方案 A：泛型 `DeepAgentBuilder<M: CompletionModel>`

```rust
pub struct DeepAgentBuilder<M: CompletionModel> {
    model: Option<M>,
    // ...其他 17 个参数
}

impl<M: CompletionModel> DeepAgentBuilder<M> {
    pub fn build(self) -> Agent<M> { ... }
}
```

**优势**：
- 编译时类型安全，零运行时开销
- 用户显式控制 model 类型
- 与 rig API 直接对齐

**劣势**：
- builder 调用方需指定泛型参数
- 纯配置测试（不调用模型）仍需提供一个具体 model 类型

#### 方案 B：type-erased model（`Box<dyn CompletionModel>`）

不可能实现 — `CompletionModel` 不 dyn-compatible。

#### 方案 C：macro 生成非泛型 builder

用宏在调用点展开为泛型代码。

**问题**：
- 宏复杂度高，IDE 支持差
- 隐藏泛型参数，调试困难
- 违反 Rust 习惯

## 决策

**使用泛型 `DeepAgentBuilder<M: CompletionModel>`，并提供默认类型参数 `<M = MockModelPlaceholder>`。**

```rust
pub struct DeepAgentBuilder<M: CompletionModel = MockModelPlaceholder> {
    model: Option<M>,
    // ...
}
```

`MockModelPlaceholder` 是一个仅用于占位的零字段 struct，实现 `CompletionModel`（所有方法返回空响应）。它**仅用于**：
- builder 的默认泛型参数
- 纯配置/组装测试（`build()` 验证 builder 正确组装，但不实际运行 agent）

### model 注入方式

用户通过两种方式设置 model：

1. **类型转换**（`model::<NewM>()`）— 切换 builder 的泛型参数：
   ```rust
   DeepAgentBuilder::new()           // DeepAgentBuilder<MockModelPlaceholder>
       .model::<OpenAiModel>(model)   // DeepAgentBuilder<OpenAiModel>
       .build()
   ```
   这通过 `DeepAgentBuilder::model<M2>(self, model: M2) -> DeepAgentBuilder<M2>` 实现。

2. **字符串 spec**（`model_spec("openai:gpt-4o")`）— 存储 spec，在 build 时解析。
   适用于配置文件驱动的场景（model 类型在运行时才能确定）。

### MockCompletionModel 的关系

`MockCompletionModel`（mock.rs）是**测试用**的 CompletionModel 实现，但它不是默认泛型参数。`MockModelPlaceholder` 是一个更轻量的占位类型，专门为 builder 的默认泛型参数设计。

区别：
- `MockModelPlaceholder` — 零字段，仅占位，builder 默认泛型参数
- `MockCompletionModel` — 有 FIFO 响应队列，用于实际驱动 agent run-loop 的测试

## 实现

```rust
/// 占位 model，仅用于 DeepAgentBuilder 的默认泛型参数。
/// 不应用于实际运行 agent。
#[derive(Debug, Clone, Default)]
pub struct MockModelPlaceholder;

impl CompletionModel for MockModelPlaceholder {
    fn completion(&self, _req: CompletionRequest)
        -> impl Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend {
        async { Err(CompletionError::ResponseError(
            "MockModelPlaceholder: no model configured".to_string()
        ))}
    }
    fn stream(&self, _req: CompletionRequest)
        -> impl Future<Output = Result<StreamingCompletionResponse, CompletionError>> + WasmCompatSend {
        async { Err(CompletionError::ResponseError(
            "MockModelPlaceholder: no model configured".to_string()
        ))}
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}
```

## 后果

### 正面

- ✅ 编译时类型安全，零运行时开销
- ✅ 默认泛型参数让纯配置测试无需显式指定 model 类型
- ✅ `model::<NewM>()` 类型转换 API 直观
- ✅ 与 rig `Agent<M>` / `AgentBuilder<M>` 完全对齐

### 负面

- ⚠️ builder 类型随 model 变化（`DeepAgentBuilder<M1>` → `DeepAgentBuilder<M2>`），不能用同一个变量名跨 model 类型赋值
- ⚠️ 用户需理解泛型参数的作用（但默认参数降低了认知负担）

## 备注

`MockModelPlaceholder` 和 `MockCompletionModel` 的角色区分是重要的：
- **不调用模型**的测试用 `DeepAgentBuilder::new()`（默认 `MockModelPlaceholder`），只验证 `build()` 组装正确性
- **调用模型**的测试用 `DeepAgentBuilder::new().model::<MockCompletionModel>(mock)`，验证 run-loop 行为
