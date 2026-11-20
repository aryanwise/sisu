use crate::languages::LanguageAdapter;
use crate::models::Diagnostic;
use crate::sandbox::runner::SandboxRunner;
use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub struct TypeScriptAdapter;

const DEFAULT_PACKAGE_JSON: &str = r#"{
  "name": "sisu-project",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {
    "test": "vitest run"
  }
}
"#;

const DEFAULT_TSCONFIG_JSON: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "esModuleInterop": true
  },
  "include": ["src/**/*", "tests/**/*"]
}
"#;

#[derive(Debug, Deserialize)]
struct PackageManifest {
    types: Option<String>,
    typings: Option<String>,
}

/// Parses `tsc --noEmit --pretty false` lines:
/// `src/app.ts(12,5): error TS2339: Property 'foo' does not exist on type 'User'.`
pub fn parse_tsc_output(stdout: &str, stderr: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let combined = format!("{stdout}\n{stderr}");

    for line in combined.lines() {
        let trimmed = line.trim();
        let Some((loc, rest)) = trimmed.split_once("): error ") else {
            continue;
        };
        let Some((file, coords)) = loc.rsplit_once('(') else {
            continue;
        };
        let Some((line_str, col_str)) = coords.split_once(',') else {
            continue;
        };
        let (Ok(line_num), Ok(col_num)) = (
            line_str.trim().parse::<usize>(),
            col_str.trim().parse::<usize>(),
        ) else {
            continue;
        };

        let (code, message) = match rest.split_once(": ") {
            Some((c, m)) => (Some(c.trim().to_string()), m.trim().to_string()),
            None => (None, rest.trim().to_string()),
        };

        diagnostics.push(Diagnostic {
            file: file.trim().trim_start_matches("./").to_string(),
            line: line_num,
            column: col_num,
            code,
            message,
            related_spans: vec![],
            local_hint: None,
        });
    }

    diagnostics
}

/// Parses `vitest run` stack frames (e.g. `❯ tests/math.test.ts:8:17`)
/// and attaches the preceding assertion/error line as the diagnostic message.
pub fn parse_vitest_output(stdout: &str, stderr: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let combined = format!("{stdout}\n{stderr}");
    let mut last_error_msg: Option<String> = None;

    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Error:") || trimmed.starts_with("AssertionError") {
            last_error_msg = Some(trimmed.to_string());
            continue;
        }

        // Skip dependency stack frames so we only point repairs at project code
        if trimmed.contains("node_modules/") {
            continue;
        }

        for token in trimmed.split_whitespace() {
            let clean = token.trim_matches(|c| c == '(' || c == ')' || c == ',');
            let parts: Vec<&str> = clean.split(':').collect();
            if parts.len() >= 2 && (parts[0].ends_with(".ts") || parts[0].ends_with(".tsx")) {
                if let Ok(line_num) = parts[1].parse::<usize>() {
                    let col_num = parts.get(2).and_then(|c| c.parse().ok()).unwrap_or(1);
                    let file = parts[0].trim_start_matches("./").to_string();

                    let already_seen = diagnostics
                        .iter()
                        .any(|d: &Diagnostic| d.file == file && d.line == line_num);
                    if !already_seen {
                        diagnostics.push(Diagnostic {
                            file,
                            line: line_num,
                            column: col_num,
                            code: Some("vitest_failure".into()),
                            message: last_error_msg
                                .clone()
                                .unwrap_or_else(|| trimmed.to_string()),
                            related_spans: vec![],
                            local_hint: None,
                        });
                    }
                }
            }
        }
    }

    if diagnostics.is_empty() {
        diagnostics.push(Diagnostic {
            file: String::new(),
            line: 0,
            column: 0,
            code: Some("vitest_error".into()),
            message: combined.trim().to_string(),
            related_spans: vec![],
            local_hint: None,
        });
    }

    diagnostics
}

