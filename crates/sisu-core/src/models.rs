use serde::{Deserialize, Serialize};

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
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) depends_on: Vec<String>,
    pub(crate) done_when: Vec<String>,
    #[serde(default)]
    pub(crate) status: TaskStatus,
    #[serde(default)]
    attempts: u32, // Tracks how many compiler/test retries this milestone took
    #[serde(default)]
    artifacts: Vec<String>, // Files created/modified during this milestone (e.g., "src/main.rs")
    #[serde(default)]
    last_error: Option<String>, // Captures compiler/sandbox stderr if it failed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPlan {
    pub(crate) goal: String,
    pub(crate) language: String, // e.g. "rust", "python", "typescript", "cpp"
    pub(crate) check_command: String, // e.g. "cargo check", "python -m py_compile", "npx tsc --noEmit"
    #[serde(default)]
    pub(crate) dependencies: Vec<String>, // Package names, separate from milestone depends_on IDs
    pub(crate) milestones: Vec<Milestone>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedPayload {
    pub(crate) files: Vec<FileWrite>,
    #[serde(default)]
    pub(crate) add_dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWrite {
    pub(crate) path: String,
    pub(crate) content: String,
}
