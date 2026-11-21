use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Only non-secret runtime settings belong in this structure and its diagnostic output.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub model: String,
    pub endpoint: String,
    pub timeout_secs: u64,
    pub context_limit: u64,
    pub origins: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    pub model: Option<String>,
    pub endpoint: Option<String>,
    pub timeout_secs: Option<u64>,
    pub context_limit: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "qwen2.5-coder:7b".into(),
            endpoint: "http://localhost:11434".into(),
            timeout_secs: 120,
            context_limit: 8192,
            origins: ["model", "endpoint", "timeout_secs", "context_limit"]
                .into_iter()
                .map(|key| (key.to_string(), "default".to_string()))
                .collect(),
        }
    }
}

impl Config {
    pub fn load(workspace: &Path, overrides: &Overrides) -> Result<Self, String> {
        Self::load_with(workspace, overrides, |key| std::env::var(key).ok())
    }

    fn load_with(
        workspace: &Path,
        overrides: &Overrides,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut config = Self::default();
        let user_path = env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join("sisu/config.json"));
        if let Some(path) = user_path {
            config.apply_file(&path)?;
        }
        config.apply_file(&workspace.join(".sisu/config.json"))?;
        let environment = Overrides {
            model: env("SISU_MODEL"),
            endpoint: env("SISU_ENDPOINT"),
            timeout_secs: parse_env_number("SISU_TIMEOUT_SECS", &env)?,
            context_limit: parse_env_number("SISU_CONTEXT_LIMIT", &env)?,
        };
        config.apply(&environment, "environment");
        config.apply(overrides, "CLI flag");
        config.validate()?;
        Ok(config)
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "Cannot read configuration {}: {error}",
                    path.display()
                ));
            }
        };
        let overrides: Overrides = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid configuration {}: {error}", path.display()))?;
        self.apply(&overrides, &path.display().to_string());
        Ok(())
    }

    fn apply(&mut self, overrides: &Overrides, origin: &str) {
        if let Some(model) = &overrides.model {
            self.model.clone_from(model);
            self.origins.insert("model".into(), origin.into());
        }
        if let Some(endpoint) = &overrides.endpoint {
            self.endpoint.clone_from(endpoint);
            self.origins.insert("endpoint".into(), origin.into());
        }
        if let Some(timeout_secs) = overrides.timeout_secs {
            self.timeout_secs = timeout_secs;
            self.origins.insert("timeout_secs".into(), origin.into());
        }
        if let Some(context_limit) = overrides.context_limit {
            self.context_limit = context_limit;
            self.origins.insert("context_limit".into(), origin.into());
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("Model must not be empty; set --model or SISU_MODEL.".into());
        }
        let address = self
            .endpoint
            .strip_prefix("http://")
            .or_else(|| self.endpoint.strip_prefix("https://"))
            .ok_or("Endpoint must start with http:// or https://.")?;
        if address.is_empty()
            || address.starts_with('/')
            || address.chars().any(char::is_whitespace)
        {
            return Err("Endpoint must include a valid host and contain no whitespace.".into());
        }
        // Never echo an endpoint containing credentials in /config or errors.
        if address.contains('@') || address.contains('?') || address.contains('#') {
            return Err(
                "Endpoint must not contain embedded credentials, query parameters, or fragments."
                    .into(),
            );
        }
        if self.timeout_secs == 0 {
            return Err("Request timeout must be greater than zero.".into());
        }
        if self.context_limit == 0 {
            return Err("Context limit must be greater than zero.".into());
        }
        // Use the transport's complete URL parser as well as the secret guard
        // above. This constructs a client but performs no provider request.
        sisu_core::ModelClient::new(
            &self.endpoint,
            std::time::Duration::from_secs(self.timeout_secs),
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn parse_env_number(
    key: &str,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<u64>, String> {
    env(key)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{key} must be a positive integer."))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sisu-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join(".sisu")).unwrap();
        std::fs::create_dir_all(dir.join("user/sisu")).unwrap();
        dir
    }

    #[test]
    fn precedence_and_origins_are_stable() {
        let dir = fixture();
        std::fs::write(
            dir.join("user/sisu/config.json"),
            r#"{"model":"user","timeout_secs":5}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".sisu/config.json"),
            r#"{"model":"project","context_limit":2048}"#,
        )
        .unwrap();
        let env = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some(dir.join("user").display().to_string()),
            "SISU_MODEL" => Some("environment".into()),
            "SISU_TIMEOUT_SECS" => Some("7".into()),
            _ => None,
        };
        let config = Config::load_with(
            &dir,
            &Overrides {
                model: Some("flag".into()),
                ..Default::default()
            },
            env,
        )
        .unwrap();
        assert_eq!(config.model, "flag");
        assert_eq!(config.timeout_secs, 7);
        assert_eq!(config.context_limit, 2048);
        assert_eq!(config.origins["model"], "CLI flag");
        assert_eq!(config.origins["timeout_secs"], "environment");
        assert!(config.origins["context_limit"].ends_with(".sisu/config.json"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_invalid_endpoint_ports_before_contacting_provider() {
        for endpoint in ["http://localhost:abc", "http://localhost:65536", "http://:"] {
            let config = Config {
                endpoint: endpoint.into(),
                ..Default::default()
            };
            assert!(config.validate().is_err(), "accepted {endpoint}");
        }
    }

    #[test]
    fn rejects_invalid_config_and_environment() {
        let dir = fixture();
        std::fs::write(dir.join(".sisu/config.json"), r#"{"typo":1}"#).unwrap();
        assert!(
            Config::load_with(&dir, &Overrides::default(), |_| None)
                .unwrap_err()
                .contains("Invalid configuration")
        );
        std::fs::remove_file(dir.join(".sisu/config.json")).unwrap();
        assert!(
            Config::load_with(&dir, &Overrides::default(), |key| (key
                == "SISU_TIMEOUT_SECS")
                .then(|| "bad".into()))
            .is_err()
        );
        assert!(
            Config::load_with(
                &dir,
                &Overrides {
                    endpoint: Some("http://secret@localhost".into()),
                    ..Default::default()
                },
                |_| None
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
