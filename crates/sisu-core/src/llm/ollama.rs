use std::{error::Error, fmt, time::Duration};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::StreamExt;

use super::{LlmConfig, ModelMessage};

const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum LlmError {
    InvalidConfig(String),
    Transport(String),
    Timeout(Duration),
    Api(String),
    InvalidResponse(String),
    Cancelled,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "Invalid model configuration: {message}"),
            Self::Transport(message) => write!(
                f,
                "Could not reach Ollama: {message}. Check the endpoint and run `ollama serve`."
            ),
            Self::Timeout(duration) => write!(
                f,
                "Ollama request timed out after {} seconds. Increase the request timeout or use a smaller model.",
                duration.as_secs_f64()
            ),
            Self::Api(message) => write!(f, "Ollama rejected the request: {message}"),
            Self::InvalidResponse(message) => write!(f, "Invalid Ollama response: {message}"),
            Self::Cancelled => write!(f, "Model request cancelled"),
        }
    }
}
impl Error for LlmError {}

/// Ollama transport with a total request deadline. Dropping a request future
/// closes the response stream; callers can use `tokio::select!` for cancellation.
#[derive(Clone, Debug)]
pub struct ModelClient {
    client: Client,
    endpoint: Url,
    timeout: Duration,
}

impl ModelClient {
    pub fn new(endpoint: &str, request_timeout: Duration) -> Result<Self, LlmError> {
        let mut endpoint = Url::parse(endpoint).map_err(|_| {
            LlmError::InvalidConfig("endpoint must be an absolute HTTP(S) URL".into())
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(LlmError::InvalidConfig(
                "endpoint must be an absolute HTTP(S) URL".into(),
            ));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(LlmError::InvalidConfig(
                "endpoint cannot contain credentials, a query, or a fragment".into(),
            ));
        }
        if request_timeout.is_zero() {
            return Err(LlmError::InvalidConfig(
                "timeout must be greater than zero".into(),
            ));
        }
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            endpoint,
            timeout: request_timeout,
        })
    }

    fn url(&self, route: &str) -> Result<Url, LlmError> {
        self.endpoint
            .join(route)
            .map_err(|e| LlmError::InvalidConfig(e.to_string()))
    }

    fn request_body(&self, model: &str, config: &LlmConfig) -> Result<Value, LlmError> {
        if model.trim().is_empty() {
            return Err(LlmError::InvalidConfig("model name cannot be empty".into()));
        }
        if !config.temperature.is_finite() || config.temperature < 0.0 || config.num_ctx == 0 {
            return Err(LlmError::InvalidConfig(
                "temperature must be finite and nonnegative; context size must be positive".into(),
            ));
        }
        let mut body = json!({"model": model, "stream": false, "options": {
            "temperature": config.temperature, "num_ctx": config.num_ctx
        }});
        if config.json_mode {
            body["format"] = json!("json");
        }
        Ok(body)
    }

    async fn response(&self, route: &str, body: &Value) -> Result<Response, LlmError> {
        let response = self
            .client
            .post(self.url(route)?)
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        self.check_status(response).await
    }

    async fn check_status(&self, response: Response) -> Result<Response, LlmError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let bytes = read_limited(response).await?;
        let message = String::from_utf8_lossy(&bytes);
        // Keep terminal diagnostics bounded; the server may return an HTML error page.
        let message: String = message.chars().take(2048).collect();
        Err(LlmError::Api(format!("HTTP {status}: {message}")))
    }

    pub async fn generate(
        &self,
        model: &str,
        prompt: String,
        system: Option<String>,
        config: LlmConfig,
    ) -> Result<String, LlmError> {
        let mut body = self.request_body(model, &config)?;
        body["prompt"] = json!(prompt);
        if let Some(system) = system {
            body["system"] = json!(system);
        }
        timeout(self.timeout, async {
            let response = self.response("api/generate", &body).await?;
            let value: Value = serde_json::from_slice(&read_limited(response).await?)
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
            reject_api_error(&value)?;
            if value.get("done").and_then(Value::as_bool) != Some(true) {
                return Err(LlmError::InvalidResponse(
                    "generation ended without a completion marker".into(),
                ));
            }
            value
                .get("response")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| LlmError::InvalidResponse("missing response text".into()))
        })
        .await
        .map_err(|_| LlmError::Timeout(self.timeout))?
    }

    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<ModelMessage>,
        config: LlmConfig,
    ) -> Result<String, LlmError> {
        if messages.is_empty() {
            return Err(LlmError::InvalidConfig(
                "chat requires at least one message".into(),
            ));
        }
        let mut body = self.request_body(model, &config)?;
        body["messages"] = json!(messages);
        timeout(self.timeout, async {
            let response = self.response("api/chat", &body).await?;
            let value: Value = serde_json::from_slice(&read_limited(response).await?)
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
            reject_api_error(&value)?;
            if value.get("done").and_then(Value::as_bool) != Some(true) {
                return Err(LlmError::InvalidResponse(
                    "chat ended without a completion marker".into(),
                ));
            }
            value
                .pointer("/message/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| LlmError::InvalidResponse("missing assistant message".into()))
        })
        .await
        .map_err(|_| LlmError::Timeout(self.timeout))?
    }

    /// Stream complete UTF-8 text fragments and return the complete assistant
    /// message only after Ollama's final `done` record. A closed receiver cancels.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<ModelMessage>,
        config: LlmConfig,
        tx: mpsc::Sender<String>,
    ) -> Result<String, LlmError> {
        if messages.is_empty() {
            return Err(LlmError::InvalidConfig(
                "chat requires at least one message".into(),
            ));
        }
        let mut body = self.request_body(model, &config)?;
        body["messages"] = json!(messages);
        body["stream"] = json!(true);
        timeout(self.timeout, async {
            let response = self.response("api/chat", &body).await?;
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut output = String::new();
            let mut received = 0usize;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| LlmError::Transport(e.to_string()))?;
                received = received.saturating_add(chunk.len());
                if received > RESPONSE_LIMIT {
                    return Err(LlmError::InvalidResponse("response exceeded 16 MiB".into()));
                }
                buffer.extend_from_slice(&chunk);
                while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=end).collect();
                    if stream_record(&line, &mut output, &tx).await? {
                        return Ok(output);
                    }
                }
            }
            if !buffer.is_empty() && stream_record(&buffer, &mut output, &tx).await? {
                return Ok(output);
            }
            Err(LlmError::InvalidResponse(
                "stream ended before the final completion marker; partial output was not accepted"
                    .into(),
            ))
        })
        .await
        .map_err(|_| LlmError::Timeout(self.timeout))?
    }

    pub async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        timeout(self.timeout, async {
            let response = self
                .client
                .get(self.url("api/tags")?)
                .send()
                .await
                .map_err(|e| LlmError::Transport(e.to_string()))?;
            let response = self.check_status(response).await?;
            let value: Value = serde_json::from_slice(&read_limited(response).await?)
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
            reject_api_error(&value)?;
            #[derive(Deserialize)]
            struct Model {
                name: String,
            }
            #[derive(Deserialize)]
            struct Models {
                models: Vec<Model>,
            }
            let mut models: Vec<String> = serde_json::from_value::<Models>(value)
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?
                .models
                .into_iter()
                .map(|m| m.name)
                .collect();
            models.sort();
            models.dedup();
            Ok(models)
        })
        .await
        .map_err(|_| LlmError::Timeout(self.timeout))?
    }
}

