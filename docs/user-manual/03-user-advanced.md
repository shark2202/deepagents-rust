# deepagents 用户手册 · 高级

> **适用对象**：需要精确控制 provider 解析逻辑、使用子命令、了解内部架构、做扩展开发的高手。
>
> **前置条件**：已读完 [初级](./01-user-beginner.md) 和 [中级](./02-user-intermediate.md) 手册。

---

## 一、Provider 解析 6 级优先级链

当 `deepagents -p "..."` 运行时（未指定 `--model`），`ProviderModel::from_env_default()` 按以下顺序解析 provider 和模型：

```
┌──────────────────────────────────────────────────────────────┐
│ 优先级 1（最高）：--model CLI 参数                            │
│   deepagents --model "openai:gpt-4o" -p "..."                │
│   → ProviderModel::new("openai", "gpt-4o")                   │
│   → 跳过下面所有步骤                                          │
├──────────────────────────────────────────────────────────────┤
│ 优先级 2：DEEPAGENTS_CODE_MODEL 环境变量（含 provider:model） │
│   DEEPAGENTS_CODE_MODEL=openai:z-ai/glm-5.2                  │
│   → split_once(':') → ProviderModel::new("openai",          │
│     "z-ai/glm-5.2")                                          │
├──────────────────────────────────────────────────────────────┤
│ 优先级 2'：DEEPAGENTS_CODE_MODEL 只有模型名（无 provider:）   │
│   DEEPAGENTS_CODE_MODEL=gpt-4o                               │
│   → infer_provider() 推断 provider                           │
│   → ProviderModel::new(&推断的provider, "gpt-4o")            │
├──────────────────────────────────────────────────────────────┤
│ 优先级 3：DEEPAGENTS_CODE_PROVIDER + 默认模型                 │
│   DEEPAGENTS_CODE_PROVIDER=anthropic                        │
│   → default_model_for("anthropic") = "claude-sonnet-4-5"    │
│   → ProviderModel::new("anthropic", "claude-sonnet-4-5")     │
├──────────────────────────────────────────────────────────────┤
│ 优先级 4：OPENAI_API_KEY 存在                                 │
│   → infer_provider() = "openai"                             │
│   → default_model_for("openai") = "gpt-4o"                  │
│   → ProviderModel::new("openai", "gpt-4o")                  │
├──────────────────────────────────────────────────────────────┤
│ 优先级 5：ANTHROPIC_API_KEY 存在                              │
│   → infer_provider() = "anthropic"                          │
│   → default_model_for("anthropic") = "claude-sonnet-4-5"    │
├──────────────────────────────────────────────────────────────┤
│ 优先级 6（最低）：OLLAMA_API_BASE_URL 或 OLLAMA_API_KEY 存在  │
│   → infer_provider() = "ollama"                             │
│   → default_model_for("ollama") = "llama3.2"               │
├──────────────────────────────────────────────────────────────┤
│ 全部不匹配 → ProviderError::NoProvider                        │
│   → "no LLM provider configured: ..."                       │
└──────────────────────────────────────────────────────────────┘
```

### 默认模型映射表

| Provider | 默认模型 | 环境变量触发条件 |
|----------|---------|-----------------|
| `openai` | `gpt-4o` | `OPENAI_API_KEY` 存在 |
| `anthropic` | `claude-sonnet-4-5` | `ANTHROPIC_API_KEY` 存在 |
| `ollama` | `llama3.2` | `OLLAMA_API_BASE_URL` 或 `OLLAMA_API_KEY` 存在 |

### Provider 别名

`anthropic` 和 `claude` 等价：

```bash
deepagents --model "claude:claude-sonnet-4-5" -p "hello"
# 等价于
deepagents --model "anthropic:claude-sonnet-4-5" -p "hello"
```

provider 名不区分大小写：

```bash
deepagents --model "OpenAI:GPT-4o" -p "hello"  # 也行
```

---

## 二、内部调用链路

当你运行 `deepagents -p "hello"` 时，内部执行路径如下：

