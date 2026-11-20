use crate::languages::LanguageAdapter;
use crate::models::{Diagnostic, Span};
use crate::sandbox::runner::SandboxRunner;
use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub struct RustAdapter;

// --- Cargo NDJSON Schema (`cargo check --message-format=json`) ---
#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    message: Option<CompilerMessage>,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    level: String,
    message: String,
    code: Option<CompilerCode>,
    #[serde(default)]
    spans: Vec<CompilerSpan>,
}

#[derive(Debug, Deserialize)]
struct CompilerCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct CompilerSpan {
    file_name: String,
    line_start: usize,
    column_start: usize,
    is_primary: bool,
}

/// Parses line-delimited `cargo check --message-format=json` stdout into `Diagnostic`s,
/// preserving secondary spans (`is_primary == false`) for Point C escalation.
pub fn parse_cargo_json(stdout: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else {
            continue;
        };
        if msg.reason != "compiler-message" {
            continue;
        }
        let Some(compiler_msg) = msg.message else {
            continue;
        };
        if compiler_msg.level != "error" {
            continue;
        }
        // Skip Cargo's trailing summary line ("aborting due to N previous errors")
        if compiler_msg.spans.is_empty() && compiler_msg.code.is_none() {
            continue;
        }

        let primary = compiler_msg
            .spans
            .iter()
            .find(|s| s.is_primary)
            .or_else(|| compiler_msg.spans.first());

        let (file, line_num, col_num) = match primary {
            Some(s) => (s.file_name.clone(), s.line_start, s.column_start),
            None => (String::new(), 0, 0),
        };

        let related_spans = compiler_msg
            .spans
            .iter()
            .filter(|s| !s.is_primary)
            .map(|s| Span {
                file: s.file_name.clone(),
                line: s.line_start,
                column: s.column_start,
            })
            .collect();

        diagnostics.push(Diagnostic {
            file,
            line: line_num,
            column: col_num,
            code: compiler_msg.code.map(|c| c.code),
            message: compiler_msg.message,
            related_spans,
            local_hint: None,
        });
    }

    diagnostics
}

/// Parses `cargo test` panic lines (`panicked at src/lib.rs:14:9:`).
pub fn parse_cargo_test_output(stdout: &str, stderr: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let combined = format!("{stdout}\n{stderr}");
    let lines: Vec<&str> = combined.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        if let Some(after_at) = line.split("panicked at ").nth(1) {
            // Modern rustc format: `src/lib.rs:14:9:` followed by panic detail on the next line
            let location = after_at.trim().trim_end_matches(':');
            let parts: Vec<&str> = location.split(':').collect();
            if parts.len() >= 2 && parts[0].ends_with(".rs") {
                if let Ok(line_num) = parts[1].parse::<usize>() {
                    let col_num = parts.get(2).and_then(|c| c.parse().ok()).unwrap_or(1);
                    let detail = lines
                        .get(idx + 1)
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(line.trim());

                    diagnostics.push(Diagnostic {
                        file: parts[0].to_string(),
                        line: line_num,
                        column: col_num,
                        code: Some("cargo_test_failure".into()),
                        message: detail.to_string(),
                        related_spans: vec![],
                        local_hint: None,
                    });
                }
            }
        }
    }

    if diagnostics.is_empty() {
        diagnostics.push(Diagnostic {
            file: String::new(),
            line: 0,
            column: 0,
            code: Some("cargo_test_error".into()),
            message: combined.trim().to_string(),
            related_spans: vec![],
            local_hint: None,
        });
    }

    diagnostics
}

/// Searches a Cargo registry `src/` directory for `<crate>-<version>/src/lib.rs`.
fn resolve_crate_in_registry(registry_src: &Path, package: &str) -> Option<PathBuf> {
    let normalized_dash = package.trim().replace('_', "-");
    let normalized_under = package.trim().replace('-', "_");
    if normalized_dash.is_empty() {
        return None;
    }

    let mut matches: Vec<PathBuf> = Vec::new();

    if let Ok(index_dirs) = fs::read_dir(registry_src) {
        for index_entry in index_dirs.flatten() {
            if !index_entry.path().is_dir() {
                continue;
            }
            if let Ok(crates) = fs::read_dir(index_entry.path()) {
                for crate_entry in crates.flatten() {
                    let name = crate_entry.file_name().to_string_lossy().into_owned();
                    let matches_name = name.starts_with(&format!("{normalized_dash}-"))
                        || name.starts_with(&format!("{normalized_under}-"));

                    if matches_name {
                        let lib_rs = crate_entry.path().join("src/lib.rs");
                        if lib_rs.is_file() {
                            matches.push(lib_rs);
                        }
                    }
                }
            }
        }
    }

    // Sort descending so the newest cached version wins if multiple exist
    matches.sort();
    matches.pop()
}

