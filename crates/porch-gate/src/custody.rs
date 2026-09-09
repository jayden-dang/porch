//! Custody of porch-authored commits in disposable worktrees.
//!
//! Porch writes commits the operator does not have: certify's correction commits
//! and fixer commits. They live in a run worktree under `$PORCH_HOME/worktrees/`,
//! and the only durable name porch gives them is `refs/porch/recover/<run_id>` on
//! the bare. Removing the worktree without that ref makes them unreachable.
//!
//! This module owns the pin, and owns the only sequence allowed to remove a run
//! worktree, so that no caller can drop the worktree while skipping the pin
//! (`ESCAPE-1.5`).

use std::path::Path;

use porch_git::GitDir;

use crate::db::RunRow;

/// Recovery ref under the bare: `refs/porch/recover/<run_id>`.
#[must_use]
pub fn recovery_ref_name(run_id: &str) -> String {
    format!("refs/porch/recover/{run_id}")
}

/// Pin the worktree's HEAD under `refs/porch/recover/<run_id>` on the bare.
///
/// Pins whenever HEAD differs from the SHA the operator pushed. It deliberately
/// does **not** require the pushed SHA to be an ancestor of HEAD (`ESCAPE-1.2`):
/// `runs.sha` is written once at admission and never rewritten, so a rebase that
/// moved commits leaves it a non-ancestor of everything porch authors afterwards.
/// An ancestry gate therefore declines in exactly the case the pin exists for, and
/// declines silently, because a non-ancestor HEAD is indistinguishable from
/// "nothing worth keeping" once the worktree is gone.
///
/// A HEAD that does not descend from the pushed SHA is still porch's own product —
/// the worktree is created detached at that SHA and only porch commits into it — so
/// there is no case where pinning captures work that is not worth keeping. A ref
/// naming a commit costs one `update-ref`.
///
/// # Errors
///
/// Returns a stringified error when HEAD cannot be read or the ref cannot be
/// written. Callers must treat that as fail-closed and keep the worktree.
pub fn pin_recovery_if_needed(bare: &GitDir, run: &RunRow, wt: &Path) -> Result<(), String> {
    if !wt.exists() {
        return Ok(());
    }
    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| e.to_string())?;
    if head == run.sha {
        return Ok(());
    }
    let name = recovery_ref_name(&run.id);
    porch_git::update_ref(bare, &name, &head).map_err(|e| e.to_string())
}

/// Pin, then remove the disposable worktree.
///
/// Fail closed: when the pin fails the worktree is kept, so the commits stay
/// reachable from its own HEAD (`ESCAPE-1.3`).
pub fn finish_remove_worktree(bare: &GitDir, run: &RunRow, wt: &Path) {
    if let Err(e) = pin_recovery_if_needed(bare, run, wt) {
        tracing::error!(
            run_id = %run.id,
            error = %e,
            worktree = %wt.display(),
            "recovery pin failed — keeping worktree (fail closed)"
        );
        return;
    }
    let _ = porch_git::worktree_remove_force(bare, wt);
    let _ = std::fs::remove_dir_all(wt);
}
