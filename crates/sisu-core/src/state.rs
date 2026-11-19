use std::fs;
use std::path::{Path, PathBuf};

use crate::models::ProjectPlan;

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
