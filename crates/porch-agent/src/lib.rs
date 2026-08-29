//! Native fixer CLI adapter: spawn, parse stdout JSON, process-group reap.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Env var naming the fixer CLI binary (required for fix; missing → fail closed).
pub const FIXER_BIN_ENV: &str = "PORCH_FIXER_BIN";

/// Env var for fixer subprocess timeout in seconds (default 600).
pub const FIXER_TIMEOUT_ENV: &str = "PORCH_FIXER_TIMEOUT_SECS";

const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Porch-owned fixer prompt body (written under `$PORCH_HOME`, outside the worktree).
pub const FIXER_PROMPT: &str = "\
Investigate the selected review findings and fix them narrowly in this worktree.
Apply the smallest change that addresses each finding.
Run one focused verification of the touched area only.
Do NOT run the full repository test or lint suite.
Do not add comments that only explain the fix.
Do not auto-apply suggestion patches from the review JSON; treat them as evidence only.
";

/// Successful fixer CLI stdout payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixerOutcome {
    pub summary: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Options for one fixer invocation.
#[derive(Debug, Clone)]
pub struct RunFixerOpts<'a> {
    pub work_tree: &'a Path,
    /// Absolute path to prompt.txt under `$PORCH_HOME`.
    pub prompt_file: &'a Path,
    /// Absolute path to findings.json under `$PORCH_HOME`.
    pub findings_file: &'a Path,
    /// Trusted `$PORCH_HOME` root used to validate `prompt_file`.
    pub porch_home: &'a Path,
    pub bin: &'a str,
    pub timeout: Duration,
    pub session_id: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("PORCH_FIXER_BIN is not set")]
    BinMissing,
    #[error("fixer CLI not found ({bin}): {source}")]
    BinNotFound {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("fixer CLI timed out after {0:?}")]
    Timeout(Duration),
    #[error("fixer CLI exited {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("fixer JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("prompt file missing or not under PORCH_HOME: {0}")]
    PromptRefuse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

/// Resolve the fixer binary from `PORCH_FIXER_BIN` (required).
///
/// # Errors
///
/// Returns [`Error::BinMissing`] when the env var is unset or empty.
pub fn fixer_bin() -> Result<String, Error> {
    fixer_bin_from(std::env::var(FIXER_BIN_ENV).ok())
}

/// Resolve fixer binary from an already-read env value.
///
/// # Errors
///
/// Returns [`Error::BinMissing`] when the value is absent or blank.
pub fn fixer_bin_from(raw: Option<String>) -> Result<String, Error> {
    match raw {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(Error::BinMissing),
    }
}

/// Resolve timeout from `PORCH_FIXER_TIMEOUT_SECS`.
#[must_use]
pub fn fixer_timeout() -> Duration {
    let secs = std::env::var(FIXER_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs.max(1))
}

/// Refuse when the prompt path is missing or not under `$PORCH_HOME`.
///
/// # Errors
///
/// Returns [`Error::PromptRefuse`] when the path is absent or escapes the home.
pub fn assert_prompt_under_home(prompt_file: &Path, porch_home: &Path) -> Result<(), Error> {
    if !prompt_file.is_file() {
        return Err(Error::PromptRefuse(format!(
            "missing {}",
            prompt_file.display()
        )));
    }
    let home = canonicalize_path(porch_home);
    let prompt = canonicalize_path(prompt_file);
    if !prompt.starts_with(&home) {
        return Err(Error::PromptRefuse(format!(
            "{} is not under {}",
            prompt.display(),
            home.display()
        )));
    }
    Ok(())
}

/// Write `prompt.txt` and `findings.json` under `fixer_dir` (caller creates dir).
///
/// # Errors
///
/// Returns I/O or JSON errors.
pub fn write_fixer_inputs(
    fixer_dir: &Path,
    findings_json: &str,
) -> Result<(PathBuf, PathBuf), Error> {
    fs::create_dir_all(fixer_dir)?;
    let prompt_file = fixer_dir.join("prompt.txt");
    let findings_file = fixer_dir.join("findings.json");
    fs::write(&prompt_file, FIXER_PROMPT)?;
    // Validate findings JSON before writing.
    let _: serde_json::Value = serde_json::from_str(findings_json)?;
    fs::write(&findings_file, findings_json)?;
    Ok((prompt_file, findings_file))
}

