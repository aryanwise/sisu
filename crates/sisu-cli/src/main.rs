mod display;

use sisu_core::languages::get_adapter;
use sisu_core::llm::{LlmConfig, OllamaClient, call_model};
use sisu_core::sandbox::runner::SandboxRunner;
use sisu_core::{Orchestrator, OrchestratorConfig, create_plan, load_plan};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_MODEL: &str = "qwen2.5-coder:7b";
const DEFAULT_PROJECT_DIR: &str = "project";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Chat,
    Project,
}

struct CliConfig {
    model: String,
    project_dir: PathBuf,
    no_sandbox: bool,
}

fn parse_cli_args() -> CliConfig {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut model = DEFAULT_MODEL.to_string();
    let mut project_dir = PathBuf::from(DEFAULT_PROJECT_DIR);
    let mut no_sandbox = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-sandbox" => no_sandbox = true,
            "--model" if i + 1 < args.len() => {
                model = args[i + 1].clone();
                i += 1;
            }
            "--dir" if i + 1 < args.len() => {
                project_dir = PathBuf::from(&args[i + 1]);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    CliConfig {
        model,
        project_dir,
        no_sandbox,
    }
}

fn prompt_line(label: &str) -> Result<String, String> {
    print!("{label}");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;

    let mut buf = String::new();
    let bytes = io::stdin()
        .read_line(&mut buf)
        .map_err(|e| format!("Failed to read input: {e}"))?;

    if bytes == 0 {
        return Ok("/quit".to_string());
    }

    Ok(buf.trim().to_string())
}

fn init_sandbox_runner(project_dir: &Path, no_sandbox: bool) -> Result<SandboxRunner, String> {
    let timeout = Duration::from_secs(30);
    if no_sandbox {
        println!(
            "[SISU] Sandbox: unsandboxed (--no-sandbox active; path jail + env strip + timeout enforced)"
        );
        return Ok(SandboxRunner::unsandboxed(timeout));
    }

    let runner = SandboxRunner::detect(project_dir, timeout)?;
    println!("[SISU] Sandbox: {}", runner.backend().label());
    Ok(runner)
}

async fn run_sandbox_project(
    project_dir: &Path,
    goal: Option<&str>,
    language: Option<&str>,
    model: &str,
    no_sandbox: bool,
) -> Result<(), String> {
    fs::create_dir_all(project_dir).map_err(|e| {
        format!(
            "Failed to create project dir '{}': {e}",
            project_dir.display()
        )
    })?;

    let runner = init_sandbox_runner(project_dir, no_sandbox)?;
    let llm = OllamaClient::new(model);

    let mut plan_language = language
        .filter(|l| !l.trim().is_empty())
        .unwrap_or("python")
        .trim()
        .to_string();

    let mut plan = match load_plan(project_dir)? {
        Some(existing) => {
            println!(
                "[SISU] Resuming existing plan from {}",
                project_dir.join("milestones.json").display()
            );
            existing
        }
        None => {
            let goal_str = match goal {
                Some(g) if !g.trim().is_empty() => g.trim().to_string(),
                _ => prompt_line("Project goal: ")?,
            };
            let lang_str = if plan_language == "python" && language.is_none() {
                let input = prompt_line("Language [python/rust/typescript] (default: python): ")?;
                if input.is_empty() {
                    "python".to_string()
                } else {
                    input
                }
            } else {
                plan_language.clone()
            };

            plan_language = lang_str.clone();
            println!("[SISU] Planning milestones for '{lang_str}' project...");
            create_plan(&llm, &goal_str, &lang_str).await?
        }
    };

    let adapter = get_adapter(&plan_language)?;

    // Keep auto_rollback = false so `display::handle_run_outcome` prompts interactively on Point A
    let orchestrator = Orchestrator::new(
        &llm,
        adapter.as_ref(),
        &runner,
        OrchestratorConfig {
            auto_rollback_on_failure: false,
        },
    );

    let outcome = orchestrator.run_project(project_dir, &mut plan).await?;
    display::handle_run_outcome(project_dir, outcome)
}

fn print_help() {
    println!(
        "\nCommands:\n\
         \x20 /chat       Switch to conversational mode\n\
         \x20 /project    Switch to autonomous sandbox mode\n\
         \x20 /resume     Resume an existing milestones.json in the project dir\n\
         \x20 /help       Show this help\n\
         \x20 /quit       Exit SISU\n"
    );
}

#[tokio::main]
async fn main() {
    let cfg = parse_cli_args();
    let mut mode = Mode::Project;

    println!("========================================");
    println!(" SISU — Local-First Autonomous Harness");
    println!(" Model:   {}", cfg.model);
    println!(" Dir:     {}", cfg.project_dir.display());
    println!(
        " Sandbox: {}",
        if cfg.no_sandbox {
            "disabled (--no-sandbox)"
        } else {
            "auto-detect"
        }
    );
    println!("========================================");
    print_help();

    loop {
        let prompt_tag = match mode {
            Mode::Chat => "sisu [chat]> ",
            Mode::Project => "sisu [project]> ",
        };

        let input = match prompt_line(prompt_tag) {
            Ok(line) => line,
            Err(e) => {
                eprintln!("[SISU] Input error: {e}");
                break;
            }
        };

        if input.is_empty() {
            continue;
        }

        match input.as_str() {
            "/quit" | "/exit" => break,
            "/help" => {
                print_help();
                continue;
            }
            "/chat" => {
                mode = Mode::Chat;
                println!("[SISU] Switched to chat mode.");
                continue;
            }
            "/project" => {
                mode = Mode::Project;
                println!("[SISU] Switched to autonomous project mode.");
                continue;
            }
            "/resume" => {
                if let Err(e) =
                    run_sandbox_project(&cfg.project_dir, None, None, &cfg.model, cfg.no_sandbox)
                        .await
                {
                    eprintln!("[SISU] Error: {e}");
                }
                continue;
            }
            _ => {}
        }

        match mode {
            Mode::Chat => {
                let reply = call_model(&cfg.model, input, None, LlmConfig::conversational()).await;
                println!("\n{reply}\n");
            }
            Mode::Project => {
                if let Err(e) = run_sandbox_project(
                    &cfg.project_dir,
                    Some(&input),
                    None,
                    &cfg.model,
                    cfg.no_sandbox,
                )
                .await
                {
                    eprintln!("[SISU] Error: {e}");
                }
            }
        }
    }
}
