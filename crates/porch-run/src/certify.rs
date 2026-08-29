//! Cheap certify adapters: trusted `commands.format` / `commands.lint`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use porch_gate::Db;
use porch_git::GitDir;

use crate::config::{Commands, load_trusted_at_sha};

pub(crate) const CERTIFY_TIMEOUT_ENV: &str = "PORCH_CERTIFY_TIMEOUT_SECS";

#[derive(Debug, thiserror::Error)]
pub(crate) enum CertifyError {
    #[error(transparent)]
    Gate(#[from] porch_gate::Error),
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

/// Resolve timeout from `PORCH_CERTIFY_TIMEOUT_SECS` (default 30s).
#[must_use]
pub(crate) fn certify_timeout() -> Duration {
    std::env::var(CERTIFY_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(Duration::from_secs(30), Duration::from_secs)
}

/// Load trusted commands and run one pass of format then lint.
///
/// Caller must assert HEAD continuity first. Empty commands complete without spawn.
/// Trusted executing fields are read from the run-pinned `trusted_config_sha`
/// (default-branch tip observed at rebase), not from a fresh remote-tracking
/// rev-parse and not from the rebase-onto / `base_sha` tip.
///
/// # Errors
///
/// Fails closed on missing `base_sha` or `trusted_config_sha`, unreadable pinned
/// commit, unparseable yaml, non-zero format/lint, timeout, or cancel.
pub(crate) fn run_certify_phase(
    db: &Db,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    _default_branch: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), CertifyError> {
    let cmds = load_trusted_commands(db, run_id, bare)?;
    let timeout = certify_timeout();

    if let Some(cmd) = non_empty(&cmds.format) {
        if cancelled(cancel) {
            return Err(CertifyError::Msg("cancelled".into()));
        }
        run_adapter(wt, "format", cmd, timeout)?;
        if maybe_correction_commit(wt, "porch: apply format")? {
            refresh_head_sha(db, run_id, wt)?;
        }
    }

    if let Some(cmd) = non_empty(&cmds.lint) {
        if cancelled(cancel) {
            return Err(CertifyError::Msg("cancelled".into()));
        }
        run_adapter(wt, "lint", cmd, timeout)?;
        if maybe_correction_commit(wt, "porch: apply lint")? {
            refresh_head_sha(db, run_id, wt)?;
        }
    }

    // commands.test is intentionally not run in M5.
    let _ = cmds.test;
    Ok(())
}

fn load_trusted_commands(db: &Db, run_id: &str, bare: &GitDir) -> Result<Commands, CertifyError> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| CertifyError::Msg(format!("unknown run {run_id}")))?;
    // Rebase must have completed (base_sha set) before certify.
    if run.base_sha.is_none() {
        return Err(CertifyError::Msg("certify requires base_sha".into()));
    }
    let trusted_sha = run.trusted_config_sha.as_deref().ok_or_else(|| {
        CertifyError::Msg("certify requires trusted_config_sha (pin at rebase)".into())
    })?;
    Ok(load_trusted_at_sha(bare, trusted_sha)
        .map_err(CertifyError::Msg)?
        .commands)
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t) }
}

fn cancelled(cancel: Option<&std::sync::atomic::AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
}

fn refresh_head_sha(db: &Db, run_id: &str, wt: &Path) -> Result<(), CertifyError> {
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;
    Ok(())
}

const OUTPUT_TRUNCATE: usize = 2_048;

struct ShellOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_adapter(
    wt: &Path,
    name: &str,
    command: &str,
    timeout: Duration,
) -> Result<(), CertifyError> {
    let out =
        run_shell(wt, command, timeout).map_err(|e| CertifyError::Msg(format!("{name}: {e}")))?;
    if out.code != 0 {
        return Err(CertifyError::Msg(format!(
            "{name} exited {}: {}: {}",
            out.code,
            command,
            format_cmd_output(&out.stderr, &out.stdout)
        )));
    }
    Ok(())
}

fn format_cmd_output(stderr: &str, stdout: &str) -> String {
    let err = truncate_output(stderr.trim());
    if !err.is_empty() {
        return format!("stderr:\n{err}");
    }
    let out = truncate_output(stdout.trim());
    if !out.is_empty() {
        return format!("stdout:\n{out}");
    }
    "(no output)".into()
}

fn truncate_output(s: &str) -> String {
    if s.len() <= OUTPUT_TRUNCATE {
        return s.to_string();
    }
    let mut end = OUTPUT_TRUNCATE;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn run_shell(wt: &Path, command: &str, timeout: Duration) -> Result<ShellOutput, String> {
    if !wt.is_absolute() {
        return Err(format!("worktree must be absolute, got {}", wt.display()));
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd.current_dir(wt);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn sh -c: {e}"))?;
    let pid = child.id();
    let deadline = Instant::now() + timeout;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child_group(pid);
                    let _ = child.wait();
                    kill_child_group(pid);
                    return Err(format!("timed out after {timeout:?}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                kill_child_group(pid);
                return Err(format!("wait: {e}"));
            }
        }
    };

    // Reap grandchildren on every end path (E5).
    kill_child_group(pid);

    let mut stdout = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }

    Ok(ShellOutput {
        code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

fn maybe_correction_commit(wt: &Path, subject: &str) -> Result<bool, CertifyError> {
    if !worktree_dirty(wt)? {
        return Ok(false);
    }
    // Porch-managed identity + hook isolation (disposable worktree has neither).
    porch_git::run_c(
        wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.email=porch@example.com",
            "-c",
            "user.name=Porch",
            "add",
            "-A",
        ],
    )?;
    porch_git::run_c(
        wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.email=porch@example.com",
            "-c",
            "user.name=Porch",
            "commit",
            "--no-verify",
            "-m",
            subject,
        ],
    )?;
    Ok(true)
}

fn worktree_dirty(wt: &Path) -> Result<bool, CertifyError> {
    let out = porch_git::run_c(wt, &["status", "--porcelain"])?;
    Ok(!porch_git::stdout_trim(&out).is_empty())
}

fn kill_child_group(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        if let Ok(raw) = i32::try_from(pid) {
            let _ = killpg(Pid::from_raw(raw), Signal::SIGTERM);
            std::thread::sleep(Duration::from_millis(100));
            let _ = killpg(Pid::from_raw(raw), Signal::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}
