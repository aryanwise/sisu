use crate::llm::prompts::EXECUTOR_SYSTEM_PROMPT;
use crate::llm::{LlmConfig, call_model};
use crate::models::{ProjectPlan, TaskStatus};
use crate::state::persist_plan;

/*pub makes the function accessible outside sisu-core.
&str accepts the user's request without taking ownership.
String is the returned result. */
pub fn create_plan(request: &str) -> String {
    format!("Planning Task: {}", request)
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
            "Project Goal: {}\nTarget Language: {}\nDefault Check Command: {}\nDeclared Project Dependencies: {:?}\nMilestone [{}]: {}\nDescription: {}\nDone when: {:?}",
            plan.goal,
            plan.language,
            plan.check_command,
            plan.dependencies,
            milestone.id,
            milestone.title,
            milestone.description,
            milestone.done_when
        );

        let result = call_model(
            model,
            task_prompt,
            Some(EXECUTOR_SYSTEM_PROMPT.to_string()),
            LlmConfig::deterministic(),
        )
        .await;
        println!("Result:\n{}\n", result);

        plan.milestones[index].status = TaskStatus::Completed;
        let _ = persist_plan(project_dir, plan);
    }
}
