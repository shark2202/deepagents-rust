//! Binary entry point for the `deepagents` CLI.
//!
//! Wires [`CliRunner`] to [`deepagents_runtime`] for single-prompt (`-p`) and
//! print (`--print`) modes. Other subcommands still print "not yet
//! implemented" via the runner.

#![forbid(unsafe_code)]

use deepagents_cli::{CliRunner, Commands};
use deepagents_env::EnvRegistry;
use deepagents_runtime::{ProviderModel, RunError};

fn main() {
    let runner = CliRunner::parse();

    // Load `.env` from the current working directory if it exists.
    // Existing process environment variables are NOT overridden by `.env`
    // values — explicit `KEY=VAL` in the shell always wins. A missing `.env`
    // is silently OK.
    let _ = EnvRegistry::new().load_dotenv_default();

    // Use a single-threaded runtime — the agent loop has no parallelism needs.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let code = rt.block_on(async { run(&runner).await });

    std::process::exit(code);
}

async fn run(runner: &CliRunner) -> i32 {
    match runner.command() {
        None => {
            if runner.print() {
                // --print: run single prompt, print output only
                match run_single_prompt(runner).await {
                    Ok(output) => {
                        println!("{output}");
                        0
                    }
                    Err(e) => {
                        eprintln!("deepagents: {e}");
                        1
                    }
                }
            } else if runner.prompt().is_some() {
                // -p: run single prompt, print output with label
                match run_single_prompt(runner).await {
                    Ok(output) => {
                        println!("{output}");
                        0
                    }
                    Err(e) => {
                        eprintln!("deepagents: {e}");
                        1
                    }
                }
            } else {
                // default TUI mode
                eprintln!("deepagents: TUI mode not yet implemented. Use -p <prompt> for single-prompt mode.");
                0
            }
        }
        Some(Commands::Doctor(_)) => {
            eprintln!("deepagents: doctor: not yet implemented");
            0
        }
        Some(Commands::ContextDoctor(_)) => {
            eprintln!("deepagents: context-doctor: not yet implemented");
            0
        }
        Some(Commands::Update(args)) => {
            if args.check {
                eprintln!("deepagents: update --check: not yet implemented");
            } else {
                eprintln!("deepagents: update: not yet implemented");
            }
            0
        }
        Some(Commands::Serve(_)) => {
            eprintln!("deepagents: serve: not yet implemented");
            0
        }
        Some(Commands::Resume(_)) => {
            eprintln!("deepagents: resume: not yet implemented");
            0
        }
        Some(Commands::Config(_)) => {
            eprintln!("deepagents: config: not yet implemented");
            0
        }
        Some(Commands::Mcp(_)) => {
            eprintln!("deepagents: mcp: not yet implemented");
            0
        }
        Some(Commands::Plugins(_)) => {
            eprintln!("deepagents: plugins: not yet implemented");
            0
        }
        Some(Commands::Skills(_)) => {
            eprintln!("deepagents: skills: not yet implemented");
            0
        }
        Some(Commands::Hooks(_)) => {
            eprintln!("deepagents: hooks: not yet implemented");
            0
        }
    }
}

/// Resolve the provider model from CLI args or environment, then run the
/// single prompt.
async fn run_single_prompt(runner: &CliRunner) -> Result<String, String> {
    let prompt = runner
        .prompt()
        .ok_or_else(|| "no prompt provided (use -p <prompt>)".to_string())?;

    // If --model is given as "provider:model", use it; otherwise resolve from env
    let model = if let Some(model_arg) = runner.model() {
        if let Some((provider, model)) = model_arg.split_once(':') {
            ProviderModel::new(provider, model).map_err(|e| e.to_string())?
        } else {
            // Bare model name — infer provider from env
            let provider = infer_provider_from_env().ok_or_else(|| {
                "no LLM provider configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_API_BASE_URL, or use --model provider:model".to_string()
            })?;
            ProviderModel::new(&provider, model_arg).map_err(|e| e.to_string())?
        }
    } else {
        ProviderModel::from_env_default().map_err(|e| e.to_string())?
    };

    let system_prompt = if let Some(name) = runner.name() {
        format!("You are a helpful assistant named {name}.")
    } else {
        "You are a helpful assistant.".to_string()
    };

    deepagents_runtime::run_prompt_with(model, &system_prompt, prompt)
        .await
        .map_err(|e: RunError| e.to_string())
}

/// Infer provider from available env vars (no error if none found).
fn infer_provider_from_env() -> Option<String> {
    use std::env;
    if env::var("OPENAI_API_KEY").is_ok() {
        return Some("openai".into());
    }
    if env::var("ANTHROPIC_API_KEY").is_ok() {
        return Some("anthropic".into());
    }
    if env::var("OLLAMA_API_BASE_URL").is_ok() || env::var("OLLAMA_API_KEY").is_ok() {
        return Some("ollama".into());
    }
    None
}
