use ollama_rs::{
    Ollama,
    generation::{completion::request::GenerationRequest, parameters::FormatType},
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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

// State Types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub done_when: Vec<String>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub attempts: u32, // Tracks how many compiler/test retries this milestone took
    #[serde(default)]
    pub artifacts: Vec<String>, // Files created/modified during this milestone (e.g., "src/main.rs")
    #[serde(default)]
    pub last_error: Option<String>, // Captures compiler/sandbox stderr if it failed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPlan {
    pub goal: String,
    pub language: String,      // e.g. "rust", "python", "typescript", "cpp"
    pub check_command: String, // e.g. "cargo check", "python -m py_compile", "npx tsc --noEmit"
    pub milestones: Vec<Milestone>,
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
- Identify the primary programming language requested by the user (e.g., "rust", "python", "typescript", "go", "cpp"). If unspecified, choose the best fit.
- Specify the default compiler/typecheck command for that language in "check_command" (e.g., "cargo check", "pytest", "tsc --noEmit", "go build ./...").
- Each milestone must have a clear objective and verifiable completion criteria.
- Order milestones by dependencies.
- Use sequential IDs: M1, M2, M3...
- Dependencies must reference earlier milestone IDs only.
- Do not generate code or explanations.
- Return valid JSON only, without markdown.

JSON structure:
{
  "goal": "Overall project objective",
  "language": "rust",
  "check_command": "cargo check",
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

// execution system prompt
pub const EXECUTOR_SYSTEM_PROMPT: &str = r#"
You are SISU's autonomous code execution engine.
Implement the given milestone strictly in the target language specified.

Rules:
- Use ONLY the target language and tech stack specified in the prompt.
- Output ONLY valid JSON with the files to write and commands to run.
- Do not include markdown fences, explanations, or tutorials.

JSON format:
{
  "files": [
    {
      "path": "relative/path/to/file",
      "content": "full file content here"
    }
  ],
  "commands": ["verification command"]
}
"#;

// LLM calling
pub async fn call_model(
    model: &str,
    user_prompt: String,
    system_prompt: Option<String>,
    json_mode: bool,
) -> String {
    let ollama = Ollama::default(); // localhost:11434
    let mut request = GenerationRequest::new(model.to_string(), user_prompt);
    if let Some(sys) = system_prompt {
        request = request.system(sys);
    }
    if json_mode {
        request = request.format(FormatType::Json);
    }

    let response = ollama
        .generate(request)
        .await
        .expect("Failed to connect to Ollama");

    response.response
}

pub fn parse_project_plan(raw_output: &str) -> Result<ProjectPlan, serde_json::Error> {
    let trimmed = raw_output.trim();
    let without_prefix = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let cleaned = without_prefix
        .strip_suffix("```")
        .unwrap_or(without_prefix)
        .trim();

    serde_json::from_str::<ProjectPlan>(cleaned)
}

// milestone structure

pub fn persist_plan(
    project_dir: &str,
    plan: &ProjectPlan,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir_path = Path::new(project_dir);
    fs::create_dir_all(dir_path)?;

    let file_path = dir_path.join("milestones.json");
    let pretty_json = serde_json::to_string_pretty(plan)?;
    fs::write(&file_path, pretty_json)?;

    Ok(file_path)
}

pub async fn execute_milestones(model: &str, project_dir: &str, plan: &mut ProjectPlan) {
    for index in 0..plan.milestones.len() {
        // skip if completed
        if plan.milestones[index].status == TaskStatus::Completed {
            continue;
        }

        let deps_met = plan.milestones[index].depends_on.iter().all(|dep_id| {
            plan.milestones
                .iter()
                .any(|m| &m.id == dep_id && m.status == TaskStatus::Completed)
        });
        if !deps_met {
            println!(
                "Skipping [{}]: unmet dependencies",
                plan.milestones[index].id
            );
            continue;
        }

        // only mark "InProgress" once we know all dependencies succeeded
        plan.milestones[index].status = TaskStatus::InProgress;
        let _ = persist_plan(project_dir, plan);

        let milestone = &plan.milestones[index];
        println!("=> Executing [{}]: {}", milestone.id, milestone.title);

        let task_prompt = format!(
            "Project Goal: {}\nTarget Language: {}\nDefault Check Command: {}\nMilestone [{}]: {}\nDescription: {}\nDone when: {:?}",
            plan.goal,
            plan.language,
            plan.check_command,
            milestone.id,
            milestone.title,
            milestone.description,
            milestone.done_when
        );

        let result = call_model(
            model,
            task_prompt,
            Some(EXECUTOR_SYSTEM_PROMPT.to_string()),
            true,
        )
        .await;
        println!("Result:\n{}\n", result);

        plan.milestones[index].status = TaskStatus::Completed;
        let _ = persist_plan(project_dir, plan);
    }
}