```
main()
  │
  ├─ CliRunner::parse()                    // clap 解析 CLI 参数
  │
  ├─ EnvRegistry::new()
  │    .load_dotenv_default()              // 加载 .env（如果存在）
  │
  ├─ tokio runtime Builder::new_current_thread()
  │    .enable_all().build()              // 单线程 tokio runtime
  │
  └─ run(&runner).await
       │
       ├─ runner.prompt() = Some("hello")
       │
       └─ run_single_prompt(runner).await
            │
            ├─ runner.model() = None → ProviderModel::from_env_default()
            │    │
            │    ├─ 检查 DEEPAGENTS_CODE_MODEL  (优先级 2)
            │    ├─ 检查 DEEPAGENTS_CODE_PROVIDER (优先级 3)
            │    └─ infer_provider()            (优先级 4-6)
            │         │
            │         ├─ OPENAI_API_KEY?  → "openai"  + "gpt-4o"
            │         ├─ ANTHROPIC_API_KEY? → "anthropic" + "claude-sonnet-4-5"
            │         └─ OLLAMA_*?  → "ollama" + "llama3.2"
            │
            ├─ ProviderModel::new("openai", "gpt-4o")
            │    │
            │    ├─ openai::Client::from_env()    // 读 OPENAI_API_KEY + OPENAI_BASE_URL
            │    └─ client.completions_api()      // 切到 Chat Completions API（非 Responses API）
            │        .completion_model("gpt-4o")
            │
            └─ deepagents_runtime::run_prompt_with(model, system_prompt, "hello")
                 │
                 ├─ DeepAgentBuilder::new()
                 │    .model(model)              // 设置 CompletionModel
                 │    .system_prompt("...")      // 设置 system prompt
                 │    .build_runner("hello")     // 构建 AgentRunner
                 │
                 ├─ AgentRunner::run().await    // 驱动 agent 循环
                 │
                 └─ PromptResponse { output: "...", messages: ... }
                      │
                      └─ println!("{output}")    // 打印到 stdout
```

### 关键设计决策

**为什么用 enum dispatch 而非 trait object？**

rig 的 `CompletionModel` trait 使用了 RPITIT（return-position `impl Trait` in trait），这使得它**不是 object-safe** 的——无法 `dyn CompletionModel`。因此 `ProviderModel` 是一个枚举：

```rust
pub enum ProviderModel {
    Openai(openai::CompletionModel),
    Anthropic(anthropic::completion::CompletionModel),
    Ollama(ollama::CompletionModel),
}
```

`impl CompletionModel for ProviderModel` 通过 `async move { match self { ... } }` 分发到具体 provider。

**为什么 OpenAI 用 `completions_api()` 而非默认 `Client`？**

rig 的 `openai::Client` 默认使用 Responses API，但 `client.completion_model()` 返回的 `GenericResponsesCompletionModel` 类型与我们的 `CompletionModel` 类型别名不匹配。`client.completions_api().completion_model()` 返回 `GenericCompletionModel`，与枚举匹配。

---

## 三、CLI 完整参数表

### 3.1 顶层参数

| 参数 | 短写 | 类型 | 说明 |
|------|------|------|------|
| `--prompt` | `-p` | `Option<String>` | 单 prompt 模式：运行一轮后退出 |
| `--print` | — | `bool` | print 模式：从 stdin 读取，打印回复后退出 |
| `--model` | — | `Option<String>` | 覆盖模型，格式 `provider:model` 或裸模型名 |
| `--debug` | — | `bool` | 开启调试 / verbose 诊断 |
| `--name` | — | `Option<String>` | 覆盖 agent 显示名 |
| `--version` | `-V` | — | 打印版本号 |
| `--help` | `-h` | — | 打印帮助 |

### 3.2 子命令一览

v0.1.0 定义了 10 个子命令，**当前全部为占位实现**（输出 "not yet implemented" 到 stderr，退出码 0）：

| 子命令 | 参数 | 计划功能 | 当前状态 |
|--------|------|---------|---------|
| `serve` | `--port <PORT>` | 启动 HTTP/ACP 服务器 | ⏳ 占位 |
| `resume` | `--session-id <ID>` | 恢复之前的会话 | ⏳ 占位 |
| `config` | `--list` / `--get <KEY>` / `--set <K=V>` | 查看/编辑配置 | ⏳ 占位 |
| `doctor` | — | 环境诊断 | ⏳ 占位 |
| `context-doctor` | — | 上下文窗口诊断 | ⏳ 占位 |
| `mcp` | `--list` / `--add <NAME URL>` / `--remove <NAME>` | 管理 MCP 服务器 | ⏳ 占位 |
| `plugins` | `--list` / `--install <NAME>` / `--remove <NAME>` | 管理插件 | ⏳ 占位 |
| `skills` | `--list` / `--add <PATH>` | 管理技能 | ⏳ 占位 |
| `hooks` | `--list` / `--run <NAME>` | 管理/运行 hooks | ⏳ 占位 |
| `update` | `--check` / `--force` | 自更新/版本检查 | ⏳ 占位 |

### 3.3 13 个用户入口点

SPEC Q13 定义了 13 个用户入口点：

1. 默认 TUI 模式（无参数）
2. `-p` / `--prompt` 单 prompt 模式 ✅ **已实现**
3. `--print` print 模式 ✅ **已实现**
4-13. 上述 10 个子命令

---

## 四、以编程方式调用

如果你想在 Rust 项目中调用 deepagents 的运行时能力（而非通过 CLI），可以直接依赖 `deepagents-runtime` crate：

```toml
# Cargo.toml
[dependencies]
deepagents-runtime = "0.1"
tokio = { version = "1", features = ["rt", "macros"] }
```

```rust
use deepagents_runtime::{ProviderModel, run_prompt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = ProviderModel::new("openai", "gpt-4o")?;
    let rt = tokio::runtime::Runtime::new()?;
    let output = rt.block_on(run_prompt(model, "解释什么是 trait"))?;
    println!("{output}");
    Ok(())
}
```

