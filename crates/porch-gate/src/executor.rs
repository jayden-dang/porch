use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Executes a porch run (worktree + phases). Implemented by `porch-run`.
///
/// Injected into the daemon from the binary so `porch-gate` does not depend on
/// `porch-run` (avoids a crate cycle).
pub trait RunExecutor: Send + Sync {
    /// Drive one run to a terminal status. Honors `cancel` between phases.
    fn execute(&self, home: &Path, run_id: &str, cancel: &AtomicBool);

    /// Fail stale `running` rows and remove their leftover worktrees.
    ///
    /// # Errors
    ///
    /// Returns a stringified error if recovery cannot update state or remove trees.
    fn recover_stale(&self, home: &Path) -> Result<(), String>;
}
