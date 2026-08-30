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

/// Start of the porch-owned visible body region.
pub const MANAGED_BEGIN: &str = "<!-- porch-managed:begin -->";

/// End of the porch-owned visible body region (attestation stays outside).
pub const MANAGED_END: &str = "<!-- porch-managed:end -->";

/// Fixed-path PR template candidates (trusted tree), in pick order.
const PR_TEMPLATE_PATHS: &[&str] = &[
    ".github/pull_request_template.md",
    "pull_request_template.md",
    "docs/pull_request_template.md",
];

/// Multi-template directory (lexicographic first `*.md` after fixed paths).
const PR_TEMPLATE_DIR: &str = ".github/PULL_REQUEST_TEMPLATE";

/// Which template source won when loading from the trusted SHA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSource {
    /// Consumer repo template blob at a trusted-tree path.
    RepoTemplate,
    /// No readable template at the trusted SHA — callers use porch default.
    PorchDefault,
}

impl TemplateSource {
    /// Packet / status string (`repo_template` | `porch_default`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepoTemplate => "repo_template",
            Self::PorchDefault => "porch_default",
        }
    }
}

/// Result of [`load_pr_template`]: bytes plus which path/source won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateBytes {
    /// Raw template file bytes when [`TemplateSource::RepoTemplate`].
    pub bytes: Option<Vec<u8>>,
    pub source: TemplateSource,
    /// Trusted-tree path that won, or `None` for porch default.
    pub path: Option<String>,
}

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

/// PR fields from `gh pr view` (body included for managed merge).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrView {
    pub number: u64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
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

/// Placeholder facts for the default scaffold (not a path-list dump).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScaffoldFacts {
    /// Short prose for Summary (commit subjects / intent — not raw paths alone).
    pub summary: String,
    pub why: String,
    pub how_tested: String,
    pub links: String,
}

fn placeholder(s: &str) -> &str {
    let t = s.trim();
    if t.is_empty() { "…" } else { t }
}

/// Default visible interior when no consumer PR template is present.
#[must_use]
pub fn default_scaffold_interior(facts: &ScaffoldFacts) -> String {
    format!(
        "## Summary\n\n{}\n\n## Why\n\n{}\n\n## How tested\n\n{}\n\n## Links\n\n{}\n",
        placeholder(&facts.summary),
        placeholder(&facts.why),
        placeholder(&facts.how_tested),
        placeholder(&facts.links),
    )
}

fn wrap_managed(interior: &str) -> String {
    let trimmed = interior.trim_end_matches('\n');
    format!("{MANAGED_BEGIN}\n{trimmed}\n{MANAGED_END}\n")
}

fn format_attestation_comment(attestation: &Attestation) -> String {
    match serde_json::to_string(attestation) {
        Ok(json) => format!("<!-- {ATTESTATION_MARKER} {json} -->\n"),
        Err(_) => String::new(),
    }
}

