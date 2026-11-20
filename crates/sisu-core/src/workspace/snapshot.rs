use std::fs;
use std::path::Path;
use std::process::Command;

const DEFAULT_GITIGNORE: &str = "\
# Sisu harness state & sandbox temp
milestones.json
.sisu_tmp/
sisu_debug_report.md

# Language dependencies & build outputs
target/
.venv/
__pycache__/
.pytest_cache/
*.pyc
node_modules/
dist/
";

fn run_git(project_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("Failed to spawn git {:?}: {e}", args))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        Err(format!("git {:?} failed: {detail}", args))
    }
}

/// Initializes a local-only Git repo (no remote/internet), writes `.gitignore`,
/// and records an initial baseline commit so `HEAD` always exists before M1.
pub fn init_repo(project_dir: &Path) -> Result<String, String> {
    if !project_dir.join(".git").exists() {
        run_git(project_dir, &["init"])?;
    }

    let gitignore_path = project_dir.join(".gitignore");
    if !gitignore_path.exists() {
        fs::write(&gitignore_path, DEFAULT_GITIGNORE)
            .map_err(|e| format!("Failed to write .gitignore: {e}"))?;
    }

    // Ensure at least one commit exists so `git reset --hard HEAD` always works
    if current_head(project_dir).is_err() {
        run_git(project_dir, &["add", "-A"])?;
        run_git(
            project_dir,
            &[
                "-c",
                "user.name=Sisu",
                "-c",
                "user.email=sisu@local",
                "commit",
                "--allow-empty",
                "-m",
                "sisu: initial checkpoint",
            ],
        )?;
    }

    current_head(project_dir)
}

/// Commits all non-ignored workspace changes as a local checkpoint.
/// Idempotent: if nothing changed, returns the existing HEAD hash without failing.
pub fn commit_checkpoint(project_dir: &Path, message: &str) -> Result<String, String> {
    let status = run_git(project_dir, &["status", "--porcelain"])?;
    if status.is_empty() {
        return current_head(project_dir);
    }

    run_git(project_dir, &["add", "-A"])?;
    run_git(
        project_dir,
        &[
            "-c",
            "user.name=Sisu",
            "-c",
            "user.email=sisu@local",
            "commit",
            "-m",
            message,
        ],
    )?;

    current_head(project_dir)
}

/// Reverts tracked edits and removes untracked LLM files while keeping
/// `.gitignore`d artifacts (`.venv/`, `target/`, `node_modules/`, `milestones.json`).
pub fn rollback_hard(project_dir: &Path) -> Result<(), String> {
    run_git(project_dir, &["reset", "--hard", "HEAD"])?;
    run_git(project_dir, &["clean", "-fd"])?;
    Ok(())
}

/// Returns the short commit hash of `HEAD`.
pub fn current_head(project_dir: &Path) -> Result<String, String> {
    run_git(project_dir, &["rev-parse", "--short", "HEAD"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_repo_creates_gitignore_and_initial_head() {
        let dir = tempdir().unwrap();
        let sha = init_repo(dir.path()).unwrap();

        assert!(!sha.is_empty());
        assert!(dir.path().join(".git").exists());
        assert!(dir.path().join(".gitignore").exists());
    }

    #[test]
    fn commit_checkpoint_is_idempotent_when_clean() {
        let dir = tempdir().unwrap();
        let head1 = init_repo(dir.path()).unwrap();
        let head2 = commit_checkpoint(dir.path(), "noop checkpoint").unwrap();
        assert_eq!(head1, head2);

        fs::write(dir.path().join("app.py"), "x = 1\n").unwrap();
        let head3 = commit_checkpoint(dir.path(), "Milestone M1").unwrap();
        assert_ne!(head1, head3);
    }

    #[test]
    fn rollback_reverts_tracked_removes_untracked_and_keeps_ignored() {
        let dir = tempdir().unwrap();
        init_repo(dir.path()).unwrap();

        // Milestone M1 succeeds and is committed
        let app = dir.path().join("app.py");
        fs::write(&app, "def add(a, b):\n    return a + b\n").unwrap();
        commit_checkpoint(dir.path(), "Milestone M1").unwrap();

        // Ignored files/folders that must survive rollback
        fs::write(dir.path().join("milestones.json"), r#"{"status":"Failed"}"#).unwrap();
        fs::create_dir_all(dir.path().join(".venv/bin")).unwrap();
        fs::write(dir.path().join(".venv/bin/python"), "stub").unwrap();

        // Milestone M2 corrupts app.py and adds a broken untracked file
        fs::write(&app, "BROKEN SYNTAX").unwrap();
        let junk = dir.path().join("broken_extra.py");
        fs::write(&junk, "raise RuntimeError()").unwrap();

        rollback_hard(dir.path()).unwrap();

        // Tracked file restored, untracked LLM file deleted
        assert!(fs::read_to_string(&app).unwrap().contains("return a + b"));
        assert!(!junk.exists());

        // Ignored state and venv survived untouched
        assert!(dir.path().join("milestones.json").exists());
        assert!(dir.path().join(".venv/bin/python").exists());
    }
}
