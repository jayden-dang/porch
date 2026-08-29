//! GitHub deliver adapter: spawn `gh`, PR create/edit, check watch, redaction.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Env var naming the `gh` binary (PATH entry or absolute path).
pub const GH_BIN_ENV: &str = "PORCH_GH_BIN";

/// Env var for `gh` subprocess timeout in seconds (default 30).
pub const GH_TIMEOUT_ENV: &str = "PORCH_GH_TIMEOUT_SECS";

/// Env var for allowlisted-check poll timeout in seconds (default 120).
pub const CHECK_TIMEOUT_ENV: &str = "PORCH_DELIVER_CHECK_TIMEOUT_SECS";

/// Env var for check poll interval in seconds (default 2).
pub const CHECK_POLL_ENV: &str = "PORCH_DELIVER_CHECK_POLL_SECS";

const DEFAULT_BIN: &str = "gh";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CHECK_POLL_SECS: u64 = 2;

const ATTESTATION_MARKER: &str = "porch-attestation";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "gh CLI not found ({bin}): {source}\nset PORCH_GH_BIN or install `gh` on PATH; see `porch doctor`"
    )]
    BinNotFound {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("gh CLI timed out after {0:?}")]
    Timeout(Duration),
    #[error("gh exited {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("gh JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

/// Resolve the `gh` binary from `PORCH_GH_BIN` (default `gh`).
#[must_use]
pub fn gh_bin() -> String {
    std::env::var(GH_BIN_ENV).unwrap_or_else(|_| DEFAULT_BIN.to_string())
}

/// Resolve `gh` spawn timeout from `PORCH_GH_TIMEOUT_SECS`.
#[must_use]
pub fn gh_timeout() -> Duration {
    let secs = std::env::var(GH_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs.max(1))
}

/// Resolve allowlisted check poll deadline.
#[must_use]
pub fn check_timeout() -> Duration {
    let secs = std::env::var(CHECK_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CHECK_TIMEOUT_SECS);
    Duration::from_secs(secs.max(1))
}

/// Resolve check poll interval.
#[must_use]
pub fn check_poll_interval() -> Duration {
    let secs = std::env::var(CHECK_POLL_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CHECK_POLL_SECS);
    Duration::from_secs(secs.max(1))
}

/// Fail closed before remote mutation when `gh` cannot be executed.
///
/// # Errors
///
/// Returns [`Error::BinNotFound`] when the binary is missing.
pub fn ensure_gh_runnable(bin: &str) -> Result<(), Error> {
    match Command::new(bin).arg("--version").output() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::BinNotFound {
            bin: bin.to_string(),
            source: e,
        }),
        Err(e) => Err(Error::Io(e)),
    }
}

/// One open PR from `gh pr list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    #[serde(default)]
    pub title: String,
}

/// One check row from `gh pr checks --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRow {
    pub name: String,
    /// GitHub check state: success / failure / pending / …
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub bucket: String,
    /// Optional details URL from `gh pr checks` (`link` field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// Filter forge checks to allowlisted names only.
#[must_use]
pub fn filter_allowlisted<'a>(checks: &'a [CheckRow], allowlist: &[String]) -> Vec<&'a CheckRow> {
    checks
        .iter()
        .filter(|c| allowlist.iter().any(|a| a == &c.name))
        .collect()
}

/// Outcome of evaluating allowlisted checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistReady {
    /// Every allowlisted name is present and successful.
    Ready,
    /// Genuine red on allowlisted names (`failure` / `failed` / `error`) — mechanical-repair eligible.
    Failed {
        /// Allowlisted rows that are genuinely red (name / state / optional link).
        checks: Vec<CheckRow>,
    },
    /// Terminal allowlisted states that are **not** mechanical (`cancelled` / `timed_out` / `action_required`).
    NonRepairable { names: Vec<String> },
    /// Still waiting (pending / missing name).
    Waiting,
}

/// Terminal result of [`watch_allowlisted_checks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchOutcome {
    Ready,
    Failed { checks: Vec<CheckRow> },
    NonRepairable { names: Vec<String> },
    Timeout,
    Cancelled,
}

