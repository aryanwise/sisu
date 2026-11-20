use crate::languages::LanguageAdapter;
use crate::models::{Diagnostic, EscalationLevel, Milestone};
use crate::syntax;
use std::fs;
use std::path::Path;

/// ~9,000 chars (~2.2k–2.5k tokens) leaves comfortable headroom for a 7B model's system prompt & output.
const MAX_GENERATION_CONTEXT_CHARS: usize = 9_000;
const MAX_STUB_HINT_CHARS: usize = 1_400;
const MAX_FULL_FILE_CHARS: usize = 6_000;

/// Builds the generation prompt for LLM Job #1 using Tree-sitter AST signatures
/// from `depends_on` artifacts instead of dumping raw file contents.
pub fn build_generation_prompt(
    project_dir: &Path,
    goal: &str,
    language: &str,
    milestone: &Milestone,
    depends_on_artifacts: &[String],
) -> String {
    let mut prompt = format!(
        "Project Goal: {goal}\n\
         Target Language: {language}\n\
         Milestone {id}: {title}\n\
         Description: {desc}\n\
         Done When:\n- {done}\n",
        id = milestone.id,
        title = milestone.title,
        desc = milestone.description,
        done = milestone.done_when.join("\n- ")
    );

    if depends_on_artifacts.is_empty() {
        return prompt;
    }

    prompt.push_str("\nExisting Public Signatures from Previous Milestones:\n");
    let mut remaining = MAX_GENERATION_CONTEXT_CHARS;

    for rel in depends_on_artifacts {
        if remaining == 0 {
            break;
        }

        let Ok(content) = fs::read_to_string(project_dir.join(rel)) else {
            continue;
        };

        let extracted = match syntax::extract_signatures(language, &content) {
            Ok(sigs) if !sigs.trim().is_empty() => sigs,
            _ => content,
        };

        let chunk = truncate_chars(&extracted, remaining);
        remaining = remaining.saturating_sub(chunk.chars().count());
        prompt.push_str(&format!("\n--- {rel} ---\n{chunk}\n"));
    }

    prompt
}

/// Populates `Diagnostic.local_hint` by resolving third-party imports from the failing file
/// directly on disk (`.venv`, `.cargo/registry`, `node_modules`) and slicing their AST signatures.
pub fn enrich_diagnostics_with_stubs(
    project_dir: &Path,
    language: &str,
    adapter: &dyn LanguageAdapter,
    diagnostics: &mut [Diagnostic],
) {
    for diag in diagnostics.iter_mut() {
        if diag.local_hint.is_some() || diag.file.is_empty() {
            continue;
        }

        let Ok(source) = fs::read_to_string(project_dir.join(&diag.file)) else {
            continue;
        };

        let imports = extract_imports(language, &source);
        if imports.is_empty() {
            continue;
        }

        // Prioritize imports whose module or imported symbols appear in the error message
        let mut ranked = imports;
        ranked.sort_by_key(|imp| {
            let matches_msg = diag.message.contains(&imp.module)
                || imp.symbols.iter().any(|s| diag.message.contains(s));
            if matches_msg { 0 } else { 1 }
        });

        let mut hints = Vec::new();
        for imp in ranked {
            if hints.len() >= 2 {
                break;
            }

            // Stdlib modules won't exist inside `.venv`/`node_modules`, so `resolve_pkg_path` filters them out naturally
            let Some(stub_path) = adapter.resolve_pkg_path(project_dir, &imp.module) else {
                continue;
            };
            let Ok(stub_src) = fs::read_to_string(&stub_path) else {
                continue;
            };

            let sigs = match syntax::extract_signatures(language, &stub_src) {
                Ok(s) if !s.trim().is_empty() => s,
                _ => stub_src,
            };

            let focused = focus_stub_on_symbols(&sigs, &imp.symbols, MAX_STUB_HINT_CHARS);
            hints.push(format!("// Local stub for `{}`:\n{}", imp.module, focused));
        }

        if !hints.is_empty() {
            diag.local_hint = Some(hints.join("\n\n"));
        }
    }
}

