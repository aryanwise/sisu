use ollama_rs::{Ollama, generation::completion::request::GenerationRequest};
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

/*pub makes the function accessible outside sisu-core.
&str accepts the user's request without taking ownership.
String is the returned result. */
pub fn create_plan(request: &str) -> String {
    format!("Planning Task: {}", request)
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

// system prompt - LLM system prompt for breaking milestones
pub const PROJECT_SYSTEM_PROMPT: &str = r#"
You are SISU's software project planner.

Convert the user's request into 3–8 logical, executable milestones.

Rules:
- Each milestone must have a clear objective and verifiable completion criteria.
- Order milestones by dependencies.
- Use sequential IDs: M1, M2, M3...
- Dependencies must reference earlier milestone IDs only.
- Avoid unnecessary complexity and implementation details.
- Do not generate code or explanations.
- Return valid JSON only, without markdown.

JSON structure:
{
  "goal": "Overall project objective",
  "milestones": [
    {
      "id": "M1",
      "title": "Short title",
      "description": "What this milestone must accomplish",
      "depends_on": [],
      "done_when": ["Measurable success condition"]
    }
  ]
}
"#;

// LLM calling
pub async fn call_model(model: &str, user_prompt: String, system_prompt: Option<String>) {
    let ollama = Ollama::default(); // localhost:11434
    let mut request = GenerationRequest::new(model.to_string(), user_prompt);
    if let Some(sys) = system_prompt {
        request = request.system(sys);
    }

    let response = ollama
        .generate(request)
        .await
        .expect("Failed to connect to Ollama");

    println!("{}", response.response);
}
// generate milestones: takes system prompt and user prompt

// milestone structure
