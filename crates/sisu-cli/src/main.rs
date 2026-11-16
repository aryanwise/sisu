use sisu_core::{
    EditorOutcome, Mode, PROJECT_SYSTEM_PROMPT, call_model, collect_prompt, select_mode,
};

#[tokio::main]
async fn main() {
    // let model_name = "qwen2.5-coder:7b";
    let model_name = "qwen2.5-coder:3b";

    loop {
        let current_mode = match select_mode() {
            Some(mode) => mode,
            None => {
                break;
            }
        };

        match collect_prompt() {
            EditorOutcome::Send(prompt) => {
                let system_prompt = match current_mode {
                    Mode::General => None,
                    Mode::Project => Some(PROJECT_SYSTEM_PROMPT.to_string()),
                };
                println!("\n[{:?} Mode] Thinking...\n", current_mode);
                call_model(model_name, prompt, system_prompt).await;
                break;
            }
            EditorOutcome::Cancel => {}
            EditorOutcome::Exit => {
                break;
            }
        }
    }
}
