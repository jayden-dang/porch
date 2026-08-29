//! Execute a porch run: disposable worktree, intent, rebase, review, stubs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use porch_gate::{Db, RunExecutor, RunRow, db_path, run_worktree_dir};
use porch_git::GitDir;
use porch_review::{Finding, RunReviewOpts, review_bin, review_timeout, run_review};
use serde::Serialize;

/// Phases in locked order (D5). Certify/deliver remain stubs in M3.
const PHASES: &[&str] = &["intent", "rebase", "review", "certify", "deliver"];

/// Production executor injected into the daemon from the `porch` binary.
#[derive(Debug, Default, Clone, Copy)]
pub struct PipelineExecutor;

impl RunExecutor for PipelineExecutor {
    fn execute(&self, home: &Path, run_id: &str, cancel: &AtomicBool) {
        if let Err(e) = execute_run(home, run_id, cancel) {
            tracing::warn!(run_id, error = %e, "run failed");
        }
    }

    fn recover_stale(&self, home: &Path) -> std::result::Result<(), String> {
        recover_stale_running(home).map_err(|e| e.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(transparent)]
    Gate(#[from] porch_gate::Error),
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Review(#[from] porch_review::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
}

type Result<T> = std::result::Result<T, RunError>;

#[derive(Debug)]
enum PhaseLoop {
    Continue,
    /// Review parked; leave worktree and stop the pipeline.
    Parked,
}

fn execute_run(home: &Path, run_id: &str, cancel: &AtomicBool) -> Result<()> {
    let db = Db::open(&db_path(home))?;
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    if run.status == "cancelled" {
        return Ok(());
    }

    let repo = db
        .repo_by_id(&run.repo_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown repo {}", run.repo_id)))?;
    let bare = GitDir::new(&repo.bare_path)?;
    let wt_path = run_worktree_dir(home, &run.repo_id, run_id);

    db.set_run_status(run_id, "running", None)?;
    db.set_worktree_dir(run_id, &wt_path)?;

    if let Err(e) = porch_git::worktree_add_detach(&bare, &wt_path, &run.sha) {
        let msg = format!("worktree add: {e}");
        let _ = db.set_run_status(run_id, "failed", Some(&msg));
        remove_run_worktree(&bare, &wt_path);
        return Err(RunError::Msg(msg));
    }
    db.set_run_shas(run_id, Some(&run.sha), None)?;

    let mut skip_remaining = false;
    let outcome = (|| -> Result<PhaseLoop> {
        for phase in PHASES {
            if cancel.load(Ordering::SeqCst) {
                return Err(RunError::Msg("cancelled".into()));
            }
            if skip_remaining {
                db.insert_step_result(run_id, phase, "skipped", Some("skip remaining"))?;
                continue;
            }
            match *phase {
                "intent" => {
                    if run.intent.as_ref().is_some_and(|s| !s.trim().is_empty()) {
                        db.insert_step_result(run_id, phase, "completed", None)?;
                    } else {
                        db.insert_step_result(run_id, phase, "skipped", Some("no intent"))?;
                    }
                }
                "rebase" => match run_rebase(&db, run_id, &bare, &wt_path, &repo.default_branch) {
                    Ok(empty) => {
                        db.insert_step_result(run_id, phase, "completed", None)?;
                        if empty {
                            skip_remaining = true;
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        db.insert_step_result(run_id, phase, "failed", Some(&msg))?;
                        return Err(e);
                    }
                },
                "review" => match run_review_phase(&db, run_id, &wt_path)? {
                    ReviewPhase::Approved => {
                        db.insert_step_result(run_id, phase, "completed", None)?;
                    }
                    ReviewPhase::Parked => {
                        db.insert_step_result(run_id, phase, "parked", None)?;
                        return Ok(PhaseLoop::Parked);
                    }
                },
                "certify" | "deliver" => {
                    db.insert_step_result(run_id, phase, "completed", None)?;
                }
                _ => {}
            }
        }
        Ok(PhaseLoop::Continue)
    })();

    let cancelled = cancel.load(Ordering::SeqCst);
    match outcome {
        Ok(PhaseLoop::Parked) => {
            // Worktree kept for agent respond.
            return Ok(());
        }
        Ok(PhaseLoop::Continue) if cancelled => {
            let _ = db.set_run_status(run_id, "cancelled", Some("superseded by new push"));
        }
        Ok(PhaseLoop::Continue) => {
            let _ = db.set_run_status(run_id, "completed", None);
        }
        Err(RunError::Msg(ref m)) if m == "cancelled" || cancelled => {
            let _ = db.set_run_status(run_id, "cancelled", Some("superseded by new push"));
        }
        Err(ref e) => {
            let _ = db.set_run_status(run_id, "failed", Some(&e.to_string()));
        }
    }
    remove_run_worktree(&bare, &wt_path);
    match outcome {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

enum ReviewPhase {
    Approved,
    Parked,
}

fn run_review_phase(db: &Db, run_id: &str, wt: &Path) -> Result<ReviewPhase> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    let base = run
        .base_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("review requires base_sha".into()))?;
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;

    let range = format!("{base}..{head}");
    let changed = porch_git::diff_name_only(wt, &range)?;
    let bin = review_bin();
    let timeout = review_timeout();

    let outcome = match run_review(&RunReviewOpts {
        work_tree: wt,
        from_sha: base,
        to_sha: &head,
        changed_files: &changed,
        bin: &bin,
        timeout,
    }) {
        Ok(o) => o,
        Err(porch_review::Error::Timeout(d)) => {
            return Err(RunError::Msg(format!("review timed out after {d:?}")));
        }
        Err(e) => return Err(RunError::Review(e)),
    };

    let findings_json = serde_json::to_string(&outcome.findings)?;
    db.set_findings_json(run_id, Some(&findings_json))?;

    if outcome.has_blocking() {
        db.set_run_status(run_id, "parked", None)?;
        return Ok(ReviewPhase::Parked);
    }

    db.set_review_approved_head_sha(run_id, Some(&head))?;
    Ok(ReviewPhase::Approved)
}

fn run_rebase(
    db: &Db,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> Result<bool> {
    let refspec = format!("{default_branch}:refs/remotes/origin/{default_branch}");
    porch_git::fetch(bare, "origin", &refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{default_branch}: {e}")))?;

    let origin_ref = format!("refs/remotes/origin/{default_branch}");
    let onto = porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| RunError::Msg(format!("resolve origin/{default_branch}: {e}")))?;
    db.set_run_shas(run_id, None, Some(&onto))?;

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    if head == onto {
        db.set_run_shas(run_id, Some(&head), Some(&onto))?;
        return Ok(true);
    }

    if porch_git::is_ancestor(wt, &head, &onto)? {
        porch_git::reset_hard(wt, &onto)?;
    } else if let Err(e) = porch_git::rebase(wt, &onto) {
        let _ = porch_git::rebase_abort(wt);
        return Err(RunError::Msg(format!("rebase conflict: {e}")));
    }

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), Some(&onto))?;
    let range = format!("{onto}..{head}");
    let empty = porch_git::diff_is_empty(wt, &range)?;
    Ok(empty)
}

fn remove_run_worktree(bare: &GitDir, wt: &Path) {
    let _ = porch_git::worktree_remove_force(bare, wt);
    let _ = std::fs::remove_dir_all(wt);
}

fn recover_stale_running(home: &Path) -> Result<()> {
    let db = Db::open(&db_path(home))?;
    let stale = db.fail_stale_running("daemon restarted while run was in progress")?;
    for run in stale {
        let Some(wt) = run.worktree_dir.as_ref() else {
            continue;
        };
        let Some(repo) = db.repo_by_id(&run.repo_id)? else {
            let _ = std::fs::remove_dir_all(wt);
            continue;
        };
        let bare_path = repo.bare_path;
        if let Ok(bare) = GitDir::new(&bare_path) {
            remove_run_worktree(&bare, wt);
        } else {
            let _ = std::fs::remove_dir_all(wt);
        }
    }
    Ok(())
}

/// Test helper: path used for a run worktree.
#[must_use]
pub fn expected_worktree_path(home: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    run_worktree_dir(home, repo_id, run_id)
}

/// Human response to a parked review (M3: no fixer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentResponse {
    Approve,
    Skip,
    Abort,
}

impl AgentResponse {
    /// Parse `approve` | `skip` | `abort`.
    ///
    /// # Errors
    ///
    /// Returns an error string when the token is not a supported response.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "approve" => Ok(Self::Approve),
            "skip" => Ok(Self::Skip),
            "abort" => Ok(Self::Abort),
            other => Err(format!(
                "unknown response {other:?}; expected approve|skip|abort"
            )),
        }
    }
}

/// JSON document for `porch agent status`.
#[derive(Debug, Serialize)]
pub struct AgentStatus {
    pub run_id: String,
    pub repo_id: String,
    pub branch: String,
    pub status: String,
    pub phase: String,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub review_approved_head_sha: Option<String>,
    pub findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Exit code for agent CLI (D11): 0 ok, 1 failed/cancelled, 2 usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliResult {
    pub exit_code: i32,
    pub json: String,
}

/// Build status JSON for a parked (or specified) run.
///
/// # Errors
///
/// Returns a usage-style error when the run cannot be resolved.
#[must_use]
pub fn agent_status(home: &Path, run_id: Option<&str>, work_tree: &Path) -> AgentCliResult {
    match agent_status_inner(home, run_id, work_tree) {
        Ok(status) => AgentCliResult {
            exit_code: status_exit(&status.status),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
        },
    }
}

/// Apply `approve` | `skip` | `abort` to a parked run.
///
/// # Errors
///
/// Returns usage or failure payloads via [`AgentCliResult`].
#[must_use]
pub fn agent_respond(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
    response: AgentResponse,
) -> AgentCliResult {
    match agent_respond_inner(home, run_id, work_tree, response) {
        Ok(status) => AgentCliResult {
            exit_code: status_exit(&status.status),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
        },
    }
}

enum UsageOrFail {
    Usage(String),
    Fail(String),
}

fn status_exit(status: &str) -> i32 {
    match status {
        "failed" | "cancelled" => 1,
        _ => 0,
    }
}

fn agent_status_inner(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
) -> std::result::Result<AgentStatus, UsageOrFail> {
    let db = Db::open(&db_path(home)).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let run = resolve_run(&db, run_id, work_tree)?;
    Ok(status_from_run(&run))
}

fn agent_respond_inner(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
    response: AgentResponse,
) -> std::result::Result<AgentStatus, UsageOrFail> {
    let db = Db::open(&db_path(home)).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let run = resolve_run(&db, run_id, work_tree)?;
    if run.status != "parked" {
        return Err(UsageOrFail::Fail(format!(
            "run {} is {}, not parked",
            run.id, run.status
        )));
    }

    let repo = db
        .repo_by_id(&run.repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail(format!("unknown repo {}", run.repo_id)))?;
    let bare = GitDir::new(&repo.bare_path).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let wt = run
        .worktree_dir
        .clone()
        .ok_or_else(|| UsageOrFail::Fail("parked run has no worktree_dir".into()))?;

    match response {
        AgentResponse::Approve => {
            let head = porch_git::rev_parse_c(&wt, "HEAD")
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            db.set_review_approved_head_sha(&run.id, Some(&head))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            db.insert_step_result(&run.id, "review", "completed", Some("approved"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            for phase in ["certify", "deliver"] {
                db.insert_step_result(&run.id, phase, "completed", None)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
            db.set_run_status(&run.id, "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            remove_run_worktree(&bare, &wt);
        }
        AgentResponse::Skip => {
            // Skip does not write review_approved_head_sha.
            db.insert_step_result(&run.id, "review", "skipped", Some("agent skip"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            for phase in ["certify", "deliver"] {
                db.insert_step_result(&run.id, phase, "skipped", Some("skip remaining"))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
            db.set_run_status(&run.id, "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            remove_run_worktree(&bare, &wt);
        }
        AgentResponse::Abort => {
            db.set_run_status(&run.id, "cancelled", Some("agent abort"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            remove_run_worktree(&bare, &wt);
        }
    }

    let run = db
        .run_by_id(&run.id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
    Ok(status_from_run(&run))
}

fn resolve_run(
    db: &Db,
    run_id: Option<&str>,
    work_tree: &Path,
) -> std::result::Result<RunRow, UsageOrFail> {
    if let Some(id) = run_id {
        return db
            .run_by_id(id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?
            .ok_or_else(|| UsageOrFail::Usage(format!("unknown run {id}")));
    }
    let abs = work_tree
        .canonicalize()
        .unwrap_or_else(|_| work_tree.to_path_buf());
    let repo_id = porch_gate::repo_id_for(&abs);
    db.latest_parked_for_repo(&repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Usage("no parked run for this repo".into()))
}

fn status_from_run(run: &RunRow) -> AgentStatus {
    let findings = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<Finding>>(s).ok())
        .unwrap_or_default();
    let phase = match run.status.as_str() {
        "parked" => "review",
        "completed" | "failed" | "cancelled" => "done",
        "running" | "pending" => "pipeline",
        other => other,
    };
    AgentStatus {
        run_id: run.id.clone(),
        repo_id: run.repo_id.clone(),
        branch: run.branch.clone(),
        status: run.status.clone(),
        phase: phase.into(),
        head_sha: run.head_sha.clone(),
        base_sha: run.base_sha.clone(),
        review_approved_head_sha: run.review_approved_head_sha.clone(),
        findings,
        error: run.error.clone(),
    }
}
