//! Execute a porch run: disposable worktree, intent, rebase, review, certify, deliver.

mod agent_run;
mod certify;
mod config;
mod deliver;
mod sync;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Serialize fetch + tip resolve across concurrent rebases in this process.
static FETCH_RESOLVE_LOCK: Mutex<()> = Mutex::new(());

use porch_agent::{
    RunFixerOpts, fixer_bin, fixer_timeout, run_fixer, write_deliver_repair_inputs,
    write_fixer_inputs, write_rebase_fix_inputs,
};
use porch_gate::{
    Db, RunExecutor, RunRow, db_path, event_hub, load_finding_notes, repo_id_for, rpc_start_run,
    run_artifact_dir, run_deliver_repair_dir, run_fixer_dir, run_worktree_dir,
};
use porch_git::GitDir;
use porch_review::{Finding, RunReviewOpts, review_bin, review_timeout, run_review};
use serde::Serialize;

pub use agent_run::{AgentRunOpts, agent_run};
pub use sync::{SyncStatus, agent_sync, recovery_ref_name, sync_hint_for};

use crate::config::{
    effective_base_branch, load_trusted_at_sha, persist_path_instructions,
    resolve_default_branch_tip,
};

/// Phases in locked order (D5).
const PHASES: &[&str] = &["intent", "rebase", "review", "certify", "deliver"];

/// Mechanical deliver auto-fix budget (architecture; not overloading `rerun_transient`).
const DELIVER_REPAIR_BUDGET: u32 = 3;

const DELIVER_REPAIR_SUBJECT: &str = "porch: repair allowlisted checks";

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
    Certify(#[from] certify::CertifyError),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
}

type Result<T> = std::result::Result<T, RunError>;

/// Publish state (+ optional activity) when a daemon `EventHub` is installed.
fn publish_run(run_id: &str, activity: &str) {
    if let Some(hub) = event_hub() {
        hub.publish_state(run_id);
        if !activity.is_empty() {
            hub.publish_activity(run_id, activity);
        }
    }
}

fn set_status(db: &Db, run_id: &str, status: &str, error: Option<&str>) -> Result<()> {
    db.set_run_status(run_id, status, error)?;
    publish_run(run_id, &format!("status={status}"));
    Ok(())
}

fn record_step(db: &Db, run_id: &str, step: &str, status: &str, error: Option<&str>) -> Result<()> {
    db.insert_step_result(run_id, step, status, error)?;
    publish_run(run_id, &format!("step={step} status={status}"));
    Ok(())
}

#[derive(Debug)]
enum PhaseLoop {
    Continue,
    /// Review parked; leave worktree and stop the pipeline.
    Parked,
}