fn reject_api_error(value: &Value) -> Result<(), LlmError> {
    if let Some(error) = value.get("error") {
        return Err(LlmError::Api(
            error
                .as_str()
                .unwrap_or("unspecified API error")
                .chars()
                .take(2048)
                .collect(),
        ));
    }
    Ok(())
}

async fn read_limited(response: Response) -> Result<Vec<u8>, LlmError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| LlmError::Transport(e.to_string()))?;
        if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err(LlmError::InvalidResponse("response exceeded 16 MiB".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn stream_record(
    line: &[u8],
    output: &mut String,
    tx: &mpsc::Sender<String>,
) -> Result<bool, LlmError> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_slice(line).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    reject_api_error(&value)?;
    let text = value
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LlmError::InvalidResponse("stream record is missing message content".into())
        })?;
    if !text.is_empty() {
        output.push_str(text);
        tx.send(text.to_owned())
            .await
            .map_err(|_| LlmError::Cancelled)?;
    }
    Ok(value.get("done").and_then(Value::as_bool) == Some(true))
}

/// Convenience entry point with local defaults. Applications that expose
/// endpoint/timeout settings should retain a `ModelClient` instead.
pub async fn call_model(
    model: &str,
    user_prompt: String,
    system_prompt: Option<String>,
    config: LlmConfig,
) -> Result<String, LlmError> {
    ModelClient::new("http://localhost:11434", Duration::from_secs(120))?
        .generate(model, user_prompt, system_prompt, config)
        .await
}