### API 一览

| 函数/类型 | 签名 | 说明 |
|-----------|------|------|
| `ProviderModel::new` | `(provider: &str, model: &str) -> Result<Self, ProviderError>` | 显式构造 |
| `ProviderModel::from_env_default` | `() -> Result<Self, ProviderError>` | 从环境变量推断 |
| `run_prompt` | `(model, prompt: &str) -> Result<String, RunError>` | 用默认 system prompt 运行 |
| `run_prompt_with` | `(model, system_prompt: &str, prompt: &str) -> Result<String, RunError>` | 用自定义 system prompt 运行 |
| `ProviderError` | enum | `UnsupportedProvider` / `NoProvider` / `Client(String)` |
| `RunError` | enum | `Prompt(String)` |

---

## 五、环境变量完整清单

### 5.1 Provider 认证（5 个）

| 变量 | 示例值 | 触发 provider |
|------|--------|--------------|
| `OPENAI_API_KEY` | `sk-xxxx` | openai |
| `OPENAI_BASE_URL` | `https://token.bytebroad.com.cn/v1` | openai（自定义 base URL） |
| `ANTHROPIC_API_KEY` | `sk-ant-xxxx` | anthropic |
| `OLLAMA_API_BASE_URL` | `http://localhost:11434` | ollama |
| `OLLAMA_API_KEY` | （通常不需要） | ollama |

### 5.2 deepagents 控制（2 个）

| 变量 | 格式 | 说明 |
|------|------|------|
| `DEEPAGENTS_CODE_MODEL` | `provider:model` 或 `model` | 直接指定 provider+model 或只指定 model |
| `DEEPAGENTS_CODE_PROVIDER` | `openai` / `anthropic` / `ollama` | 只指定 provider，model 用默认 |

### 5.3 `.env` 加载行为

- 加载方式：`dotenvy::from_path()`（非 override 模式）
- 加载位置：当前工作目录的 `.env`
- 加载时机：`main()` 函数最开头，在 tokio runtime 构建之前
- 缺失行为：静默跳过（`Ok(())`）
- 加载失败：输出 `tracing::warn!` 日志到 stderr，返回 `Err`

---

## 六、扩展点与路线图

### 6.1 当前可用的扩展点

| 扩展点 | crate | 状态 |
|--------|-------|------|
| 新增 LLM Provider | `deepagents-runtime` | 需在 `ProviderModel` enum 加 variant + match arm |
| 自定义 system prompt | `deepagents-runtime` | `run_prompt_with()` |
| `.env` 加载逻辑 | `deepagents-env` | `EnvRegistry::load_dotenv` / `load_dotenv_overriding` |
| CLI 子命令实现 | `deepagents-cli` | `CliRunner::run()` 中替换 `not_yet_implemented` |
| 配置系统 | `deepagents-config` | 已有骨架，待接 |
| MCP 服务器 | `deepagents-mcp` | 已有骨架，待接 |
| Hooks | `deepagents-hooks` | 已有骨架，待接 |
| 插件 | `deepagents-plugins` | 已有骨架，待接 |
| 技能 | `deepagents-skills` | 已有骨架，待接 |
| 沙箱 | `deepagents-sandbox` | 已有骨架，待接 |

### 6.2 如何新增一个 Provider

以新增 `gemini` 为例：

1. 在 `crates/deepagents-runtime/src/lib.rs` 的 `ProviderModel` enum 加一个 variant：
   ```rust
   pub enum ProviderModel {
       // ...
       Gemini(rig_core::providers::gemini::CompletionModel),
   }
   ```

2. 在 `ProviderModel::new()` 的 match 加 arm：
   ```rust
   "gemini" => {
       let client = rig_core::providers::gemini::Client::from_env()
           .map_err(ProviderError::client)?;
       Ok(Self::Gemini(client.completion_model(model)))
   }
   ```

3. 在 `impl CompletionModel for ProviderModel` 的两个 match 加 arm：
   ```rust
   ProviderModel::Gemini(m) => m.completion(request).await,
   ProviderModel::Gemini(m) => m.stream(request).await,
   ```

4. 在 `default_model_for()` 加默认模型：
   ```rust
   "gemini" => "gemini-2.0-flash",
   ```

5. 在 `infer_provider()` 加检测：
   ```rust
   if env::var("GEMINI_API_KEY").is_ok() {
       return Ok("gemini".into());
   }
   ```

6. 更新 `ProviderError::UnsupportedProvider` 的错误消息。

---

## 下一步

- 遇到编译/部署/排障问题？→ [运维手册（初级）](./04-ops-beginner.md)
- 要做多环境部署、配置管理？→ [运维手册（中级）](./05-ops-intermediate.md)
- 要深入架构、crate 依赖、性能调优？→ [运维手册（高级）](./06-ops-advanced.md)
