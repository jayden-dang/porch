//! External review CLI / coding-agent review adapter: spawn, parse JSON, map to findings.
//!
//! Also owns operator `$PORCH_HOME/config.yaml` load + review-engine setup
//! (wrapper write/verify). Gate must not depend on this crate.

mod agent_review;
mod coverage_state;
mod engine;
mod home_config;
mod identity;
mod pathutil;
mod plan;
mod reconcile;
mod setup;

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use agent_review::{
    REVIEW_AGENT_BIN_ENV, REVIEW_ARTIFACT_REL, REVIEWER_PROMPT, RunAgentReviewOpts,
    agent_review_bin, assert_prompt_under_home, build_reviewer_prompt, parse_agent_review_json,
    review_uses_agent, run_agent_review, write_reviewer_prompt,
};
pub use coverage_state::{
    CoverageEntry, CoverageState, PathSignal, ProducerOutput, StatusRow, derive_states,
};
pub use engine::{
    AGENT_DETECT_BINS, DetectedEngine, EngineKind, agent_detect_bins, known_engines, wrapper_script,
};
pub use home_config::{
    CONFIG_FILE, FixerConfig, GithubConfig, HomeConfig, ReviewConfig, ToolsConfig, config_path,
    load_home_config, write_home_config,
};
pub use identity::{
    Anchor, AnchorContext, AnchorKind, CandidateKey, Confidence, ConfidenceKind, CriterionMapping,
    FINGERPRINT_VERSION, Provenance, apply_contract, derive, enrich_from_comment, path_key,
};
pub use pathutil::{is_executable, resolve_bin, which};
pub use plan::{
    AdapterKind, ArtifactStamp, BackendObservation, DeclaredEngineKind, InvocationPlan,
    InvocationRecord, ObservedVersionIdentity, PrepareOpts, PreparedContextElement,
    PreparedInvocation, ProducerDescriptor, ReportedVersion, SelectionSource, WrapperObservation,
    check_artifacts_stable, composite_artifact_identity, prepare,
};
pub use reconcile::{
    Assignment, CurrentFinding, History, PriorInstance, Proposal, RenameEvidence, SourceRange,
    mint_fingerprint, reconcile,
};
pub use setup::{
    SetupResult, WRAPPER_REL, default_engine, detect_engines, detect_optional_tools,
    review_setup_ok, setup_apply, setup_verify, setup_yes, verify_setup, wrapper_path,
    write_wrapper,
};

/// Env var naming the review CLI binary (PATH entry or absolute path).
pub const REVIEW_BIN_ENV: &str = "PORCH_REVIEW_BIN";

/// Env var for review subprocess timeout in seconds (default 600).
pub const REVIEW_TIMEOUT_ENV: &str = "PORCH_REVIEW_TIMEOUT_SECS";

const DEFAULT_BIN: &str = "review";
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Porch finding severity after mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// Suggested disposition (M3 does not run a fixer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    AutoFix,
    AskUser,
    NoOp,
}

/// One mapped finding from a review comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable id assigned at map time (`f0`, `f1`, …). Empty when deserializing old JSON.
    #[serde(default)]
    pub id: String,
    pub path: String,
    pub message: String,
    pub severity: Severity,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// Porch-normalized criterion (optional on legacy rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<identity::Provenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<identity::Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_value: Option<String>,
}

impl Default for Finding {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            message: String::new(),
            severity: Severity::Info,
            action: Action::NoOp,
            category: None,
            start_line: None,
            end_line: None,
            criterion_id: None,
            evidence: None,
            consequence: None,
            provenance: None,
            confidence: None,
            anchor_kind: None,
            anchor_value: None,
        }
    }
}

impl Finding {
    /// Whether this finding parks the run.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        matches!(self.severity, Severity::Error | Severity::Warning)
            || self.action == Action::AskUser
    }
}

/// Raw comment object from the review CLI JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub existing_code: Option<String>,
    #[serde(default)]
    pub suggestion_code: Option<String>,
    #[serde(default)]
    pub start_line: Option<u32>,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    /// Producer-local rule / finding key (provenance only).
    #[serde(default)]
    pub rule_id: Option<String>,
    /// Optional producer-supplied confidence.
    #[serde(default)]
    pub confidence: Option<identity::Confidence>,
}

