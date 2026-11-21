use crate::languages::LanguageAdapter;
use crate::models::Diagnostic;
use crate::sandbox::runner::SandboxRunner;
use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub struct PythonAdapter;

// --- Pyright JSON Schema (`pyright --outputjson`) ---
#[derive(Debug, Deserialize)]
struct PyrightReport {
    #[serde(rename = "generalDiagnostics", default)]
    general_diagnostics: Vec<PyrightDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct PyrightDiagnostic {
    file: String,
    severity: String,
    message: String,
    rule: Option<String>,
    range: PyrightRange,
}

#[derive(Debug, Deserialize)]
struct PyrightRange {
    start: PyrightPos,
}

#[derive(Debug, Deserialize)]
struct PyrightPos {
    line: usize,
    character: usize,
}

/// Converts raw `pyright --outputjson` stdout into uniform 1-indexed `Diagnostic`s.
pub fn parse_pyright_output(stdout: &str, project_dir: &Path) -> Result<Vec<Diagnostic>, String> {
    let report: PyrightReport = serde_json::from_str(stdout)
        .map_err(|e| format!("Failed to parse pyright JSON output: {e}"))?;

    let canonical_root = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let diagnostics = report
        .general_diagnostics
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| {
            let rel_file = Path::new(&d.file)
                .strip_prefix(&canonical_root)
                .or_else(|_| Path::new(&d.file).strip_prefix(project_dir))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(d.file);

            Diagnostic {
                file: rel_file,
                line: d.range.start.line + 1,
                column: d.range.start.character + 1,
                code: d.rule,
                message: d.message,
                related_spans: vec![],
                local_hint: None,
            }
        })
        .collect();

    Ok(diagnostics)
}

/// Parses `pytest -q --tb=short` failure lines (`path/file.py:12: AssertionError...`).
pub fn parse_pytest_output(stdout: &str, stderr: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let combined = format!("{stdout}\n{stderr}");

    for line in combined.lines() {
        let trimmed = line.trim();
        // Match short-traceback failure locations like `tests/test_math.py:8: AssertionError`
        let parts: Vec<&str> = trimmed.splitn(3, ':').collect();
        if parts.len() == 3 && parts[0].ends_with(".py") {
            if let Ok(line_num) = parts[1].trim().parse::<usize>() {
                diagnostics.push(Diagnostic {
                    file: parts[0].to_string(),
                    line: line_num,
                    column: 1,
                    code: Some("pytest_failure".into()),
                    message: parts[2].trim().to_string(),
                    related_spans: vec![],
                    local_hint: None,
                });
            }
        }
    }

    // Fallback: never silently drop a non-zero test failure if format was unexpected
    if diagnostics.is_empty() {
        diagnostics.push(Diagnostic {
            file: String::new(),
            line: 0,
            column: 0,
            code: Some("pytest_error".into()),
            message: combined.trim().to_string(),
            related_spans: vec![],
            local_hint: None,
        });
    }

    diagnostics
}

/// Locates `.venv/lib/python3.*/site-packages` (Unix) or `.venv/Lib/site-packages` (Windows)
/// without hardcoding a Python minor version.
fn find_site_packages_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    let unix_lib = project_dir.join(".venv/lib");
    if let Ok(entries) = fs::read_dir(&unix_lib) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("python") {
                let sp = entry.path().join("site-packages");
                if sp.is_dir() {
                    dirs.push(sp);
                }
            }
        }
    }

    let win_sp = project_dir.join(".venv/Lib/site-packages");
    if win_sp.is_dir() {
        dirs.push(win_sp);
    }

    dirs
}

fn probe_module_in_dir(base: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut pkg_dir = base.to_path_buf();
    for part in parts {
        pkg_dir.push(part);
    }

    // Prefer `.pyi` type stubs (smaller, cleaner API signatures) before `.py` source
    let candidates = [
        pkg_dir.join("__init__.pyi"),
        pkg_dir.with_extension("pyi"),
        pkg_dir.join("__init__.py"),
        pkg_dir.with_extension("py"),
    ];

    candidates.into_iter().find(|p| p.is_file())
}

