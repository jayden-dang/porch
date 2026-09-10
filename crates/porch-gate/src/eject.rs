//! Remove the porch remote (and optionally this repo's home state).

use std::path::{Path, PathBuf};

use porch_git::GitDir;

use crate::Result;
use crate::db::Db;
use crate::home::{db_path, run_artifact_dir, worktrees_dir};
use crate::purge;

/// Options for [`eject`].
#[derive(Clone, Copy)]
pub struct EjectOptions<'a> {
    pub work_tree: &'a Path,
    pub porch_home: &'a Path,
    /// When true, delete this repo's bare, worktrees, run artifacts, and DB row.
    /// Never touches other repos under `$PORCH_HOME`.
    pub purge: bool,
    /// Explicit override: proceed with `--purge` despite unforwarded tips or
    /// active runs. Clap-tied to `purge` at the CLI.
    pub abandon: bool,
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
/// Returns an error when this checkout cannot be identified as a porch clone
/// at all — no `porch.repo-id` and no `porch` remote to derive one from — or when
/// `--purge` is refused over unforwarded custody tips or active runs.
pub fn eject(opts: EjectOptions<'_>) -> Result<EjectResult> {
    let work = opts.work_tree.canonicalize()?;
    let porch_home = opts
        .porch_home
        .canonicalize()
        .unwrap_or_else(|_| opts.porch_home.to_path_buf());
    let repo_id = resolve_repo_id(&work)?;
    let bare_path = crate::home::repos_dir(&porch_home).join(format!("{repo_id}.git"));

    if opts.purge {
        let block = purge::evaluate(&porch_home, &work, &repo_id, &bare_path);
        if block.is_blocked() && !opts.abandon {
            return Err(crate::Error::Other(block.manifest()));
        }
        if opts.abandon {
            purge::write_abandon_record(&porch_home, &repo_id, &block)?;
        }
        let gate_state = match Db::open(&db_path(&porch_home)) {
            Ok(db) => {
                if !opts.abandon {
                    match db.active_runs(Some(&repo_id), None) {
                        Ok(active) if !active.is_empty() => {
                            let ids: Vec<_> = active.iter().map(|r| r.id.clone()).collect();
                            return Err(crate::Error::Other(format!(
                                "refuse --purge: active runs appeared after the read-only check: {}\n\
                                 inspect:  porch status\n\
                                 override: porch eject --purge --abandon",
                                ids.join(", ")
                            )));
                        }
                        Err(e) => {
                            return Err(crate::Error::Other(format!(
                                "refuse --purge: cannot re-check active runs: {e}"
                            )));
                        }
                        Ok(_) => {}
                    }
                }
                detach(&work, &bare_path);
                match purge_repo_state(&db, &porch_home, &repo_id, &bare_path) {
                    Ok(()) => GateState::Purged,
                    Err(e) => {
                        GateState::LeftBehind(format!("gate state could not be removed: {e}"))
                    }
                }
            }
            Err(e) => {
                detach(&work, &bare_path);
                GateState::LeftBehind(format!("porch database could not be opened: {e}"))
            }
        };
        return Ok(EjectResult {
            repo_id,
            bare_path,
            purged: gate_state == GateState::Purged,
            gate_state,
        });
    }

    detach(&work, &bare_path);
    Ok(EjectResult {
        repo_id,
        bare_path,
        purged: false,
        gate_state: GateState::Preserved,
    })
}

fn detach(work: &Path, bare_path: &Path) {
    // Remote removal is best-effort when already gone, which is what makes a
    // retry after an interrupted eject succeed rather than report "not
    // initialized" (`ESCAPE-2.5`).
    let _ = porch_git::run_c(work, &["remote", "remove", "porch"]);
    let _ = porch_git::run_c(work, &["config", "--unset", "porch.repo-id"]);
    neutralize_bare_hooks(bare_path);
}