/// Builds the surgical repair prompt scaled to EscalationLevel:
/// - D: Exact Tree-sitter enclosing function/class node + diagnostic + Disk Sniffer local stubs
/// - C: Point D + module-level signatures of the failing file + AST slices of `related_spans`
/// - B: Full failing file + AST signatures of `depends_on` milestones
pub fn build_repair_prompt(
    project_dir: &Path,
    language: &str,
    milestone: &Milestone,
    level: EscalationLevel,
    diagnostics: &[Diagnostic],
    depends_on_artifacts: &[String],
) -> String {
    let mut prompt = format!(
        "Target Language: {language}\n\
         Milestone: {} ({})\n\
         Escalation Level: {:?}\n\n\
         Diagnostics:\n",
        milestone.id, milestone.title, level
    );

    for d in diagnostics {
        prompt.push_str(&format!(
            "- {}:{} [{}] {}\n",
            d.file,
            d.line,
            d.code.as_deref().unwrap_or("error"),
            d.message
        ));
        if let Some(hint) = &d.local_hint {
            prompt.push_str(&format!("  Local Library Stub:\n{hint}\n"));
        }
    }

    let Some(primary) = diagnostics.iter().find(|d| !d.file.is_empty()) else {
        return prompt;
    };

    let Ok(source) = fs::read_to_string(project_dir.join(&primary.file)) else {
        return prompt;
    };

    match level {
        EscalationLevel::D => {
            let slice = slice_node_or_window(language, &source, primary.line, 8);
            prompt.push_str(&format!(
                "\n--- {} (Enclosing AST Scope around line {}) ---\n{}\n",
                primary.file, primary.line, slice
            ));
        }
        EscalationLevel::C => {
            let slice = slice_node_or_window(language, &source, primary.line, 20);
            prompt.push_str(&format!(
                "\n--- {} (Enclosing AST Scope around line {}) ---\n{}\n",
                primary.file, primary.line, slice
            ));

            if let Ok(file_sigs) = syntax::extract_signatures(language, &source) {
                if !file_sigs.trim().is_empty() {
                    prompt.push_str(&format!(
                        "\n--- {} (File Signatures & Imports) ---\n{}\n",
                        primary.file,
                        truncate_chars(&file_sigs, 2_500)
                    ));
                }
            }

            for span in &primary.related_spans {
                if let Ok(rel_src) = fs::read_to_string(project_dir.join(&span.file)) {
                    let rel_slice = slice_node_or_window(language, &rel_src, span.line, 10);
                    prompt.push_str(&format!(
                        "\n--- Related: {} (Line {}) ---\n{}\n",
                        span.file, span.line, rel_slice
                    ));
                }
            }
        }
        EscalationLevel::B | EscalationLevel::A => {
            let numbered_full = format_numbered_full(&source, MAX_FULL_FILE_CHARS);
            prompt.push_str(&format!(
                "\n--- {} (Full File) ---\n{}\n",
                primary.file, numbered_full
            ));

            for dep_file in depends_on_artifacts {
                if dep_file == &primary.file {
                    continue;
                }
                if let Ok(dep_src) = fs::read_to_string(project_dir.join(dep_file)) {
                    let sigs = match syntax::extract_signatures(language, &dep_src) {
                        Ok(s) if !s.trim().is_empty() => s,
                        _ => dep_src,
                    };
                    prompt.push_str(&format!(
                        "\n--- Dependency Signatures: {dep_file} ---\n{}\n",
                        truncate_chars(&sigs, 1_500)
                    ));
                }
            }
        }
    }

    prompt
}

// --- Internal Helpers ---

#[derive(Debug, Clone)]
struct ImportedModule {
    module: String,
    symbols: Vec<String>,
}

fn slice_node_or_window(
    language: &str,
    source: &str,
    line: usize,
    fallback_radius: usize,
) -> String {
    if let Ok(Some(ast_slice)) = syntax::extract_enclosing_node(language, source, line) {
        return ast_slice.format_numbered();
    }
    read_line_window(source, line, fallback_radius)
}

fn read_line_window(source: &str, center_line: usize, radius: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let zero_idx = center_line.saturating_sub(1).min(lines.len() - 1);
    let start = zero_idx.saturating_sub(radius);
    let end = (zero_idx + radius + 1).min(lines.len());

    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4} | {line}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_numbered_full(source: &str, max_chars: usize) -> String {
    let numbered = source
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{:>4} | {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&numbered, max_chars)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}\n... [truncated]")
}

/// If the file imported specific symbols (e.g. `from pydantic import BaseModel`),
/// prioritize blocks mentioning those symbols so the 7B model sees the right class first.
fn focus_stub_on_symbols(sigs: &str, symbols: &[String], max_chars: usize) -> String {
    if !symbols.is_empty() {
        let matched: Vec<&str> = sigs
            .split("\n\n")
            .filter(|block| symbols.iter().any(|sym| block.contains(sym.as_str())))
            .collect();
        if !matched.is_empty() {
            return truncate_chars(&matched.join("\n\n"), max_chars);
        }
    }
    truncate_chars(sigs, max_chars)
}

