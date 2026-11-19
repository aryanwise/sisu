use std::io::{self, Write};

// modes - General, Project
#[derive(Debug, PartialEq)]
pub enum Mode {
    General,
    Project,
}

// send, cancel, exit commands for prompt input

pub enum InputAction {
    UserPrompt(String),
    Send(Option<String>),
    Cancel,
    Exit,
}

pub enum EditorOutcome {
    Send(String),
    Cancel,
    Exit,
}

// modes 1-General 2-Project
pub fn select_mode() -> Option<Mode> {
    loop {
        print!("Enter: [1] General or [2] Project or to /exit: ");
        io::stdout().flush().expect("Failed to flush stdout");

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read input");

        match input.trim().to_lowercase().as_str() {
            "1" | "general" => return Some(Mode::General),
            "2" | "project" => return Some(Mode::Project),
            "/exit" => return None,
            _ => println!("Invalid choice. Please enter 1 or 2.\n"),
        }
    }
}

fn parse_line(raw: &str) -> InputAction {
    let cleaned = raw.trim();

    // Check if the line ends with "/send"
    if let Some(before_cmd) = cleaned.strip_suffix("/send") {
        // only treat it as a command if "/send" is standalone OR preceded by whitespace
        if before_cmd.is_empty() || before_cmd.ends_with(char::is_whitespace) {
            let trailing_text = before_cmd.trim();
            if trailing_text.is_empty() {
                return InputAction::Send(None);
            } else {
                return InputAction::Send(Some(trailing_text.to_string()));
            }
        }
    }

    if let Some(before_cmd) = cleaned.strip_suffix("/cancel") {
        if before_cmd.is_empty() || before_cmd.ends_with(char::is_whitespace) {
            return InputAction::Cancel;
        }
    }

    if let Some(before_cmd) = cleaned.strip_suffix("/exit") {
        if before_cmd.is_empty() || before_cmd.ends_with(char::is_whitespace) {
            return InputAction::Exit;
        }
    }

    InputAction::UserPrompt(cleaned.to_string())
}

pub fn collect_prompt() -> EditorOutcome {
    println!("Enter Prompt below or type /send, /cancel, /exit");

    let mut lines: Vec<String> = Vec::new();
    loop {
        print!(">>> ");
        io::stdout().flush().expect("Failed to flush stdout");
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read input");

        let action = parse_line(&input);

        match action {
            InputAction::UserPrompt(text) => {
                lines.push(text);
            }
            InputAction::Send(maybe_last_line) => {
                if let Some(last_line) = maybe_last_line {
                    lines.push(last_line);
                }
                let full_prompt = lines.join("\n").trim().to_string();
                if full_prompt.is_empty() {
                    println!("Prompt is empty! Type something before /send.");
                    lines.clear();
                } else {
                    return EditorOutcome::Send(full_prompt);
                }
            }
            InputAction::Cancel => {
                return EditorOutcome::Cancel;
            }
            InputAction::Exit => {
                return EditorOutcome::Exit;
            }
        }
    }
}
