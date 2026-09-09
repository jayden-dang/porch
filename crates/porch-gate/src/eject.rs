//! Remove the porch remote (and optionally this repo's home state).

use std::path::{Path, PathBuf};

use porch_git::GitDir;

use crate::Result;
use crate::db::Db;
use crate::home::{db_path, run_artifact_dir, worktrees_dir};

/// Options for [`eject`].
#[derive(Clone, Copy)]
pub struct EjectOptions<'a> {
    pub work_tree: &'a Path,
    pub porch_home: &'a Path,
    /// When true, delete this repo's bare, worktrees, run artifacts, and DB row.
    /// Never touches other repos under `$PORCH_HOME`.
    pub purge: bool,
}

/// What eject did to this repo's gate state under `$PORCH_HOME`.
///
/// Named so that the operator is never told state was removed when it was not
/// (`ESCAPE-4.1`, `ESCAPE-4.3`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateState {
    /// Deliberately kept: eject without `--purge`.
    Preserved,
    /// This repo's bare, worktrees, run artifacts, and rows are gone.
    Purged,
    /// Detach succeeded, but gate state could not be removed. Carries the reason.
    LeftBehind(String),
}

/// Result of a successful eject.
#[derive(Debug, Clone)]
pub struct EjectResult {
    pub repo_id: String,
    pub bare_path: PathBuf,
    pub purged: bool,
    pub gate_state: GateState,
}

/// Remove the `porch` remote and neutralize bare hooks.
///
/// Default (no purge): leaves `$PORCH_HOME` intact (bare, sqlite, config).
/// With `purge`: deletes **this** repo's bare, worktrees, per-run artifacts, and
/// DB rows only — other repos under the same home are untouched.
///
/// Detach never opens the database (`ESCAPE-2.1`). `Db::open` migrates, raises
/// `min_writer_protocol`, and refuses a binary older than the state root, so
/// requiring it here made the escape hatch inherit every failure of the state the
/// operator is escaping. The bare path is derived from the home layout instead of
/// read from the row (`ESCAPE-2.2`), and a database that cannot be opened downgrades
/// purge to a reported `LeftBehind` rather than failing the detach (`ESCAPE-2.3`).
///
/// # Errors
///
/// Returns an error only when this checkout cannot be identified as a porch clone
/// at all — no `porch.repo-id` and no `porch` remote to derive one from.
pub fn eject(opts: EjectOptions<'_>) -> Result<EjectResult> {
    let work = opts.work_tree.canonicalize()?;
    let porch_home = opts
        .porch_home
        .canonicalize()
        .unwrap_or_else(|_| opts.porch_home.to_path_buf());
    let repo_id = resolve_repo_id(&work)?;
    let bare_path = crate::home::repos_dir(&porch_home).join(format!("{repo_id}.git"));

    // Remote removal is best-effort when already gone, which is what makes a
    // retry after an interrupted eject succeed rather than report "not
    // initialized" (`ESCAPE-2.5`).
    let _ = porch_git::run_c(&work, &["remote", "remove", "porch"]);
    let _ = porch_git::run_c(&work, &["config", "--unset", "porch.repo-id"]);

    neutralize_bare_hooks(&bare_path);

    let gate_state = if opts.purge {
        purge_or_report(&porch_home, &repo_id, &bare_path)
    } else {
        GateState::Preserved
    };

    Ok(EjectResult {
        repo_id,
        bare_path,
        purged: gate_state == GateState::Purged,
        gate_state,
    })
}

/// This repo's porch id, from `porch.repo-id` or from the `porch` remote's path.
///
/// An eject interrupted after the config unset leaves the remote in place. Falling
/// back to the remote is what lets the operator retry instead of hand-editing git
/// config to get past "not initialized" (`ESCAPE-2.4`).
fn resolve_repo_id(work: &Path) -> Result<String> {
    if let Some(id) = existing_repo_id(work)? {
        return Ok(id);
    }
    if let Some(id) = repo_id_from_remote(work) {
        return Ok(id);
    }
    Err(crate::Error::Other(format!(
        "not a porch clone: no porch.repo-id and no `porch` remote ({})",
        work.display()
    )))
}

