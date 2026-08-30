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

/// Resolve timeout from `PORCH_CERTIFY_TIMEOUT_SECS` (default 600s).
///
/// Cold monorepo lint/typecheck (e.g. moon) routinely exceeds 30s; keep the
/// env override for tighter tests.
#[must_use]
pub(crate) fn certify_timeout() -> Duration {
    std::env::var(CERTIFY_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(Duration::from_secs(600), Duration::from_secs)
}

/// Load trusted commands and run one pass of format then lint.
///
/// Caller must assert HEAD continuity first. Empty commands complete without spawn.
/// Trusted executing fields are read from the run-pinned `trusted_config_sha`
/// (default-branch tip observed at rebase), not from a fresh remote-tracking
/// rev-parse and not from the rebase-onto / `base_sha` tip.
///
/// Child PATH is enriched with parent dirs of `$PORCH_HOME/config.yaml` `tools.*`
/// so cold daemons (thin PATH) still see recorded binaries such as `biome`.
///
/// # Errors
///
/// Fails closed on missing `base_sha` or `trusted_config_sha`, unreadable pinned
/// commit, unparseable yaml, non-zero format/lint, timeout, or cancel.
pub(crate) fn run_certify_phase(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    _default_branch: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), CertifyError> {
    let cmds = load_trusted_commands(db, run_id, bare)?;
    let timeout = certify_timeout();
    let path_extra = tools_path_prefix(home);

    if let Some(cmd) = non_empty(&cmds.format) {
        if cancelled(cancel) {
            return Err(CertifyError::Msg("cancelled".into()));
        }
        run_adapter(wt, "format", cmd, timeout, &path_extra)?;
        if maybe_correction_commit(wt, "porch: apply format")? {
            refresh_head_sha(db, run_id, wt)?;
        }
    }

    if let Some(cmd) = non_empty(&cmds.lint) {
        if cancelled(cancel) {
            return Err(CertifyError::Msg("cancelled".into()));
        }
        run_adapter(wt, "lint", cmd, timeout, &path_extra)?;
        if maybe_correction_commit(wt, "porch: apply lint")? {
            refresh_head_sha(db, run_id, wt)?;
        }
    }

    // commands.test is intentionally not run in M5.
    let _ = cmds.test;
    Ok(())
}

/// Parent directories of recorded `tools.*` paths, joined for PATH prepend.
fn tools_path_prefix(home: &Path) -> String {
    let Ok(Some(cfg)) = porch_review::load_home_config(home) else {
        return String::new();
    };
    let mut dirs = Vec::new();
    for path in [
        cfg.tools.biome.as_deref(),
        cfg.tools.bun.as_deref(),
        cfg.tools.cargo.as_deref(),
        cfg.tools.just.as_deref(),
        cfg.tools.moon.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let p = Path::new(path);
        if let Some(parent) = p.parent() {
            let s = parent.to_string_lossy();
            if !s.is_empty() && !dirs.iter().any(|d: &String| d == s.as_ref()) {
                dirs.push(s.into_owned());
            }
        }
    }
    dirs.join(":")
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
    path_extra: &str,
) -> Result<(), CertifyError> {
    let out = run_shell(wt, command, timeout, path_extra)
        .map_err(|e| CertifyError::Msg(format!("{name}: {e}")))?;
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
    // Keep head + tail: moon/cargo noise is front-loaded; the real error is usually last.
    let keep_tail = OUTPUT_TRUNCATE / 2;
    let keep_head = OUTPUT_TRUNCATE.saturating_sub(keep_tail);
    let mut head_end = keep_head.min(s.len());
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len().saturating_sub(keep_tail);
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        return s.to_string();
    }
    format!("{}…\n{}", &s[..head_end], &s[tail_start..])
}

fn run_shell(
    wt: &Path,
    command: &str,
    timeout: Duration,
    path_extra: &str,
) -> Result<ShellOutput, String> {
    if !wt.is_absolute() {
        return Err(format!("worktree must be absolute, got {}", wt.display()));
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd.current_dir(wt);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if !path_extra.is_empty() {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut combined = std::ffi::OsString::from(path_extra);
        if !inherited.is_empty() {
            combined.push(":");
            combined.push(inherited);
        }
        cmd.env("PATH", combined);
    }
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
