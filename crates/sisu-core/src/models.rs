use serde::{Deserialize, Serialize};

// State Types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    /// Model output exists, but it has not been applied or verified.
    Generated,
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
    #[serde(default)]
    pub dependencies: Vec<String>, // Package names, separate from milestone depends_on IDs
    pub milestones: Vec<Milestone>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedPayload {
    pub files: Vec<FileWrite>,
    #[serde(default)]
    pub add_dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWrite {
    pub path: String,
    pub content: String,
}
