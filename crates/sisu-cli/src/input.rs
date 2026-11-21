//! Terminal editing and literal-safe multiline prompt collection.
use std::{
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, DefaultPromptSegment, EditCommand, Emacs,
    FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu,
    Signal, Span, Suggestion, default_emacs_keybindings,
};

use crate::commands::{Command, command_completions, parse_slash};

#[derive(Debug, PartialEq, Eq)]
pub enum EditorOutcome {
    Send(String),
    Command(Command),
    Cancel,
    Exit,
}

#[derive(Default)]
struct CompletionState {
    words: Vec<String>,
    workspace: PathBuf,
}

struct CommandCompleter(Arc<Mutex<CompletionState>>);

impl Completer for CommandCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let Some(prefix) = line.get(..pos) else {
            return Vec::new();
        };
        let Ok(state) = self.0.lock() else {
            return Vec::new();
        };
        let mut suggestions: Vec<_> = state
            .words
            .iter()
            .filter(|word| word.starts_with(prefix))
            .map(|word| Suggestion {
                value: word.clone(),
                span: Span::new(0, pos),
                append_whitespace: true,
                ..Suggestion::default()
            })
            .collect();
        if let Some(path_prefix) = prefix.strip_prefix("/context add ") {
            suggestions.extend(path_completions(&state.workspace, path_prefix, pos));
        }
        suggestions.sort_by(|a, b| a.value.cmp(&b.value));
        suggestions.dedup_by(|a, b| a.value == b.value);
        suggestions
    }
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn path_completions(workspace: &Path, typed: &str, pos: usize) -> Vec<Suggestion> {
    let typed = typed.trim_matches(['\'', '"']);
    let requested = Path::new(typed);
    let (parent, partial) = if typed.ends_with('/') || typed.is_empty() {
        (requested, "")
    } else {
        (
            requested.parent().unwrap_or_else(|| Path::new("")),
            requested.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        )
    };
    let Ok(directory) = workspace.join(parent).canonicalize() else {
        return Vec::new();
    };
    if !directory.starts_with(workspace) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(partial) || name == ".sisu" || name == ".git" {
                return None;
            }
            let mut path = parent.join(name).to_string_lossy().into_owned();
            let directory = entry.file_type().ok()?.is_dir();
            if directory {
                path.push('/')
            }
            Some(Suggestion {
                value: format!("/context add {}", shell_quote(&path)),
                span: Span::new(0, pos),
                append_whitespace: !directory,
                ..Suggestion::default()
            })
        })
        .take(100)
        .collect()
}

pub struct Editor {
    terminal: Option<Reedline>,
    completions: Arc<Mutex<CompletionState>>,
}

impl Editor {
    pub fn new(history_path: PathBuf) -> Result<Self, String> {
        let completions = Arc::new(Mutex::new(CompletionState {
            words: command_completions(),
            workspace: std::env::current_dir().map_err(|e| e.to_string())?,
        }));
        let terminal = if io::stdin().is_terminal() && io::stdout().is_terminal() {
            if let Some(parent) = history_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create input history directory: {e}"))?;
            }
            let history = FileBackedHistory::with_file(1000, history_path)
                .map_err(|e| format!("Cannot open input history: {e}"))?;
            let mut bindings = default_emacs_keybindings();
            bindings.add_binding(
                KeyModifiers::NONE,
                KeyCode::Tab,
                ReedlineEvent::UntilFound(vec![
                    ReedlineEvent::Menu("completion_menu".into()),
                    ReedlineEvent::MenuNext,
                ]),
            );
            bindings.add_binding(
                KeyModifiers::ALT,
                KeyCode::Enter,
                ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
            );
            Some(
                Reedline::create()
                    .with_history(Box::new(history))
                    .with_completer(Box::new(CommandCompleter(completions.clone())))
                    .with_menu(ReedlineMenu::EngineCompleter(Box::new(
                        ColumnarMenu::default().with_name("completion_menu"),
                    )))
                    .with_edit_mode(Box::new(Emacs::new(bindings)))
                    .with_ansi_colors(false)
                    .use_bracketed_paste(true),
            )
        } else {
            None
        };
        Ok(Self {
            terminal,
            completions,
        })
    }

    /// Add runtime candidates such as `/resume <id>` and `/model <installed-model>`.
    pub fn update_completions(&mut self, extra: Vec<String>) {
        if let Ok(mut state) = self.completions.lock() {
            state.words = command_completions();
            state.words.extend(extra);
            state.words.sort();
            state.words.dedup();
        }
    }

    pub fn set_no_color(&mut self, no_color: bool) {
        if let Some(editor) = self.terminal.take() {
            self.terminal = Some(editor.with_ansi_colors(!no_color));
        }
    }

    pub fn set_workspace(&mut self, workspace: PathBuf) {
        if let Ok(mut state) = self.completions.lock() {
            state.workspace = workspace;
        }
    }

    /// Accumulate literal input until `/send`; slash commands use a dedicated line.
    pub fn read(&mut self, prompt: &str) -> Result<EditorOutcome, String> {
        let mut draft = Draft::default();
        loop {
            let signal = if let Some(editor) = &mut self.terminal {
                let left = if draft.lines.is_empty() {
                    prompt
                } else {
                    "… /send to submit"
                };
                let prompt = DefaultPrompt::new(
                    DefaultPromptSegment::Basic(left.into()),
                    DefaultPromptSegment::Empty,
                );
                let signal = editor
                    .read_line(&prompt)
                    .map_err(|e| format!("Terminal input failed: {e}"))?;
                editor
                    .sync_history()
                    .map_err(|e| format!("Cannot save input history: {e}"))?;
                signal
            } else {
                let mut line = String::new();
                match io::stdin().read_line(&mut line) {
                    Ok(0) => Signal::CtrlD,
                    Ok(_) => Signal::Success(line.trim_end_matches(['\r', '\n']).to_string()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => Signal::CtrlC,
                    Err(e) => return Err(format!("Input failed: {e}")),
                }
            };
            match signal {
                Signal::Success(line) => match draft.accept(line) {
                    Ok(Some(outcome)) => return Ok(outcome),
                    Ok(None) => {}
                    Err(error) => eprintln!("{error}"),
                },
                Signal::CtrlC => return Ok(EditorOutcome::Cancel),
                Signal::CtrlD => {
                    if !draft.lines.is_empty() {
                        eprintln!(
                            "Discarded unsent input at EOF; the saved conversation is retained."
                        );
                    }
                    return Ok(EditorOutcome::Exit);
                }
            }
        }
    }
}

