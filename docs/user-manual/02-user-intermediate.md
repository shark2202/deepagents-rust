# deepagents 用户手册 · 中级

> **适用对象**：已跑通第一个 prompt，想配置多个 provider、灵活切换模型、用管道模式集成到工作流。
>
> **前置条件**：已读完 [初级用户手册](./01-user-beginner.md)，至少有一个 provider 跑通。

---

## 一、环境变量全景

deepagents 的所有配置都通过环境变量完成，支持两种来源：

| 来源 | 优先级 | 说明 |
|------|--------|------|
| **Shell 环境变量** | 高 | `KEY=val deepagents -p "..."` 或 `export KEY=val` |
| **`.env` 文件** | 低 | 当前工作目录下的 `.env`，启动时自动加载 |

> **规则**：Shell 里显式设置的变量**永远不会被 `.env` 覆盖**。`.env` 只填补 Shell 里没设的变量。

### 1.1 Provider 认证变量

| 变量名 | Provider | 说明 |
|--------|----------|------|
| `OPENAI_API_KEY` | OpenAI | OpenAI 官方或任何 OpenAI 兼容 API 的密钥 |
| `OPENAI_BASE_URL` | OpenAI | 自定义 API 地址（第三方平台必设，如 `https://token.bytebroad.com.cn/v1`） |
| `ANTHROPIC_API_KEY` | Anthropic | Claude 系列密钥 |
| `OLLAMA_API_BASE_URL` | Ollama | Ollama 服务地址，如 `http://localhost:11434` |
| `OLLAMA_API_KEY` | Ollama | Ollama 认证密钥（本地通常不需要） |

### 1.2 deepagents 专用变量

| 变量名 | 格式 | 说明 |
|--------|------|------|
| `DEEPAGENTS_CODE_MODEL` | `provider:model` | 直接指定 provider 和模型，如 `openai:z-ai/glm-5.2` |
| `DEEPAGENTS_CODE_PROVIDER` | `openai` / `anthropic` / `ollama` | 只指定 provider，模型用默认值 |

---

## 二、多 Provider 配置实战

### 场景：同时配置 OpenAI 官方 + bbgate 第三方 + Ollama 本地

`.env` 文件：

```bash
# ── OpenAI 官方（默认 provider）──
OPENAI_API_KEY=sk-你的OpenAI密钥

# ── bbgate 第三方（OpenAI 兼容 API）──
# 注意：OPENAI_API_KEY 和 OPENAI_BASE_URL 只能存一个值，
# 所以 bbgate 用 --model 参数临时指定，不写死在 .env 里
# bbgate 的 key 和 base_url 在命令行设置（见下文）

# ── Ollama 本地 ──
OLLAMA_API_BASE_URL=http://localhost:11434

# ── 默认用 OpenAI 官方的 gpt-4o ──
# 不设 DEEPAGENTS_CODE_MODEL 时，自动检测到 OPENAI_API_KEY → 用 openai:gpt-4o
```

日常使用：

```bash
# 用默认配置（OpenAI gpt-4o）
deepagents -p "解释 Rust 的所有权"

# 临时切到 bbgate 的 glm-5.2
OPENAI_API_KEY=sk-bbgate密钥 OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1 \
  deepagents --model "openai:z-ai/glm-5.2" -p "解释 Rust 的所有权"

# 临时切到 Ollama 本地
deepagents --model "ollama:llama3.2" -p "解释 Rust 的所有权"

# 临时切到 Anthropic（需要 .env 里有 ANTHROPIC_API_KEY）
deepagents --model "anthropic:claude-sonnet-4-5" -p "解释 Rust 的所有权"
```

### 技巧：用 Shell 别名简化常用配置

```bash
# ~/.zshrc 或 ~/.bashrc

# bbgate GLM-5.2
alias da-bbgate='OPENAI_API_KEY=sk-bbgate密钥 OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1 deepagents --model openai:z-ai/glm-5.2'

# Ollama 本地
alias da-local='deepagents --model ollama:llama3.2'

# OpenAI 官方
alias da-gpt='deepagents --model openai:gpt-4o'
```

使用：

```bash
da-bbgate -p "写一个快速排序的 Rust 实现"
da-local -p "解释什么是 trait"
da-gpt -p "对比 Rust 和 Go 的并发模型"
```

---

## 三、`--model` 参数详解

`--model` 接受两种格式：

### 格式一：`provider:model`（推荐）

```bash
deepagents --model "openai:gpt-4o" -p "hello"
deepagents --model "openai:z-ai/glm-5.2" -p "hello"
deepagents --model "anthropic:claude-sonnet-4-5" -p "hello"
deepagents --model "ollama:llama3.2" -p "hello"
```

`provider` 不区分大小写，`anthropic` 和 `claude` 等价：

