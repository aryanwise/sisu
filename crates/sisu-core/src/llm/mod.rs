mod ollama;
pub mod prompts;

use crate::orchestrator::LlmClient;
use async_trait::async_trait;

pub use ollama::call_model;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    temperature: f32,
    num_ctx: u64,
    json_mode: bool,
}

impl LlmConfig {
    pub fn deterministic() -> Self {
        Self {
            temperature: 0.0,
            num_ctx: 8192,
            json_mode: true,
        }
    }
    pub fn conversational() -> Self {
        Self {
            temperature: 0.7,
            num_ctx: 4096,
            json_mode: false,
        }
    }
}

pub struct OllamaClient {
    pub model: String,
    pub config: LlmConfig,
}

impl OllamaClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            config: LlmConfig::deterministic(),
        }
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    async fn generate(&self, system_prompt: &str, user_prompt: &str) -> Result<String, String> {
        let response = call_model(
            &self.model,
            user_prompt.to_string(),
            Some(system_prompt.to_string()),
            self.config.clone(),
        )
        .await;

        Ok(response)
    }
}
