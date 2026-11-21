//! One typed command catalog shared by shell arguments and interactive commands.
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "sisu",
    version,
    about = "Plan and discuss software projects with a persistent local session",
    disable_help_subcommand = true
)]
pub struct Args {
    /// Project directory (defaults to the current directory).
    #[arg(short = 'C', long, default_value = ".", global = true)]
    pub workspace: PathBuf,
    /// Continue an existing session for this command.
    #[arg(long, global = true)]
    pub session: Option<String>,
    /// Override the configured model.
    #[arg(long, global = true)]
    pub model: Option<String>,
    /// Ollama HTTP endpoint.
    #[arg(long, global = true)]
    pub endpoint: Option<String>,
    /// Request timeout in seconds.
    #[arg(long = "timeout", global = true)]
    pub timeout_secs: Option<u64>,
    /// Model context window in tokens.
    #[arg(long, global = true)]
    pub context_limit: Option<u64>,
    /// Emit machine-readable JSON lines instead of terminal formatting.
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress progress output.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Disable terminal colors.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Send a chat request; accepts piped input when no prompt is given.
    Chat {
        #[arg(num_args = 0..)]
        prompt: Vec<String>,
    },
    /// Generate a project plan; without a goal, display the saved plan. Use `plan validate` to validate it.
    Plan {
        #[arg(num_args = 0..)]
        goal: Vec<String>,
    },
    /// Inspect or switch the active mode (chat or project).
    Mode { mode: Option<String> },
    /// Inspect or change the active model.
    Model { name: Option<String> },
    /// List models available at the configured Ollama endpoint.
    Models,
    /// Show effective configuration and the source of each setting.
    Config { action: Option<String> },
    /// Show the session, workspace, model, and operation status.
    Status,
    /// Inspect explicit context, or add/remove a workspace file.
    Context {
        action: Option<String>,
        path: Option<PathBuf>,
    },
    /// List saved workspace sessions (`sessions list` is also accepted).
    Sessions { action: Option<String> },
    /// Resume a saved session.
    Resume { id: String },
    /// Create a new conversation in the current workspace.
    New,
    /// Clear the terminal display, retaining conversation history.
    Clear,
    /// Cancel an input draft; Ctrl+C cancels an active request.
    Cancel,
    /// Save the session and exit.
    Exit,
    /// Check workspace access, configuration, and provider connectivity.
    Doctor,
    /// Execute a milestone (unavailable until a sandbox backend is implemented).
    Run { milestone: Option<String> },
    /// Verify generated files (requires execution support).
    Check,
    /// Retry a milestone (requires execution support).
    Retry { milestone: Option<String> },
    /// Show tracked workspace changes (requires workspace snapshots).
    Diff,
    /// Save a workspace checkpoint (requires workspace snapshots).
    Checkpoint,
    /// Restore a workspace checkpoint (requires workspace snapshots).
    Restore { id: Option<String> },
    /// Inspect execution isolation capabilities.
    Sandbox { action: Option<String> },
    /// Display available commands or help for one command.
    Help { command: Option<String> },
}

#[derive(Parser)]
#[command(name = "sisu", disable_help_subcommand = true)]
struct InteractiveArgs {
    #[command(subcommand)]
    command: Command,
}

pub fn parse_slash(line: &str) -> Result<Command, String> {
    let command = line.strip_prefix('/').ok_or("Commands must start with /")?;
    let words = split_words(command)?;
    InteractiveArgs::try_parse_from(std::iter::once("sisu".to_string()).chain(words))
        .map(|args| args.command)
        .map_err(|error| error.to_string())
}

pub fn help_text(command: Option<&str>) -> Result<String, String> {
    let mut root = Args::command();
    let target = if let Some(name) = command {
        root.find_subcommand_mut(name.trim_start_matches('/'))
            .ok_or_else(|| {
                format!("Unknown command '{name}'. Use /help to see available commands.")
            })?
    } else {
        &mut root
    };
    Ok(target.render_long_help().to_string())
}

/// Completion and help names come from the same clap catalog as argument parsing.
pub fn command_completions() -> Vec<String> {
    let mut words: Vec<_> = Args::command()
        .get_subcommands()
        .map(|command| format!("/{}", command.get_name()))
        .collect();
    words.extend(
        [
            "/send",
            "/mode chat",
            "/mode project",
            "/plan validate",
            "/config show",
            "/context add",
            "/context remove",
            "/sessions list",
            "/sandbox status",
        ]
        .into_iter()
        .map(str::to_string),
    );
    words
}

/// Tokenize arguments without invoking a shell or expanding variables or substitutions.
fn split_words(input: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for ch in input.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            started = true;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(ch);
            started = true;
        }
    }
    if quote.is_some() || escaped {
        return Err(
            "Unfinished quote or escape in command. Close the quote or escape a literal backslash."
                .into(),
        );
    }
    if started {
        result.push(word);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_and_slash_commands_share_types() {
        let shell = Args::try_parse_from(["sisu", "context", "add", "src/a file.rs"]).unwrap();
        let interactive = parse_slash("/context add 'src/a file.rs'").unwrap();
        assert_eq!(shell.command.unwrap(), interactive);
        assert_eq!(
            parse_slash("/plan validate").unwrap(),
            Command::Plan {
                goal: vec!["validate".into()]
            }
        );
    }

    #[test]
    fn no_shell_expansion_or_silent_unknown_commands() {
        assert_eq!(
            parse_slash("/chat '$(touch nope)' \"$HOME\"").unwrap(),
            Command::Chat {
                prompt: vec!["$(touch nope)".into(), "$HOME".into()]
            }
        );
        assert!(parse_slash("/unknown").is_err());
        assert!(parse_slash("/model 'unfinished").is_err());
        assert!(parse_slash("/exit extra").is_err());
    }

    #[test]
    fn global_flags_work_after_prompt_arguments() {
        let args = Args::try_parse_from(["sisu", "chat", "hello", "--json"]).unwrap();
        assert!(args.json);
        assert_eq!(
            args.command,
            Some(Command::Chat {
                prompt: vec!["hello".into()]
            })
        );
    }

    #[test]
    fn clap_schema_is_consistent() {
        Args::command().debug_assert();
        assert!(help_text(Some("plan")).unwrap().contains("validate"));
        assert!(command_completions().contains(&"/resume".into()));
    }
}
