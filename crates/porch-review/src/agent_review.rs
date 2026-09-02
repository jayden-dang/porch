//! Session-free coding-agent review (M10). Distinct from the fixer adapter.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::{
    Action, Error, Finding, ReviewComment, ReviewOutcome, Severity, assert_coverage,
    coverage_state::{ProducerOutput, StatusRow},
};

/// Env override for the review agent binary (`claude` / `codex` / PATH fake).
pub const REVIEW_AGENT_BIN_ENV: &str = "PORCH_REVIEW_AGENT_BIN";

/// Relative review artifact dir under `$PORCH_HOME/runs/<id>/` (legacy flat path).
pub const REVIEW_ARTIFACT_REL: &str = "review";

/// Per-invocation artifact directory:
/// `$PORCH_HOME/runs/<run>/rounds/<round>/producers/<invocation>/`.
#[must_use]
pub fn producer_artifact_dir(
    porch_home: &Path,
    run_id: &str,
    round_id: &str,
    invocation_id: &str,
) -> PathBuf {
    porch_home
        .join("runs")
        .join(run_id)
        .join("rounds")
        .join(round_id)
        .join("producers")
        .join(invocation_id)
}

/// Porch-owned reviewer prompt body (written under `$PORCH_HOME`, outside the worktree).
pub const REVIEWER_PROMPT: &str = "\
You are the porch reviewer. This turn is session-free: do not resume any prior session.
Review the changed files in this worktree. Emit JSON findings only — no prose.
Do NOT edit files. Do NOT run the full repository test or lint suite.
Do NOT apply patches. Prefer readonly inspection (git diff, reading files).

Required JSON shape (either is accepted):
  {\"comments\":[{\"path\":\"...\",\"content\":\"...\",\"category\":\"bug\",\"severity\":\"high\",\
\"start_line\":1,\"end_line\":2}],\"files\":[\"path/a.rs\",\"path/b.rs\"]}
  {\"findings\":[{\"path\":\"...\",\"message\":\"...\",\"severity\":\"warning\",\"action\":\"ask-user\",\
\"category\":\"bug\",\"start_line\":1,\"end_line\":2}],\
\"coverage\":[{\"path\":\"path/a.rs\",\"status\":\"pass\"}],\"files\":[\"path/a.rs\"]}

Every changed path listed below MUST appear in files[] or coverage[] (pass or explicit skip).
Missing paths fail the review. Severity: error|warning|info (or critical|high|medium|low).
Action: ask-user|auto-fix|no-op.
";

/// Options for one session-free agent review invocation.
#[derive(Debug, Clone)]
pub struct RunAgentReviewOpts<'a> {
    pub work_tree: &'a Path,
    /// Absolute path to prompt.txt under `$PORCH_HOME`.
    pub prompt_file: &'a Path,
    /// Trusted `$PORCH_HOME` root used to validate `prompt_file`.
    pub porch_home: &'a Path,
    pub from_sha: &'a str,
    pub to_sha: &'a str,
    pub changed_files: &'a [String],
    pub bin: &'a str,
    pub timeout: Duration,
    /// When set, spawn uses this plan's absolute target without re-resolving.
    pub plan: Option<&'a crate::plan::InvocationPlan>,
    /// When set, `result.json` is written here (invocation namespace).
    pub artifact_dir: Option<&'a Path>,
}

#[derive(Debug, Deserialize)]
struct AgentFindingIn {
    path: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AgentCoverageIn {
    path: String,
    #[serde(default)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct AgentReviewJson {
    #[serde(default)]
    comments: Vec<ReviewComment>,
    #[serde(default)]
    findings: Vec<AgentFindingIn>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    coverage: Vec<AgentCoverageIn>,
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

/// Build reviewer prompt text with intent, path instructions, and changed files.
#[must_use]
pub fn build_reviewer_prompt(
    intent: Option<&str>,
    path_instructions_json: Option<&str>,
    changed_files: &[String],
) -> String {
    let mut body = String::from(REVIEWER_PROMPT);
    body.push_str("\n## Intent\n");
    match intent.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            body.push_str(s);
            body.push('\n');
        }
        None => body.push_str("(none)\n"),
    }
    body.push_str("\n## Path instructions\n");
    match path_instructions_json
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => {
            body.push_str(s);
            body.push('\n');
        }
        None => body.push_str("(none)\n"),
    }
    body.push_str("\n## Changed files (coverage required)\n");
    if changed_files.is_empty() {
        body.push_str("(none)\n");
    } else {
        for f in changed_files {
            body.push_str("- ");
            body.push_str(f);
            body.push('\n');
        }
    }
    body
}