/// Evaluate allowlisted readiness. Empty forge list with non-empty allowlist is
/// never Ready.
///
/// Genuine red (`failure`/`failed`/`error`) → [`AllowlistReady::Failed`].
/// `cancelled` / `timed_out` / `action_required` → [`AllowlistReady::NonRepairable`]
/// (fail closed; not mechanical auto-fix). Non-repairable **states** and bucket
/// `cancel` are classified before any `bucket=fail` mechanical rule (real `gh`
/// maps `timed_out` / `action_required` to bucket `fail`).
///
/// Path-filtered jobs that report skip / skipped / skipping / neutral (state or
/// bucket) are **Ready for that name** — missing name is still Waiting.
#[must_use]
pub fn evaluate_allowlist(checks: &[CheckRow], allowlist: &[String]) -> AllowlistReady {
    if allowlist.is_empty() {
        return AllowlistReady::Ready;
    }
    let filtered = filter_allowlisted(checks, allowlist);
    let mut any_waiting = false;
    let mut failed_checks = Vec::new();
    let mut non_repairable_names = Vec::new();
    for name in allowlist {
        let Some(row) = filtered.iter().find(|c| c.name == *name) else {
            any_waiting = true;
            continue;
        };
        let state = row.state.to_ascii_lowercase();
        let bucket = row.bucket.to_ascii_lowercase();
        if is_check_success(&state, &bucket) || is_check_skipped(&state, &bucket) {
            continue;
        }
        // Non-repairable states/buckets before bucket=fail mechanical matching.
        if is_check_non_repairable(&state, &bucket) {
            non_repairable_names.push(name.clone());
            continue;
        }
        if is_check_mechanical_failed(&state, &bucket) {
            failed_checks.push((*row).clone());
            continue;
        }
        any_waiting = true;
    }
    // Prefer waiting while anything is still pending; only terminal when all settled.
    if any_waiting {
        return AllowlistReady::Waiting;
    }
    if !failed_checks.is_empty() {
        return AllowlistReady::Failed {
            checks: failed_checks,
        };
    }
    if !non_repairable_names.is_empty() {
        return AllowlistReady::NonRepairable {
            names: non_repairable_names,
        };
    }
    AllowlistReady::Ready
}

fn is_check_success(state: &str, bucket: &str) -> bool {
    matches!(state, "success" | "pass" | "passed") || matches!(bucket, "pass" | "success")
}

/// Path-filter skip / neutral — Ready for that allowlisted name (not Waiting).
fn is_check_skipped(state: &str, bucket: &str) -> bool {
    matches!(state, "skip" | "skipped" | "skipping" | "neutral")
        || matches!(bucket, "skip" | "skipped" | "skipping" | "neutral")
}

/// Genuine job failure — eligible for mechanical deliver repair.
/// Call only after [`is_check_non_repairable`] so `timed_out`+`bucket=fail` is excluded.
fn is_check_mechanical_failed(state: &str, bucket: &str) -> bool {
    matches!(state, "failure" | "failed" | "fail" | "error") || matches!(bucket, "fail" | "failure")
}

/// Provider terminal that must fail closed without spawning a fixer.
fn is_check_non_repairable(state: &str, bucket: &str) -> bool {
    matches!(state, "cancelled" | "timed_out" | "action_required") || matches!(bucket, "cancel")
}