```bash
deepagents --model "claude:claude-sonnet-4-5" -p "hello"  # 等价于 anthropic
```

### 格式二：裸模型名（从环境推断 provider）

```bash
deepagents --model "gpt-4o" -p "hello"
# → 自动检测到 OPENAI_API_KEY，用 openai provider + gpt-4o 模型
```

> 如果设了 `OPENAI_BASE_URL`（第三方 API），裸模型名也会走第三方。这在 bbgate 场景下很有用：
> ```bash
> # .env 已设 OPENAI_API_KEY + OPENAI_BASE_URL 指向 bbgate
> deepagents --model "z-ai/glm-5.2" -p "hello"
> ```

### `--model` 与 `.env` 的优先级

`--model` 参数 **总是覆盖** `.env` 和 `DEEPAGENTS_CODE_MODEL`：

```bash
# .env 里设了 DEEPAGENTS_CODE_MODEL=openai:gpt-4o
# 命令行 --model 覆盖它：
deepagents --model "ollama:llama3.2" -p "hello"  # → 用 ollama，不是 openai
```

---

## 四、`--print` 管道模式

`--print` 模式从 stdin 读取输入，适合管道集成：

```bash
# 把文件内容发给 LLM 总结
cat README.md | deepagents --print

# 把命令输出发给 LLM 分析
kubectl get pods --no-headers | deepagents --print

# 组合多个命令
echo "把这段代码翻译成 Python：" && cat main.rs | deepagents --print
```

> **`-p` 和 `--print` 的区别**：
> - `-p "prompt"`：prompt 内容在命令行参数里
> - `--print`：prompt 内容从 stdin 读取
>
> 当前版本两者行为一致（都是单 prompt → 打印输出 → 退出），`--print` 更适合管道场景。

---

## 五、`--name` 和 `--debug`

### `--name`：设置 agent 显示名

```bash
deepagents --name "Rust专家" -p "解释什么是生命周期"
```

这会改变 system prompt，agent 会以这个名字自称。

### `--debug`：开启调试日志

```bash
deepagents --debug -p "hello" 2>&1
```

调试信息输出到 stderr，正常回复输出到 stdout，可以用 `2>/dev/null` 分离：

```bash
# 只看回复，隐藏调试
deepagents --debug -p "hello" 2>/dev/null

# 只看调试
deepagents --debug -p "hello" 2>&1 >/dev/null
```

---

## 六、`.env` 文件进阶

### 6.1 `.env` 的加载规则

1. deepagents 启动时，在**当前工作目录**查找 `.env` 文件
2. 如果找到，用 `dotenvy` 库加载其中的变量
3. **Shell 已设的变量不会被 `.env` 覆盖**（非 override 模式）
4. 如果没找到 `.env`，静默跳过（不报错）

### 6.2 多项目隔离

每个项目目录可以有自己的 `.env`：

```
~/projects/myapp/      → .env (OPENAI_API_KEY=sk-项目A的key)
~/projects/other/      → .env (OPENAI_API_KEY=sk-项目B的key)
```

在对应目录下运行 `deepagents -p "..."` 即可使用该项目的配置。

### 6.3 `.env` 文件格式

```bash
# 注释以 # 开头
OPENAI_API_KEY=sk-xxxxx

# 等号两边不要加空格
OPENAI_BASE_URL=https://api.example.com/v1

# 值不需要引号（除非含特殊字符）
DEEPAGENTS_CODE_MODEL=openai:gpt-4o

# 包含空格的值可以用引号
# SYSTEM_PROMPT="You are a helpful assistant."
```

### 6.4 安全提醒

- ⚠️ `.env` 文件包含 API 密钥，**永远不要提交到 git**
- 项目 `.gitignore` 已包含 `.env` 和 `.env.*`
- 如果不小心提交了，立即在 provider 控制台 revoke 密钥

---

## 七、完整配置示例

### 示例：bbgate GLM-5.2 日常使用

`.env`：
```bash
OPENAI_API_KEY=sk-your-bbgate-key
OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1
DEEPAGENTS_CODE_MODEL=openai:z-ai/glm-5.2
```

日常命令：
```bash
# 日常提问
deepagents -p "用 Rust 写一个二分查找"

# 临时切模型
deepagents --model "openai:deepseek-v4-pro" -p "分析这段代码的性能瓶颈"

# 管道模式
cat src/main.rs | deepagents --print

# 带名字
deepagents --name "CodeReviewer" -p "Review this code: ..."
```

---

## 下一步

- 想了解完整的 6 级优先级链、所有 10 个子命令、扩展点？→ [高级用户手册](./03-user-advanced.md)
- 遇到部署问题？→ [运维手册](./04-ops-beginner.md)