#[async_trait]
impl LanguageAdapter for RustAdapter {
    fn name(&self) -> &'static str {
        "rust"
    }

    async fn scaffold(&self, project_dir: &Path, runner: &SandboxRunner) -> Result<(), String> {
        if !project_dir.join("Cargo.toml").exists() {
            let init = runner
                .run_host_tool("cargo", &["init", "--vcs", "none"], project_dir)
                .await?;
            if init.exit_code != 0 {
                return Err(format!("cargo init failed: {}", init.stderr.trim()));
            }
        }
        Ok(())
    }

    async fn install_deps(
        &self,
        project_dir: &Path,
        deps: &[String],
        runner: &SandboxRunner,
    ) -> Result<(), String> {
        if deps.is_empty() {
            return Ok(());
        }

        let mut args = vec!["add"];
        args.extend(deps.iter().map(|s| s.as_str()));

        let out = runner.run_host_tool("cargo", &args, project_dir).await?;
        if out.timed_out {
            return Err("cargo add timed out".into());
        }
        if out.exit_code != 0 {
            return Err(format!("cargo add failed: {}", out.stderr.trim()));
        }

        // Pre-fetch crate sources while Network is ON so Gate 1/2 work `--offline`
        let fetch = runner
            .run_host_tool("cargo", &["fetch"], project_dir)
            .await?;
        if fetch.exit_code != 0 {
            return Err(format!("cargo fetch failed: {}", fetch.stderr.trim()));
        }

        Ok(())
    }

    async fn check_static(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        let out = runner
            .run_isolated(
                "cargo",
                &["check", "--tests", "--offline", "--message-format=json"],
                project_dir,
            )
            .await?;

        if out.timed_out {
            return Err("cargo check timed out".into());
        }

        let diags = parse_cargo_json(&out.stdout);
        if diags.is_empty() && out.exit_code != 0 {
            return Err(format!("cargo check failed: {}", out.stderr.trim()));
        }

        Ok(diags)
    }

    async fn run_tests(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        let out = runner
            .run_isolated("cargo", &["test", "--offline"], project_dir)
            .await?;

        if out.timed_out {
            return Ok(vec![Diagnostic {
                file: String::new(),
                line: 0,
                column: 0,
                code: Some("timeout".into()),
                message: out.stderr,
                related_spans: vec![],
                local_hint: None,
            }]);
        }

        if out.exit_code == 0 {
            return Ok(vec![]);
        }

        Ok(parse_cargo_test_output(&out.stdout, &out.stderr))
    }

    fn resolve_pkg_path(&self, _project_dir: &Path, package: &str) -> Option<PathBuf> {
        let cargo_home = std::env::var("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cargo")))
            .ok()?;

        resolve_crate_in_registry(&cargo_home.join("registry/src"), package)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_cargo_ndjson_with_primary_and_related_spans() {
        let ndjson = r#"{"reason":"compiler-artifact","package_id":"demo 0.1.0"}
{"reason":"compiler-message","message":{"level":"warning","message":"unused var","code":null,"spans":[]}}
{"reason":"compiler-message","message":{"level":"error","message":"borrow of moved value: `s`","code":{"code":"E0382"},"spans":[{"file_name":"src/main.rs","line_start":4,"column_start":9,"is_primary":false},{"file_name":"src/main.rs","line_start":5,"column_start":20,"is_primary":true}]}}
{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 1 previous error","code":null,"spans":[]}}"#;

        let diags = parse_cargo_json(ndjson);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "src/main.rs");
        assert_eq!(diags[0].line, 5);
        assert_eq!(diags[0].column, 20);
        assert_eq!(diags[0].code.as_deref(), Some("E0382"));
        assert_eq!(diags[0].related_spans.len(), 1);
        assert_eq!(diags[0].related_spans[0].line, 4);
    }

    #[test]
    fn parses_cargo_test_panic_location() {
        let sample = "\
---- tests::adds_numbers stdout ----
thread 'tests::adds_numbers' panicked at src/lib.rs:14:9:
assertion `left == right` failed
  left: -1
 right: 5";

        let diags = parse_cargo_test_output(sample, "");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "src/lib.rs");
        assert_eq!(diags[0].line, 14);
        assert_eq!(diags[0].column, 9);
        assert!(
            diags[0]
                .message
                .contains("assertion `left == right` failed")
        );
    }

    #[test]
    fn disk_sniffer_resolves_crate_with_hyphen_normalization() {
        let dir = tempdir().unwrap();
        let crate_dir = dir
            .path()
            .join("index.crates.io-abc/async-trait-0.1.80/src");
        fs::create_dir_all(&crate_dir).unwrap();
        fs::write(crate_dir.join("lib.rs"), "pub fn macro_stub() {}\n").unwrap();

        let found = resolve_crate_in_registry(dir.path(), "async_trait").unwrap();
        assert!(found.ends_with("async-trait-0.1.80/src/lib.rs"));
    }
}
