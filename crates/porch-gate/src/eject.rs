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

/// Result of a successful eject.
#[derive(Debug, Clone)]
pub struct EjectResult {
    pub repo_id: String,
    pub bare_path: PathBuf,
    pub purged: bool,
}

/// Remove the `porch` remote and neutralize bare hooks.
///
/// Default (no purge): leaves `$PORCH_HOME` intact (bare, sqlite, config).
/// With `purge`: deletes **this** repo's bare, worktrees, per-run artifacts, and
/// DB rows only — other repos under the same home are untouched.
///
/// # Errors
///
/// Returns an error when the work tree is not a porch-initialized clone, git
/// remote removal fails hard, or purge cleanup cannot open the database.
pub fn eject(opts: EjectOptions<'_>) -> Result<EjectResult> {
    let work = opts.work_tree.canonicalize()?;
    let porch_home = opts
        .porch_home
        .canonicalize()
        .unwrap_or_else(|_| opts.porch_home.to_path_buf());
    let repo_id = existing_repo_id(&work)?.ok_or_else(|| {
        crate::Error::Other(format!(
            "not initialized (no porch.repo-id); run `porch init` first ({})",
            work.display()
        ))
    })?;

    let db = Db::open(&db_path(&porch_home))?;
    let repo = db.repo_by_id(&repo_id)?.ok_or_else(|| {
        crate::Error::Other(format!(
            "repo {repo_id} not in porch database under {}",
            porch_home.display()
        ))
    })?;
    let bare_path = repo.bare_path.clone();

    // Remote removal is best-effort when already gone.
    let _ = porch_git::run_c(&work, &["remote", "remove", "porch"]);
    let _ = porch_git::run_c(&work, &["config", "--unset", "porch.repo-id"]);

    neutralize_bare_hooks(&bare_path);

    if opts.purge {
        purge_repo_state(&db, &porch_home, &repo_id, &bare_path)?;
    }

    Ok(EjectResult {
        repo_id,
        bare_path,
        purged: opts.purge,
    })
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

fn purge_repo_state(db: &Db, home: &Path, repo_id: &str, bare: &Path) -> Result<()> {
    let runs = db.runs_for_repo(repo_id)?;
    for run in &runs {
        if let Some(wt) = run.worktree_dir.as_ref() {
            if let Ok(g) = GitDir::new(bare) {
                let _ = porch_git::worktree_remove_force(&g, wt);
            }
            let _ = std::fs::remove_dir_all(wt);
        }
        let art = run_artifact_dir(home, &run.id);
        let _ = std::fs::remove_dir_all(art);
    }

    let wt_root = worktrees_dir(home).join(repo_id);
    let _ = std::fs::remove_dir_all(&wt_root);

    if bare.exists() {
        let _ = std::fs::remove_dir_all(bare);
    }

    db.delete_repo(repo_id)?;
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
            .args(["init"])
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
}
