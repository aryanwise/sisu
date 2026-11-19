mod ollama;
pub mod prompts;

pub use ollama::call_model;

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