fn strip_attestation_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    let open = format!("<!-- {ATTESTATION_MARKER}");
    while let Some(idx) = rest.find(&open) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + open.len()..];
        match after.find("-->") {
            Some(end) => rest = after[end + 3..].trim_start_matches('\n'),
            None => {
                // Unclosed marker — drop the rest of the comment opener.
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn finish_body(visible: &str, attestation: &Attestation) -> String {
    let mut body = visible.trim_end().to_string();
    body.push('\n');
    let comment = format_attestation_comment(attestation);
    if !comment.is_empty() {
        body.push('\n');
        body.push_str(&comment);
    }
    redact_home_paths(&body)
}

/// Load PR template bytes from the trusted default-branch SHA only.
///
/// Pick order: first existing among [`PR_TEMPLATE_PATHS`], then the lexicographically
/// first `*.md` under [`PR_TEMPLATE_DIR`]. Never reads the feature tip alone.
///
/// # Errors
///
/// Returns [`Error::Msg`] when git cannot read the trusted commit or a chosen blob.
pub fn load_pr_template(
    bare: &porch_git::GitDir,
    trusted_sha: &str,
) -> Result<TemplateBytes, Error> {
    for path in PR_TEMPLATE_PATHS {
        if let Some(bytes) = porch_git::show_path_at(bare, trusted_sha, path)
            .map_err(|e| Error::Msg(format!("read PR template {path} at {trusted_sha}: {e}")))?
        {
            return Ok(TemplateBytes {
                bytes: Some(bytes),
                source: TemplateSource::RepoTemplate,
                path: Some((*path).to_string()),
            });
        }
    }

    let names = porch_git::list_tree_names_at(bare, trusted_sha, PR_TEMPLATE_DIR).map_err(|e| {
        Error::Msg(format!(
            "list PR templates under {PR_TEMPLATE_DIR} at {trusted_sha}: {e}"
        ))
    })?;
    if let Some(names) = names {
        let mut md: Vec<String> = names
            .into_iter()
            .filter(|name| {
                !name.contains('/')
                    && !name.contains('\\')
                    && Path::new(name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            })
            .collect();
        md.sort();
        if let Some(name) = md.first() {
            let path = format!("{PR_TEMPLATE_DIR}/{name}");
            if let Some(bytes) = porch_git::show_path_at(bare, trusted_sha, &path)
                .map_err(|e| Error::Msg(format!("read PR template {path} at {trusted_sha}: {e}")))?
            {
                return Ok(TemplateBytes {
                    bytes: Some(bytes),
                    source: TemplateSource::RepoTemplate,
                    path: Some(path),
                });
            }
        }
    }

    Ok(TemplateBytes {
        bytes: None,
        source: TemplateSource::PorchDefault,
        path: None,
    })
}

/// Build scaffold PR body: managed markers + template or default skeleton + attestation.
///
/// `template_or_default`: `Some(template_bytes)` uses those as the managed interior;
/// `None` uses [`default_scaffold_interior`].
#[must_use]
pub fn build_scaffold_body(
    template_or_default: Option<&str>,
    facts: &ScaffoldFacts,
    attestation: &Attestation,
) -> String {
    let interior = match template_or_default {
        Some(template) => {
            let mut t = template.to_string();
            if !t.ends_with('\n') {
                t.push('\n');
            }
            t
        }
        None => default_scaffold_interior(facts),
    };
    finish_body(&wrap_managed(&interior), attestation)
}

/// Replace porch-managed region + refresh attestation; preserve human regions outside markers.
///
/// `new_visible` is the new managed **interior** (without begin/end markers).
/// When existing body has no managed pair, the whole body is replaced by a fresh scaffold wrap.
#[must_use]
pub fn merge_porch_managed(
    existing_body: &str,
    new_visible: &str,
    attestation: &Attestation,
) -> String {
    let cleaned = strip_attestation_comments(existing_body);
    let wrapped = wrap_managed(new_visible);
    let merged = match (cleaned.find(MANAGED_BEGIN), cleaned.find(MANAGED_END)) {
        (Some(begin), Some(end)) if begin < end => {
            let after_end = end + MANAGED_END.len();
            let mut out = String::with_capacity(cleaned.len() + wrapped.len());
            out.push_str(&cleaned[..begin]);
            out.push_str(wrapped.trim_end());
            out.push('\n');
            out.push_str(cleaned[after_end..].trim_start_matches('\n'));
            out
        }
        _ => wrapped,
    };
    finish_body(&merged, attestation)
}

/// Compatibility shim: builds the default scaffold (ignores theater section inputs).
///
/// Prefer [`build_scaffold_body`] / [`merge_porch_managed`]. Intent maps into Summary when present.
#[must_use]
pub fn build_pr_body(
    intent: Option<&str>,
    _what_changed: &str,
    _risk: &str,
    _review: &str,
    _certify: &str,
    _pipeline: &str,
    attestation: &Attestation,
) -> String {
    let facts = ScaffoldFacts {
        summary: intent.unwrap_or("").to_string(),
        ..ScaffoldFacts::default()
    };
    build_scaffold_body(None, &facts, attestation)
}

/// Deterministic scaffold PR title (no agent).
///
/// Preference: first non-empty intent line, else commit subject, else
/// `porch: {branch}` (legacy-compatible fallback).
#[must_use]
pub fn deterministic_pr_title(
    branch: &str,
    intent: Option<&str>,
    commit_subject: Option<&str>,
) -> String {
    if let Some(line) = first_nonempty_line(intent) {
        return line;
    }
    if let Some(line) = first_nonempty_line(commit_subject) {
        return line;
    }
    format!("porch: {branch}")
}

/// Thin wrapper for the branch-only fallback. Prefer [`deterministic_pr_title`].
#[must_use]
pub fn pr_title(branch: &str) -> String {
    deterministic_pr_title(branch, None, None)
}

/// Whether `current` is still porch-owned (design §8 title heuristic).
///
/// True when equal to `last_written`, matches `^porch: `, or equals the current
/// scaffold deterministic title.
#[must_use]
pub fn is_porch_managed_title(
    current: &str,
    last_written: Option<&str>,
    scaffold_title: &str,
) -> bool {
    if last_written.is_some_and(|last| current == last) {
        return true;
    }
    if current.starts_with("porch: ") {
        return true;
    }
    current == scaffold_title
}

fn first_nonempty_line(raw: Option<&str>) -> Option<String> {
    raw.and_then(|text| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    })
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

/// Update an existing PR title via `gh pr edit --title`.
///
/// # Errors
///
/// Returns spawn / exit errors.
pub fn edit_pr_title(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    number: u64,
    title: &str,
) -> Result<(), Error> {
    let num = number.to_string();
    let _ = run_gh(
        bin,
        timeout,
        work_tree,
        &["pr", "edit", &num, "--title", title],
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

/// Fetch title/body (and identity) for an open PR via `gh pr view`.
///
/// # Errors
///
/// Returns spawn / exit / JSON errors.
pub fn view_pr(
    bin: &str,
    timeout: Duration,
    work_tree: &Path,
    number: u64,
) -> Result<PrView, Error> {
    let num = number.to_string();
    let out = run_gh(
        bin,
        timeout,
        work_tree,
        &["pr", "view", &num, "--json", "number,url,title,body"],
    )?;
    let text =
        std::str::from_utf8(&out.stdout).map_err(|e| Error::Msg(format!("pr view utf-8: {e}")))?;
    Ok(serde_json::from_str(text.trim())?)
}

/// Structured theater-rejection rules shipped in the compose packet (design §8).
#[must_use]
pub fn theater_reject_rules() -> serde_json::Value {
    serde_json::json!({
        "forbid_pipeline_board": true,
        "forbid_certify_transcript": true,
        "forbid_review_findings_dump": true,
        "forbid_approved_at_sha_line": true,
        "forbid_visible_attestation_restatement": true,
    })
}

/// Extract the managed interior from an Agent compose body (markers optional).
#[must_use]
pub fn compose_managed_interior(body: &str) -> String {
    let cleaned = strip_attestation_comments(body);
    match (cleaned.find(MANAGED_BEGIN), cleaned.find(MANAGED_END)) {
        (Some(begin), Some(end)) if begin < end => {
            let interior = cleaned[begin + MANAGED_BEGIN.len()..end].trim();
            if interior.is_empty() {
                String::new()
            } else {
                format!("{interior}\n")
            }
        }
        _ => {
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                String::new()
            } else {
                format!("{trimmed}\n")
            }
        }
    }
}

/// Validate Agent compose body before merge (PRCMP-4.4).
///
/// # Errors
///
/// Returns a human-readable rejection when the body is empty or reintroduces
/// porch self-review theater signatures.
pub fn validate_compose_body(body: &str) -> Result<(), String> {
    let interior = compose_managed_interior(body);
    if interior.trim().is_empty() {
        return Err("compose body is empty".into());
    }

    let visible = strip_attestation_comments(body);
    if has_pipeline_board(&visible) {
        return Err("compose body rejected: porch theater signature (Pipeline board)".into());
    }
    if has_approved_at_sha_line(&visible) {
        return Err("compose body rejected: porch theater signature (approved-at SHA)".into());
    }
    if has_certify_transcript(&visible) {
        return Err("compose body rejected: porch theater signature (Certify transcript)".into());
    }
    if has_review_findings_dump(&visible) {
        return Err("compose body rejected: porch theater signature (Review findings dump)".into());
    }
    if has_visible_attestation_restatement(&visible) {
        return Err("compose body rejected: porch theater signature (visible attestation)".into());
    }
    Ok(())
}

fn has_pipeline_board(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("## pipeline") {
        return false;
    }
    // intent → … → deliver (ASCII or unicode arrows)
    let collapsed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    collapsed.contains("intent")
        && collapsed.contains("deliver")
        && (collapsed.contains("→") || collapsed.contains("->") || collapsed.contains("=>"))
}

fn has_approved_at_sha_line(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("approved at") && lower.chars().filter(char::is_ascii_hexdigit).count() >= 7
}

fn has_certify_transcript(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("## certify") {
        return false;
    }
    // Gate-style transcript cues (not a bare consumer checklist heading).
    lower.contains("continuity")
        || lower.contains("certified head")
        || lower.contains("certify:")
        || (lower.contains("passed") && lower.contains("failed"))
}

fn has_review_findings_dump(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("## review") {
        return false;
    }
    lower.contains("\"severity\"")
        || lower.contains("finding id")
        || lower.contains("findings:")
        || lower.contains("blocking finding")
}

fn has_visible_attestation_restatement(body: &str) -> bool {
    // HTML comment attestation is stripped before this check; bare marker is theater.
    body.to_ascii_lowercase().contains("porch-attestation")
        || (body.contains("\"head_sha\"") && body.contains("\"steps\""))
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

    fn sample_attestation() -> Attestation {
        Attestation {
            head_sha: "abc123deadbeef".into(),
            steps: vec![StepSnapshot {
                step: "review".into(),
                status: "completed".into(),
            }],
        }
    }

    #[test]
    fn default_scaffold_has_summary_why_how_tested_links() {
        let body = build_scaffold_body(None, &ScaffoldFacts::default(), &sample_attestation());
        assert!(body.contains("## Summary"), "{body}");
        assert!(body.contains("## Why"), "{body}");
        assert!(body.contains("## How tested"), "{body}");
        assert!(body.contains("## Links"), "{body}");
    }

    #[test]
    fn default_scaffold_omits_gate_theater_headings() {
        let body = build_scaffold_body(
            None,
            &ScaffoldFacts {
                summary: "short prose about the change".into(),
                ..ScaffoldFacts::default()
            },
            &sample_attestation(),
        );
        assert!(!body.contains("## Review"), "{body}");
        assert!(!body.contains("## Certify"), "{body}");
        assert!(!body.contains("## Pipeline"), "{body}");
        assert!(!body.contains("## Intent"), "{body}");
        assert!(!body.contains("## What Changed"), "{body}");
        assert!(!body.contains("approved at"), "{body}");
    }

    #[test]
    fn scaffold_wraps_visible_body_in_managed_markers() {
        let body = build_scaffold_body(None, &ScaffoldFacts::default(), &sample_attestation());
        let begin = body.find(MANAGED_BEGIN).expect("managed begin");
        let end = body.find(MANAGED_END).expect("managed end");
        assert!(begin < end, "{body}");
        let attest = body.find("<!-- porch-attestation").expect("attestation");
        assert!(end < attest, "attestation must follow managed end: {body}");
    }

    #[test]
    fn scaffold_attestation_binds_head_sha_outside_managed() {
        let body = build_scaffold_body(None, &ScaffoldFacts::default(), &sample_attestation());
        assert!(body.contains("<!-- porch-attestation"), "{body}");
        assert!(body.contains("\"head_sha\":\"abc123deadbeef\""), "{body}");
        let managed_end = body.find(MANAGED_END).unwrap();
        let attest_at = body.find("<!-- porch-attestation").unwrap();
        assert!(managed_end < attest_at, "{body}");
    }

    #[test]
    fn scaffold_uses_template_bytes_as_managed_interior() {
        let template = "## Custom checklist\n\n- [ ] item\n";
        let body = build_scaffold_body(
            Some(template),
            &ScaffoldFacts::default(),
            &sample_attestation(),
        );
        assert!(body.contains("## Custom checklist"), "{body}");
        assert!(body.contains("- [ ] item"), "{body}");
        assert!(!body.contains("## Summary"), "{body}");
        assert!(body.contains(MANAGED_BEGIN), "{body}");
        assert!(body.contains("<!-- porch-attestation"), "{body}");
    }

    #[test]
    fn scaffold_redacts_home_paths_in_visible_and_facts() {
        let body = build_scaffold_body(
            None,
            &ScaffoldFacts {
                summary: "touched /Users/jayden/secret/file".into(),
                why: "see /home/alice/notes".into(),
                ..ScaffoldFacts::default()
            },
            &sample_attestation(),
        );
        assert!(!body.contains("/Users/jayden"), "{body}");
        assert!(!body.contains("/home/alice"), "{body}");
        assert!(body.contains("~/secret/file"), "{body}");
        assert!(body.contains("~/notes"), "{body}");
    }

    #[test]
    fn merge_replaces_managed_region_and_refreshes_attestation() {
        let existing = format!(
            "operator note\n{MANAGED_BEGIN}\n## Summary\n\nold\n{MANAGED_END}\n\n<!-- porch-attestation {{\"head_sha\":\"oldsha\",\"steps\":[]}} -->\n"
        );
        let merged = merge_porch_managed(
            &existing,
            "## Summary\n\nnew prose\n",
            &Attestation {
                head_sha: "newsha".into(),
                steps: vec![],
            },
        );
        assert!(merged.contains("operator note"), "{merged}");
        assert!(merged.contains("new prose"), "{merged}");
        assert!(!merged.contains("oldsha"), "{merged}");
        assert!(merged.contains("\"head_sha\":\"newsha\""), "{merged}");
        assert!(!merged.contains("\nold\n"), "{merged}");
        assert!(
            merged.contains(MANAGED_BEGIN) && merged.contains(MANAGED_END),
            "{merged}"
        );
    }

    #[test]
    fn merge_preserves_human_regions_outside_markers() {
        let existing = format!(
            "## Human preface\n\nkeep me\n{MANAGED_BEGIN}\ninterior\n{MANAGED_END}\n## Human footer\n\nalso keep\n"
        );
        let merged = merge_porch_managed(&existing, "replaced interior\n", &sample_attestation());
        assert!(merged.contains("## Human preface"), "{merged}");
        assert!(merged.contains("keep me"), "{merged}");
        assert!(merged.contains("## Human footer"), "{merged}");
        assert!(merged.contains("also keep"), "{merged}");
        assert!(merged.contains("replaced interior"), "{merged}");
        assert!(!merged.contains("\ninterior\n"), "{merged}");
    }

    #[test]
    fn merge_redacts_home_paths() {
        let existing = format!("{MANAGED_BEGIN}\nold\n{MANAGED_END}\n");
        let merged = merge_porch_managed(
            &existing,
            "path /Users/jayden/proj\n",
            &sample_attestation(),
        );
        assert!(!merged.contains("/Users/jayden"), "{merged}");
        assert!(merged.contains("~/proj"), "{merged}");
    }

    #[test]
    fn compatibility_build_pr_body_uses_scaffold_not_theater() {
        let body = build_pr_body(
            Some("fix it"),
            "one file",
            "low",
            "clean",
            "ok",
            "intent→deliver",
            &sample_attestation(),
        );
        assert!(body.contains("## Summary"), "{body}");
        assert!(body.contains("<!-- porch-attestation"), "{body}");
        assert!(body.contains("\"head_sha\":\"abc123deadbeef\""), "{body}");
        assert!(!body.contains("## Review"), "{body}");
        assert!(!body.contains("## Certify"), "{body}");
        assert!(!body.contains("## Pipeline"), "{body}");
    }

    #[test]
    fn validate_compose_body_rejects_theater_and_empty() {
        assert!(validate_compose_body("").is_err());
        assert!(validate_compose_body("   \n").is_err());
        let theater =
            "## Summary\nok\n\n## Pipeline\n\nintent → rebase → review → certify → deliver\n";
        assert!(
            validate_compose_body(theater)
                .unwrap_err()
                .contains("theater")
        );
        let approved = "## Summary\nok\n\napproved at `deadbeefcafebabe`\n";
        assert!(validate_compose_body(approved).is_err());
        let ok = "## Summary\n\nShip it.\n\n## Why\n\nBecause.\n";
        assert!(validate_compose_body(ok).is_ok());
    }

    fn bare_with_files(files: &[(&str, &str)]) -> (tempfile::TempDir, porch_git::GitDir, String) {
        use std::process::Command;

        use porch_git::{init_bare, run, stdout_trim};

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["init"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["config", "user.email", "porch@example.com"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["config", "user.name", "Porch"])
            .status()
            .unwrap();
        for (rel, content) in files {
            let path = work.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
        }
        Command::new("git")
            .current_dir(&work)
            .args(["add", "-A"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["commit", "-m", "templates"])
            .status()
            .unwrap();
        let bare_path = root.join("bare.git");
        let bare = init_bare(&bare_path).unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["push", bare_path.to_str().unwrap(), "HEAD:refs/heads/main"])
            .status()
            .unwrap();
        let sha = stdout_trim(&run(&bare, &["rev-parse", "refs/heads/main"]).unwrap());
        (tmp, bare, sha)
    }

    #[test]
    fn load_template_from_github_path_becomes_managed_interior() {
        let (_tmp, bare, trusted) = bare_with_files(&[(
            ".github/pull_request_template.md",
            "## Repo checklist\n\n- [ ] done\n",
        )]);
        let loaded = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(loaded.source, TemplateSource::RepoTemplate);
        assert_eq!(
            loaded.path.as_deref(),
            Some(".github/pull_request_template.md")
        );
        let text = std::str::from_utf8(loaded.bytes.as_ref().unwrap()).unwrap();
        let body =
            build_scaffold_body(Some(text), &ScaffoldFacts::default(), &sample_attestation());
        assert!(body.contains("## Repo checklist"), "{body}");
        assert!(body.contains("- [ ] done"), "{body}");
        assert!(body.contains(MANAGED_BEGIN), "{body}");
        assert!(!body.contains("## Summary"), "{body}");
    }

    #[test]
    fn load_template_missing_uses_porch_default() {
        let (_tmp, bare, trusted) = bare_with_files(&[("README", "hi\n")]);
        let loaded = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(loaded.source, TemplateSource::PorchDefault);
        assert!(loaded.path.is_none());
        assert!(loaded.bytes.is_none());
        let text = loaded
            .bytes
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok());
        let body = build_scaffold_body(text, &ScaffoldFacts::default(), &sample_attestation());
        assert!(body.contains("## Summary"), "{body}");
        assert!(body.contains("## Why"), "{body}");
    }

    #[test]
    fn load_template_ignores_feature_tip_alone() {
        use std::process::Command;

        use porch_git::{init_bare, stdout_trim};

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["init"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["config", "user.email", "porch@example.com"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["config", "user.name", "Porch"])
            .status()
            .unwrap();
        std::fs::write(work.join("README"), "base\n").unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["add", "README"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["commit", "-m", "trusted"])
            .status()
            .unwrap();
        let trusted = stdout_trim(
            &Command::new("git")
                .current_dir(&work)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap(),
        );
        // Feature tip alone carries a template the trusted SHA does not.
        std::fs::create_dir_all(work.join(".github")).unwrap();
        std::fs::write(
            work.join(".github/pull_request_template.md"),
            "## Tip-only template\n",
        )
        .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["add", "-A"])
            .status()
            .unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["commit", "-m", "feature tip"])
            .status()
            .unwrap();
        let tip = stdout_trim(
            &Command::new("git")
                .current_dir(&work)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap(),
        );
        let bare_path = root.join("bare.git");
        let bare = init_bare(&bare_path).unwrap();
        Command::new("git")
            .current_dir(&work)
            .args(["push", bare_path.to_str().unwrap(), "HEAD:refs/heads/feat"])
            .status()
            .unwrap();
        // Pin trusted commit on main without the tip-only template.
        Command::new("git")
            .current_dir(&work)
            .args([
                "push",
                bare_path.to_str().unwrap(),
                &format!("{trusted}:refs/heads/main"),
            ])
            .status()
            .unwrap();

        let from_trusted = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(from_trusted.source, TemplateSource::PorchDefault);
        assert!(from_trusted.bytes.is_none());

        // Control: tip blob exists; load still keyed off trusted SHA only.
        let tip_blob =
            porch_git::show_path_at(&bare, &tip, ".github/pull_request_template.md").unwrap();
        assert!(tip_blob.is_some());
    }

    #[test]
    fn load_template_pick_order_prefers_github_path() {
        let (_tmp, bare, trusted) = bare_with_files(&[
            (".github/pull_request_template.md", "## From .github\n"),
            ("pull_request_template.md", "## From root\n"),
            ("docs/pull_request_template.md", "## From docs\n"),
            (
                ".github/PULL_REQUEST_TEMPLATE/alpha.md",
                "## From dir alpha\n",
            ),
        ]);
        let loaded = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(
            loaded.path.as_deref(),
            Some(".github/pull_request_template.md")
        );
        let text = std::str::from_utf8(loaded.bytes.as_ref().unwrap()).unwrap();
        assert!(text.contains("From .github"), "{text}");
    }

    #[test]
    fn load_template_falls_through_fixed_paths() {
        let (_tmp, bare, trusted) =
            bare_with_files(&[("docs/pull_request_template.md", "## From docs only\n")]);
        let loaded = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(
            loaded.path.as_deref(),
            Some("docs/pull_request_template.md")
        );
        let text = std::str::from_utf8(loaded.bytes.as_ref().unwrap()).unwrap();
        assert!(text.contains("From docs only"), "{text}");
    }

    #[test]
    fn load_template_dir_picks_lexicographic_first_md() {
        let (_tmp, bare, trusted) = bare_with_files(&[
            (".github/PULL_REQUEST_TEMPLATE/zebra.md", "## Zebra\n"),
            (".github/PULL_REQUEST_TEMPLATE/alpha.md", "## Alpha\n"),
            (".github/PULL_REQUEST_TEMPLATE/notes.txt", "ignore me\n"),
        ]);
        let loaded = load_pr_template(&bare, &trusted).unwrap();
        assert_eq!(
            loaded.path.as_deref(),
            Some(".github/PULL_REQUEST_TEMPLATE/alpha.md")
        );
        let text = std::str::from_utf8(loaded.bytes.as_ref().unwrap()).unwrap();
        assert!(text.contains("Alpha"), "{text}");
        assert!(!text.contains("Zebra"), "{text}");
    }

    #[test]
    fn deterministic_pr_title_falls_back_to_porch_branch() {
        assert_eq!(
            deterministic_pr_title("feat/x", None, None),
            "porch: feat/x"
        );
        assert_eq!(
            deterministic_pr_title("feat/x", Some("  \n  "), Some("")),
            "porch: feat/x"
        );
        // Thin wrapper keeps the branch-only fallback.
        assert_eq!(pr_title("feat/x"), "porch: feat/x");
    }

    #[test]
    fn deterministic_pr_title_prefers_intent_over_branch() {
        let title = deterministic_pr_title(
            "feat/x",
            Some("ship the compose packet\n\nmore detail"),
            Some("feat: unrelated commit subject"),
        );
        assert_eq!(title, "ship the compose packet");
        assert_ne!(title, "porch: feat/x");
        assert!(!title.starts_with("porch: "), "{title}");
    }

    #[test]
    fn deterministic_pr_title_uses_commit_subject_when_intent_absent() {
        let title = deterministic_pr_title(
            "feat/x",
            None,
            Some("feat(deliver): improve PR scaffold title"),
        );
        assert_eq!(title, "feat(deliver): improve PR scaffold title");
        assert_ne!(title, "porch: feat/x");
    }

    #[test]
    fn is_porch_managed_title_matches_design_heuristic() {
        let scaffold = deterministic_pr_title("feat/x", Some("ship it"), None);
        assert!(is_porch_managed_title("porch: feat/x", None, &scaffold));
        assert!(is_porch_managed_title(
            "porch: legacy leftover",
            None,
            &scaffold
        ));
        assert!(is_porch_managed_title(&scaffold, None, &scaffold));
        assert!(is_porch_managed_title(
            "agent wrote this earlier",
            Some("agent wrote this earlier"),
            &scaffold
        ));
        assert!(!is_porch_managed_title(
            "Human: please review carefully",
            Some("agent wrote this earlier"),
            &scaffold
        ));
        assert!(!is_porch_managed_title(
            "Human: please review carefully",
            None,
            &scaffold
        ));
    }

    #[test]
    fn edit_pr_title_invokes_gh_pr_edit_title() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let wt = wt.canonicalize().unwrap();

        let bin = tmp.path().join("fake-gh");
        let log = fake_gh_log_path(&home);
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(
                &bin,
                format!(
                    r#"#!/bin/sh
LOG="{}"
{{
  printf '+'
  for a in "$@"; do
    printf ' %s' "$a"
  done
  printf '\n'
}} >> "$LOG"
exit 0
"#,
                    log.display()
                ),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }

        edit_pr_title(
            bin.to_str().unwrap(),
            Duration::from_secs(5),
            &wt,
            42,
            "ship the compose packet",
        )
        .unwrap();

        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(
            logged.contains("pr edit 42 --title ship the compose packet"),
            "expected gh pr edit --title in log, got: {logged}"
        );
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
