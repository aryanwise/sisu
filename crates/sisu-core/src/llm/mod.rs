mod ollama;
pub mod prompts;

use serde::{Deserialize, Serialize};

pub use ollama::{LlmError, ModelClient, call_model};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub temperature: f32,
    pub num_ctx: u64,
    pub json_mode: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: MessageRole,
    pub content: String,
}