/// Write `prompt.txt` under `review_dir` (caller creates parent as needed).
///
/// # Errors
///
/// Returns I/O errors.
pub fn write_reviewer_prompt(
    review_dir: &Path,
    intent: Option<&str>,
    path_instructions_json: Option<&str>,
    changed_files: &[String],
) -> Result<PathBuf, Error> {
    fs::create_dir_all(review_dir)?;
    let prompt_file = review_dir.join("prompt.txt");
    let body = build_reviewer_prompt(intent, path_instructions_json, changed_files);
    fs::write(&prompt_file, body)?;
    Ok(prompt_file)
}

/// Parse agent review JSON (`findings`/`coverage` or OCR `comments`/`files`). Fail closed.
///
/// # Errors
///
/// Returns [`Error::Json`] when the payload is not valid JSON of either shape.
pub fn parse_agent_review_json(bytes: &[u8]) -> Result<ReviewOutcome, Error> {
    let parsed: AgentReviewJson = serde_json::from_slice(bytes)?;
    let mapping = crate::CriterionMapping::builtin();
    let mut findings = Vec::new();
    if parsed.findings.is_empty() {
        for (i, c) in parsed.comments.iter().enumerate() {
            let deterministic = c.rule_id.as_deref().is_some_and(|s| !s.trim().is_empty());
            if let Some(mut f) = crate::enrich_from_comment(
                c,
                &mapping,
                &crate::AnchorContext::default(),
                deterministic,
            ) {
                f.id = format!("f{i}");
                findings.push(f);
            }
        }
    } else {
        for (i, raw) in parsed.findings.iter().enumerate() {
            let mut f = map_agent_finding(raw);
            f.id = format!("f{i}");
            findings.push(f);
        }
    }
    Ok(ReviewOutcome {
        findings,
        covered_files: derive_agent_coverage(&parsed),
        coverage: agent_coverage_output(&parsed),
    })
}

fn agent_coverage_output(parsed: &AgentReviewJson) -> ProducerOutput {
    let rows: Vec<StatusRow> = parsed
        .coverage
        .iter()
        .filter(|c| !c.path.is_empty())
        .map(|c| StatusRow {
            path: c.path.clone(),
            status: c.status.clone(),
            ..StatusRow::default()
        })
        .collect();
    let mut out = ProducerOutput::from_status_rows(&rows);
    for path in &parsed.files {
        if !path.is_empty() {
            out.present_paths.push(path.clone());
        }
    }
    if out.completed.is_empty()
        && out.waived.is_empty()
        && out.failed.is_empty()
        && out.selected.is_empty()
    {
        // Findings/comments alone are presence, never completion.
        for f in &parsed.findings {
            if !f.path.is_empty() {
                out.present_paths.push(f.path.clone());
            }
        }
        for c in &parsed.comments {
            if !c.path.is_empty() {
                out.present_paths.push(c.path.clone());
            }
        }
    }
    out
}

fn map_agent_finding(raw: &AgentFindingIn) -> Finding {
    let message = if raw.message.is_empty() {
        raw.content.clone().unwrap_or_default()
    } else {
        raw.message.clone()
    };
    // Prefer OCR-style mapping for category/severity when action unset.
    let comment = ReviewComment {
        path: raw.path.clone(),
        content: message.clone(),
        existing_code: None,
        suggestion_code: None,
        start_line: raw.start_line,
        end_line: raw.end_line,
        category: raw.category.clone(),
        severity: raw.severity.clone(),
        rule_id: None,
        confidence: None,
    };
    if let Some(mut f) = crate::enrich_from_comment(
        &comment,
        &crate::CriterionMapping::builtin(),
        &crate::AnchorContext::default(),
        false,
    ) {
        if let Some(action) = raw.action.as_deref() {
            f.action = parse_action(action);
        }
        return f;
    }
    let severity = match raw
        .severity
        .as_deref()
        .unwrap_or("warning")
        .to_ascii_lowercase()
        .as_str()
    {
        "error" | "critical" => Severity::Error,
        "info" | "low" => Severity::Info,
        _ => Severity::Warning,
    };
    Finding {
        path: raw.path.clone(),
        message,
        severity,
        action: raw.action.as_deref().map_or(Action::AskUser, parse_action),
        category: raw.category.clone(),
        start_line: raw.start_line,
        end_line: raw.end_line,
        ..Finding::default()
    }
}

