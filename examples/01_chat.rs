//! 最小 REPL demo —— `cargo run --example 01_chat`。
//!
//! 优先用真实 LLM（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` env），无 key 时 fallback
//! `MockChatModel`（验证 API 调用链路跑通，不调真实模型）。
//!
//! agent 装配：FilesystemMiddleware（本地磁盘 backend）+ 默认栈（Summarization + PatchToolCalls
//! + PromptCaching）。可用 `ls`/`read_file`/`write_file`/`grep`/`glob`/`execute`(无 sandbox) 等工具。
//!
//! 用法：输入 prompt 回车 → agent 跑 ReAct loop → 打印 AI 回复。`/quit` 退出。

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use deepagents::{
    Backend, DeepAgentBuilder, DeepAgentState, FilesystemBackend, FilesystemMiddleware,
};
use juncture::RunnableConfig;
use juncture::llm::{ChatModel, Message};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 选模型：优先真实 LLM，无 key fallback MockChatModel。
    // 两路分支而非 Box<dyn ChatModel>，因 ChatModel: Clone supertrait，dyn 不满足 Clone。
    if let Ok(m) = juncture::llm::ChatOpenAI::from_env() {
        let m = m.with_model("gpt-4o");
        eprintln!("[deepagents demo] using ChatOpenAI (gpt-4o)");
        run_repl(m).await?;
    } else if let Ok(m) = juncture::llm::ChatAnthropic::from_env() {
        let m = m.with_model("claude-3-5-sonnet-20241022");
        eprintln!("[deepagents demo] using ChatAnthropic (claude-3-5-sonnet)");
        run_repl(m).await?;
    } else {
        eprintln!("[deepagents demo] no OPENAI_API_KEY/ANTHROPIC_API_KEY; using MockChatModel");
        eprintln!("[deepagents demo] (real LLM needs env keys; mock returns fixed reply)");
        let m = juncture::llm::MockChatModel::new("mock").with_response(
            "I'm a mock model. Set OPENAI_API_KEY or ANTHROPIC_API_KEY for real LLM.",
        );
        run_repl(m).await?;
    }
    Ok(())
}

/// REPL 循环：stdin → agent.invoke_async → 打印回复。维护跨轮 messages（上下文延续）。
async fn run_repl<M: ChatModel>(model: M) -> Result<(), Box<dyn std::error::Error>> {
    let backend = Arc::new(FilesystemBackend::new(".")) as Arc<dyn Backend>;
    let agent = DeepAgentBuilder::new(model)
        .system_prompt(
            "You are a helpful coding agent in the current directory. \
             Use read_file/ls/grep/glob/write_file/edit_file to inspect and edit files.",
        )
        .middleware_one(FilesystemMiddleware::new(backend))
        .with_default_middleware()
        .build()?;

    let stdin = io::stdin();
    let mut messages: Vec<Message> = Vec::new();

    println!("Type a prompt and Enter. '/quit' to exit.\n");
    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/quit" {
            break;
        }
        messages.push(Message::human(trimmed));
        let state = DeepAgentState {
            messages: messages.clone(),
        };
        let out = match agent.invoke_async(state, &RunnableConfig::new()).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[error] {e}");
                continue;
            }
        };
        // 打印最后一条无 tool_calls 的 AI 消息（最终回复）。
        let reply = out
            .value
            .messages
            .iter()
            .rev()
            .find(|m| m.tool_calls.is_empty())
            .map(|m| m.content_text())
            .unwrap_or("(no reply)");
        println!("{reply}\n");
        // 保留完整上下文（含工具调用/结果）。
        messages = out.value.messages;
    }
    Ok(())
}
