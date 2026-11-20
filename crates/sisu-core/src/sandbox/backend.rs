use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Network ON, host toolchain caches writable (scaffold & install_deps only).
    HostToolchain,
    /// Uses the active `SandboxBackend` (static checks & tests).
    Isolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    /// macOS Seatbelt: network blocked + writes jailed to project_dir.
    MacOsSeatbelt,
    /// Linux bwrap: network unshared + writes jailed to project_dir.
    LinuxBwrapFull,
    /// Linux bwrap fallback: writes jailed to project_dir, network unrestricted.
    LinuxBwrapFsOnly,
    /// Explicit `--no-sandbox` opt-in: relies only on `.env_clear()`, timeout, and path jail.
    Unsandboxed,
}

impl SandboxBackend {
    pub fn is_fs_isolated(&self) -> bool {
        !matches!(self, Self::Unsandboxed)
    }

    pub fn is_network_isolated(&self) -> bool {
        matches!(self, Self::MacOsSeatbelt | Self::LinuxBwrapFull)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::MacOsSeatbelt => "macos-seatbelt (fs + net)",
            Self::LinuxBwrapFull => "linux-bwrap (fs + net)",
            Self::LinuxBwrapFsOnly => "linux-bwrap (fs-only; net unrestricted)",
            Self::Unsandboxed => "unsandboxed (--no-sandbox)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WrappedCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Prefer `/usr/bin/bwrap` so Ubuntu AppArmor path-based profiles match reliably.
fn bwrap_bin() -> String {
    if Path::new("/usr/bin/bwrap").exists() {
        "/usr/bin/bwrap".to_string()
    } else {
        "bwrap".to_string()
    }
}

pub fn wrap_command(
    program: &str,
    args: &[&str],
    project_dir: &Path,
    mode: ExecutionMode,
    backend: SandboxBackend,
) -> Result<WrappedCommand, String> {
    if mode == ExecutionMode::HostToolchain || backend == SandboxBackend::Unsandboxed {
        return Ok(WrappedCommand {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
    }

    let canonical_dir = project_dir
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize project_dir for sandbox: {e}"))?;
    let dir_str = canonical_dir.to_string_lossy().into_owned();

    match backend {
        SandboxBackend::MacOsSeatbelt => Ok(wrap_seatbelt(program, args, &dir_str)),
        SandboxBackend::LinuxBwrapFull => Ok(wrap_bwrap(program, args, &dir_str, true)),
        SandboxBackend::LinuxBwrapFsOnly => Ok(wrap_bwrap(program, args, &dir_str, false)),
        SandboxBackend::Unsandboxed => unreachable!(),
    }
}

fn wrap_seatbelt(program: &str, args: &[&str], canonical_project_dir: &str) -> WrappedCommand {
    let escaped = canonical_project_dir
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    let profile = format!(
        r#"(version 1)
(allow default)
(deny network-outbound (remote ip))
(deny network-inbound)
(deny file-write*)
(allow file-write*
    (subpath "{escaped}")
    (literal "/dev/null"))"#
    );

    let mut wrapped_args = vec!["-p".to_string(), profile, program.to_string()];
    wrapped_args.extend(args.iter().map(|s| s.to_string()));

    WrappedCommand {
        program: "/usr/bin/sandbox-exec".to_string(),
        args: wrapped_args,
    }
}

fn wrap_bwrap(
    program: &str,
    args: &[&str],
    canonical_project_dir: &str,
    unshare_net: bool,
) -> WrappedCommand {
    // Order matters: mount ephemeral `/tmp` BEFORE `--bind <project_dir>` so
    // projects located under `/tmp` (such as unit tests) are not hidden.
    let mut wrapped_args = vec![
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--bind".into(),
        canonical_project_dir.into(),
        canonical_project_dir.into(),
        "--chdir".into(),
        canonical_project_dir.into(),
        "--die-with-parent".into(),
    ];

    if unshare_net {
        wrapped_args.push("--unshare-net".into());
    }

    wrapped_args.push("--".into());
    wrapped_args.push(program.into());
    wrapped_args.extend(args.iter().map(|s| s.to_string()));

    WrappedCommand {
        program: bwrap_bin(),
        args: wrapped_args,
    }
}

/// Runs a 5ms no-op command (`sh -c true`) to verify a backend works on this host.
fn probe_backend(backend: SandboxBackend, probe_dir: &Path) -> Result<(), String> {
    let wrapped = wrap_command(
        "sh",
        &["-c", "true"],
        probe_dir,
        ExecutionMode::Isolated,
        backend,
    )?;
    let output = Command::new(&wrapped.program)
        .args(&wrapped.args)
        .current_dir(probe_dir)
        .output()
        .map_err(|e| format!("Failed to spawn '{}': {e}", wrapped.program))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("Exited with status {}", output.status)
        } else {
            stderr
        })
    }
}

/// Detects the strongest working OS sandbox on the current machine.
/// Never silently falls back to `Unsandboxed`; returns actionable remediation instead.
pub fn detect_sandbox_backend(probe_dir: &Path) -> Result<SandboxBackend, String> {
    #[cfg(target_os = "macos")]
    {
        match probe_backend(SandboxBackend::MacOsSeatbelt, probe_dir) {
            Ok(()) => Ok(SandboxBackend::MacOsSeatbelt),
            Err(e) => Err(format!(
                "macOS sandbox-exec failed: {e}\nPass --no-sandbox to run without OS isolation."
            )),
        }
    }

    #[cfg(target_os = "linux")]
    {
        let full_err = match probe_backend(SandboxBackend::LinuxBwrapFull, probe_dir) {
            Ok(()) => return Ok(SandboxBackend::LinuxBwrapFull),
            Err(e) => e,
        };

        let fs_err = match probe_backend(SandboxBackend::LinuxBwrapFsOnly, probe_dir) {
            Ok(()) => return Ok(SandboxBackend::LinuxBwrapFsOnly),
            Err(e) => e,
        };

        let hint = if full_err.contains("No such file") || full_err.contains("Failed to spawn") {
            "Install bubblewrap (e.g., `sudo apt install bubblewrap`), or pass `--no-sandbox`."
        } else {
            "On Ubuntu 24.04+, AppArmor blocks unprivileged user namespaces for bwrap.\n\
             Whitelist `/usr/bin/bwrap` in `/etc/apparmor.d/bwrap`, or pass `--no-sandbox`."
        };

        Err(format!(
            "No working Linux OS sandbox detected.\n\
             - bwrap (full):    {full_err}\n\
             - bwrap (fs-only): {fs_err}\n\
             Fix: {hint}"
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = probe_dir;
        Err(
            "OS sandboxing is only supported on macOS and Linux. Pass `--no-sandbox` to opt out."
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bwrap_mounts_tmpfs_before_project_bind() {
        let dir = tempdir().unwrap();
        let wrapped = wrap_command(
            "pytest",
            &["-q"],
            dir.path(),
            ExecutionMode::Isolated,
            SandboxBackend::LinuxBwrapFull,
        )
        .unwrap();

        let tmpfs_pos = wrapped.args.iter().position(|a| a == "--tmpfs").unwrap();
        let bind_pos = wrapped.args.iter().position(|a| a == "--bind").unwrap();
        assert!(
            tmpfs_pos < bind_pos,
            "--tmpfs must come before --bind so /tmp projects stay visible"
        );
        assert!(wrapped.args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn bwrap_fs_only_omits_unshare_net() {
        let dir = tempdir().unwrap();
        let wrapped = wrap_command(
            "pytest",
            &["-q"],
            dir.path(),
            ExecutionMode::Isolated,
            SandboxBackend::LinuxBwrapFsOnly,
        )
        .unwrap();

        assert!(!wrapped.args.contains(&"--unshare-net".to_string()));
        assert!(wrapped.args.contains(&"--bind".to_string()));
    }

    #[test]
    fn host_toolchain_mode_never_wraps() {
        let dir = tempdir().unwrap();
        let wrapped = wrap_command(
            "uv",
            &["add", "pydantic"],
            dir.path(),
            ExecutionMode::HostToolchain,
            SandboxBackend::LinuxBwrapFull,
        )
        .unwrap();

        assert_eq!(wrapped.program, "uv");
        assert_eq!(wrapped.args, vec!["add", "pydantic"]);
    }
}
