use crate::sandbox::backend::{
    ExecutionMode, SandboxBackend, detect_sandbox_backend, wrap_command,
};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct SandboxRunner {
    pub timeout: Duration,
    pub backend: SandboxBackend,
}

/// Only forward host variables required for toolchains—never API keys or secrets.
const ALLOWED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "VIRTUAL_ENV",
    "USER",
];

impl SandboxRunner {
    pub fn new(timeout: Duration, backend: SandboxBackend) -> Self {
        Self { timeout, backend }
    }

    /// Probes the host OS once and constructs a runner with the strongest working jail.
    pub fn detect(probe_dir: &Path, timeout: Duration) -> Result<Self, String> {
        let backend = detect_sandbox_backend(probe_dir)?;
        Ok(Self { timeout, backend })
    }

    /// Explicit `--no-sandbox` runner (still enforces `.env_clear()` and timeouts).
    pub fn unsandboxed(timeout: Duration) -> Self {
        Self {
            timeout,
            backend: SandboxBackend::Unsandboxed,
        }
    }

    pub fn backend(&self) -> SandboxBackend {
        self.backend
    }

    /// Install Phase: Network ON, host toolchain caches writable.
    pub async fn run_host_tool(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<CommandOutput, String> {
        self.run_with_mode(program, args, cwd, ExecutionMode::HostToolchain)
            .await
    }

    /// Verify/Test Phase: Runs inside the configured `SandboxBackend`.
    pub async fn run_isolated(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<CommandOutput, String> {
        self.run_with_mode(program, args, cwd, ExecutionMode::Isolated)
            .await
    }

    pub async fn run_with_mode(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        mode: ExecutionMode,
    ) -> Result<CommandOutput, String> {
        let canonical_cwd = cwd
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize cwd: {e}"))?;

        let wrapped = wrap_command(program, args, &canonical_cwd, mode, self.backend)?;

        let mut cmd = Command::new(&wrapped.program);
        cmd.args(&wrapped.args)
            .current_dir(&canonical_cwd)
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for key in ALLOWED_ENV_VARS {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }

        // Keep scratch/temp files strictly inside the jailed workspace
        if mode == ExecutionMode::Isolated {
            let local_tmp = canonical_cwd.join(".sisu_tmp");
            fs::create_dir_all(&local_tmp)
                .map_err(|e| format!("Failed to create sandbox tmp dir: {e}"))?;
            cmd.env("TMPDIR", &local_tmp);
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn '{}': {e}", wrapped.program))?;

        match timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(format!("Process I/O error for '{program}': {e}")),
            Err(_) => Ok(CommandOutput {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Command '{program}' timed out after {:?}", self.timeout),
                timed_out: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use tempfile::tempdir;

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let dir = tempdir().unwrap();
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(10));

        let out = runner
            .run_host_tool("echo", &["hello sisu"], dir.path())
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(!out.timed_out);
        assert_eq!(out.stdout.trim(), "hello sisu");
    }

    #[tokio::test]
    async fn strips_unlisted_env_vars() {
        let dir = tempdir().unwrap();
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(10));

        let out = runner.run_host_tool("env", &[], dir.path()).await.unwrap();
        assert_eq!(out.exit_code, 0);

        for line in out.stdout.lines() {
            if let Some((key, _)) = line.split_once('=') {
                assert!(
                    ALLOWED_ENV_VARS.contains(&key),
                    "Unexpected env var leaked into sandbox: {key}"
                );
            }
        }
    }

    #[tokio::test]
    async fn kills_hung_process_on_timeout() {
        let dir = tempdir().unwrap();
        let runner = SandboxRunner::unsandboxed(Duration::from_millis(200));

        let out = runner
            .run_host_tool("sleep", &["5"], dir.path())
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, -1);
    }

    #[tokio::test]
    async fn os_sandbox_enforces_filesystem_jail() {
        let project_dir = tempdir().unwrap();
        let outside_dir = tempdir().unwrap();
        let outside_file = outside_dir.path().join("escaped.txt");

        let runner = match SandboxRunner::detect(project_dir.path(), Duration::from_secs(10)) {
            Ok(r) => {
                eprintln!("[sisu test] detected backend: {}", r.backend().label());
                r
            }
            Err(e) => {
                eprintln!("[sisu test] skipping OS fs-jail test:\n{e}");
                return;
            }
        };

        // 1. Write inside project_dir must succeed
        let inside = runner
            .run_isolated("sh", &["-c", "echo ok > inside.txt"], project_dir.path())
            .await
            .unwrap();
        assert_eq!(inside.exit_code, 0, "stderr: {}", inside.stderr);
        assert!(project_dir.path().join("inside.txt").exists());

        // 2. Write outside project_dir must be blocked by the kernel (not a bwrap crash)
        let cmd = format!("touch '{}'", outside_file.display());
        let outside = runner
            .run_isolated("sh", &["-c", &cmd], project_dir.path())
            .await
            .unwrap();
        assert!(
            !outside.stderr.contains("bwrap:"),
            "Sandbox failed to start: {}",
            outside.stderr
        );
        assert_ne!(
            outside.exit_code, 0,
            "Write outside project_dir should fail"
        );
        assert!(!outside_file.exists());
    }

    #[tokio::test]
    async fn os_sandbox_blocks_outbound_network_when_supported() {
        let dir = tempdir().unwrap();
        let runner = match SandboxRunner::detect(dir.path(), Duration::from_secs(10)) {
            Ok(r) if r.backend().is_network_isolated() => r,
            Ok(r) => {
                eprintln!(
                    "[sisu test] skipping network test (backend '{}' is fs-only)",
                    r.backend().label()
                );
                return;
            }
            Err(e) => {
                eprintln!("[sisu test] skipping network test:\n{e}");
                return;
            }
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let script =
            format!("import socket; socket.create_connection(('127.0.0.1', {port}), timeout=2)");
        let out = runner
            .run_isolated("python3", &["-c", &script], dir.path())
            .await
            .unwrap();

        assert!(
            !out.stderr.contains("bwrap:"),
            "Sandbox failed to start: {}",
            out.stderr
        );
        assert_ne!(
            out.exit_code, 0,
            "OS sandbox should have blocked outbound TCP connection"
        );
    }
}