#[derive(Default)]
struct Draft {
    lines: Vec<String>,
    fence: Option<char>,
}

impl Draft {
    fn accept(&mut self, line: String) -> Result<Option<EditorOutcome>, String> {
        // A bracketed multiline paste is always literal, including any slash-command lines.
        let dedicated = !line.contains('\n') && self.fence.is_none() && line.starts_with('/');
        if dedicated && line.starts_with("//") {
            self.append(line[1..].to_string());
            return Ok(None);
        }
        if dedicated {
            let trimmed = line.trim_end();
            if trimmed == "/send" {
                let prompt = self.lines.join("\n");
                if prompt.trim().is_empty() {
                    return Err("Prompt is empty. Enter text before /send.".into());
                }
                self.lines.clear();
                return Ok(Some(EditorOutcome::Send(prompt)));
            }
            if trimmed == "/cancel" {
                self.lines.clear();
                self.fence = None;
                return Ok(Some(EditorOutcome::Cancel));
            }
            let command = parse_slash(trimmed)?;
            if !self.lines.is_empty() {
                return Err("An input draft is pending. Use /send to submit it or /cancel to discard it before running a command.".into());
            }
            return Ok(Some(match command {
                Command::Exit => EditorOutcome::Exit,
                other => EditorOutcome::Command(other),
            }));
        }
        self.append(line);
        Ok(None)
    }

    fn append(&mut self, text: String) {
        for line in text.lines() {
            let trimmed = line.trim_start();
            for marker in ['`', '~'] {
                if trimmed.starts_with(&marker.to_string().repeat(3)) {
                    if self.fence == Some(marker) {
                        self.fence = None
                    } else if self.fence.is_none() {
                        self.fence = Some(marker)
                    }
                    break;
                }
            }
        }
        self.lines.push(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_code_indentation_and_trailing_slash_words() {
        let mut draft = Draft::default();
        draft.accept("    print('hi')  ".into()).unwrap();
        draft.accept("Explain /exit".into()).unwrap();
        assert_eq!(
            draft.accept("/send".into()).unwrap(),
            Some(EditorOutcome::Send(
                "    print('hi')  \nExplain /exit".into()
            ))
        );
    }

    #[test]
    fn pasted_commands_and_fenced_code_remain_literal() {
        let mut draft = Draft::default();
        draft.accept("example\n/exit\n/send".into()).unwrap();
        draft.accept("```text".into()).unwrap();
        draft.accept("/cancel".into()).unwrap();
        draft.accept("```".into()).unwrap();
        assert_eq!(
            draft.accept("/send".into()).unwrap(),
            Some(EditorOutcome::Send(
                "example\n/exit\n/send\n```text\n/cancel\n```".into()
            ))
        );
    }

    #[test]
    fn commands_do_not_discard_pending_input() {
        let mut draft = Draft::default();
        draft.accept("keep me".into()).unwrap();
        assert!(draft.accept("/status".into()).is_err());
        assert!(draft.accept("/exit".into()).is_err());
        assert_eq!(
            draft.accept("/send".into()).unwrap(),
            Some(EditorOutcome::Send("keep me".into()))
        );
    }

    #[test]
    fn literal_escape_cancel_and_empty_input() {
        let mut draft = Draft::default();
        assert!(draft.accept("/send".into()).is_err());
        draft.accept("//exit".into()).unwrap();
        assert_eq!(
            draft.accept("/send".into()).unwrap(),
            Some(EditorOutcome::Send("/exit".into()))
        );
        draft.accept("draft".into()).unwrap();
        assert_eq!(
            draft.accept("/cancel".into()).unwrap(),
            Some(EditorOutcome::Cancel)
        );
        assert_eq!(
            draft.accept("/status".into()).unwrap(),
            Some(EditorOutcome::Command(Command::Status))
        );
    }
}