fn parse_action(raw: &str) -> Action {
    match raw.to_ascii_lowercase().as_str() {
        "auto-fix" | "autofix" => Action::AutoFix,
        "no-op" | "noop" => Action::NoOp,
        _ => Action::AskUser,
    }
}

fn derive_agent_coverage(parsed: &AgentReviewJson) -> Vec<String> {
    let mut set = BTreeSet::new();
    for f in &parsed.files {
        if !f.is_empty() {
            set.insert(f.clone());
        }
    }
    for c in &parsed.coverage {
        if c.path.is_empty() {
            continue;
        }
        let st = c.status.to_ascii_lowercase();
        if st == "pass" || st == "skip" || st == "skipped" {
            set.insert(c.path.clone());
        }
    }
    if set.is_empty() {
        for f in &parsed.findings {
            if !f.path.is_empty() {
                set.insert(f.path.clone());
            }
        }
        for c in &parsed.comments {
            if !c.path.is_empty() {
                set.insert(c.path.clone());
            }
        }
    }
    set.into_iter().collect()
}

/// Resolve review agent binary: `PORCH_REVIEW_AGENT_BIN` > config `agent_bin`/`bin`.
///
/// # Errors
///
/// Returns [`Error::Msg`] when no agent binary can be resolved.
pub fn agent_review_bin(porch_home: &Path) -> Result<String, Error> {
    if let Ok(v) = std::env::var(REVIEW_AGENT_BIN_ENV) {
        if !v.trim().is_empty() {
            return Ok(v);
        }
    }
    if let Ok(Some(cfg)) = crate::load_home_config(porch_home) {
        if let Some(b) = cfg
            .review
            .agent_bin
            .as_deref()
            .or(cfg.review.bin.as_deref())
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(b.to_string());
        }
    }
    for name in crate::engine::AGENT_DETECT_BINS {
        if let Some(p) = crate::which(name) {
            return Ok(p.display().to_string());
        }
    }
    Err(Error::Msg(format!(
        "review agent not found — set {REVIEW_AGENT_BIN_ENV} or install `claude` / `codex`; see `porch doctor`"
    )))
}

/// True when review should use the session-free agent path.
///
/// `PORCH_REVIEW_BIN` wins (keeps generic/OCR PATH fakes green).
#[must_use]
pub fn review_uses_agent(porch_home: Option<&Path>) -> bool {
    if std::env::var(crate::REVIEW_BIN_ENV)
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return false;
    }
    if std::env::var(REVIEW_AGENT_BIN_ENV)
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    let home = porch_home
        .map(Path::to_path_buf)
        .or_else(crate::porch_home_dir);
    let Some(home) = home else {
        return false;
    };
    matches!(
        crate::load_home_config(&home)
            .ok()
            .flatten()
            .and_then(|c| c.review.engine_kind()),
        Some(crate::EngineKind::Agent)
    )
}