/// OCR / generic file-group entry (optional coverage source).
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewGroup {
    #[serde(default)]
    pub files: Vec<String>,
}

/// One OCR manifest coverage item.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CoverageItem {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub authority: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

/// OCR coverage sets (optional).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReviewCoverage {
    #[serde(default)]
    pub selected: Vec<CoverageItem>,
    #[serde(default)]
    pub completed: Vec<CoverageItem>,
    #[serde(default)]
    pub reused: Vec<CoverageItem>,
    #[serde(default)]
    pub failed: Vec<CoverageItem>,
    #[serde(default)]
    pub waived: Vec<CoverageItem>,
}

/// OCR run manifest subset used for coverage derivation.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewManifest {
    #[serde(default)]
    pub coverage: ReviewCoverage,
}

/// Top-level review CLI JSON (`comments` + coverage `files`).
///
/// OCR often omits top-level `files`; porch then derives coverage from
/// `groups` / `manifest.coverage` / comment paths.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewJson {
    #[serde(default)]
    pub comments: Vec<ReviewComment>,
    /// Paths the engine claims to have covered (or explicitly skipped).
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub groups: Vec<ReviewGroup>,
    #[serde(default)]
    pub manifest: Option<ReviewManifest>,
}

/// Successful parse + map of a review CLI run.
#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    pub findings: Vec<Finding>,
    pub covered_files: Vec<String>,
}

impl ReviewOutcome {
    /// True when any finding should park.
    #[must_use]
    pub fn has_blocking(&self) -> bool {
        self.findings.iter().any(Finding::is_blocking)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "review CLI not found ({bin}): {source}\nset PORCH_REVIEW_BIN or install `review` on PATH; see `porch doctor`"
    )]
    BinNotFound {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("review CLI timed out after {0:?}")]
    Timeout(Duration),
    #[error("review CLI exited {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("review JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("coverage: changed file `{0}` missing from review manifest without skip")]
    Coverage(String),
    #[error("prompt file missing or not under PORCH_HOME: {0}")]
    PromptRefuse(String),
    #[error("producer artifact changed after plan resolution")]
    ProducerArtifactChanged,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

/// Resolve the review binary: `PORCH_REVIEW_BIN` > `$PORCH_HOME/config.yaml` wrapper > `review`.
#[must_use]
pub fn review_bin() -> String {
    if let Ok(v) = std::env::var(REVIEW_BIN_ENV) {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Some(home) = porch_home_dir() {
        if let Ok(Some(cfg)) = load_home_config(&home) {
            if let Some(w) = cfg.review.wrapper.as_deref() {
                if !w.trim().is_empty() {
                    return w.to_string();
                }
            }
        }
    }
    DEFAULT_BIN.to_string()
}

pub(crate) fn porch_home_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("PORCH_HOME") {
        return Some(PathBuf::from(v));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".porch"))
}

/// Resolve timeout from `PORCH_REVIEW_TIMEOUT_SECS`.
#[must_use]
pub fn review_timeout() -> Duration {
    let secs = std::env::var(REVIEW_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs.max(1))
}

/// Map one raw comment to a porch finding (conservative M3 rules).
#[must_use]
pub fn map_comment(comment: &ReviewComment) -> Option<Finding> {
    let category = comment
        .category
        .as_deref()
        .unwrap_or("other")
        .to_ascii_lowercase();
    let sev_raw = comment
        .severity
        .as_deref()
        .unwrap_or("medium")
        .to_ascii_lowercase();

    if matches!(category.as_str(), "style" | "documentation") {
        return Some(Finding {
            path: comment.path.clone(),
            message: comment.content.clone(),
            severity: Severity::Info,
            action: Action::NoOp,
            category: Some(category),
            start_line: comment.start_line,
            end_line: comment.end_line,
            ..Finding::default()
        });
    }

    let text = comment.content.to_ascii_lowercase();
    let extends_scope = text.contains("schema")
        || text.contains("on-chain")
        || text.contains("onchain")
        || text.contains("new subsystem");

    let (severity, action) = if extends_scope {
        (Severity::Warning, Action::AskUser)
    } else {
        match sev_raw.as_str() {
            "critical" => (Severity::Error, Action::AskUser),
            "low" => {
                if matches!(category.as_str(), "bug" | "security" | "performance") {
                    (Severity::Warning, Action::AskUser)
                } else {
                    (Severity::Info, Action::NoOp)
                }
            }
            // high, medium, and unknown → blocking warning
            _ => (Severity::Warning, Action::AskUser),
        }
    };

    Some(Finding {
        path: comment.path.clone(),
        message: comment.content.clone(),
        severity,
        action,
        category: Some(category),
        start_line: comment.start_line,
        end_line: comment.end_line,
        ..Finding::default()
    })
}