#[async_trait]
impl LanguageAdapter for PythonAdapter {
    fn name(&self) -> &'static str {
        "python"
    }

    async fn scaffold(&self, project_dir: &Path, runner: &SandboxRunner) -> Result<(), String> {
        let venv_python = project_dir.join(".venv/bin/python");
        if !venv_python.exists() {
            let venv = runner
                .run_host_tool("uv", &["venv", ".venv"], project_dir)
                .await?;
            if venv.exit_code != 0 {
                return Err(format!(
                    "Failed to create .venv via uv: {}",
                    venv.stderr.trim()
                ));
            }
        }

        let venv_pytest = project_dir.join(".venv/bin/pytest");
        if !venv_pytest.exists() {
            let install = runner
                .run_host_tool(
                    "uv",
                    &["pip", "install", "--python", ".venv/bin/python", "pytest"],
                    project_dir,
                )
                .await?;
            if install.exit_code != 0 {
                return Err(format!(
                    "Failed to install pytest into .venv: {}",
                    install.stderr.trim()
                ));
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

        let mut args = vec!["pip", "install", "--python", ".venv/bin/python"];
        args.extend(deps.iter().map(|s| s.as_str()));

        let out = runner.run_host_tool("uv", &args, project_dir).await?;
        if out.timed_out {
            return Err("uv add timed out".into());
        }
        if out.exit_code != 0 {
            return Err(format!("uv add failed: {}", out.stderr.trim()));
        }

        Ok(())
    }
    async fn check_static(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        // Canonicalize `project_dir` (NOT `.venv/bin/python`, which is a symlink to base Python)
        let abs_project = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let venv_python = abs_project.join(".venv/bin/python");
        let venv_python_str = venv_python.to_string_lossy().into_owned();

        let mut args = vec!["--outputjson"];
        if venv_python.exists() {
            args.push("--pythonpath");
            args.push(&venv_python_str);
        }

        let out = runner.run_isolated("pyright", &args, project_dir).await?;
        if out.timed_out {
            return Err("pyright timed out".into());
        }

        // Pyright returns exit_code 1 when type errors are found; parse stdout JSON
        if out.stdout.trim().is_empty() && out.exit_code != 0 {
            return Err(format!("pyright execution failed: {}", out.stderr.trim()));
        }

        parse_pyright_output(&out.stdout, project_dir)
    }

    async fn run_tests(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        let venv_pytest = project_dir.join(".venv/bin/pytest");
        if !venv_pytest.exists() {
            return Err(format!(
                "Harness error: '{}' not found. Ensure scaffold() installed pytest.",
                venv_pytest.display()
            ));
        }

        let pytest_bin = venv_pytest
            .canonicalize()
            .unwrap_or(venv_pytest)
            .to_string_lossy()
            .into_owned();

        let out = runner
            .run_isolated(&pytest_bin, &["-q", "--tb=short"], project_dir)
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

        // Exit code 0 = all tests passed; Exit code 5 = no tests collected yet (normal for M1 setup)
        if out.exit_code == 0 || out.exit_code == 5 {
            return Ok(vec![]);
        }

        // Never send OS/sandbox launch failures into the LLM repair loop
        if out.stderr.contains("execvp()") || out.exit_code == 71 || out.exit_code == 127 {
            return Err(format!(
                "Sandbox failed to launch pytest: {}",
                out.stderr.trim()
            ));
        }

        Ok(parse_pytest_output(&out.stdout, &out.stderr))
    }

    fn resolve_pkg_path(&self, project_dir: &Path, package: &str) -> Option<PathBuf> {
        let normalized = package.trim().replace('-', "_");
        if normalized.is_empty() {
            return None;
        }

        let parts: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        for sp in find_site_packages_dirs(project_dir) {
            // Try exact dotted module first (e.g. `pydantic.main`), then top-level (`pydantic`)
            if let Some(found) = probe_module_in_dir(&sp, &parts) {
                return Some(found);
            }
            if parts.len() > 1 {
                if let Some(found) = probe_module_in_dir(&sp, &parts[..1]) {
                    return Some(found);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_pyright_errors_and_ignores_warnings() {
        let project_dir = PathBuf::from("/tmp/demo");
        let sample_json = r#"{
            "generalDiagnostics": [
                {
                    "file": "/tmp/demo/src/app.py",
                    "severity": "error",
                    "message": "Cannot access attribute \"dict\" for class \"User\"",
                    "rule": "reportAttributeAccessIssue",
                    "range": { "start": { "line": 13, "character": 4 } }
                },
                {
                    "file": "/tmp/demo/src/app.py",
                    "severity": "warning",
                    "message": "Unused import",
                    "rule": "reportUnusedImport",
                    "range": { "start": { "line": 0, "character": 0 } }
                }
            ]
        }"#;

        let diags = parse_pyright_output(sample_json, &project_dir).unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "src/app.py");
        assert_eq!(diags[0].line, 14); // 0-indexed 13 -> 1-indexed 14
        assert_eq!(diags[0].column, 5); // 0-indexed 4  -> 1-indexed 5
        assert_eq!(diags[0].code.as_deref(), Some("reportAttributeAccessIssue"));
    }

    #[test]
    fn parses_pytest_assertion_location() {
        let sample_stdout = "\
F                                                                        [100%]
=================================== FAILURES ===================================
tests/test_math.py:8: in test_add
    assert add(2, 3) == 5
E   assert -1 == 5
1 failed in 0.03s";

        let diags = parse_pytest_output(sample_stdout, "");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "tests/test_math.py");
        assert_eq!(diags[0].line, 8);
        assert_eq!(diags[0].code.as_deref(), Some("pytest_failure"));
    }

    #[test]
    fn registry_resolves_python_adapter() {
        let adapter = crate::languages::get_adapter("Python").unwrap();
        assert_eq!(adapter.name(), "python");
    }

    #[test]
    fn disk_sniffer_prefers_pyi_stubs_in_venv() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir
            .path()
            .join(".venv/lib/python3.12/site-packages/fake_pydantic");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("__init__.py"), "class BaseModel:\n    pass\n").unwrap();
        fs::write(
            pkg_dir.join("__init__.pyi"),
            "class BaseModel:\n    def model_dump(self) -> dict: ...\n",
        )
        .unwrap();

        let adapter = PythonAdapter;
        let found = adapter
            .resolve_pkg_path(dir.path(), "fake_pydantic")
            .unwrap();
        assert!(found.ends_with("__init__.pyi"));
        assert!(
            adapter
                .resolve_pkg_path(dir.path(), "nonexistent_pkg")
                .is_none()
        );
    }
}
