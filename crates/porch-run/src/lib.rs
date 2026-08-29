//! Execute a porch run: disposable worktree, intent, rebase, review, stubs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Serialize fetch + tip resolve across concurrent rebases in this process.
static FETCH_RESOLVE_LOCK: Mutex<()> = Mutex::new(());

use porch_agent::{RunFixerOpts, fixer_bin, fixer_timeout, run_fixer, write_fixer_inputs};
use porch_gate::{Db, RunExecutor, RunRow, db_path, run_fixer_dir, run_worktree_dir};
use porch_git::GitDir;
use porch_review::{Finding, RunReviewOpts, review_bin, review_timeout, run_review};
use serde::Serialize;

/// Phases in locked order (D5). Certify/deliver remain stubs in M4.
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
    Agent(#[from] porch_agent::Error),
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
                "review" => match run_review_phase(&db, run_id, &wt_path, false)? {
                    ReviewPhase::Approved => {
                        db.insert_step_result(run_id, phase, "completed", None)?;
                    }
                    ReviewPhase::Parked => {
                        db.insert_step_result(run_id, phase, "parked", None)?;
                        return Ok(PhaseLoop::Parked);
                    }
                },
                "certify" | "deliver" => {
                    assert_head_continuity(&db, run_id, &wt_path)?;
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

fn run_review_phase(db: &Db, run_id: &str, wt: &Path, after_fix: bool) -> Result<ReviewPhase> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    let base = run
        .base_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("review requires base_sha".into()))?;
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;

    let from_sha = resolve_review_from(db, wt, &run, base, &head, after_fix)?;
    let range = format!("{from_sha}..{head}");
    let changed = porch_git::diff_name_only(wt, &range)?;
    let bin = review_bin();
    let timeout = review_timeout();

    let outcome = match run_review(&RunReviewOpts {
        work_tree: wt,
        from_sha: &from_sha,
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
    clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)?;
    Ok(ReviewPhase::Approved)
}

fn resolve_review_from(
    db: &Db,
    wt: &Path,
    run: &RunRow,
    base: &str,
    head: &str,
    after_fix: bool,
) -> Result<String> {
    let Some(rng) = db.get_uncertified_pipeline_range(&run.repo_id, &run.branch)? else {
        return Ok(base.to_string());
    };

    if after_fix {
        if porch_git::is_ancestor(wt, &rng.from_sha, head)? {
            return Ok(rng.from_sha);
        }
        return Ok(base.to_string());
    }

    // Initial review: bind when range tip is HEAD or an ancestor of HEAD,
    // and range from is an ancestor of HEAD.
    let tip_ok = rng.to_sha == head || porch_git::is_ancestor(wt, &rng.to_sha, head)?;
    if tip_ok && porch_git::is_ancestor(wt, &rng.from_sha, head)? {
        return Ok(rng.from_sha);
    }
    Ok(base.to_string())
}

fn clear_uncertified_if_certified(
    db: &Db,
    wt: &Path,
    repo_id: &str,
    branch: &str,
    approved_head: &str,
) -> Result<()> {
    let Some(rng) = db.get_uncertified_pipeline_range(repo_id, branch)? else {
        return Ok(());
    };
    let certified =
        rng.to_sha == approved_head || porch_git::is_ancestor(wt, &rng.to_sha, approved_head)?;
    if certified {
        db.delete_uncertified_pipeline_range(repo_id, branch)?;
    }
    Ok(())
}

fn assert_head_continuity(db: &Db, run_id: &str, wt: &Path) -> Result<()> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    let approved = run
        .review_approved_head_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("HEAD continuity: review_approved_head_sha missing".into()))?;
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    if head == approved {
        return Ok(());
    }
    if porch_git::is_ancestor(wt, approved, &head)? {
        return Ok(());
    }
    Err(RunError::Msg(format!(
        "HEAD continuity: live HEAD {head} is not a descendant of approved {approved}"
    )))
}