/// Parse review JSON bytes into an outcome (no coverage check yet).
///
/// Finding ids `f0`, `f1`, … are assigned in comment order after mapping.
/// When top-level `files` is empty (typical OCR), coverage is derived from
/// `groups`, `manifest.coverage`, and comment paths.
///
/// # Errors
///
/// Returns [`Error::Json`] when the payload is not valid review JSON.
pub fn parse_review_json(bytes: &[u8]) -> Result<ReviewOutcome, Error> {
    let parsed: ReviewJson = serde_json::from_slice(bytes)?;
    let mapping = CriterionMapping::builtin();
    let mut findings: Vec<Finding> = parsed
        .comments
        .iter()
        .filter_map(|c| {
            // Quality (and other rule-keyed) producers are deterministic: never keep model confidence.
            let deterministic = c.rule_id.as_deref().is_some_and(|s| !s.trim().is_empty());
            enrich_from_comment(c, &mapping, &AnchorContext::default(), deterministic)
        })
        .collect();
    for (i, f) in findings.iter_mut().enumerate() {
        f.id = format!("f{i}");
    }
    let covered_files = derive_covered_files(&parsed);
    Ok(ReviewOutcome {
        findings,
        covered_files,
    })
}

/// Build the coverage path list porch asserts against.
#[must_use]
pub fn derive_covered_files(parsed: &ReviewJson) -> Vec<String> {
    if !parsed.files.is_empty() {
        return parsed.files.clone();
    }
    let mut set = BTreeSet::new();
    for c in &parsed.comments {
        if !c.path.is_empty() {
            set.insert(c.path.clone());
        }
    }
    for g in &parsed.groups {
        for f in &g.files {
            if !f.is_empty() {
                set.insert(f.clone());
            }
        }
    }
    if let Some(m) = &parsed.manifest {
        for item in m
            .coverage
            .selected
            .iter()
            .chain(m.coverage.completed.iter())
            .chain(m.coverage.reused.iter())
            .chain(m.coverage.failed.iter())
            .chain(m.coverage.waived.iter())
        {
            if !item.path.is_empty() {
                set.insert(item.path.clone());
            }
        }
    }
    set.into_iter().collect()
}

/// Fail if a changed path is absent from the coverage manifest.
///
/// # Errors
///
/// Returns [`Error::Coverage`] for the first missing path.
pub fn assert_coverage(changed: &[String], covered: &[String]) -> Result<(), Error> {
    for path in changed {
        if !covered.iter().any(|c| c == path) {
            return Err(Error::Coverage(path.clone()));
        }
    }
    Ok(())
}

/// Derive structured coverage states for `changed`, then fail closed on shortfall.
///
/// # Errors
///
/// Returns [`Error::Coverage`] when a changed path lacks a producer claim, or
/// [`Error::Msg`] when a failed/waived/completed signal omits required fields.
pub fn assert_coverage_states(
    changed: &[String],
    output: &ProducerOutput,
) -> Result<Vec<CoverageEntry>, Error> {
    derive_states(changed, output)
}

/// Options for one range-review invocation.
#[derive(Debug, Clone)]
pub struct RunReviewOpts<'a> {
    pub work_tree: &'a Path,
    pub from_sha: &'a str,
    pub to_sha: &'a str,
    pub changed_files: &'a [String],
    pub bin: &'a str,
    pub timeout: Duration,
    /// `$PORCH_HOME` — required for agent-engine dispatch and prompt artifacts.
    pub porch_home: Option<&'a Path>,
    /// Run id for `$PORCH_HOME/runs/<id>/review/` artifacts (agent engine).
    pub run_id: Option<&'a str>,
    /// Authoritative intent text, if any (agent prompt).
    pub intent: Option<&'a str>,
    /// When set, spawn uses this plan's absolute target without re-resolving.
    pub plan: Option<&'a plan::InvocationPlan>,
}