fn extract_imports(language: &str, source: &str) -> Vec<ImportedModule> {
    let mut results = Vec::new();
    match language.to_lowercase().as_str() {
        "python" | "py" => {
            for line in source.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("from ") {
                    if let Some((mod_part, sym_part)) = rest.split_once(" import ") {
                        let module = mod_part.trim();
                        if module.is_empty() || module.starts_with('.') {
                            continue;
                        }
                        let symbols = sym_part
                            .trim_matches(|c| c == '(' || c == ')')
                            .split(',')
                            .filter_map(|s| s.split_whitespace().next())
                            .filter(|s| !s.is_empty() && *s != "*")
                            .map(String::from)
                            .collect();
                        results.push(ImportedModule {
                            module: module.to_string(),
                            symbols,
                        });
                    }
                } else if let Some(rest) = t.strip_prefix("import ") {
                    for item in rest.split(',') {
                        if let Some(module) = item.split_whitespace().next() {
                            if !module.is_empty() {
                                results.push(ImportedModule {
                                    module: module.to_string(),
                                    symbols: vec![],
                                });
                            }
                        }
                    }
                }
            }
        }
        "rust" | "rs" => {
            for line in source.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("use ") {
                    if let Some(crate_name) = rest.split("::").next() {
                        let name = crate_name.trim_matches(|c| c == ';' || c == ' ');
                        if !matches!(name, "" | "crate" | "self" | "super" | "std" | "core") {
                            results.push(ImportedModule {
                                module: name.to_string(),
                                symbols: vec![],
                            });
                        }
                    }
                }
            }
        }
        "typescript" | "ts" => {
            for line in source.lines() {
                let t = line.trim();
                if t.starts_with("import ") {
                    if let Some((_, after_from)) = t.split_once(" from ") {
                        let pkg = after_from
                            .trim()
                            .trim_end_matches(';')
                            .trim_matches(|c| c == '\'' || c == '"');
                        if !pkg.is_empty() && !pkg.starts_with('.') {
                            results.push(ImportedModule {
                                module: pkg.to_string(),
                                symbols: vec![],
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::python::PythonAdapter;
    use crate::models::TaskStatus;
    use tempfile::tempdir;

    fn dummy_milestone() -> Milestone {
        Milestone {
            id: "M2".into(),
            title: "Serialize user".into(),
            description: "Convert User model to JSON".into(),
            depends_on: vec!["M1".into()],
            done_when: vec!["Returns dict".into()],
            status: TaskStatus::InProgress,
            attempts: 0,
            artifacts: vec![],
            last_error: None,
        }
    }

    #[test]
    fn generation_prompt_uses_ast_signatures_not_function_bodies() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/models.py"),
            "class User:\n    id: int\n    def helper(self) -> str:\n        secret = 'HIDDEN_BODY'\n        return secret\n",
        )
        .unwrap();

        let prompt = build_generation_prompt(
            dir.path(),
            "Demo API",
            "python",
            &dummy_milestone(),
            &["src/models.py".into()],
        );

        assert!(prompt.contains("class User:"));
        assert!(prompt.contains("def helper(self) -> str: ..."));
        assert!(!prompt.contains("HIDDEN_BODY"));
    }

    #[test]
    fn enrich_diagnostics_attaches_venv_stub_hint() {
        let dir = tempdir().unwrap();
        // Fake installed library inside `.venv`
        let pkg_dir = dir
            .path()
            .join(".venv/lib/python3.12/site-packages/pydantic");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("__init__.pyi"),
            "class BaseModel:\n    def model_dump(self) -> dict: ...\n\nclass OtherThing:\n    def unused(self) -> None: ...\n",
        )
        .unwrap();

        // Failing user file calling deprecated `.dict()`
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.py"),
            "from pydantic import BaseModel\n\nclass User(BaseModel):\n    id: int\n\ndef dump_user(u: User):\n    return u.dict()\n",
        )
        .unwrap();

        let mut diags = vec![Diagnostic {
            file: "src/app.py".into(),
            line: 7,
            column: 14,
            code: Some("reportAttributeAccessIssue".into()),
            message: r#"Cannot access attribute "dict" for class "User""#.into(),
            related_spans: vec![],
            local_hint: None,
        }];

        let adapter = PythonAdapter;
        enrich_diagnostics_with_stubs(dir.path(), "python", &adapter, &mut diags);

        let hint = diags[0].local_hint.as_deref().expect("Expected local_hint");
        assert!(hint.contains("def model_dump(self) -> dict: ..."));
        assert!(!hint.contains("OtherThing"));
    }

    #[test]
    fn point_d_repair_prompt_slices_only_failing_function_and_includes_stub() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.py"),
            "def untouched():\n    return 1\n\ndef buggy(x: int) -> int:\n    return x - 1\n\ndef also_untouched():\n    return 2\n",
        )
        .unwrap();

        let diags = vec![Diagnostic {
            file: "src/app.py".into(),
            line: 5,
            column: 5,
            code: Some("pytest_failure".into()),
            message: "expected x + 1".into(),
            related_spans: vec![],
            local_hint: Some("// Local stub hint".into()),
        }];

        let prompt = build_repair_prompt(
            dir.path(),
            "python",
            &dummy_milestone(),
            EscalationLevel::D,
            &diags,
            &[],
        );

        assert!(prompt.contains("def buggy(x: int) -> int:"));
        assert!(prompt.contains("// Local stub hint"));
        assert!(!prompt.contains("untouched"));
        assert!(!prompt.contains("also_untouched"));
    }
}