fn persist_uncertified_after_fix(
    db: &Db,
    wt: &Path,
    run: &RunRow,
    pre_fix_head: &str,
    new_head: &str,
) -> Result<()> {
    if pre_fix_head == new_head {
        return Ok(());
    }
    let mut from_sha = pre_fix_head.to_string();
    if let Some(existing) = db.get_uncertified_pipeline_range(&run.repo_id, &run.branch)? {
        if porch_git::is_ancestor(wt, &existing.to_sha, pre_fix_head)? {
            from_sha = existing.from_sha;
        }
    }
    db.upsert_uncertified_pipeline_range(&run.repo_id, &run.branch, &from_sha, new_head, &run.id)?;
    Ok(())
}

fn run_rebase(
    db: &Db,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> Result<bool> {
    let onto = {
        let _guard = FETCH_RESOLVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refspec = format!("refs/heads/{default_branch}:refs/remotes/origin/{default_branch}");
        porch_git::fetch(bare, "origin", &refspec)
            .map_err(|e| RunError::Msg(format!("fetch origin/{default_branch}: {e}")))?;

        let origin_ref = format!("refs/remotes/origin/{default_branch}");
        porch_git::rev_parse(bare, &origin_ref)
            .map_err(|e| RunError::Msg(format!("resolve origin/{default_branch}: {e}")))?
    };
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

/// Human response to a parked review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentResponse {
    Approve,
    Skip,
    Abort,
    /// Spawn fixer for selected findings, then session-free rereview.
    Fix {
        /// Explicit finding ids; `None` means all blocking findings.
        finding_ids: Option<Vec<String>>,
        /// Standing consent: one fix round then approve remaining.
        yes: bool,
    },
}

impl AgentResponse {
    /// Parse `approve` | `skip` | `abort` | `fix` (without findings/`--yes`).
    ///
    /// # Errors
    ///
    /// Returns an error string when the token is not a supported response.
    pub fn parse_verb(s: &str) -> std::result::Result<Self, String> {
        match s {
            "approve" => Ok(Self::Approve),
            "skip" => Ok(Self::Skip),
            "abort" => Ok(Self::Abort),
            "fix" => Ok(Self::Fix {
                finding_ids: None,
                yes: false,
            }),
            other => Err(format!(
                "unknown response {other:?}; expected approve|skip|abort|fix"
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

/// Apply `approve` | `skip` | `abort` | `fix` to a parked run.
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
            clear_uncertified_if_certified(&db, &wt, &run.repo_id, &run.branch, &head)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            db.insert_step_result(&run.id, "review", "completed", Some("approved"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            for phase in ["certify", "deliver"] {
                assert_head_continuity(&db, &run.id, &wt)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
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
        AgentResponse::Fix { finding_ids, yes } => {
            respond_fix(&db, home, &run, &bare, &wt, finding_ids.as_ref(), yes)?;
        }
    }

    let run = db
        .run_by_id(&run.id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
    Ok(status_from_run(&run))
}

fn respond_fix(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    finding_ids: Option<&Vec<String>>,
    yes: bool,
) -> std::result::Result<(), UsageOrFail> {
    if !wt.exists() {
        return Err(UsageOrFail::Fail("parked run worktree missing".into()));
    }

    let all_findings: Vec<Finding> = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let selected = select_findings(&all_findings, finding_ids)?;
    if selected.is_empty() {
        return Err(UsageOrFail::Usage(
            "no findings selected; pass --findings or ensure blocking findings exist".into(),
        ));
    }

    let Some(pre_fix_head) = spawn_and_wait_fixer(db, home, run, bare, wt, &selected)? else {
        // Fixer failed closed; run already marked failed.
        return Ok(());
    };
    let new_head =
        porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    db.set_run_shas(&run.id, Some(&new_head), None)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    persist_uncertified_after_fix(db, wt, run, &pre_fix_head, &new_head)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    finish_rereview(db, run, bare, wt, yes)
}

/// Returns `Ok(None)` when the fixer failed closed (run already marked failed).
fn spawn_and_wait_fixer(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    selected: &[Finding],
) -> std::result::Result<Option<String>, UsageOrFail> {
    let findings_json =
        serde_json::to_string_pretty(selected).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let fixer_dir = run_fixer_dir(home, &run.id);
    let (prompt_file, findings_file) = write_fixer_inputs(&fixer_dir, &findings_json)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    db.set_run_status(&run.id, "running", None)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let pre_fix_head =
        porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let bin = match fixer_bin() {
        Ok(b) => b,
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &pre_fix_head, &e.to_string())?;
            return Ok(None);
        }
    };

    match run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    }) {
        Ok(outcome) => {
            if let Some(sid) = outcome.session_id.as_deref() {
                db.set_fixer_session_id(&run.id, Some(sid))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
            Ok(Some(pre_fix_head))
        }
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &pre_fix_head, &e.to_string())?;
            Ok(None)
        }
    }
}

fn fail_fix_run(
    db: &Db,
    bare: &GitDir,
    wt: &Path,
    run: &RunRow,
    pre_fix_head: &str,
    msg: &str,
) -> std::result::Result<(), UsageOrFail> {
    if let Ok(new_head) = porch_git::rev_parse_c(wt, "HEAD") {
        let _ = persist_uncertified_after_fix(db, wt, run, pre_fix_head, &new_head);
    }
    db.set_run_status(&run.id, "failed", Some(msg))
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    remove_run_worktree(bare, wt);
    Ok(())
}

fn finish_rereview(
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    yes: bool,
) -> std::result::Result<(), UsageOrFail> {
    // Session-free rereview (never pass fixer session).
    match run_review_phase(db, &run.id, wt, true) {
        Ok(ReviewPhase::Approved) => {
            complete_after_review(db, bare, wt, run, None)?;
        }
        Ok(ReviewPhase::Parked) => {
            if yes {
                let head = porch_git::rev_parse_c(wt, "HEAD")
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                db.set_review_approved_head_sha(&run.id, Some(&head))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                complete_after_review(db, bare, wt, run, Some("approved remaining after --yes"))?;
            } else {
                db.insert_step_result(&run.id, "review", "parked", Some("fix_review"))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
        }
        Err(e) => {
            let msg = e.to_string();
            db.set_run_status(&run.id, "failed", Some(&msg))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            remove_run_worktree(bare, wt);
        }
    }
    Ok(())
}

fn complete_after_review(
    db: &Db,
    bare: &GitDir,
    wt: &Path,
    run: &RunRow,
    review_note: Option<&str>,
) -> std::result::Result<(), UsageOrFail> {
    db.insert_step_result(&run.id, "review", "completed", review_note)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    for phase in ["certify", "deliver"] {
        assert_head_continuity(db, &run.id, wt).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        db.insert_step_result(&run.id, phase, "completed", None)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    }
    db.set_run_status(&run.id, "completed", None)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    remove_run_worktree(bare, wt);
    Ok(())
}

fn select_findings(
    all: &[Finding],
    finding_ids: Option<&Vec<String>>,
) -> std::result::Result<Vec<Finding>, UsageOrFail> {
    match finding_ids {
        None => Ok(all.iter().filter(|f| f.is_blocking()).cloned().collect()),
        Some(ids) => {
            let mut selected = Vec::new();
            for id in ids {
                let Some(f) = all.iter().find(|f| f.id == *id) else {
                    return Err(UsageOrFail::Usage(format!("unknown finding id {id}")));
                };
                selected.push(f.clone());
            }
            Ok(selected)
        }
    }
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

#[cfg(test)]
mod continuity_tests {
    use super::*;
    use porch_git::init_bare;
    use tempfile::TempDir;

    fn git(work: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn head_continuity_fails_if_approved_sha_missing_on_certify() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let db = Db::open(&home.join("state.sqlite")).unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init"]);
        git(&work, &["config", "user.email", "porch@example.com"]);
        git(&work, &["config", "user.name", "Porch"]);
        std::fs::write(work.join("README"), "x\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "c"]);

        db.upsert_repo("r1", &work, &work, "main").unwrap();
        let run = db.insert_run("r1", "feat", "deadbeef", None, None).unwrap();
        let err = assert_head_continuity(&db, &run.id, &work).unwrap_err();
        assert!(
            err.to_string().contains("review_approved_head_sha missing"),
            "{err}"
        );
    }

    #[test]
    fn skip_review_empty_diff_does_not_require_approved_sha() {
        // Documented contract: empty-diff skip_remaining never calls assert_head_continuity.
        // Smoke: execute path with empty diff is covered by m2_run integration.
        let _ = init_bare;
    }
}
