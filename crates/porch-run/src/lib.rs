//! Execute a porch run: disposable worktree, intent, rebase, later stubs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use porch_gate::{Db, RunExecutor, db_path, run_worktree_dir};
use porch_git::GitDir;

/// Phases in locked order (D5). Later ones are stubs in M2.
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
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

type Result<T> = std::result::Result<T, RunError>;

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
    let outcome = (|| -> Result<()> {
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
                "review" | "certify" | "deliver" => {
                    // M2 stubs: no-op complete unless SkipRemaining already set.
                    db.insert_step_result(run_id, phase, "completed", None)?;
                }
                _ => {}
            }
        }
        Ok(())
    })();

    let cancelled = cancel.load(Ordering::SeqCst);
    match outcome {
        Ok(()) if cancelled => {
            let _ = db.set_run_status(run_id, "cancelled", Some("superseded by new push"));
        }
        Ok(()) => {
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
    outcome?;
    Ok(())
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
