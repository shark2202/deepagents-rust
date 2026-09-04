# deepagents 用户手册 · 初级

> **适用对象**：第一次使用 deepagents 的终端用户，只需要跑通 `deepagents -p "你好"` 并拿到 LLM 回复。
>
> **前置条件**：一台 macOS / Linux 机器，已安装 Rust 1.88+ 工具链，有一个 LLM provider 的 API Key。

---

## 一、什么是 deepagents

`deepagents` 是 LangChain Deep Agents SDK 的 Rust 实现，构建在 [rig](https://crates.io/crates/rig-core) 之上。当前版本（v0.1.0）已支持：

- ✅ **单 prompt 模式**：`deepagents -p "你的问题"` → 调用 LLM → 打印回复 → 退出
- ✅ **三种 LLM Provider**：OpenAI、Anthropic、Ollama（以及任何 OpenAI 兼容 API）
- ✅ **`.env` 自动加载**：把 API Key 写进 `.env` 文件，不用每次设环境变量
- ✅ **`--model` 覆盖**：临时切换模型，不改配置文件
- ⏳ TUI 交互模式、serve、plugins 等子命令尚未实现（运行时会提示 "not yet implemented"）

---

## 二、安装

### 方式一：从源码编译（推荐）

```bash
git clone https://github.com/shark2202/deepagents-rust.git
cd deepagents-rust
cargo build --release -p deepagents-cli
```

编译产物在 `target/release/deepagents`。把它加到 PATH：

```bash
# macOS / Linux（zsh / bash）
echo 'export PATH="$HOME/codes/deepagents-rust/target/release:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

验证安装：

```bash
deepagents --version
# deepagents 0.1.0
```

### 方式二：开发模式（调试用）

不编译 release，直接 `cargo run`：

```bash
cd deepagents-rust
cargo run -p deepagents-cli -- --version
```

> 日常开发用 `cargo run -p deepagents-cli --` 后面接参数即可。

---

## 三、配置你的第一个 LLM

deepagents 需要知道两件事：**用哪家 provider** 和 **API Key 是什么**。最简单的方式是创建 `.env` 文件。

### 场景 A：用 OpenAI 官方

在项目根目录创建 `.env` 文件：

```bash
# .env
OPENAI_API_KEY=sk-你的OpenAI密钥
```

然后运行：

```bash
deepagents -p "你好，请用一句话介绍你自己"
```

你会看到类似这样的输出：

```
你好！我是一个AI助手，致力于为你提供信息、解答问题和完成任务。
```

### 场景 B：用 Anthropic Claude

```bash
# .env
ANTHROPIC_API_KEY=sk-ant-你的Anthropic密钥
```

```bash
deepagents -p "你好"
```

deepagents 会自动检测到 `ANTHROPIC_API_KEY` 已设置，选用 Anthropic provider 和默认模型 `claude-sonnet-4-5`。

### 场景 C：用本地 Ollama

先安装并启动 [Ollama](https://ollama.com)：

```bash
ollama pull llama3.2
ollama serve   # 默认监听 http://localhost:11434
```

配置 `.env`：

```bash
# .env
OLLAMA_API_BASE_URL=http://localhost:11434
```

```bash
deepagents -p "你好"
```

deepagents 检测到 `OLLAMA_API_BASE_URL`，选用 Ollama provider 和默认模型 `llama3.2`。

### 场景 D：用第三方 OpenAI 兼容 API（如 bbgate）

很多第三方平台（bbgate、OpenRouter、硅基流动等）提供 OpenAI 兼容 API，只需额外设一个 `OPENAI_BASE_URL`：

```bash
# .env
OPENAI_API_KEY=sk-你的第三方密钥
OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1
DEEPAGENTS_CODE_MODEL=openai:z-ai/glm-5.2
```

```bash
deepagents -p "你好，请用一句话介绍你自己"
# 你好，我是由Z.ai训练的GLM大语言模型……
```

> **关键**：rig 读的环境变量名是 `OPENAI_BASE_URL`（不是 `OPENAI_API_BASE_URL`），务必写对。

---

## 四、常用命令速查

| 命令 | 说明 |
|------|------|
| `deepagents -p "你的问题"` | 单 prompt 模式：发送一个问题，打印回复，退出 |
| `deepagents --print` | print 模式：从 stdin 读取输入，打印回复（适合管道） |
| `deepagents --model "openai:gpt-4o" -p "你好"` | 临时覆盖模型 |
| `deepagents --name "Alice" -p "你是谁"` | 设置 agent 显示名 |
| `deepagents --debug -p "你好"` | 开启调试日志 |
| `deepagents --version` | 查看版本 |
| `deepagents --help` | 查看所有命令和参数 |

---

## 五、常见问题

### Q1：报错 "no LLM provider configured"

```
deepagents: no LLM provider configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_API_BASE_URL, or set DEEPAGENTS_CODE_MODEL=provider:model
```

**原因**：deepagents 没有找到任何 API Key。

**解决**：
1. 确认 `.env` 文件在**你运行命令的目录**下（即当前工作目录的 `.env`）。
2. 确认 `.env` 文件内容正确，如 `OPENAI_API_KEY=sk-xxx`（等号两边不要加空格）。
3. 或者直接在命令行设环境变量测试：`OPENAI_API_KEY=sk-xxx deepagents -p "hello"`

### Q2：报错 "HttpError: error sending request for url"

```
deepagents: agent run failed: CompletionError: HttpError: Http client error: error sending request for url (https://api.openai.com/v1/chat/completions)
```

**原因**：Provider 解析成功了（找到了 Key），但 HTTP 请求失败。可能是：
- API Key 无效 / 余额不足
- 用第三方 API 但没设 `OPENAI_BASE_URL`，请求发到了 `api.openai.com`
- 网络不通

**解决**：检查 `.env` 里 `OPENAI_BASE_URL` 是否正确（第三方 API 必须设这个）。

### Q3：TUI 模式提示 "not yet implemented"

```
deepagents: TUI mode not yet implemented. Use -p <prompt> for single-prompt mode.
```

**原因**：v0.1.0 的 TUI 交互界面尚未实现。当前版本只支持 `-p` 单 prompt 模式和 `--print` 模式。

**解决**：加 `-p` 参数即可。

---

## 下一步

- 想同时配置多个 provider、用 `--model` 灵活切换？→ [中级用户手册](./02-user-intermediate.md)
- 想了解完整的优先级链和所有子命令？→ [高级用户手册](./03-user-advanced.md)