/// Decode `gh pr list --json` output. Undecodable → error (do not create).
///
/// # Errors
///
/// Returns [`Error::Json`] / [`Error::Msg`] when the listing cannot be trusted.
pub fn parse_pr_list(stdout: &[u8]) -> Result<Vec<PullRequest>, Error> {
    let text =
        std::str::from_utf8(stdout).map_err(|e| Error::Msg(format!("pr list utf-8: {e}")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<PullRequest> = serde_json::from_str(trimmed)?;
    for row in &rows {
        if row.number == 0 || row.url.trim().is_empty() {
            return Err(Error::Msg(format!(
                "undecodable pr list entry: number={} url={}",
                row.number, row.url
            )));
        }
    }
    Ok(rows)
}

/// Decode `gh pr checks --json` output.
///
/// # Errors
///
/// Returns JSON errors when the payload is not a check array.
pub fn parse_pr_checks(stdout: &[u8]) -> Result<Vec<CheckRow>, Error> {
    let text =
        std::str::from_utf8(stdout).map_err(|e| Error::Msg(format!("pr checks utf-8: {e}")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(trimmed)?)
}

/// Replace conventional home-directory prefixes with `~`.
#[must_use]
pub fn redact_home_paths(text: &str) -> String {
    redact_home_paths_with(text, std::env::var("HOME").ok().as_deref())
}

/// Like [`redact_home_paths`], with an explicit operator home (for tests).
#[must_use]
pub fn redact_home_paths_with(text: &str, home: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(home) = home {
        if !home.is_empty() {
            out = redact_exact_home(&out, home);
        }
    }
    out = redact_prefix(&out, "/Users/");
    out = redact_prefix(&out, "/home/");
    out = redact_windows_users(&out);
    out
}

/// Replace `home` only when followed by a path separator or end-of-string.
fn redact_exact_home(text: &str, home: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(home) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + home.len()..];
        if after.is_empty() || after.starts_with('/') || after.starts_with('\\') {
            out.push('~');
            rest = after;
        } else {
            // Mere string prefix of another path — keep literally.
            out.push_str(home);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn redact_prefix(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(prefix) {
        out.push_str(&rest[..idx]);
        out.push('~');
        let after = &rest[idx + prefix.len()..];
        // Skip the username segment up to next path separator or end.
        let end = after.find(['/', '\\']).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn redact_windows_users(text: &str) -> String {
    // C:\Users\<name> and c:/Users/<name>
    let mut out = text.to_string();
    for prefix in ["C:\\Users\\", "c:\\Users\\", "C:/Users/", "c:/Users/"] {
        out = redact_prefix(&out, prefix);
    }
    out
}

/// Pipeline step snapshot for attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSnapshot {
    pub step: String,
    pub status: String,
}

/// Attestation JSON bound into an HTML comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub head_sha: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepSnapshot>,
}

/// Build porch PR body sections + HTML attestation, then redact homes.
#[must_use]
pub fn build_pr_body(
    intent: Option<&str>,
    what_changed: &str,
    risk: &str,
    review: &str,
    certify: &str,
    pipeline: &str,
    attestation: &Attestation,
) -> String {
    let mut body = String::new();
    body.push_str("## Intent\n\n");
    body.push_str(intent.unwrap_or("_none_"));
    body.push_str("\n\n## What Changed\n\n");
    body.push_str(what_changed);
    body.push_str("\n\n## Risk\n\n");
    body.push_str(risk);
    body.push_str("\n\n## Review\n\n");
    body.push_str(review);
    body.push_str("\n\n## Certify\n\n");
    body.push_str(certify);
    body.push_str("\n\n## Pipeline\n\n");
    body.push_str(pipeline);
    body.push('\n');
    if let Ok(json) = serde_json::to_string(attestation) {
        use std::fmt::Write as _;
        let _ = write!(body, "\n<!-- {ATTESTATION_MARKER} {json} -->\n");
    }
    redact_home_paths(&body)
}

/// Deterministic PR title (no agent).
#[must_use]
pub fn pr_title(branch: &str) -> String {
    format!("porch: {branch}")
}

/// Options for finding / creating / updating a PR.
#[derive(Debug, Clone)]
pub struct PrOpts<'a> {
    pub bin: &'a str,
    pub timeout: Duration,
    pub work_tree: &'a Path,
    pub head_branch: &'a str,
    pub base_branch: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

/// Find an open PR by head branch (no `--base` filter).
///
/// # Errors
///
/// Undecodable listings and `gh` failures are errors — never treated as “no PR”.
pub fn find_open_pr(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    head_branch: &str,
) -> Result<Option<PullRequest>, Error> {
    let out = run_gh(
        bin,
        timeout,
        work_tree,
        &[
            "pr",
            "list",
            "--head",
            head_branch,
            "--state",
            "open",
            "--json",
            "number,url,title",
        ],
    )?;
    let list = parse_pr_list(&out.stdout)?;
    Ok(list.into_iter().next())
}

/// Create a PR; returns the URL from stdout.
///
/// # Errors
///
/// Returns spawn / exit / empty-URL errors.
pub fn create_pr(opts: &PrOpts<'_>) -> Result<String, Error> {
    let out = run_gh_with_stdin(
        opts.bin,
        opts.timeout,
        opts.work_tree,
        &[
            "pr",
            "create",
            "--head",
            opts.head_branch,
            "--base",
            opts.base_branch,
            "--title",
            opts.title,
            "--body-file",
            "-",
        ],
        opts.body.as_bytes(),
    )?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        return Err(Error::Msg("gh pr create produced empty URL".into()));
    }
    Ok(url)
}

/// Update an existing PR body.
///
/// # Errors
///
/// Returns spawn / exit errors.
pub fn edit_pr_body(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    number: u64,
    body: &str,
) -> Result<(), Error> {
    let num = number.to_string();
    let _ = run_gh_with_stdin(
        bin,
        timeout,
        work_tree,
        &["pr", "edit", &num, "--body-file", "-"],
        body.as_bytes(),
    )?;
    Ok(())
}

/// List PR checks JSON via `gh pr checks --json name,state,bucket,link`.
///
/// # Errors
///
/// Returns spawn / exit / JSON errors. Exit 1 with JSON on stdout is accepted
/// (gh returns 1 when any check failed).
pub fn list_pr_checks(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    number: u64,
) -> Result<Vec<CheckRow>, Error> {
    let num = number.to_string();
    let out = run_gh_allow_nonzero(
        bin,
        timeout,
        work_tree,
        &["pr", "checks", &num, "--json", "name,state,bucket,link"],
    )?;
    parse_pr_checks(&out.stdout)
}

/// PR mergeability from `gh pr view --json mergeable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeableState {
    ConflictFree,
    Conflicting,
    Unknown(String),
}

/// Decode `gh pr view --json mergeable` stdout.
///
/// # Errors
///
/// Returns JSON / UTF-8 errors when the payload cannot be trusted.
pub fn parse_mergeable(stdout: &[u8]) -> Result<MergeableState, Error> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        mergeable: String,
    }
    let text =
        std::str::from_utf8(stdout).map_err(|e| Error::Msg(format!("pr view utf-8: {e}")))?;
    let row: Row = serde_json::from_str(text.trim())?;
    Ok(match row.mergeable.to_ascii_uppercase().as_str() {
        "MERGEABLE" => MergeableState::ConflictFree,
        "CONFLICTING" => MergeableState::Conflicting,
        other => MergeableState::Unknown(other.to_string()),
    })
}