/// This repo's porch id, from `porch.repo-id` or from the `porch` remote's path.
///
/// Inspect uses the same resolver as detach so a preset `porch.repo-id` is
/// not silently replaced by a path hash (`LOOK-3.2`). An eject interrupted after
/// the config unset leaves the remote in place; falling back to the remote lets
/// the operator retry instead of hand-editing git config (`ESCAPE-2.4`).
///
/// # Errors
///
/// Returns an error when neither the config key nor the remote can supply an id.
pub fn resolve_repo_id(work: &Path) -> Result<String> {
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
        std::fs::remove_dir_all(bare)?;
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
    use crate::rounds::RunEffects;
    use crate::rounds::forward::{
        ForwardIntent, ForwardKind, ForwardOutcome, ObservedRemote, append_intent, append_outcome,
    };
    use crate::rounds::phase::{PhaseName, PhaseTransition, persist_phase_transition};
    use tempfile::TempDir;

    /// A git invocation that cannot see the ambient user or system configuration.
    ///
    /// Signing is the expensive one: with `commit.gpgsign` set on the host, each
    /// `commit` here calls the operator's signing helper, which turns a 0.2 s
    /// module into an intermittent 20 s one.
    fn git(work: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .current_dir(work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed in {}", work.display());
    }

    fn git_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().canonicalize().unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "porch@example.com"]);
        git(&work, &["config", "user.name", "Porch"]);
        std::fs::write(work.join("README"), "hi\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "init"]);
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
            abandon: false,
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
            abandon: false,
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
            abandon: false,
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

    /// An unreadable database refuses `--purge` without `--abandon` and stays attached.
    #[test]
    fn purge_that_cannot_open_the_database_refuses_and_stays_attached() {
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
        // A directory where the database file belongs: open_read fails.
        std::fs::remove_file(db_path(&home_path)).unwrap();
        std::fs::create_dir(db_path(&home_path)).unwrap();

        let err = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect_err("must refuse rather than detach");
        let msg = err.to_string();
        assert!(
            msg.contains("refuse --purge"),
            "manifest names the refusal: {msg}"
        );
        assert!(msg.contains("database"), "reason names the cause: {msg}");

        let remotes = std::process::Command::new("git")
            .current_dir(&work)
            .args(["remote"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&remotes.stdout)
                .lines()
                .any(|l| l.trim() == "porch"),
            "refusal must not remove the porch remote"
        );
        assert!(result.bare_path.is_dir(), "bare survives a refused purge");
    }

    /// `--abandon` on an unreadable database still detaches and reports `LeftBehind`.
    #[test]
    fn purge_abandon_on_unreadable_database_detaches_and_leaves_the_bare() {
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
        std::fs::create_dir(db_path(&home_path)).unwrap();

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: true,
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
        let abandoned = crate::home::abandoned_dir(&home_path);
        let entries: Vec<_> = std::fs::read_dir(&abandoned)
            .expect("abandon record written before detach")
            .collect();
        assert_eq!(entries.len(), 1, "one abandon record");
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
            abandon: false,
        })
        .expect("a retry must not report `not initialized`");
        assert_eq!(resumed.repo_id, result.repo_id);

        // Fully detached now: neither the config nor the remote remains.
        let again = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: false,
            abandon: false,
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

    fn git_stdout(work: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn pin_recover_and_reset(work: &Path, bare: &Path, run_id: &str) -> String {
        std::fs::write(work.join("extra"), "porch-authored\n").unwrap();
        git(work, &["add", "extra"]);
        git(work, &["commit", "-m", "extra"]);
        let sha = git_stdout(work, &["rev-parse", "HEAD"]);
        let git_dir = GitDir::new(bare).unwrap();
        porch_git::run(
            &git_dir,
            &[
                "fetch",
                work.to_str().unwrap(),
                &format!("+HEAD:refs/porch/recover/{run_id}"),
            ],
        )
        .unwrap();
        git(work, &["reset", "--hard", "HEAD~1"]);
        sha
    }

    #[test]
    fn purge_refuses_an_unforwarded_recover_ref_and_stays_attached() {
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
        let sha = pin_recover_and_reset(&work, &result.bare_path, "01TESTREFUSE");

        let err = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect_err("unforwarded recover ref must refuse");
        let msg = err.to_string();
        assert!(msg.contains("refuse --purge"), "{msg}");
        assert!(msg.contains(&sha), "manifest names the SHA: {msg}");
        let remotes = git_stdout(&work, &["remote"]);
        assert!(
            remotes.lines().any(|l| l.trim() == "porch"),
            "still attached: {remotes}"
        );
        assert!(result.bare_path.is_dir());
    }

    #[test]
    fn purge_proceeds_when_origin_tracking_contains_the_tip() {
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
        let sha = pin_recover_and_reset(&work, &result.bare_path, "01TESTTRACK");
        let git_dir = GitDir::new(&result.bare_path).unwrap();
        porch_git::run(&git_dir, &["update-ref", "refs/remotes/origin/main", &sha]).unwrap();

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect("tracking ancestry is origin proof");
        assert_eq!(ejected.gate_state, GateState::Purged);
        assert!(!result.bare_path.exists());
    }

    #[test]
    fn purge_proceeds_when_a_pushed_record_matches_the_tip() {
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
        let db = Db::open(&db_path(&home_path)).unwrap();
        let run = db
            .insert_run(&result.repo_id, "feat", "deadbeef", None, None)
            .unwrap();
        db.set_run_status(&run.id, "completed", None).unwrap();
        drop(db);
        let sha = pin_recover_and_reset(&work, &result.bare_path, &run.id);
        let db = Db::open(&db_path(&home_path)).unwrap();
        let attempt = persist_phase_transition(
            &db,
            PhaseTransition::Start {
                run_id: run.id.clone(),
                phase: PhaseName::Deliver,
            },
            RunEffects::none(),
        )
        .unwrap();
        append_intent(
            &db,
            &ForwardIntent {
                run_id: &run.id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: &sha,
                observed: ObservedRemote::Absent,
            },
        )
        .unwrap();
        append_outcome(
            &db,
            &ForwardOutcome {
                run_id: &run.id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: &sha,
                kind: ForwardKind::Pushed,
                landed_sha: Some(&sha),
                detail: None,
            },
        )
        .unwrap();
        drop(db);

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect("pushed record is origin proof even without tracking");
        assert_eq!(ejected.gate_state, GateState::Purged);
    }

    #[test]
    fn purge_refuses_a_tip_ahead_of_the_authorized_sha() {
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
        let parent = git_stdout(&work, &["rev-parse", "HEAD"]);
        let db = Db::open(&db_path(&home_path)).unwrap();
        let run = db
            .insert_run(&result.repo_id, "feat", &parent, None, None)
            .unwrap();
        db.set_run_status(&run.id, "completed", None).unwrap();
        drop(db);
        let child = pin_recover_and_reset(&work, &result.bare_path, &run.id);
        assert_ne!(child, parent);
        let db = Db::open(&db_path(&home_path)).unwrap();
        let attempt = persist_phase_transition(
            &db,
            PhaseTransition::Start {
                run_id: run.id.clone(),
                phase: PhaseName::Deliver,
            },
            RunEffects::none(),
        )
        .unwrap();
        append_intent(
            &db,
            &ForwardIntent {
                run_id: &run.id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: &parent,
                observed: ObservedRemote::Absent,
            },
        )
        .unwrap();
        append_outcome(
            &db,
            &ForwardOutcome {
                run_id: &run.id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: &parent,
                kind: ForwardKind::Pushed,
                landed_sha: Some(&parent),
                detail: None,
            },
        )
        .unwrap();
        drop(db);

        let err = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect_err("a leftover tip ahead of origin is unforwarded work");
        assert!(err.to_string().contains(&child), "{}", err);
    }

    #[test]
    fn purge_proceeds_when_the_tip_is_in_the_checkout() {
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
        let sha = git_stdout(&work, &["rev-parse", "HEAD"]);
        let git_dir = GitDir::new(&result.bare_path).unwrap();
        porch_git::run(
            &git_dir,
            &[
                "fetch",
                work.to_str().unwrap(),
                "+HEAD:refs/porch/recover/01INCHECKOUT",
            ],
        )
        .unwrap();
        assert_eq!(sha, git_stdout(&work, &["rev-parse", "HEAD"]));

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect("checkout ancestry is enough");
        assert_eq!(ejected.gate_state, GateState::Purged);
    }

    #[test]
    fn purge_refuses_active_runs_and_abandon_overrides() {
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
        let db = Db::open(&db_path(&home_path)).unwrap();
        let run = db
            .insert_run(&result.repo_id, "feat", "deadbeef", None, None)
            .unwrap();
        db.set_run_status(&run.id, "parked", None).unwrap();
        drop(db);

        let err = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect_err("parked run must refuse");
        assert!(err.to_string().contains(&run.id), "{}", err);
        let still = Db::open_read(&db_path(&home_path)).unwrap();
        assert_eq!(
            still.run_by_id(&run.id).unwrap().unwrap().status,
            "parked",
            "a refused --purge must not rewrite run status"
        );
        drop(still);

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: true,
        })
        .expect("abandon overrides active runs");
        assert_eq!(ejected.gate_state, GateState::Purged);
        let abandoned = crate::home::abandoned_dir(&home_path);
        assert!(abandoned.read_dir().unwrap().next().is_some());
    }

    #[test]
    fn purge_ignores_active_runs_on_another_repo() {
        let (_keep, work) = git_repo();
        let home = TempDir::new().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        init(InitOptions {
            work_tree: &work,
            porch_home: &home_path,
            porch_bin: &dummy_bin(&work),
            start_daemon: false,
        })
        .unwrap();
        let db = Db::open(&db_path(&home_path)).unwrap();
        db.upsert_repo("otherrepo", &work, &home_path.join("other.git"), "main")
            .unwrap();
        let other = db
            .insert_run("otherrepo", "feat", "deadbeef", None, None)
            .unwrap();
        db.set_run_status(&other.id, "parked", None).unwrap();
        drop(db);

        let ejected = eject(EjectOptions {
            work_tree: &work,
            porch_home: &home_path,
            purge: true,
            abandon: false,
        })
        .expect("another repo's parked run must not block this purge");
        assert_eq!(ejected.gate_state, GateState::Purged);
    }

    #[test]
    fn purge_repo_state_propagates_bare_delete_failure() {
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
        std::fs::remove_dir_all(&result.bare_path).unwrap();
        std::fs::write(&result.bare_path, "not a directory\n").unwrap();
        let db = Db::open(&db_path(&home_path)).unwrap();
        let err = super::purge_repo_state(&db, &home_path, &result.repo_id, &result.bare_path)
            .expect_err("file-as-bare must fail closed");
        assert!(result.bare_path.is_file(), "bare file remains: {err}");
    }
}