#[allow(clippy::too_many_lines)]
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

    set_status(&db, run_id, "running", None)?;
    db.set_worktree_dir(run_id, &wt_path)?;

    if let Err(e) = porch_git::worktree_add_detach(&bare, &wt_path, &run.sha) {
        let msg = format!("worktree add: {e}");
        let _ = set_status(&db, run_id, "failed", Some(&msg));
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
                record_step(&db, run_id, phase, "skipped", Some("skip remaining"))?;
                continue;
            }
            match *phase {
                "intent" => {
                    if run.intent.as_ref().is_some_and(|s| !s.trim().is_empty()) {
                        record_step(&db, run_id, phase, "completed", None)?;
                    } else {
                        record_step(&db, run_id, phase, "skipped", Some("no intent"))?;
                    }
                }
                "rebase" => {
                    match run_rebase(&db, home, run_id, &bare, &wt_path, &repo.default_branch)? {
                        RebaseOutcome::Completed { empty } => {
                            record_step(&db, run_id, phase, "completed", None)?;
                            if empty {
                                skip_remaining = true;
                            }
                        }
                        RebaseOutcome::Parked { detail } => {
                            record_step(&db, run_id, phase, "parked", Some(&detail))?;
                            set_status(&db, run_id, "parked", Some(&detail))?;
                            return Ok(PhaseLoop::Parked);
                        }
                    }
                }
                "review" => match run_review_phase(&db, home, run_id, &wt_path, false)? {
                    ReviewPhase::Approved => {
                        record_step(&db, run_id, phase, "completed", None)?;
                    }
                    ReviewPhase::Parked => {
                        record_step(&db, run_id, phase, "parked", None)?;
                        return Ok(PhaseLoop::Parked);
                    }
                },
                "certify" => {
                    execute_certify_step(
                        &db,
                        home,
                        run_id,
                        &bare,
                        &wt_path,
                        &repo.default_branch,
                        cancel,
                    )?;
                }
                "deliver" => {
                    match execute_deliver_step(
                        &db,
                        home,
                        run_id,
                        &bare,
                        &wt_path,
                        &repo.default_branch,
                        cancel,
                    )? {
                        PhaseLoop::Parked => return Ok(PhaseLoop::Parked),
                        PhaseLoop::Continue => {}
                    }
                }
                _ => {}
            }
        }
        Ok(PhaseLoop::Continue)
    })();

    let cancelled = cancel.load(Ordering::SeqCst);
    match &outcome {
        Ok(PhaseLoop::Parked) => {
            // Worktree kept for agent respond.
            return Ok(());
        }
        // Supersede wins over success and over deliver/certify failure (e.g.
        // watch poll timeout after cancel while babysitting checks).
        _ if cancelled => {
            let _ = set_status(&db, run_id, "cancelled", Some("superseded by new push"));
        }
        Ok(PhaseLoop::Continue) => {
            let _ = set_status(&db, run_id, "completed", None);
        }
        Err(RunError::Msg(m)) if m == "cancelled" => {
            let _ = set_status(&db, run_id, "cancelled", Some("superseded by new push"));
        }
        Err(e) => {
            let _ = set_status(&db, run_id, "failed", Some(&e.to_string()));
        }
    }
    if let Ok(Some(final_run)) = db.run_by_id(run_id) {
        finish_remove_worktree(&bare, &final_run, &wt_path);
    } else {
        remove_run_worktree(&bare, &wt_path);
    }
    match outcome {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

enum ReviewPhase {
    Approved,
    Parked,
}

fn run_review_phase(
    db: &Db,
    home: &Path,
    run_id: &str,
    wt: &Path,
    after_fix: bool,
) -> Result<ReviewPhase> {
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
        porch_home: Some(home),
        run_id: Some(run_id),
        intent: run.intent.as_deref(),
    }) {
        Ok(o) => o,
        Err(porch_review::Error::Timeout(d)) => {
            return Err(RunError::Msg(format!("review timed out after {d:?}")));
        }
        Err(e) => return Err(RunError::Review(e)),
    };

    let findings_json = serde_json::to_string(&outcome.findings)?;
    db.set_findings_json(run_id, Some(&findings_json))?;
    publish_run(run_id, "findings updated");

    if outcome.has_blocking() {
        set_status(db, run_id, "parked", None)?;
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

fn execute_certify_step(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    assert_head_continuity(db, run_id, wt)?;
    match certify::run_certify_phase(db, home, run_id, bare, wt, default_branch, Some(cancel)) {
        Ok(()) => {
            record_step(db, run_id, "certify", "completed", None)?;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            record_step(db, run_id, "certify", "failed", Some(&msg))?;
            if msg == "cancelled" {
                return Err(RunError::Msg("cancelled".into()));
            }
            Err(RunError::Certify(e))
        }
    }
}

fn execute_deliver_step(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: &AtomicBool,
) -> Result<PhaseLoop> {
    assert_head_continuity(db, run_id, wt)?;
    deliver_with_repair(db, home, run_id, bare, wt, default_branch, Some(cancel))
}

/// Push/PR/watch; on mechanical allowlisted red or CONFLICTING PR, repair and
/// restart at review → certify → deliver (same `run_id`, no intent/rebase).
fn deliver_with_repair(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: Option<&AtomicBool>,
) -> Result<PhaseLoop> {
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err(RunError::Msg("cancelled".into()));
        }
        match deliver::run_deliver_phase(db, home, run_id, bare, wt, default_branch, cancel) {
            Ok(deliver::DeliverOutcome::ParkedCompose) => {
                // compose+parked already recorded inside deliver; do not complete deliver.
                return Ok(PhaseLoop::Parked);
            }
            Ok(deliver::DeliverOutcome::Completed) => {
                record_step(db, run_id, "deliver", "completed", None)?;
                return Ok(PhaseLoop::Continue);
            }
            Err(e) => {
                let msg = e.to_string();
                record_step(db, run_id, "deliver", "failed", Some(&msg))?;
                if msg == "cancelled" {
                    return Err(RunError::Msg("cancelled".into()));
                }
                let repairable = matches!(
                    &e,
                    deliver::DeliverError::AllowlistFailed { .. }
                        | deliver::DeliverError::MergeConflicting
                );
                if !repairable {
                    return Err(RunError::Deliver(e));
                }
                let run = db
                    .run_by_id(run_id)?
                    .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
                if run.deliver_repair_attempts >= DELIVER_REPAIR_BUDGET {
                    return Err(RunError::Msg(format!(
                        "deliver repair budget exhausted ({DELIVER_REPAIR_BUDGET})"
                    )));
                }
                let attempt = db.increment_deliver_repair_attempts(run_id)?;
                let pre_repair_head = porch_git::rev_parse_c(wt, "HEAD")?;
                match &e {
                    deliver::DeliverError::AllowlistFailed { checks } => {
                        attempt_allowlist_repair(db, home, run_id, wt, checks)?;
                    }
                    deliver::DeliverError::MergeConflicting => {
                        attempt_merge_conflict_rebase(db, home, bare, wt, run_id, default_branch)?;
                    }
                    _ => unreachable!("filtered by repairable"),
                }
                let new_head = porch_git::rev_parse_c(wt, "HEAD")?;
                if new_head == pre_repair_head {
                    // Attempt counted; loop will re-deliver / re-watch or exhaust.
                    tracing::warn!(run_id, attempt, "deliver repair attempt did not move HEAD");
                    continue;
                }
                // Revoke review binding; do not upsert uncertified_pipeline_ranges.
                db.set_review_approved_head_sha(run_id, None)?;
                db.set_run_shas(run_id, Some(&new_head), None)?;
                record_step(
                    db,
                    run_id,
                    "deliver_repair",
                    "completed",
                    Some(&format!("attempt {attempt}")),
                )?;

                // Session-free rereview (after_fix never passes fixer session).
                match run_review_phase(db, home, run_id, wt, true)? {
                    ReviewPhase::Approved => {
                        record_step(db, run_id, "review", "completed", Some("deliver_repair"))?;
                        let local_cancel = AtomicBool::new(false);
                        let cancel_flag = cancel.unwrap_or(&local_cancel);
                        execute_certify_step(
                            db,
                            home,
                            run_id,
                            bare,
                            wt,
                            default_branch,
                            cancel_flag,
                        )?;
                        assert_head_continuity(db, run_id, wt)?;
                        // Loop: lease-push + PR update + re-watch.
                    }
                    ReviewPhase::Parked => {
                        record_step(db, run_id, "review", "parked", Some("deliver_repair"))?;
                        return Ok(PhaseLoop::Parked);
                    }
                }
            }
        }
    }
}