/// Fetch mergeability for an open PR.
///
/// # Errors
///
/// Returns spawn / exit / JSON errors.
pub fn pr_mergeable(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    number: u64,
) -> Result<MergeableState, Error> {
    let num = number.to_string();
    let out = run_gh(
        bin,
        timeout,
        work_tree,
        &["pr", "view", &num, "--json", "mergeable"],
    )?;
    parse_mergeable(&out.stdout)
}

/// Options for [`watch_allowlisted_checks`].
#[derive(Debug, Clone)]
pub struct WatchChecksOpts<'a> {
    pub bin: &'a str,
    pub gh_timeout: Duration,
    pub work_tree: &'a Path,
    pub pr_number: u64,
    pub allowlist: &'a [String],
    pub poll_deadline: Duration,
    pub poll_interval: Duration,
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Poll allowlisted checks until a terminal [`WatchOutcome`].
///
/// Never runs `gh run rerun` (M6 keeps `rerun_transient = 0`).
/// `cancel` is checked before each poll and during interruptible sleep so a
/// supersede does not wait out the full poll deadline.
///
/// # Errors
///
/// Returns `gh` / I/O errors. Terminal allowlist states are `Ok(WatchOutcome)`.
pub fn watch_allowlisted_checks(opts: &WatchChecksOpts<'_>) -> Result<WatchOutcome, Error> {
    use std::sync::atomic::Ordering;
    if opts.allowlist.is_empty() {
        return Ok(WatchOutcome::Ready);
    }
    let start = Instant::now();
    loop {
        if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Ok(WatchOutcome::Cancelled);
        }
        let checks = list_pr_checks(opts.bin, opts.gh_timeout, opts.work_tree, opts.pr_number)?;
        match evaluate_allowlist(&checks, opts.allowlist) {
            AllowlistReady::Ready => return Ok(WatchOutcome::Ready),
            AllowlistReady::Failed { checks } => {
                return Ok(WatchOutcome::Failed { checks });
            }
            AllowlistReady::NonRepairable { names } => {
                return Ok(WatchOutcome::NonRepairable { names });
            }
            AllowlistReady::Waiting => {
                if start.elapsed() >= opts.poll_deadline {
                    return Ok(WatchOutcome::Timeout);
                }
                // Interruptible sleep so supersede cancel is prompt.
                let sleep_deadline = Instant::now() + opts.poll_interval;
                while Instant::now() < sleep_deadline {
                    if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                        return Ok(WatchOutcome::Cancelled);
                    }
                    let remaining = sleep_deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(remaining.min(Duration::from_millis(50)));
                }
            }
        }
    }
}

