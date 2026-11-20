//! Reserved for language adapter contract and registry. No implementation yet.

pub mod python;
pub mod rust;
pub mod typescript;

use crate::models::Diagnostic;
use crate::sandbox::runner::SandboxRunner;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[async_trait]
pub trait LanguageAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// Pre-Flight: create project config & virtual env (runs once before M1).
    async fn scaffold(&self, project_dir: &Path, runner: &SandboxRunner) -> Result<(), String>;

    /// Install Phase (Network ON): add third-party packages via native manager.
    async fn install_deps(
        &self,
        project_dir: &Path,
        deps: &[String],
        runner: &SandboxRunner,
    ) -> Result<(), String>;

    /// Gate 1 (Network OFF): static type/syntax check -> structured Diagnostics.
    async fn check_static(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String>;

    /// Gate 2 (Network OFF): run behavioral tests -> structured Diagnostics.
    async fn run_tests(
        &self,
        project_dir: &Path,
        runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String>;

    /// Slice 4 hook: map an imported package name to its local stub/source file on disk.
    fn resolve_pkg_path(&self, _project_dir: &Path, _package: &str) -> Option<PathBuf> {
        None
    }
}

pub fn get_adapter(language: &str) -> Result<Box<dyn LanguageAdapter>, String> {
    match language.to_lowercase().as_str() {
        "python" | "py" => Ok(Box::new(python::PythonAdapter)),
        "rust" | "rs" => Ok(Box::new(rust::RustAdapter)),
        "typescript" | "ts" => Ok(Box::new(typescript::TypeScriptAdapter)),
        other => Err(format!("Unsupported language adapter: '{other}'")),
    }
}
