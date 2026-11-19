use sisu_core::{
    EditorOutcome, Mode, PROJECT_SYSTEM_PROMPT, call_model, collect_prompt, execute_milestones,
    parse_project_plan, persist_plan, select_mode,
};

#[tokio::main]
async fn main() {
    let model_name = "qwen2.5-coder:7b";
    // let model_name = "qwen2.5-coder:3b";

    loop {
        let current_mode = match select_mode() {
            Some(mode) => mode,
            None => {
                break;
            }
        };

        match collect_prompt() {
            EditorOutcome::Send(prompt) => {
                println!("\n[{:?} Mode] Thinking...\n", current_mode);

                match current_mode {
                    Mode::General => {
                        let response = call_model(model_name, prompt, None, false).await;
                        println!("{}", response);
                    }
                    Mode::Project => {
                        let md_response = call_model(
                            model_name,
                            prompt,
                            Some(PROJECT_SYSTEM_PROMPT.to_string()),
                            true,
                        )
                        .await;

                        let mut plan = parse_project_plan(&md_response)
                            .expect("Model returned invalid JSON for ProjectPlan");

                        persist_plan("project", &plan).expect("Failed to save milestones.json");
                        execute_milestones(model_name, "project", &mut plan).await;
                    }
                }

                break;
            }
            EditorOutcome::Cancel => {}
            EditorOutcome::Exit => {
                break;
            }
        }
    }
}