/// Spawn the review agent (session-free), parse JSON, enforce coverage-lite.
///
/// Detects worktree file writes after the turn and fails the review.
/// Never passes `--session-id` / `--resume`.
///
/// # Errors
///
/// Returns spawn, timeout, exit, JSON, coverage, prompt-refuse, or I/O errors.
pub fn run_agent_review(opts: &RunAgentReviewOpts<'_>) -> Result<ReviewOutcome, Error> {
    assert_prompt_under_home(opts.prompt_file, opts.porch_home)?;

    let out_file = prepare_result_json(opts)?;
    let out_s = abs_str(&out_file)?;
    let prompt_s = abs_str(opts.prompt_file)?;

    let bin_path = spawn_bin_path(opts);
    let bin_label = bin_path.display().to_string();
    let family = agent_cli_family(&bin_label);
    // Neutralize before the dirty snapshot so rename/restore is not mistaken for
    // a reviewer write. Claude/Codex skip AGENTS.md neutralize because readonly /
    // plan flags are used; dirty worktree (file-write) detection still fail-closes.
    let neutralized = if family == AgentCliFamily::Generic {
        neutralize_jailbreak_docs(opts.work_tree)?
    } else {
        Vec::new()
    };
    let dirty_before = worktree_porcelain(opts.work_tree)?;

    let result = (|| {
        let mut cmd = Command::new(bin_path);
        cmd.current_dir(opts.work_tree);
        apply_agent_argv(&mut cmd, family, &prompt_s, &out_s, opts)?;
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
                    bin: bin_label,
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
        kill_child_group(pid);

        let mut stderr = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let mut stdout = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_string(&mut stdout);
        }

        if !status.success() {
            return Err(Error::Exit {
                status: status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }

        let dirty_after = worktree_porcelain(opts.work_tree)?;
        if dirty_after != dirty_before {
            return Err(Error::Msg(
                "reviewer modified the worktree (file writes are not allowed for agent review)"
                    .into(),
            ));
        }

        let bytes = if out_file.is_file() {
            fs::read(&out_file)?
        } else if !stdout.trim().is_empty() {
            stdout.into_bytes()
        } else {
            return Err(Error::Msg(format!(
                "review agent produced no output file at {} and empty stdout",
                out_file.display()
            )));
        };

        let outcome = parse_agent_review_json(&bytes)?;
        assert_coverage(opts.changed_files, &outcome.covered_files)?;
        Ok(outcome)
    })();

    restore_neutralized(&neutralized);
    result
}

fn prepare_result_json(opts: &RunAgentReviewOpts<'_>) -> Result<PathBuf, Error> {
    let prompt_parent = opts.prompt_file.parent();
    let out_dir: &Path = match opts.artifact_dir {
        Some(d) => d,
        None => prompt_parent.unwrap_or(opts.porch_home),
    };
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join("result.json");
    if out_file.exists() {
        let _ = fs::remove_file(&out_file);
    }
    Ok(out_file)
}

fn spawn_bin_path<'a>(opts: &'a RunAgentReviewOpts<'a>) -> &'a Path {
    opts.plan.map_or_else(
        || Path::new(opts.bin),
        |p| p.spawned_target_absolute.as_path(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentCliFamily {
    Claude,
    Codex,
    Generic,
}

fn agent_cli_family(bin: &str) -> AgentCliFamily {
    let name = Path::new(bin)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(bin);
    match name {
        "claude" => AgentCliFamily::Claude,
        "codex" => AgentCliFamily::Codex,
        _ => AgentCliFamily::Generic,
    }
}

fn apply_agent_argv(
    cmd: &mut Command,
    family: AgentCliFamily,
    prompt_s: &str,
    out_s: &str,
    opts: &RunAgentReviewOpts<'_>,
) -> Result<(), Error> {
    match family {
        AgentCliFamily::Claude => {
            // Readonly-ish: plan mode + no project settings + no session persist.
            let prompt = fs::read_to_string(opts.prompt_file)?;
            cmd.args([
                "-p",
                "--permission-mode",
                "plan",
                "--no-session-persistence",
                "--setting-sources",
                "user",
                "--output-format",
                "text",
            ]);
            cmd.arg(prompt);
        }
        AgentCliFamily::Codex => {
            let prompt = fs::read_to_string(opts.prompt_file)?;
            cmd.args(["exec", "-s", "read-only"]);
            cmd.arg(prompt);
        }
        AgentCliFamily::Generic => {
            // Porch PATH-fake contract (tests + custom wrappers).
            cmd.args([
                "--prompt-file",
                prompt_s,
                "--output",
                out_s,
                "--from",
                opts.from_sha,
                "--to",
                opts.to_sha,
            ]);
        }
    }
    Ok(())
}

const NEUTRALIZE_NAMES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".cursorrules",
    "ORDERS.md",
];

fn neutralize_jailbreak_docs(work_tree: &Path) -> Result<Vec<(PathBuf, PathBuf)>, Error> {
    let mut moved = Vec::new();
    for name in NEUTRALIZE_NAMES {
        let src = work_tree.join(name);
        if src.is_file() {
            let dst = work_tree.join(format!(".porch-neutralized-{name}"));
            fs::rename(&src, &dst)?;
            moved.push((dst, src));
        }
    }
    Ok(moved)
}

fn restore_neutralized(moved: &[(PathBuf, PathBuf)]) {
    for (dst, src) in moved {
        let _ = fs::rename(dst, src);
    }
}

fn worktree_porcelain(work_tree: &Path) -> Result<String, Error> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(work_tree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| Error::Msg(format!("git status failed: {e}")))?;
    if !out.status.success() {
        // Unit tests may use a non-git directory; treat as clean.
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
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
    use std::process::Command as StdCommand;

    fn install_fake(bin_dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(bin_dir).unwrap();
        let path = bin_dir.join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn git_init_wt(wt: &Path) {
        fs::create_dir_all(wt).unwrap();
        assert!(
            StdCommand::new("git")
                .args(["init"])
                .current_dir(wt)
                .status()
                .unwrap()
                .success()
        );
        let _ = StdCommand::new("git")
            .args(["config", "user.email", "t@example.com"])
            .current_dir(wt)
            .status();
        let _ = StdCommand::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(wt)
            .status();
        fs::write(wt.join("README"), "a\n").unwrap();
        let _ = StdCommand::new("git")
            .args(["add", "README"])
            .current_dir(wt)
            .status();
        let _ = StdCommand::new("git")
            .args(["commit", "-m", "c1"])
            .current_dir(wt)
            .status();
    }

    #[test]
    fn parse_agent_findings_and_coverage() {
        let raw = br#"{
          "findings":[
            {"path":"a.rs","message":"null deref","severity":"high","category":"bug"}
          ],
          "coverage":[
            {"path":"a.rs","status":"pass"},
            {"path":"b.rs","status":"skip","reason":"unsupported"}
          ]
        }"#;
        let out = parse_agent_review_json(raw).unwrap();
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].id, "f0");
        assert!(out.findings[0].is_blocking());
        assert!(out.covered_files.iter().any(|p| p == "a.rs"));
        assert!(out.covered_files.iter().any(|p| p == "b.rs"));
    }

    #[test]
    fn parse_agent_files_shape() {
        let raw = br#"{"findings":[],"files":["README"]}"#;
        let out = parse_agent_review_json(raw).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.covered_files, vec!["README"]);
    }

    #[test]
    fn parse_legacy_comments_files_shape() {
        let raw = br#"{"comments":[{"path":"a.rs","content":"x","category":"bug","severity":"high"}],"files":["a.rs"]}"#;
        let out = parse_agent_review_json(raw).unwrap();
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.covered_files, vec!["a.rs"]);
    }

    #[test]
    fn unparseable_json_fails_closed() {
        let err = parse_agent_review_json(b"not-json").unwrap_err();
        assert!(matches!(err, Error::Json(_)));
    }

    #[test]
    fn coverage_miss_from_agent_json() {
        let raw = br#"{"findings":[],"files":["a.rs"]}"#;
        let out = parse_agent_review_json(raw).unwrap();
        let err = assert_coverage(&["a.rs".into(), "b.rs".into()], &out.covered_files).unwrap_err();
        assert!(matches!(err, Error::Coverage(ref p) if p == "b.rs"));
    }

    #[test]
    fn refuse_missing_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let missing = home.join("runs/r1/review/prompt.txt");
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
    fn empty_coverage_status_is_not_covered() {
        let raw = br#"{
          "findings":[],
          "coverage":[{"path":"a.rs","status":""}]
        }"#;
        let out = parse_agent_review_json(raw).unwrap();
        assert!(!out.covered_files.iter().any(|p| p == "a.rs"));
    }

    #[test]
    fn write_prompt_includes_changed_files_and_intent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("review");
        let path = write_reviewer_prompt(
            &dir,
            Some("ship the gate"),
            Some(r#"[{"path":"crates/**","instructions":"careful"}]"#),
            &["src/a.rs".into(), "src/b.rs".into()],
        )
        .unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("ship the gate"));
        assert!(body.contains("crates/**"));
        assert!(body.contains("src/a.rs"));
        assert!(body.contains("src/b.rs"));
        assert!(body.contains("session-free"));
    }

    #[test]
    fn run_agent_review_with_fake_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let review_dir = home.join("runs/r1/review");
        git_init_wt(&wt);
        let prompt = write_reviewer_prompt(&review_dir, None, None, &["README".into()]).unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "fake-agent",
            r#"#!/bin/sh