/// Parse fixer stdout JSON.
///
/// # Errors
///
/// Returns [`Error::Json`] when the payload is not valid fixer JSON.
pub fn parse_fixer_stdout(bytes: &[u8]) -> Result<FixerOutcome, Error> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Spawn the fixer CLI in `work_tree`, parse stdout JSON, reap the process group.
///
/// # Errors
///
/// Returns spawn, timeout, exit, JSON, prompt-refuse, or I/O errors.
pub fn run_fixer(opts: &RunFixerOpts<'_>) -> Result<FixerOutcome, Error> {
    assert_prompt_under_home(opts.prompt_file, opts.porch_home)?;
    if !opts.findings_file.is_file() {
        return Err(Error::Msg(format!(
            "findings file missing: {}",
            opts.findings_file.display()
        )));
    }

    let prompt_s = abs_str(opts.prompt_file)?;
    let findings_s = abs_str(opts.findings_file)?;

    let mut cmd = Command::new(opts.bin);
    cmd.current_dir(opts.work_tree);
    cmd.args(["--prompt-file", &prompt_s, "--findings-file", &findings_s]);
    if let Some(sid) = opts.session_id {
        cmd.args(["--session-id", sid]);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::BinNotFound {
                bin: opts.bin.to_string(),
                source: e,
            }
        } else {
            Error::Io(e)
        }
    })?;

    let pid = child.id();
    let deadline = Instant::now() + opts.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child_group(pid);
                    let _ = child.wait();
                    kill_child_group(pid);
                    return Err(Error::Timeout(opts.timeout));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                kill_child_group(pid);
                return Err(Error::Io(e));
            }
        }
    };

    // Reap grandchildren after wait (E5 / E23).
    kill_child_group(pid);

    let mut stdout = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }

    if !status.success() {
        return Err(Error::Exit {
            status: status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }

    parse_fixer_stdout(stdout.as_bytes())
}

fn abs_str(path: &Path) -> Result<String, Error> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    abs.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Msg(format!("non-utf8 path {}", path.display())))
}

fn canonicalize_path(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn install_fake(bin_dir: &Path, body: &str) -> PathBuf {
        fs::create_dir_all(bin_dir).unwrap();
        let path = bin_dir.join("fake-fixer");
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn parse_stdout_json() {
        let out =
            parse_fixer_stdout(br#"{"summary":"address review findings","session_id":"sess-1"}"#)
                .unwrap();
        assert_eq!(out.summary, "address review findings");
        assert_eq!(out.session_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn missing_bin_env_fails_closed() {
        assert!(matches!(fixer_bin_from(None), Err(Error::BinMissing)));
        assert!(matches!(
            fixer_bin_from(Some(String::new())),
            Err(Error::BinMissing)
        ));
        assert!(matches!(
            fixer_bin_from(Some("   ".into())),
            Err(Error::BinMissing)
        ));
        assert_eq!(fixer_bin_from(Some("fixer".into())).unwrap(), "fixer");
    }

    #[test]
    fn refuse_missing_prompt_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let missing = home.join("runs/r1/fixer/prompt.txt");
        let err = assert_prompt_under_home(&missing, &home).unwrap_err();
        assert!(matches!(err, Error::PromptRefuse(_)));
    }

    #[test]
    fn refuse_prompt_outside_home() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let prompt = outside.join("prompt.txt");
        fs::write(&prompt, "x").unwrap();
        let err = assert_prompt_under_home(&prompt, &home).unwrap_err();
        assert!(matches!(err, Error::PromptRefuse(_)));
    }

    #[test]
    fn timeout_kills_process_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let fixer_dir = home.join("runs/r1/fixer");
        fs::create_dir_all(&fixer_dir).unwrap();
        fs::create_dir_all(&wt).unwrap();
        let (prompt, findings) = write_fixer_inputs(&fixer_dir, "[]").unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "#!/bin/sh\nwhile true; do sleep 60; done\n",
        );
        let err = run_fixer(&RunFixerOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            findings_file: &findings,
            porch_home: &home,
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_millis(200),
            session_id: None,
        })
        .unwrap_err();
        assert!(matches!(err, Error::Timeout(_)));
    }

    #[test]
    fn success_parses_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let fixer_dir = home.join("runs/r1/fixer");
        fs::create_dir_all(&fixer_dir).unwrap();
        fs::create_dir_all(&wt).unwrap();
        let (prompt, findings) = write_fixer_inputs(&fixer_dir, "[]").unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "#!/bin/sh\nprintf '{\"summary\":\"ok\",\"session_id\":\"s1\"}\\n'\n",
        );
        let out = run_fixer(&RunFixerOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            findings_file: &findings,
            porch_home: &home,
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_secs(5),
            session_id: None,
        })
        .unwrap();
        assert_eq!(out.summary, "ok");
        assert_eq!(out.session_id.as_deref(), Some("s1"));
    }
}
