use std::{error::Error, fmt};

use crate::models::ProjectPlan;

pub fn create_plan(request: &str) -> String {
    format!("Planning Task: {request}")
}

#[derive(Debug)]
pub struct ExecutionUnavailable;
impl fmt::Display for ExecutionUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Execution unavailable: no sandbox backend or verifier is configured. Plans can be generated and validated, but milestones cannot be executed yet."
        )
    }
}
impl Error for ExecutionUnavailable {}

/// Kept as an explicit capability boundary for existing callers. Generating a
/// response is not execution and must never mark a milestone completed.
pub async fn execute_milestones(
    _model: &str,
    _project_dir: &str,
    _plan: &mut ProjectPlan,
) -> Result<(), ExecutionUnavailable> {
    Err(ExecutionUnavailable)
}
