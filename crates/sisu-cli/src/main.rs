mod app;
mod commands;
mod config;
mod display;
mod input;
mod session;

use clap::{CommandFactory, FromArgMatches};
use commands::{Args, Command};
use display::{Display, Event};
use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let options: Vec<_> = std::env::args_os().take_while(|arg| arg != "--").collect();
    let json_requested = options.iter().any(|arg| arg == "--json");
    let no_color =
        options.iter().any(|arg| arg == "--no-color") || std::env::var_os("NO_COLOR").is_some();
    let mut command = Args::command();
    if no_color || json_requested {
        command = command.color(clap::ColorChoice::Never);
    }
    let matches = match command.try_get_matches() {
        Ok(matches) => matches,
        Err(error) if json_requested && error.use_stderr() => {
            let _ = Display::new(true, false).render(&Event::Failed {
                message: error.to_string(),
            });
            return ExitCode::from(2);
        }
        Err(error) => error.exit(),
    };
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    let mut display = Display::new(args.json, args.quiet);
    match run(args, &mut display).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = display.render(&Event::Failed { message: error });
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args, display: &mut Display) -> Result<u8, String> {
    let workspace = std::fs::canonicalize(&args.workspace)
        .map_err(|e| format!("Cannot open workspace {}: {e}", args.workspace.display()))?;
    if !workspace.is_dir() {
        return Err("Workspace must be a directory.".into());
    }
    let config = config::Config::load(
        &workspace,
        &config::Overrides {
            model: args.model,
            endpoint: args.endpoint,
            timeout_secs: args.timeout_secs,
            context_limit: args.context_limit,
        },
    )?;
    let interactive = args.command.is_none() && io::stdin().is_terminal();
    let mut app = app::App::new(workspace, config, display);
    if let Some(id) = args.session {
        app.restore_session(&id)?;
    }
    if !interactive {
        let command = match args.command {
            Some(command) => command,
            None => Command::Chat {
                prompt: vec!["-".into()],
            },
        };
        let command = resolve_stdin(command)?;
        if let Err(error) = app.dispatch(command).await {
            app.record_failure(&error)?;
            return Ok(1);
        }
        return Ok(app.exit_code);
    }
    // Do not place a line editor on stdout when machine output was requested.
    if app.display.json {
        return Err("Interactive --json is unsupported. Use a subcommand or pipe a prompt.".into());
    }
    app.output("welcome", serde_json::json!("Sisu — /help for commands. Write a prompt, then /send on its own line. // escapes a literal slash command."))?;
    let history = app.history_path()?;
    let mut editor = input::Editor::new(history)?;
    editor.set_workspace(app.workspace.clone());
    editor.set_no_color(args.no_color || std::env::var_os("NO_COLOR").is_some());
    loop {
        editor.update_completions(app.completions());
        let prompt = app.prompt();
        // The editor owns terminal input only while idle. During requests Ctrl+C
        // belongs to the async operation; no output races with an active editor.
        let (returned_editor, outcome) = tokio::task::spawn_blocking(move || {
            let outcome = editor.read(&prompt);
            (editor, outcome)
        })
        .await
        .map_err(|e| format!("Input task failed: {e}"))?;
        editor = returned_editor;
        let command = match outcome? {
            input::EditorOutcome::Send(prompt) => app.prompt_command(prompt),
            input::EditorOutcome::Command(command) => command,
            input::EditorOutcome::Cancel => continue,
            input::EditorOutcome::Exit => break,
        };
        match app.dispatch(command).await {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => app.record_failure(&error)?,
        }
    }
    app.save()?;
    Ok(0)
}

fn resolve_stdin(command: Command) -> Result<Command, String> {
    match command {
        Command::Chat { prompt } if prompt.is_empty() || prompt == ["-"] => {
            if io::stdin().is_terminal() {
                return Err("Provide a prompt or pipe text into `sisu chat -`.".into());
            }
            let prompt = read_prompt(io::stdin().lock())?;
            Ok(Command::Chat {
                prompt: vec![prompt],
            })
        }
        Command::Plan { goal } if goal == ["-"] => {
            if io::stdin().is_terminal() {
                return Err("Pipe a goal into `sisu plan -`.".into());
            }
            let goal = read_prompt(io::stdin().lock())?;
            Ok(Command::Plan { goal: vec![goal] })
        }
        other => Ok(other),
    }
}

fn read_prompt(input: impl Read) -> Result<String, String> {
    const MAX: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    input
        .take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read stdin: {e}"))?;
    if bytes.len() as u64 > MAX {
        return Err("Piped input exceeds 1 MiB. Provide a smaller prompt.".into());
    }
    String::from_utf8(bytes).map_err(|_| "Piped input must be UTF-8 text.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdin_is_bounded_and_literal() {
        assert_eq!(
            read_prompt(&b"code:\n    return 42\n"[..]).unwrap(),
            "code:\n    return 42\n"
        );
        assert!(
            read_prompt(std::io::repeat(b'x'))
                .unwrap_err()
                .contains("1 MiB")
        );
        assert!(read_prompt(&[0xff][..]).unwrap_err().contains("UTF-8"));
    }
}
