use ollama_rs::{
    Ollama,
    generation::{completion::request::GenerationRequest, parameters::FormatType},
    models::ModelOptions,
};

use super::LlmConfig;

// LLM calling
pub async fn call_model(
    model: &str,
    user_prompt: String,
    system_prompt: Option<String>,
    config: LlmConfig,
) -> String {
    let ollama = Ollama::default(); // localhost:11434
    let options = ModelOptions::default()
        .temperature(config.temperature)
        .num_ctx(config.num_ctx);

    let mut request = GenerationRequest::new(model.to_string(), user_prompt).options(options);
    if let Some(sys) = system_prompt {
        request = request.system(sys);
    }
    if config.json_mode {
        request = request.format(FormatType::Json);
    }

    let response = ollama
        .generate(request)
        .await
        .expect("Failed to connect to Ollama");

    response.response
}