/// Spawn review: agent path when configured (`PORCH_REVIEW_BIN` unset); else CLI.
///
/// # Errors
///
/// Returns spawn, timeout, exit, JSON, coverage, prompt-refuse, or I/O errors.
pub fn run_review(opts: &RunReviewOpts<'_>) -> Result<ReviewOutcome, Error> {
    let use_agent = match opts.plan.map(|p| p.descriptor.adapter_kind) {
        Some(plan::AdapterKind::NativeAgent) => true,
        Some(plan::AdapterKind::PorchJsonCli) => false,
        None => review_uses_agent(opts.porch_home),
    };
    let outcome = if use_agent {
        let home = opts.porch_home.ok_or_else(|| {
            Error::Msg("agent review requires porch_home for prompt artifacts".into())
        })?;
        run_review_via_agent(opts, home)?
    } else {
        run_review_cli(opts)?
    };
    if let Some(plan) = opts.plan {
        plan::check_artifacts_stable(plan)?;
    }
    Ok(outcome)
}

fn run_review_via_agent(
    opts: &RunReviewOpts<'_>,
    porch_home: &Path,
) -> Result<ReviewOutcome, Error> {
    let run_id = opts.run_id.ok_or_else(|| {
        Error::Msg("agent review requires run_id for prompt artifacts under PORCH_HOME".into())
    })?;
    let review_dir = porch_home
        .join("runs")
        .join(run_id)
        .join(REVIEW_ARTIFACT_REL);
    let path_instructions = {
        let p = porch_home
            .join("runs")
            .join(run_id)
            .join("path_instructions.json");
        if p.is_file() {
            Some(fs::read_to_string(&p)?)
        } else {
            None
        }
    };
    let prompt_file = write_reviewer_prompt(
        &review_dir,
        opts.intent,
        path_instructions.as_deref(),
        opts.changed_files,
    )?;
    let planned = opts
        .plan
        .map(|p| p.spawned_target_absolute.display().to_string());
    let resolved;
    let bin = if let Some(ref absolute) = planned {
        absolute.as_str()
    } else {
        resolved = agent_review_bin(porch_home)?;
        resolved.as_str()
    };
    run_agent_review(&RunAgentReviewOpts {
        work_tree: opts.work_tree,
        prompt_file: &prompt_file,
        porch_home,
        from_sha: opts.from_sha,
        to_sha: opts.to_sha,
        changed_files: opts.changed_files,
        bin,
        timeout: opts.timeout,
        plan: opts.plan,
    })
}