/// Recover the repo id from the `porch` remote URL: `.../repos/<repo_id>.git`.
fn repo_id_from_remote(work: &Path) -> Option<String> {
    let out = porch_git::run_c(work, &["remote", "get-url", "porch"]).ok()?;
    let url = porch_git::stdout_trim(&out);
    let name = Path::new(&url).file_name()?.to_str()?;
    let id = name.strip_suffix(".git")?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Purge this repo's gate state, or say why it was left behind.
///
/// Detach has already happened by this point and must not be undone, so every
/// failure here is reported rather than propagated (`ESCAPE-2.3`).
fn purge_or_report(porch_home: &Path, repo_id: &str, bare_path: &Path) -> GateState {
    let db = match Db::open(&db_path(porch_home)) {
        Ok(db) => db,
        Err(e) => {
            return GateState::LeftBehind(format!("porch database could not be opened: {e}"));
        }
    };
    match purge_repo_state(&db, porch_home, repo_id, bare_path) {
        Ok(()) => GateState::Purged,
        Err(e) => GateState::LeftBehind(format!("gate state could not be removed: {e}")),
    }
}

fn neutralize_bare_hooks(bare: &Path) {
    for name in ["pre-receive", "post-receive"] {
        let path = bare.join("hooks").join(name);
        if path.is_file() {
            let body = "#!/bin/sh\n# porch ejected — hooks disabled\nexit 0\n";
            let _ = std::fs::write(&path, body);
        }
    }
}

/// Delete this repo's rows, then its files.
///
/// The row deletion is transactional and is the only step that can fail, so it runs
/// before anything is destroyed on disk: a purge that cannot complete leaves the
/// bare, the worktrees, and the run artifacts intact (`ESCAPE-3.3`, `ESCAPE-3.4`).
fn purge_repo_state(db: &Db, home: &Path, repo_id: &str, bare: &Path) -> Result<()> {
    let runs = db.runs_for_repo(repo_id)?;

    db.delete_repo(repo_id)?;

    for run in &runs {
        if let Some(wt) = run.worktree_dir.as_ref() {
            if let Ok(g) = GitDir::new(bare) {
                let _ = porch_git::worktree_remove_force(&g, wt);
            }
            let _ = std::fs::remove_dir_all(wt);
        }
        let _ = std::fs::remove_dir_all(run_artifact_dir(home, &run.id));
    }

    let wt_root = worktrees_dir(home).join(repo_id);
    let _ = std::fs::remove_dir_all(&wt_root);

    if let Ok(g) = GitDir::new(bare) {
        let _ = crate::rounds::retention::sweep_unreferenced(&g, db);
    }

    if bare.exists() {
        let _ = std::fs::remove_dir_all(bare);
    }
    Ok(())
}

fn existing_repo_id(work: &Path) -> Result<Option<String>> {
    let git_dir = GitDir::new(work.join(".git"))?;
    match porch_git::run(&git_dir, &["config", "--get", "porch.repo-id"]) {
        Ok(out) => {
            let id = porch_git::stdout_trim(&out);
            if id.is_empty() {
                Ok(None)
            } else {
                Ok(Some(id))
            }
        }
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitOptions;
    use crate::init;
    use tempfile::TempDir;

    fn git_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().canonicalize().unwrap();
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["init", "-b", "main"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["config", "user.email", "porch@example.com"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["config", "user.name", "Porch"])
            .status()
            .unwrap();
        std::fs::write(work.join("README"), "hi\n").unwrap();
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["add", "README"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap();
        (tmp, work)
    }

    fn dummy_bin(work: &Path) -> PathBuf {
        let dummy = work.join("porch-dummy");
        std::fs::write(&dummy, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&dummy).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&dummy, p).unwrap();
        }
        dummy
    }

    #[test]
    fn init_then_eject_removes_remote_keeps_home() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let result = init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();
        assert!(result.bare_path.is_dir());

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: false,
        })
        .unwrap();
        assert!(!ejected.purged);
        assert_eq!(ejected.repo_id, result.repo_id);
        assert!(result.bare_path.is_dir(), "bare remains without --purge");

        let remotes = std::process::Command::new("git")
            .current_dir(&work)
            .args(["remote"])
            .output()
            .unwrap();
        let names = String::from_utf8_lossy(&remotes.stdout);
        assert!(!names.lines().any(|l| l.trim() == "porch"));

        let hook = std::fs::read_to_string(result.bare_path.join("hooks/post-receive")).unwrap();
        assert!(hook.contains("ejected"));
        assert!(home_path.join("state.sqlite").is_file());
    }

    #[test]
    fn eject_purge_removes_only_this_repo_state() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let other_bare = home_path.join("repos").join("otherrepo.git");
        std::fs::create_dir_all(&other_bare).unwrap();
        std::fs::write(other_bare.join("KEEP"), "other\n").unwrap();

        let result = init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
        })
        .unwrap();
        assert!(ejected.purged);
        assert!(!result.bare_path.exists());
        assert!(
            other_bare.join("KEEP").is_file(),
            "other repo bare untouched"
        );
        let db = Db::open(&db_path(&home_path)).unwrap();
        assert!(db.repo_by_id(&result.repo_id).unwrap().is_none());
    }

    /// Detach works with no database at all.
    ///
    /// `Db::open` migrates, raises `min_writer_protocol`, and refuses a binary
    /// older than the state root, so requiring it made the escape hatch inherit
    /// every failure of the state being escaped (`ESCAPE-2.1`, `ESCAPE-2.3`).
    #[test]
    fn detach_succeeds_without_a_database() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let result = init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();
        std::fs::remove_file(db_path(&home_path)).unwrap();

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: false,
        })
        .expect("detach must not depend on the database");

        assert_eq!(ejected.repo_id, result.repo_id);
        assert_eq!(ejected.gate_state, GateState::Preserved);
        assert_eq!(ejected.bare_path, result.bare_path);
        let hook = std::fs::read_to_string(result.bare_path.join("hooks/post-receive")).unwrap();
        assert!(hook.contains("ejected"), "hooks neutralized without the db");
        let remotes = std::process::Command::new("git")
            .current_dir(&work)
            .args(["remote"])
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&remotes.stdout)
                .lines()
                .any(|l| l.trim() == "porch")
        );
    }

    /// An unpurgeable state root is reported, not propagated as a detach failure.
    #[test]
    fn purge_that_cannot_open_the_database_is_reported_and_leaves_the_bare() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let result = init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();
        // A directory where the database file belongs: open fails, nothing else does.
        std::fs::remove_file(db_path(&home_path)).unwrap();
        std::fs::create_dir(db_path(&home_path)).unwrap();

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
        })
        .expect("detach still succeeds");

        assert!(!ejected.purged, "must not claim a purge it did not perform");
        match &ejected.gate_state {
            GateState::LeftBehind(reason) => {
                assert!(
                    reason.contains("database"),
                    "reason names the cause: {reason}"
                );
            }
            other => panic!("expected LeftBehind, got {other:?}"),
        }
        assert!(result.bare_path.is_dir(), "bare survives a failed purge");
    }

    /// An eject interrupted after the config unset is retryable.
    ///
    /// The repo id comes back from the `porch` remote's URL, so the operator is not
    /// told the checkout was never initialized (`ESCAPE-2.4`, `ESCAPE-2.5`).
    #[test]
    fn detach_resumes_after_an_interrupted_eject_and_is_idempotent() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let result = init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();
        // The residue of an eject that died between its first two steps.
        let _ = porch_git::run_c(&work, &["config", "--unset", "porch.repo-id"]);

        let resumed = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: false,
        })
        .expect("a retry must not report `not initialized`");
        assert_eq!(resumed.repo_id, result.repo_id);

        // Fully detached now: neither the config nor the remote remains.
        let again = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: false,
        });
        assert!(
            again.is_err(),
            "a checkout with no porch marks at all is not a porch clone"
        );
        let msg = again.unwrap_err().to_string();
        assert!(
            msg.contains("not a porch clone"),
            "the message must not send the operator to `porch init`: {msg}"
        );
    }
}
