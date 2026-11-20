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
    pub attempts: u8,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub last_error: Option<String>,
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

// --- LLM Job #2: Surgical Repair ---
#[derive(Debug, Clone, Deserialize)]
pub struct RepairPayload {
    pub patches: Vec<CodePatch>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodePatch {
    pub path: String,
    pub original_snippet: String,
    pub replacement_snippet: String,
}

// --- Deterministic Sandbox Diagnostics ---
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub code: Option<String>, // e.g. "E0433" or "reportAttributeAccessIssue"
    pub message: String,
    #[serde(default)]
    pub related_spans: Vec<Span>, // Line X vs. Line Y secondary locations
    #[serde(default)]
    pub local_hint: Option<String>, // Filled by Disk Sniffer (Approach 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationLevel {
    D, // Failing ~15-line AST slice + local stubs
    C, // Call-graph / related_spans (Line X vs Line Y)
    B, // Full file + depends_on public signatures
    A, // Total failure -> rollback + copy-paste frontier report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_payload_defaults_missing_dependencies() {
        let raw = r#"{"files":[{"path":"src/main.rs","content":"fn main() {}"}]}"#;
        let payload: GeneratedPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(payload.files.len(), 1);
        assert!(payload.add_dependencies.is_empty());
    }
}