struct GhOutput {
    stdout: Vec<u8>,
    #[allow(dead_code)]
    stderr: String,
}

fn run_gh(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    args: &[&str],
) -> Result<GhOutput, Error> {
    run_gh_inner(bin, timeout, work_tree, args, None, false)
}

fn run_gh_with_stdin(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    args: &[&str],
    stdin: &[u8],
) -> Result<GhOutput, Error> {
    run_gh_inner(bin, timeout, work_tree, args, Some(stdin), false)
}

fn run_gh_allow_nonzero(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    args: &[&str],
) -> Result<GhOutput, Error> {
    run_gh_inner(bin, timeout, work_tree, args, None, true)
}

fn run_gh_inner(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    allow_nonzero: bool,
) -> Result<GhOutput, Error> {
    let mut cmd = Command::new(bin);
    cmd.current_dir(work_tree);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::BinNotFound {
                bin: bin.to_string(),
                source: e,
            }
        } else {
            Error::Io(e)
        }
    })?;

    if let Some(data) = stdin {
        if let Some(mut child_stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = child_stdin.write_all(data);
        }
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child_group(child.id());
                    let _ = child.wait();
                    return Err(Error::Timeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                kill_child_group(child.id());
                return Err(Error::Io(e));
            }
        }
    };

    let mut stdout = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_end(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }

    if !status.success() && !allow_nonzero {
        return Err(Error::Exit {
            status: status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }

    Ok(GhOutput { stdout, stderr })
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

/// Absolute path helper for tests that install a fake `gh`.
#[must_use]
pub fn fake_gh_log_path(porch_home: &Path) -> PathBuf {
    porch_home.join("gh-argv.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_unix_homes() {
        let s = redact_home_paths("see /Users/jayden/secret and /home/alice/x");
        assert!(s.contains("~/secret"), "{s}");
        assert!(s.contains("~/x"), "{s}");
        assert!(!s.contains("/Users/jayden"), "{s}");
        assert!(!s.contains("/home/alice"), "{s}");
    }

    #[test]
    fn redact_windows_home() {
        let s = redact_home_paths(r"path C:\Users\bob\app and done");
        assert!(s.contains(r"~\app"), "{s}");
        assert!(!s.contains(r"C:\Users\bob"), "{s}");
    }

    #[test]
    fn attestation_binds_head_sha() {
        let body = build_pr_body(
            Some("fix it"),
            "one file",
            "low",
            "clean",
            "ok",
            "intent→deliver",
            &Attestation {
                head_sha: "abc123deadbeef".into(),
                steps: vec![StepSnapshot {
                    step: "review".into(),
                    status: "completed".into(),
                }],
            },
        );
        assert!(body.contains("<!-- porch-attestation"), "{body}");
        assert!(body.contains("\"head_sha\":\"abc123deadbeef\""), "{body}");
        assert!(body.contains("## Intent"), "{body}");
        assert!(body.contains("fix it"), "{body}");
    }

    #[test]
    fn pr_title_deterministic() {
        assert_eq!(pr_title("feat/x"), "porch: feat/x");
    }

    #[test]
    fn bin_not_found_mentions_env_and_doctor() {
        let err = Error::BinNotFound {
            bin: "gh".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        };
        let s = err.to_string();
        assert!(s.contains("PORCH_GH_BIN"), "{s}");
        assert!(s.contains("porch doctor"), "{s}");
    }

    #[test]
    fn parse_pr_list_empty_ok() {
        assert!(parse_pr_list(b"").unwrap().is_empty());
        assert!(parse_pr_list(b"[]").unwrap().is_empty());
    }

    #[test]
    fn parse_pr_list_undecodable_fails() {
        let err = parse_pr_list(br#"[{"number":0,"url":""}]"#).unwrap_err();
        assert!(err.to_string().contains("undecodable"), "{err}");
    }

    #[test]
    fn parse_pr_list_valid() {
        let list = parse_pr_list(br#"[{"number":7,"url":"https://example.com/pr/7","title":"t"}]"#)
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].number, 7);
    }

    fn check(name: &str, state: &str, bucket: &str) -> CheckRow {
        CheckRow {
            name: name.into(),
            state: state.into(),
            bucket: bucket.into(),
            link: None,
        }
    }

    #[test]
    fn allowlist_empty_is_ready() {
        assert_eq!(evaluate_allowlist(&[], &[]), AllowlistReady::Ready);
    }

    #[test]
    fn allowlist_missing_names_wait() {
        let checks = vec![check("e2e", "failure", "fail")];
        assert_eq!(
            evaluate_allowlist(&checks, &["lint".into()]),
            AllowlistReady::Waiting
        );
    }

    #[test]
    fn allowlist_failed_carries_red_names() {
        let checks = vec![
            check("lint", "failure", "fail"),
            check("types", "error", ""),
            check("e2e", "failure", "fail"),
        ];
        match evaluate_allowlist(&checks, &["lint".into(), "types".into()]) {
            AllowlistReady::Failed { checks: failed } => {
                assert_eq!(
                    failed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                    vec!["lint", "types"]
                );
                assert_eq!(failed[0].state, "failure");
                assert_eq!(failed[1].state, "error");
            }
            other => panic!("expected Failed with checks, got {other:?}"),
        }
    }

    #[test]
    fn allowlist_cancelled_is_non_repairable_not_mechanical() {
        let checks = vec![check("lint", "cancelled", "cancel")];
        match evaluate_allowlist(&checks, &["lint".into()]) {
            AllowlistReady::NonRepairable { names } => {
                assert_eq!(names, vec!["lint".to_string()]);
            }
            other => panic!("expected NonRepairable, got {other:?}"),
        }
    }

    #[test]
    fn allowlist_timed_out_with_fail_bucket_is_non_repairable() {
        // Real `gh pr checks` maps timed_out → bucket=fail.
        let checks = vec![check("lint", "timed_out", "fail")];
        match evaluate_allowlist(&checks, &["lint".into()]) {
            AllowlistReady::NonRepairable { names } => {
                assert_eq!(names, vec!["lint".to_string()]);
            }
            other => panic!("expected NonRepairable, got {other:?}"),
        }
    }

    #[test]
    fn allowlist_action_required_with_fail_bucket_is_non_repairable() {
        // Real `gh pr checks` maps action_required → bucket=fail.
        let checks = vec![check("lint", "action_required", "fail")];
        match evaluate_allowlist(&checks, &["lint".into()]) {
            AllowlistReady::NonRepairable { names } => {
                assert_eq!(names, vec!["lint".to_string()]);
            }
            other => panic!("expected NonRepairable, got {other:?}"),
        }
    }

    #[test]
    fn allowlist_success_ready_ignores_unlisted_red() {
        let checks = vec![
            check("lint", "success", "pass"),
            check("e2e", "failure", "fail"),
        ];
        assert_eq!(
            evaluate_allowlist(&checks, &["lint".into()]),
            AllowlistReady::Ready
        );
    }

    #[test]
    fn allowlist_skipped_types_check_with_lint_pass_is_ready() {
        // Path-filtered PR jobs show as skipped/skipping; treat as Ready for that name.
        let checks = vec![
            check("types-check", "skipped", "skipping"),
            check("lint", "success", "pass"),
            check("e2e", "failure", "fail"),
        ];
        assert_eq!(
            evaluate_allowlist(&checks, &["lint".into(), "types-check".into()]),
            AllowlistReady::Ready
        );
    }

    #[test]
    fn allowlist_failed_lint_still_failed_when_peer_skipped() {
        let checks = vec![
            check("lint", "failure", "fail"),
            check("types-check", "skipped", "skipping"),
        ];
        match evaluate_allowlist(&checks, &["lint".into(), "types-check".into()]) {
            AllowlistReady::Failed { checks: failed } => {
                assert_eq!(
                    failed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                    vec!["lint"]
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn allowlist_unlisted_e2e_fail_ignored_with_skips() {
        let checks = vec![
            check("lint", "success", "pass"),
            check("types-check", "skip", "skip"),
            check("e2e", "failure", "fail"),
        ];
        assert_eq!(
            evaluate_allowlist(&checks, &["lint".into(), "types-check".into()]),
            AllowlistReady::Ready
        );
    }

    #[test]
    fn allowlist_missing_name_still_waiting_even_if_peers_skipped() {
        let checks = vec![check("lint", "skipped", "neutral")];
        assert_eq!(
            evaluate_allowlist(&checks, &["lint".into(), "types-check".into()]),
            AllowlistReady::Waiting
        );
    }

    #[test]
    fn allowlist_neutral_state_or_bucket_is_ready() {
        assert_eq!(
            evaluate_allowlist(&[check("lint", "neutral", "")], &["lint".into()]),
            AllowlistReady::Ready
        );
        assert_eq!(
            evaluate_allowlist(&[check("lint", "success", "neutral")], &["lint".into()]),
            AllowlistReady::Ready
        );
        assert_eq!(
            evaluate_allowlist(&[check("lint", "", "neutral")], &["lint".into()]),
            AllowlistReady::Ready
        );
    }

    #[test]
    fn filter_ignores_unlisted() {
        let checks = vec![check("lint", "success", ""), check("deploy", "failure", "")];
        let filtered = filter_allowlisted(&checks, &["lint".into()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "lint");
    }

    #[test]
    fn parse_mergeable_conflicting() {
        assert_eq!(
            parse_mergeable(br#"{"mergeable":"CONFLICTING"}"#).unwrap(),
            MergeableState::Conflicting
        );
        assert_eq!(
            parse_mergeable(br#"{"mergeable":"MERGEABLE"}"#).unwrap(),
            MergeableState::ConflictFree
        );
    }

    #[test]
    fn watch_returns_cancelled_when_flag_already_set() {
        use std::sync::atomic::AtomicBool;
        let tmp = tempfile::TempDir::new().unwrap();
        let wt = tmp.path().canonicalize().unwrap();
        let cancel = AtomicBool::new(true);
        let start = Instant::now();
        let outcome = watch_allowlisted_checks(&WatchChecksOpts {
            bin: "false", // unused when cancel trips first
            gh_timeout: Duration::from_secs(5),
            work_tree: &wt,
            pr_number: 1,
            allowlist: &["lint".into()],
            poll_deadline: Duration::from_secs(30),
            poll_interval: Duration::from_secs(5),
            cancel: Some(&cancel),
        })
        .unwrap();
        assert_eq!(outcome, WatchOutcome::Cancelled);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "cancel must not wait for poll timeout"
        );
    }

    #[test]
    fn redact_home_requires_path_boundary() {
        // A short HOME that is a prefix of another path must not corrupt it.
        let s = redact_home_paths_with(
            "see /Users/jay/secret and /Users/jayden/other",
            Some("/Users/jay"),
        );
        assert!(s.contains("~/secret"), "{s}");
        // Unbounded replace would yield "~/den/other"; boundary keep + /Users/
        // redactor yields "~/other".
        assert!(
            !s.contains("~/den"),
            "HOME must not replace a mere prefix: {s}"
        );
        assert!(s.contains("~/other"), "{s}");
    }
}