fn attempt_allowlist_repair(
    db: &Db,
    home: &Path,
    run_id: &str,
    wt: &Path,
    checks: &[porch_deliver::CheckRow],
) -> Result<()> {
    let findings = serde_json::json!(
        checks
            .iter()
            .map(|c| {
                let mut row = serde_json::json!({
                    "name": c.name,
                    "state": c.state,
                });
                if let Some(link) = c.link.as_deref() {
                    row["link"] = serde_json::json!(link);
                }
                row
            })
            .collect::<Vec<_>>()
    );
    let findings_json = findings.to_string();
    let repair_dir = run_deliver_repair_dir(home, run_id);
    let (prompt_file, findings_file) = write_deliver_repair_inputs(&repair_dir, &findings_json)?;

    let bin = fixer_bin().map_err(|e| RunError::Msg(e.to_string()))?;
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;

    run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    })?;

    // If the fixer left a dirty tree, commit with porch identity (same as certify).
    maybe_deliver_repair_commit(wt)?;
    Ok(())
}

fn attempt_merge_conflict_rebase(
    db: &Db,
    home: &Path,
    bare: &GitDir,
    wt: &Path,
    run_id: &str,
    default_branch: &str,
) -> Result<()> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    // Keep the initial-rebase pin; do not refresh trusted_config_sha on repair.
    let trusted_sha = run
        .trusted_config_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("merge conflict repair requires trusted_config_sha".into()))?;
    let cfg = load_trusted_at_sha(bare, trusted_sha).map_err(RunError::Msg)?;
    let onto = {
        let _guard = FETCH_RESOLVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        resolve_onto_tip(bare, default_branch, &cfg.pr_base_branch)?
    };
    if let Err(e) = porch_git::rebase(wt, &onto) {
        let _ = porch_git::rebase_abort(wt);
        return Err(RunError::Msg(format!("rebase conflict: {e}")));
    }
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;
    maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &cfg.path_instructions)?;
    Ok(())
}

/// Fetch and resolve the effective rebase-onto tip without changing the trusted pin.
fn resolve_onto_tip(bare: &GitDir, default_branch: &str, pr_base_branch: &str) -> Result<String> {
    let base_branch = effective_base_branch(pr_base_branch, default_branch);
    let refspec = format!("refs/heads/{base_branch}:refs/remotes/origin/{base_branch}");
    porch_git::fetch(bare, "origin", &refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{base_branch}: {e}")))?;
    let origin_ref = format!("refs/remotes/origin/{base_branch}");
    porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| RunError::Msg(format!("resolve origin/{base_branch}: {e}")))
}

