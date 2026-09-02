use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Executes a porch run (worktree + phases). Implemented by `porch-run`.
///
/// Injected into the daemon from the binary so `porch-gate` does not depend on
/// `porch-run` (avoids a crate cycle).
pub trait RunExecutor: Send + Sync {
    /// Drive one run to a terminal status. Honors `cancel` between phases.
    fn execute(&self, home: &Path, run_id: &str, cancel: &AtomicBool);

    /// Recover after an unclean shutdown: reconcile open review rounds to
    /// `interrupted`/`incomplete`, fail stale `running` rows, and remove leftover
    /// worktrees. The daemon refuses to serve when this returns an error.
    ///
    /// # Errors
    ///
    /// Returns a stringified error if round reconciliation, run recovery, or
    /// worktree cleanup cannot complete.
    fn recover_stale(&self, home: &Path) -> Result<(), String>;
}