/// Spawn the review CLI in `work_tree`, parse JSON, enforce coverage.
fn run_review_cli(opts: &RunReviewOpts<'_>) -> Result<ReviewOutcome, Error> {
    let out_dir = opts.work_tree.join(".porch-review");
    fs::create_dir_all(&out_dir)?;
    let out_file = out_dir.join("result.json");
    if out_file.exists() {
        let _ = fs::remove_file(&out_file);
    }
    let out_s = out_file
        .to_str()
        .ok_or_else(|| Error::Msg(format!("non-utf8 output path {}", out_file.display())))?
        .to_string();

    let bin = opts.plan.map_or_else(
        || Path::new(opts.bin),
        |p| p.spawned_target_absolute.as_path(),
    );
    let bin_label = bin.display().to_string();

    let mut cmd = Command::new(bin);
    cmd.current_dir(opts.work_tree);
    cmd.args([
        "--from",
        opts.from_sha,
        "--to",
        opts.to_sha,
        "--format",
        "json",
        "--output",
        &out_s,
    ]);
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

    let deadline = Instant::now() + opts.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child_group(child.id());
                    let _ = child.wait();
                    return Err(Error::Timeout(opts.timeout));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(Error::Io(e)),
        }
    };

    let stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
        .unwrap_or_default();

    if !status.success() {
        return Err(Error::Exit {
            status: status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }

    let bytes = fs::read(&out_file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Msg(format!(
                "review CLI produced no output file at {}",
                out_file.display()
            ))
        } else {
            Error::Io(e)
        }
    })?;
    let outcome = parse_review_json(&bytes)?;
    assert_coverage(opts.changed_files, &outcome.covered_files)?;
    Ok(outcome)
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

    #[test]
    fn high_bug_is_blocking() {
        let c = ReviewComment {
            path: "src/a.rs".into(),
            content: "null deref".into(),
            existing_code: None,
            suggestion_code: None,
            start_line: Some(1),
            end_line: Some(2),
            category: Some("bug".into()),
            severity: Some("high".into()),
            rule_id: None,
            confidence: None,
        };
        let f = map_comment(&c).unwrap();
        assert!(f.is_blocking());
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn style_is_info_non_blocking() {
        let c = ReviewComment {
            path: "src/a.rs".into(),
            content: "prefer rename".into(),
            existing_code: None,
            suggestion_code: None,
            start_line: None,
            end_line: None,
            category: Some("style".into()),
            severity: Some("medium".into()),
            rule_id: None,
            confidence: None,
        };
        let f = map_comment(&c).unwrap();
        assert!(!f.is_blocking());
        assert_eq!(f.severity, Severity::Info);
    }

    #[test]
    fn schema_mention_is_ask_user() {
        let c = ReviewComment {
            path: "db.rs".into(),
            content: "needs a schema migration".into(),
            existing_code: None,
            suggestion_code: None,
            start_line: None,
            end_line: None,
            category: Some("maintainability".into()),
            severity: Some("low".into()),
            rule_id: None,
            confidence: None,
        };
        let f = map_comment(&c).unwrap();
        assert!(f.is_blocking());
        assert_eq!(f.action, Action::AskUser);
    }

    #[test]
    fn coverage_requires_all_changed_files() {
        let err = assert_coverage(&["a.rs".into(), "b.rs".into()], &["a.rs".into()]).unwrap_err();
        assert!(matches!(err, Error::Coverage(ref p) if p == "b.rs"));
    }

    #[test]
    fn bin_not_found_mentions_env_and_doctor() {
        let err = Error::BinNotFound {
            bin: "review".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        };
        let s = err.to_string();
        assert!(s.contains("PORCH_REVIEW_BIN"), "{s}");
        assert!(s.contains("porch doctor"), "{s}");
    }

    #[test]
    fn parse_empty_comments() {
        let raw = br#"{"comments":[],"files":["README"]}"#;
        let out = parse_review_json(raw).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.covered_files, vec!["README"]);
        assert!(!out.has_blocking());
    }

    #[test]
    fn parse_assigns_finding_ids_in_order() {
        let raw = br#"{"comments":[
            {"path":"a.rs","content":"bug a","category":"bug","severity":"high"},
            {"path":"b.rs","content":"bug b","category":"bug","severity":"high"}
        ],"files":["a.rs","b.rs"]}"#;
        let out = parse_review_json(raw).unwrap();
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.findings[0].id, "f0");
        assert_eq!(out.findings[1].id, "f1");
    }

    #[test]
    fn finding_id_defaults_empty_on_old_json() {
        let raw = br#"{"path":"a.rs","message":"x","severity":"warning","action":"ask-user"}"#;
        let f: Finding = serde_json::from_slice(raw).unwrap();
        assert!(f.id.is_empty());
    }

    #[test]
    fn ocr_fixture_parses_and_derives_files() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/review/ocr-sample.json");
        let bytes = fs::read(&path).expect("ocr-sample.json");
        let out = parse_review_json(&bytes).expect("parse ocr fixture");
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].id, "f0");
        assert!(out.findings[0].is_blocking());
        assert!(
            out.covered_files.iter().any(|f| f == "src/lib.rs"),
            "covered={:?}",
            out.covered_files
        );
        assert!(
            out.covered_files.iter().any(|f| f == "src/util.rs"),
            "covered={:?}",
            out.covered_files
        );
    }

    #[test]
    fn wrapper_script_ocr_prefixes_review() {
        let body = wrapper_script(EngineKind::Ocr, Path::new("/opt/homebrew/bin/ocr"));
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains("exec /opt/homebrew/bin/ocr review \"$@\""));
    }
}