fn local_bin(project_dir: &Path, name: &str) -> String {
    let local = project_dir.join("node_modules/.bin").join(name);
    if local.exists() {
        local.to_string_lossy().into_owned()
    } else {
        name.to_string()
    }
}

#[async_trait]
impl LanguageAdapter for TypeScriptAdapter {
    fn name(&self) -> &'static str {
        "typescript"
    }

    async fn scaffold(&self, project_dir: &Path, runner: &SandboxRunner) -> Result<(), String> {
        let pkg_json = project_dir.join("package.json");
        if !pkg_json.exists() {
            fs::write(&pkg_json, DEFAULT_PACKAGE_JSON)
                .map_err(|e| format!("Failed to write package.json: {e}"))?;
        }

        let tsconfig = project_dir.join("tsconfig.json");
        if !tsconfig.exists() {
            fs::write(&tsconfig, DEFAULT_TSCONFIG_JSON)
                .map_err(|e| format!("Failed to write tsconfig.json: {e}"))?;
        }

        let has_tsc = project_dir.join("node_modules/.bin/tsc").exists();
        let has_vitest = project_dir.join("node_modules/.bin/vitest").exists();
        if !has_tsc || !has_vitest {
            let out = runner
                .run_host_tool(
                    "npm",
                    &[
                        "install",
                        "--save-dev",
                        "typescript",
                        "vitest",
                        "@types/node",
                    ],
                    project_dir,
                )
                .await?;
            if out.timed_out {
                return Err("npm install (dev toolchain) timed out".into());
            }
            if out.exit_code != 0 {
                return Err(format!(
                    "npm install dev deps failed: {}",
                    out.stderr.trim()
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

        let mut args = vec!["install"];
        args.extend(deps.iter().map(|s| s.as_str()));

        let out = runner.run_host_tool("npm", &args, project_dir).await?;
        if out.timed_out {
            return Err("npm install timed out".into());
        }
        if out.exit_code != 0 {
            return Err(format!("npm install failed: {}", out.stderr.trim()));
        }

        Ok(())
    }

    async fn check_static(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        let tsc_bin = local_bin(project_dir, "tsc");
        let out = runner
            .run_isolated(&tsc_bin, &["--noEmit", "--pretty", "false"], project_dir)
            .await?;

        if out.timed_out {
            return Err("tsc timed out".into());
        }

        if out.exit_code == 0 {
            return Ok(vec![]);
        }

        let diags = parse_tsc_output(&out.stdout, &out.stderr);
        if diags.is_empty() {
            return Err(format!(
                "tsc failed without parseable diagnostics: {}{}",
                out.stdout.trim(),
                out.stderr.trim()
            ));
        }

        Ok(diags)
    }

    async fn run_tests(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        let vitest_bin = local_bin(project_dir, "vitest");
        let out = runner
            .run_isolated(
                &vitest_bin,
                &["run", "--passWithNoTests", "--no-color"],
                project_dir,
            )
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

        Ok(parse_vitest_output(&out.stdout, &out.stderr))
    }

    fn resolve_pkg_path(&self, project_dir: &Path, package: &str) -> Option<PathBuf> {
        let trimmed = package.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Normalize subpath imports (`zod/lib/types` -> `zod`; `@scope/pkg/sub` -> `@scope/pkg`)
        let pkg_root_name = if trimmed.starts_with('@') {
            let parts: Vec<&str> = trimmed.split('/').collect();
            if parts.len() >= 2 {
                format!("{}/{}", parts[0], parts[1])
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.split('/').next().unwrap_or(trimmed).to_string()
        };

        let pkg_dir = project_dir.join("node_modules").join(&pkg_root_name);

        // 1. Prefer explicit `types` / `typings` field from the package's `package.json`
        if let Ok(raw) = fs::read_to_string(pkg_dir.join("package.json")) {
            if let Ok(manifest) = serde_json::from_str::<PackageManifest>(&raw) {
                if let Some(types_rel) = manifest.types.or(manifest.typings) {
                    let candidate = pkg_dir.join(types_rel);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }

        // 2. Fallback to standard `.d.ts` entry points inside the package
        let candidates = [
            pkg_dir.join("index.d.ts"),
            pkg_dir.join("dist/index.d.ts"),
            pkg_dir.join("lib/index.d.ts"),
            pkg_dir.join("types/index.d.ts"),
        ];
        if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
            return Some(found);
        }

        // 3. Fallback to DefinitelyTyped (`node_modules/@types/<pkg>/index.d.ts`)
        let types_pkg_name = pkg_root_name.trim_start_matches('@').replace('/', "__");
        let at_types = project_dir
            .join("node_modules/@types")
            .join(types_pkg_name)
            .join("index.d.ts");
        if at_types.is_file() {
            return Some(at_types);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_tsc_errors_accurately() {
        let sample = "\
src/app.ts(12,5): error TS2339: Property 'foo' does not exist on type 'User'.
src/util.ts(3,10): error TS2322: Type 'string' is not assignable to type 'number'.
Found 2 errors in 2 files.";

        let diags = parse_tsc_output(sample, "");
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, "src/app.ts");
        assert_eq!(diags[0].line, 12);
        assert_eq!(diags[0].column, 5);
        assert_eq!(diags[0].code.as_deref(), Some("TS2339"));
        assert!(diags[0].message.contains("Property 'foo'"));
    }

    #[test]
    fn parses_vitest_failure_and_ignores_node_modules() {
        let sample = "\
 FAIL  tests/math.test.ts > adds two numbers
AssertionError: expected -1 to be 5
 ❯ tests/math.test.ts:8:17
 ❯ node_modules/vitest/dist/runner.ts:99:10";

        let diags = parse_vitest_output(sample, "");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "tests/math.test.ts");
        assert_eq!(diags[0].line, 8);
        assert_eq!(diags[0].column, 17);
        assert!(diags[0].message.contains("expected -1 to be 5"));
    }

    #[test]
    fn disk_sniffer_resolves_package_json_types_and_at_types() {
        let dir = tempdir().unwrap();

        // Case 1: Package ships its own `types` entry in `package.json`
        let zod_dir = dir.path().join("node_modules/zod");
        fs::create_dir_all(zod_dir.join("lib")).unwrap();
        fs::write(
            zod_dir.join("package.json"),
            r#"{"types":"lib/index.d.ts"}"#,
        )
        .unwrap();
        fs::write(
            zod_dir.join("lib/index.d.ts"),
            "export function object(): void;\n",
        )
        .unwrap();

        // Case 2: Package relies on `@types/*`
        let express_types = dir.path().join("node_modules/@types/express");
        fs::create_dir_all(&express_types).unwrap();
        fs::write(
            express_types.join("index.d.ts"),
            "export function express(): void;\n",
        )
        .unwrap();

        let adapter = TypeScriptAdapter;
        let zod = adapter
            .resolve_pkg_path(dir.path(), "zod/lib/types")
            .unwrap();
        assert!(zod.ends_with("zod/lib/index.d.ts"));

        let express = adapter.resolve_pkg_path(dir.path(), "express").unwrap();
        assert!(express.ends_with("@types/express/index.d.ts"));
        assert!(
            adapter
                .resolve_pkg_path(dir.path(), "unknown_pkg")
                .is_none()
        );
    }

    #[test]
    fn registry_resolves_all_three_v1_adapters() {
        assert_eq!(
            crate::languages::get_adapter("python").unwrap().name(),
            "python"
        );
        assert_eq!(
            crate::languages::get_adapter("rust").unwrap().name(),
            "rust"
        );
        assert_eq!(
            crate::languages::get_adapter("ts").unwrap().name(),
            "typescript"
        );
    }
}