/// Fetch trusted default tip, pin SHA, honor `pr.base_branch`.
/// Returns `(onto_sha, config, trusted_config_sha)`.
///
/// Unparseable trusted yaml is treated as empty for rebase onto selection;
/// certify/deliver still fail closed on the same pinned bytes.
fn resolve_rebase_onto(
    bare: &GitDir,
    default_branch: &str,
) -> Result<(String, crate::config::PorchConfig, String)> {
    let default_refspec =
        format!("refs/heads/{default_branch}:refs/remotes/origin/{default_branch}");
    porch_git::fetch(bare, "origin", &default_refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{default_branch}: {e}")))?;
    let trusted_sha = resolve_default_branch_tip(bare, default_branch).map_err(RunError::Msg)?;
    let cfg = match load_trusted_at_sha(bare, &trusted_sha) {
        Ok(c) => c,
        Err(e) if e.contains("parse error") || e.contains("not utf-8") => {
            tracing::warn!(error = %e, "trusted yaml unparseable at rebase; using default_branch");
            crate::config::PorchConfig::default()
        }
        Err(e) => return Err(RunError::Msg(e)),
    };
    let base_branch = effective_base_branch(&cfg.pr_base_branch, default_branch).to_string();
    if base_branch == default_branch {
        return Ok((trusted_sha.clone(), cfg, trusted_sha));
    }
    let refspec = format!("refs/heads/{base_branch}:refs/remotes/origin/{base_branch}");
    porch_git::fetch(bare, "origin", &refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{base_branch}: {e}")))?;
    let origin_ref = format!("refs/remotes/origin/{base_branch}");
    let onto = porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| RunError::Msg(format!("resolve origin/{base_branch}: {e}")))?;
    Ok((onto, cfg, trusted_sha))
}

fn maybe_deliver_repair_commit(wt: &Path) -> Result<bool> {
    let out = porch_git::run_c(wt, &["status", "--porcelain"])?;
    if porch_git::stdout_trim(&out).is_empty() {
        return Ok(false);
    }
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
            DELIVER_REPAIR_SUBJECT,
        ],
    )?;
    Ok(true)
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

enum RebaseOutcome {
    Completed { empty: bool },
    Parked { detail: String },
}

fn run_rebase(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> Result<RebaseOutcome> {
    let (onto, path_instructions, trusted_sha) = {
        let _guard = FETCH_RESOLVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (onto, cfg, trusted_sha) = resolve_rebase_onto(bare, default_branch)?;
        (onto, cfg.path_instructions, trusted_sha)
    };
    db.set_trusted_config_sha(run_id, &trusted_sha)?;
    db.set_run_shas(run_id, None, Some(&onto))?;

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    if head == onto {
        db.set_run_shas(run_id, Some(&head), Some(&onto))?;
        maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &path_instructions)?;
        return Ok(RebaseOutcome::Completed { empty: true });
    }

    if porch_git::is_ancestor(wt, &head, &onto)? {
        porch_git::reset_hard(wt, &onto)?;
    } else if let Err(e) = porch_git::rebase(wt, &onto) {
        // Fail closed if abort itself fails (E15 superseded: park after clean abort).
        porch_git::rebase_abort(wt).map_err(|abort_err| {
            RunError::Msg(format!(
                "rebase conflict: {e}; rebase --abort failed: {abort_err}"
            ))
        })?;
        return Ok(RebaseOutcome::Parked {
            detail: format!("rebase conflict: {e}"),
        });
    }

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), Some(&onto))?;
    maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &path_instructions)?;
    let range = format!("{onto}..{head}");
    let empty = porch_git::diff_is_empty(wt, &range)?;
    Ok(RebaseOutcome::Completed { empty })
}

fn maybe_persist_path_instructions(
    home: &Path,
    run_id: &str,
    wt: &Path,
    onto: &str,
    head: &str,
    instructions: &[crate::config::PathInstruction],
) -> Result<()> {
    if instructions.is_empty() {
        return Ok(());
    }
    let range = format!("{onto}..{head}");
    let changed = porch_git::diff_name_only(wt, &range).unwrap_or_default();
    persist_path_instructions(home, run_id, instructions, &changed).map_err(RunError::Msg)?;
    Ok(())
}

fn remove_run_worktree(bare: &GitDir, wt: &Path) {
    let _ = porch_git::worktree_remove_force(bare, wt);
    let _ = std::fs::remove_dir_all(wt);
}