set -e
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id|--resume)
      echo "agent review must not receive session flags" >&2
      exit 1
      ;;
    --output) OUT="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' '{"findings":[],"files":["README"]}' > "$OUT"
"#,
        );
        let out = run_agent_review(&RunAgentReviewOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            porch_home: &home,
            from_sha: "aaa",
            to_sha: "bbb",
            changed_files: &["README".into()],
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_secs(5),
            plan: None,
            artifact_dir: None,
        })
        .unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.covered_files, vec!["README"]);
    }

    #[test]
    fn run_agent_review_coverage_miss_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let review_dir = home.join("runs/r1/review");
        git_init_wt(&wt);
        let prompt =
            write_reviewer_prompt(&review_dir, None, None, &["a.rs".into(), "b.rs".into()])
                .unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "fake-agent",
            r#"#!/bin/sh
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' '{"findings":[],"files":["a.rs"]}' > "$OUT"
"#,
        );
        let err = run_agent_review(&RunAgentReviewOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            porch_home: &home,
            from_sha: "aaa",
            to_sha: "bbb",
            changed_files: &["a.rs".into(), "b.rs".into()],
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_secs(5),
            plan: None,
            artifact_dir: None,
        })
        .unwrap_err();
        assert!(matches!(err, Error::Coverage(ref p) if p == "b.rs"));
    }

    #[test]
    fn run_agent_review_detects_file_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let review_dir = home.join("runs/r1/review");
        git_init_wt(&wt);
        let prompt = write_reviewer_prompt(&review_dir, None, None, &["README".into()]).unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "fake-agent-write",
            r#"#!/bin/sh
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo dirty >> README
printf '%s\n' '{"findings":[],"files":["README"]}' > "$OUT"
"#,
        );
        let err = run_agent_review(&RunAgentReviewOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            porch_home: &home,
            from_sha: "aaa",
            to_sha: "bbb",
            changed_files: &["README".into()],
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_secs(5),
            plan: None,
            artifact_dir: None,
        })
        .unwrap_err();
        assert!(
            matches!(err, Error::Msg(ref m) if m.contains("modified")),
            "{err}"
        );
    }

    #[test]
    fn run_agent_review_missing_prompt_refuses() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&home).unwrap();
        git_init_wt(&wt);
        let missing = home.join("runs/r1/review/prompt.txt");
        let err = run_agent_review(&RunAgentReviewOpts {
            work_tree: &wt,
            prompt_file: &missing,
            porch_home: &home,
            from_sha: "a",
            to_sha: "b",
            changed_files: &[],
            bin: "true",
            timeout: Duration::from_secs(1),
            plan: None,
            artifact_dir: None,
        })
        .unwrap_err();
        assert!(matches!(err, Error::PromptRefuse(_)));
    }

    #[test]
    fn timeout_kills_process_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let wt = tmp.path().join("wt");
        let review_dir = home.join("runs/r1/review");
        git_init_wt(&wt);
        let prompt = write_reviewer_prompt(&review_dir, None, None, &[]).unwrap();
        let bin = install_fake(
            &tmp.path().join("bin"),
            "fake-hang",
            "#!/bin/sh\nwhile true; do sleep 60; done\n",
        );
        let err = run_agent_review(&RunAgentReviewOpts {
            work_tree: &wt,
            prompt_file: &prompt,
            porch_home: &home,
            from_sha: "a",
            to_sha: "b",
            changed_files: &[],
            bin: bin.to_str().unwrap(),
            timeout: Duration::from_millis(200),
            plan: None,
            artifact_dir: None,
        })
        .unwrap_err();
        assert!(matches!(err, Error::Timeout(_)));
    }
}
