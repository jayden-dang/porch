//! M2 behaviors: worktree, intent, rebase, cancel, stale recovery.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::{Db, kill_group, repo_id_for, run_worktree_dir};
use porch_git::{GitDir, init_bare, run as git_run, stdout_trim, worktree_add_detach};
use porch_review::REVIEW_BIN_ENV;
use tempfile::TempDir;

/// Minimal clean review fake so non-empty M2 diffs pass the M3 review phase.
fn install_clean_review_fake(bin_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(bin_dir).unwrap();
    let path = bin_dir.join("fake-review");
    let script = r#"#!/bin/sh
set -e
OUT=""
FROM=""
TO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
"#;
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn git_out(work: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
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

fn kill_daemon(home: &Path) {
    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn wait_repo_idle(db: &Db, repo_id: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        let busy = runs
            .iter()
            .any(|r| r.status == "pending" || r.status == "running");
        if !busy {
            return;
        }
        assert!(start.elapsed() <= timeout, "repo still busy: {runs:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Origin bare + author work tree with origin remote; returns (tmp, work, home, origin).
fn setup_with_origin() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    let fake = install_clean_review_fake(&bin_dir);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    init_bare(&origin).unwrap();

    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "base"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);

    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    let work = work.canonicalize().unwrap();
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    // Detached daemon from init may omit PORCH_REVIEW_BIN; restart with it.
    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake.as_os_str()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    (tmp, work, home, origin)
}

fn push_branch(work: &Path, home: &Path, branch: &str) {
    let out = StdCommand::new("git")
        .current_dir(work)
        .env("PORCH_HOME", home)
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn worktree_created_at_recorded_path_after_push() {
    let (_tmp, work, home, _origin) = setup_with_origin();
    push_branch(&work, &home, "main");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(10));
    let runs = db.runs_for_repo(&repo_id).unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    let expected = run_worktree_dir(&home, &repo_id, &run.id);
    let recorded = run.worktree_dir.as_ref().expect("worktree_dir recorded");
    assert_eq!(recorded, &expected);
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert!(
        recorded.starts_with(home.join("worktrees")),
        "recorded path {}",
        recorded.display()
    );

    kill_daemon(&home);
}

#[test]
fn same_branch_second_push_cancels_in_flight() {
    let (_tmp, work, home, _origin) = setup_with_origin();

    std::fs::write(work.join("README"), "feature-1\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "feature-1"]);
    push_branch(&work, &home, "feature");

    std::fs::write(work.join("README"), "feature-2\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "feature-2"]);
    push_branch(&work, &home, "feature");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(15));
    let runs = db.runs_for_repo(&repo_id).unwrap();
    assert!(
        runs.len() >= 2,
        "expected multiple runs, got {}",
        runs.len()
    );
    assert!(
        runs.iter().all(|r| r.status != "running"),
        "no run left running: {runs:?}"
    );

    let bare = db.repo_by_id(&repo_id).unwrap().unwrap().bare_path;
    let list = git_run(&GitDir::new(&bare).unwrap(), &["worktree", "list"]).unwrap();
    let list_s = stdout_trim(&list);
    let lines: Vec<_> = list_s.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines.len() <= 2,
        "expected at most one live worktree, got: {list_s}"
    );

    kill_daemon(&home);
}

#[test]
fn porch_intent_is_persisted_on_run() {
    let (_tmp, work, home, _origin) = setup_with_origin();
    std::fs::write(work.join("extra.txt"), "x\n").unwrap();
    git(&work, &["add", "extra.txt"]);
    git(&work, &["commit", "-m", "extra"]);

    let out = StdCommand::new("git")
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env("PORCH_INTENT", "ship the extra file")
        .args(["push", "porch", "HEAD:refs/heads/intent-branch"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(10));
    let runs = db.runs_for_repo(&repo_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.branch == "intent-branch")
        .expect("intent-branch run");
    assert_eq!(run.intent.as_deref(), Some("ship the extra file"));
    assert_eq!(run.intent_source.as_deref(), Some("env"));
    assert_ne!(run.status, "pending");

    kill_daemon(&home);
}

#[test]
fn empty_diff_after_rebase_completes_and_skips_later_phases() {
    let (_tmp, work, home, _origin) = setup_with_origin();
    push_branch(&work, &home, "main");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(10));
    let runs = db.runs_for_repo(&repo_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "completed", "err={:?}", runs[0].error);

    let steps = db.step_results_for_run(&runs[0].id).unwrap();
    let by_name: std::collections::HashMap<_, _> = steps
        .iter()
        .map(|s| (s.step.as_str(), s.status.as_str()))
        .collect();
    assert_eq!(by_name.get("rebase"), Some(&"completed"));
    assert_eq!(by_name.get("review"), Some(&"skipped"));
    assert_eq!(by_name.get("certify"), Some(&"skipped"));
    assert_eq!(by_name.get("deliver"), Some(&"skipped"));

    kill_daemon(&home);
}

#[test]
fn rebase_conflict_aborts_and_fails_run() {
    let (_tmp, work, home, origin) = setup_with_origin();

    git(&work, &["checkout", "-b", "conflict"]);
    std::fs::write(work.join("README"), "feature side\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "feature edit"]);

    let other = home.parent().unwrap().join("other");
    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), other.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    git(&other, &["config", "user.email", "porch@example.com"]);
    git(&other, &["config", "user.name", "Porch"]);
    std::fs::write(other.join("README"), "origin side\n").unwrap();
    git(&other, &["add", "README"]);
    git(&other, &["commit", "-m", "origin edit"]);
    git(&other, &["push", "origin", "main"]);

    push_branch(&work, &home, "conflict");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(10));
    let runs = db.runs_for_repo(&repo_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.branch == "conflict")
        .expect("conflict run");
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert!(
        run.error.as_deref().is_some_and(|e| e.contains("rebase")),
        "error={:?}",
        run.error
    );

    if let Some(wt) = &run.worktree_dir {
        assert!(
            !wt.join(".git/rebase-merge").exists() && !wt.join(".git/rebase-apply").exists(),
            "rebase state left in worktree"
        );
    }

    kill_daemon(&home);
}

#[test]
fn daemon_restart_fails_running_run_and_removes_worktree() {
    let (_tmp, work, home, _origin) = setup_with_origin();
    // Populate bare with objects.
    push_branch(&work, &home, "main");
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    wait_repo_idle(&db, &repo_id, Duration::from_secs(10));

    let repo = db.repo_by_id(&repo_id).unwrap().unwrap();
    let sha = git_out(&work, &["rev-parse", "HEAD"]);

    let run = db
        .insert_run(&repo_id, "stale-branch", &sha, None, None)
        .unwrap();
    let wt = run_worktree_dir(&home, &repo_id, &run.id);
    db.set_worktree_dir(&run.id, &wt).unwrap();
    db.set_run_status(&run.id, "running", None).unwrap();
    worktree_add_detach(&GitDir::new(&repo.bare_path).unwrap(), &wt, &sha).unwrap();
    assert!(wt.exists(), "precondition: worktree on disk");

    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached(&bin, &home).unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    let start = Instant::now();
    let failed = loop {
        let r = db.run_by_id(&run.id).unwrap().unwrap();
        if r.status == "failed" {
            break r;
        }
        assert!(
            start.elapsed() <= Duration::from_secs(5),
            "stale run not failed: {} err={:?}",
            r.status,
            r.error
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|e| e.contains("daemon restarted")),
        "error={:?}",
        failed.error
    );
    assert!(
        !wt.exists(),
        "worktree should be removed after stale recovery"
    );

    kill_daemon(&home);
}