/// Pin recovery ref when required, then remove the disposable worktree.
///
/// Fail closed: if pinning unpublished pipeline commits fails, keep the worktree.
fn finish_remove_worktree(bare: &GitDir, run: &RunRow, wt: &Path) {
    if let Err(e) = sync::pin_recovery_if_needed(bare, run, wt) {
        tracing::error!(
            run_id = %run.id,
            error = %e,
            worktree = %wt.display(),
            "recovery pin failed — keeping worktree (fail closed)"
        );
        return;
    }
    remove_run_worktree(bare, wt);
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
            finish_remove_worktree(&bare, &run, wt);
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

/// Human response to a parked review or compose.
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
    /// Compose park: merge Agent-authored title/body into the scaffold PR.
    Compose {
        body: String,
        title: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_packet_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_actions: Option<Vec<String>>,
}

/// Exit code for agent CLI (D11): 0 ok, 1 failed/cancelled, 2 usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliResult {
    pub exit_code: i32,
    pub json: String,
    /// When true, JSONL/JSON was already written to stdout (e.g. `agent run --wait`).
    pub already_emitted: bool,
}

/// Build status JSON for a parked (or specified) run.
#[must_use]
pub fn agent_status(home: &Path, run_id: Option<&str>, work_tree: &Path) -> AgentCliResult {
    match agent_status_inner(home, run_id, work_tree) {
        Ok(status) => AgentCliResult {
            exit_code: status_exit(&status.status),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            already_emitted: false,
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
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
            already_emitted: false,
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        },
    }
}

enum UsageOrFail {
    Usage(String),
    Fail(String),
}

pub(crate) fn status_exit(status: &str) -> i32 {
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
    Ok(status_from_run(&db, &run, home))
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

    let phase = parked_phase(&db, &run);

    // Compose park MUST be branched before review Skip (skip continues deliver).
    if phase == "compose" {
        respond_compose(home, &db, &run, &bare, &wt, &repo.default_branch, response)?;
        let run = db
            .run_by_id(&run.id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?
            .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
        return Ok(status_from_run(&db, &run, home));
    }

    match response {
        AgentResponse::Compose { .. } => {
            return Err(UsageOrFail::Usage(
                "--body-file is only valid when phase=compose".into(),
            ));
        }
        AgentResponse::Approve | AgentResponse::Skip if phase == "rebase" => {
            return Err(UsageOrFail::Usage(
                "rebase park accepts fix|abort only (not approve/skip)".into(),
            ));
        }
        AgentResponse::Approve => {
            let head = porch_git::rev_parse_c(&wt, "HEAD")
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            db.set_review_approved_head_sha(&run.id, Some(&head))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            clear_uncertified_if_certified(&db, &wt, &run.repo_id, &run.branch, &head)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            record_step(&db, &run.id, "review", "completed", Some("approved"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            let parked =
                finish_certify_and_deliver(home, &db, &bare, &wt, &run.id, &repo.default_branch)?;
            if !parked {
                finish_remove_worktree(&bare, &run, &wt);
            }
        }
        AgentResponse::Skip => {
            // Skip does not write review_approved_head_sha.
            record_step(&db, &run.id, "review", "skipped", Some("agent skip"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            for phase in ["certify", "deliver"] {
                record_step(&db, &run.id, phase, "skipped", Some("skip remaining"))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
            set_status(&db, &run.id, "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            finish_remove_worktree(&bare, &run, &wt);
        }
        AgentResponse::Abort => {
            set_status(&db, &run.id, "cancelled", Some("agent abort"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            finish_remove_worktree(&bare, &run, &wt);
        }
        AgentResponse::Fix { finding_ids, yes } => {
            if phase == "rebase" {
                respond_rebase_fix(&db, home, &run, &bare, &wt, &repo.default_branch)?;
            } else {
                respond_fix(&db, home, &run, &bare, &wt, finding_ids.as_ref(), yes)?;
            }
        }
    }

    let run = db
        .run_by_id(&run.id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
    Ok(status_from_run(&db, &run, home))
}

fn respond_compose(
    home: &Path,
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    response: AgentResponse,
) -> std::result::Result<(), UsageOrFail> {
    let resolution = match response {
        AgentResponse::Approve | AgentResponse::Fix { .. } => {
            return Err(UsageOrFail::Usage(
                "compose park accepts respond|--body-file|skip|abort (not approve/fix)".into(),
            ));
        }
        AgentResponse::Compose { body, title } => {
            deliver::ComposeResolution::Respond { body, title }
        }
        AgentResponse::Skip => deliver::ComposeResolution::Skip,
        AgentResponse::Abort => deliver::ComposeResolution::Abort,
    };

    match deliver::resume_deliver_after_compose(
        db,
        home,
        &run.id,
        bare,
        wt,
        default_branch,
        resolution,
    ) {
        Ok(()) => {
            let run = db
                .run_by_id(&run.id)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?
                .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
            if run.status != "parked" {
                finish_remove_worktree(bare, &run, wt);
            }
            Ok(())
        }
        Err(deliver::DeliverError::ComposeRejected(msg)) => {
            let _ = db.set_run_status(&run.id, "parked", Some(&msg));
            Err(UsageOrFail::Fail(msg))
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = set_status(db, &run.id, "failed", Some(&msg));
            if let Ok(Some(run)) = db.run_by_id(&run.id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            Err(UsageOrFail::Fail(msg))
        }
    }
}

fn parked_phase(db: &Db, run: &RunRow) -> String {
    if run.status != "parked" {
        return String::new();
    }
    if let Ok(steps) = db.step_results_for_run(&run.id) {
        if let Some(step) = steps.iter().rev().find(|s| s.status == "parked") {
            return step.step.clone();
        }
    }
    "review".into()
}

/// Fixer for rebase-parked runs: edit tip, then retry rebase and continue pipeline.
fn respond_rebase_fix(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> std::result::Result<(), UsageOrFail> {
    if !wt.exists() {
        return Err(UsageOrFail::Fail("parked run worktree missing".into()));
    }
    let onto = run
        .base_sha
        .clone()
        .ok_or_else(|| UsageOrFail::Fail("rebase park missing base_sha".into()))?;
    let detail = run
        .error
        .clone()
        .unwrap_or_else(|| "rebase conflict".into());
    let findings_json = serde_json::json!([{
        "id": "rebase0",
        "path": "",
        "message": detail,
        "severity": "error",
        "action": "ask-user",
        "category": "rebase",
        "base_sha": onto,
    }])
    .to_string();

    let fixer_dir = run_fixer_dir(home, &run.id);
    let (prompt_file, findings_file) = write_rebase_fix_inputs(&fixer_dir, &findings_json)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    set_status(db, &run.id, "running", None).map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let bin = match fixer_bin() {
        Ok(b) => b,
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &run.sha, &e.to_string())?;
            return Ok(());
        }
    };

    if let Err(e) = run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    }) {
        fail_fix_run(db, bare, wt, run, &run.sha, &e.to_string())?;
        return Ok(());
    }

    // Retry rebase onto the recorded base (do not refresh trusted pin).
    if let Err(e) = porch_git::rebase(wt, &onto) {
        match porch_git::rebase_abort(wt) {
            Ok(()) => {
                let msg = format!("rebase conflict: {e}");
                record_step(db, &run.id, "rebase", "parked", Some(&msg))
                    .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
                set_status(db, &run.id, "parked", Some(&msg))
                    .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
                return Ok(());
            }
            Err(abort_err) => {
                let msg = format!("rebase conflict: {e}; rebase --abort failed: {abort_err}");
                set_status(db, &run.id, "failed", Some(&msg))
                    .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
                finish_remove_worktree(bare, run, wt);
                return Ok(());
            }
        }
    }

    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    db.set_run_shas(&run.id, Some(&head), Some(&onto))
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    record_step(db, &run.id, "rebase", "completed", Some("after fix"))
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let range = format!("{onto}..{head}");
    let empty = porch_git::diff_is_empty(wt, &range).unwrap_or(false);
    if empty {
        for phase in ["review", "certify", "deliver"] {
            record_step(db, &run.id, phase, "skipped", Some("empty after rebase"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        set_status(db, &run.id, "completed", None).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        finish_remove_worktree(bare, run, wt);
        return Ok(());
    }

    match run_review_phase(db, home, &run.id, wt, false) {
        Ok(ReviewPhase::Approved) => {
            complete_after_review(db, home, bare, wt, run, None)?;
        }
        Ok(ReviewPhase::Parked) => {
            record_step(db, &run.id, "review", "parked", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        Err(e) => {
            let msg = e.to_string();
            set_status(db, &run.id, "failed", Some(&msg))
                .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
            finish_remove_worktree(bare, run, wt);
        }
    }
    let _ = default_branch;
    Ok(())
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

    finish_rereview(db, home, run, bare, wt, yes)
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
        findings_json_with_notes(home, &run.id, selected).map_err(UsageOrFail::Fail)?;
    let fixer_dir = run_fixer_dir(home, &run.id);
    let (prompt_file, findings_file) = write_fixer_inputs(&fixer_dir, &findings_json)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    set_status(db, &run.id, "running", None).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
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
    set_status(db, &run.id, "failed", Some(msg)).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    finish_remove_worktree(bare, run, wt);
    Ok(())
}

fn finish_rereview(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    yes: bool,
) -> std::result::Result<(), UsageOrFail> {
    // Session-free rereview (never pass fixer session).
    match run_review_phase(db, home, &run.id, wt, true) {
        Ok(ReviewPhase::Approved) => {
            complete_after_review(db, home, bare, wt, run, None)?;
        }
        Ok(ReviewPhase::Parked) => {
            if yes {
                let head = porch_git::rev_parse_c(wt, "HEAD")
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                db.set_review_approved_head_sha(&run.id, Some(&head))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                complete_after_review(
                    db,
                    home,
                    bare,
                    wt,
                    run,
                    Some("approved remaining after --yes"),
                )?;
            } else {
                record_step(db, &run.id, "review", "parked", Some("fix_review"))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
        }
        Err(e) => {
            let msg = e.to_string();
            set_status(db, &run.id, "failed", Some(&msg))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            finish_remove_worktree(bare, run, wt);
        }
    }
    Ok(())
}

fn complete_after_review(
    db: &Db,
    home: &Path,
    bare: &GitDir,
    wt: &Path,
    run: &RunRow,
    review_note: Option<&str>,
) -> std::result::Result<(), UsageOrFail> {
    record_step(db, &run.id, "review", "completed", review_note)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let repo = db
        .repo_by_id(&run.repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail(format!("unknown repo {}", run.repo_id)))?;
    let parked = finish_certify_and_deliver(home, db, bare, wt, &run.id, &repo.default_branch)?;
    if !parked {
        finish_remove_worktree(bare, run, wt);
    }
    Ok(())
}

/// Shared certify → deliver(+repair) path for approve / post-fix complete.
///
/// Returns `true` when deliver repair rereview parked (worktree kept).
fn finish_certify_and_deliver(
    home: &Path,
    db: &Db,
    bare: &GitDir,
    wt: &Path,
    run_id: &str,
    default_branch: &str,
) -> std::result::Result<bool, UsageOrFail> {
    assert_head_continuity(db, run_id, wt).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    match certify::run_certify_phase(db, home, run_id, bare, wt, default_branch, None) {
        Ok(()) => {
            record_step(db, run_id, "certify", "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = record_step(db, run_id, "certify", "failed", Some(&msg));
            let _ = set_status(db, run_id, "failed", Some(&msg));
            if let Ok(Some(run)) = db.run_by_id(run_id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            return Err(UsageOrFail::Fail(msg));
        }
    }
    assert_head_continuity(db, run_id, wt).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    match deliver_with_repair(db, home, run_id, bare, wt, default_branch, None) {
        Ok(PhaseLoop::Continue) => {
            set_status(db, run_id, "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            Ok(false)
        }
        Ok(PhaseLoop::Parked) => Ok(true),
        Err(e) => {
            let msg = e.to_string();
            let _ = set_status(db, run_id, "failed", Some(&msg));
            if let Ok(Some(run)) = db.run_by_id(run_id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            Err(UsageOrFail::Fail(msg))
        }
    }
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

/// Serialize selected findings for the fixer, merging optional operator notes.
fn findings_json_with_notes(
    home: &Path,
    run_id: &str,
    selected: &[Finding],
) -> std::result::Result<String, String> {
    let mut value = serde_json::to_value(selected).map_err(|e| e.to_string())?;
    let notes = load_finding_notes(home, run_id).unwrap_or_default();
    if let Some(arr) = value.as_array_mut() {
        for item in arr {
            let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(note) = notes.get(id) {
                if !note.is_empty() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("note".into(), serde_json::Value::String(note.clone()));
                    }
                }
            }
        }
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
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

pub(crate) fn status_from_run(db: &Db, run: &RunRow, home: &Path) -> AgentStatus {
    let findings = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<Finding>>(s).ok())
        .unwrap_or_default();
    let phase = match run.status.as_str() {
        "parked" => parked_phase(db, run),
        "completed" | "failed" | "cancelled" => "done".into(),
        "running" | "pending" => "pipeline".into(),
        other => other.to_string(),
    };
    let (compose_packet_path, allowed_actions) = if phase == "compose" {
        let path = run_artifact_dir(home, &run.id).join("compose-packet.json");
        (
            Some(path.display().to_string()),
            Some(vec!["respond".into(), "skip".into(), "abort".into()]),
        )
    } else {
        (None, None)
    };
    AgentStatus {
        run_id: run.id.clone(),
        repo_id: run.repo_id.clone(),
        branch: run.branch.clone(),
        status: run.status.clone(),
        phase,
        head_sha: run.head_sha.clone(),
        base_sha: run.base_sha.clone(),
        review_approved_head_sha: run.review_approved_head_sha.clone(),
        findings,
        error: run.error.clone(),
        pr_url: run.pr_url.clone(),
        compose_packet_path,
        allowed_actions,
    }
}

/// Enqueue a **new** run from a prior run's recorded tip (or branch tip).
///
/// Always allocates a fresh run id / worktree — never reuses a half-applied tree.
///
/// # Errors
///
/// Returns a string error on missing run, detached HEAD, or DB/RPC failure.
pub fn rerun(
    home: &Path,
    work_tree: &Path,
    run_id: Option<&str>,
) -> std::result::Result<String, String> {
    let work = work_tree.canonicalize().map_err(|e| e.to_string())?;
    let db = Db::open(&db_path(home)).map_err(|e| e.to_string())?;
    let repo_id = repo_id_for(&work);
    let prior = if let Some(id) = run_id {
        db.run_by_id(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown run {id}"))?
    } else {
        let branch = porch_git::stdout_trim(
            &porch_git::run_c(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map_err(|e| e.to_string())?,
        );
        if branch == "HEAD" {
            return Err("detached HEAD — checkout a branch or pass --run-id".into());
        }
        db.latest_run_for_branch(&repo_id, &branch)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no prior run for branch {branch}"))?
    };

    let sha = prior.sha.clone();
    let intent = prior.intent.as_deref();
    let intent_source = if intent.is_some() {
        Some("rerun")
    } else {
        None
    };
    let row = db
        .insert_run(&prior.repo_id, &prior.branch, &sha, intent, intent_source)
        .map_err(|e| e.to_string())?;
    if let Err(e) = rpc_start_run(home, &row.id) {
        tracing::warn!(run_id = %row.id, "start_run rpc: {e}");
    }
    Ok(row.id)
}

#[cfg(test)]
mod custody_tests {
    use super::*;
    use porch_git::{init_bare, worktree_add_detach};
    use tempfile::TempDir;

    fn git(work: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn git_out(work: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn pin_failure_keeps_worktree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "main"]);
        std::fs::write(seed.join("README"), "submit\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "submit"]);
        let submit = git_out(&seed, &["rev-parse", "HEAD"]);
        std::fs::write(seed.join("README"), "descendant\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "pipeline"]);
        let head = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["push", bare_path.to_str().unwrap(), "main:refs/heads/main"],
        );

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("r-pin", &seed, &bare_path, "main").unwrap();
        let run = db.insert_run("r-pin", "feat", &submit, None, None).unwrap();
        let wt = root.join("wt-pin");
        worktree_add_detach(&bare, &wt, &head).unwrap();
        assert!(wt.exists());
        assert_ne!(submit, head);

        // Force update-ref refs/porch/recover/<run> to fail: refs/porch is a file.
        std::fs::create_dir_all(bare_path.join("refs")).unwrap();
        std::fs::write(bare_path.join("refs/porch"), "not-a-directory\n").unwrap();

        finish_remove_worktree(&bare, &run, &wt);
        assert!(
            wt.exists(),
            "worktree must be kept when required recovery pin fails"
        );
        assert!(
            porch_git::rev_parse(&bare, &sync::recovery_ref_name(&run.id)).is_err(),
            "recovery ref must not exist after failed pin"
        );
    }

    #[test]
    fn pin_success_then_removes_worktree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "main"]);
        std::fs::write(seed.join("README"), "submit\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "submit"]);
        let submit = git_out(&seed, &["rev-parse", "HEAD"]);
        std::fs::write(seed.join("README"), "descendant\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "pipeline"]);
        let head = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["push", bare_path.to_str().unwrap(), "main:refs/heads/main"],
        );

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("r-pin-ok", &seed, &bare_path, "main")
            .unwrap();
        let run = db
            .insert_run("r-pin-ok", "feat", &submit, None, None)
            .unwrap();
        let wt = root.join("wt-pin-ok");
        worktree_add_detach(&bare, &wt, &head).unwrap();

        finish_remove_worktree(&bare, &run, &wt);
        assert!(!wt.exists(), "worktree removed after successful pin");
        assert_eq!(
            porch_git::rev_parse(&bare, &sync::recovery_ref_name(&run.id)).unwrap(),
            head
        );
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

#[cfg(test)]
mod notes_tests {
    use super::*;
    use porch_gate::set_finding_note;
    use porch_review::{Action, Severity};
    use tempfile::TempDir;

    #[test]
    fn findings_json_merges_operator_notes() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        set_finding_note(home, "run-n", "f0", "keep the helper public").unwrap();
        let selected = vec![Finding {
            id: "f0".into(),
            path: "src/a.rs".into(),
            message: "unused".into(),
            severity: Severity::Warning,
            action: Action::AskUser,
            category: None,
            start_line: Some(1),
            end_line: Some(2),
        }];
        let json = findings_json_with_notes(home, "run-n", &selected).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["note"], "keep the helper public");
        assert_eq!(v[0]["id"], "f0");
    }
}
