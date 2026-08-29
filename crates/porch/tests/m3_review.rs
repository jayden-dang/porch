//! M3: review CLI adapter, park, agent status/respond (PATH fake only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::{Db, kill_group, repo_id_for};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use serde_json::Value;
use tempfile::TempDir;

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn wait_status(db: &Db, repo_id: &str, want: &[&str], timeout: Duration) -> porch_gate::RunRow {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        if let Some(run) = runs.last() {
            if want.contains(&run.status.as_str()) {
                return run.clone();
            }
        }
        assert!(
            start.elapsed() <= timeout,
            "wanted {want:?}, got {:?}",
            db.runs_for_repo(repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fake review CLI controlled by `PORCH_FAKE_REVIEW_MODE`.
fn install_fake_review(bin_dir: &Path) -> PathBuf {
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
MODE="${PORCH_FAKE_REVIEW_MODE:-clean}"
if [ "$MODE" = "hang" ]; then
  while true; do sleep 60; done
fi
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
    ;;
  blocking)
    # Prefer a real changed path when present.
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref on empty input","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s}\n' \
      "$TARGET" "$FILES_JSON" > "$OUT"
    ;;
  missing-file)
    printf '{"comments":[],"files":[]}\n' > "$OUT"
    ;;
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
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

fn setup_with_origin_and_fake(mode: &str) -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake = install_fake_review(&bin_dir);

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

    // Init with PATH that includes the fake so the detached daemon inherits it.
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake)
        .env("PORCH_FAKE_REVIEW_MODE", mode)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    // Restart daemon with review env (init's daemon may lack PORCH_REVIEW_BIN).
    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", mode.as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "5".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    (tmp, work, home, origin, fake)
}

fn push_with_env(work: &Path, home: &Path, branch: &str, fake: &Path, mode: &str) {
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = StdCommand::new("git")
        .current_dir(work)
        .env("PORCH_HOME", home)
        .env(REVIEW_BIN_ENV, fake)
        .env("PORCH_FAKE_REVIEW_MODE", mode)
        .env("PATH", path)
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_change(work: &Path, name: &str, body: &str) {
    std::fs::write(work.join(name), body).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
}

#[test]
fn clean_review_sets_approved_sha_and_completes() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("clean");
    commit_change(&work, "extra.txt", "x\n");
    push_with_env(&work, &home, "feat-clean", &fake, "clean");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(20),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert!(
        run.review_approved_head_sha
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
        "approved sha missing: {run:?}"
    );
    let steps = db.step_results_for_run(&run.id).unwrap();
    let by: std::collections::HashMap<_, _> = steps
        .iter()
        .map(|s| (s.step.as_str(), s.status.as_str()))
        .collect();
    assert_eq!(by.get("review"), Some(&"completed"));
    assert_eq!(by.get("certify"), Some(&"completed"));
    assert_eq!(by.get("deliver"), Some(&"completed"));

    kill_daemon(&home);
}

#[test]
fn blocking_review_parks_without_approved_sha() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-block", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(20),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(run.review_approved_head_sha.is_none());
    assert!(run.worktree_dir.as_ref().is_some_and(|p| p.exists()));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "parked");
    assert!(!v["findings"].as_array().unwrap().is_empty());

    kill_daemon(&home);
}

#[test]
fn respond_approve_writes_sha_and_completes() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-approve", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "completed");
    assert!(v["review_approved_head_sha"].as_str().unwrap().len() >= 7);

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed");
    assert!(run.review_approved_head_sha.is_some());
    assert!(
        run.worktree_dir.as_ref().is_none_or(|p| !p.exists()),
        "worktree should be removed after approve"
    );

    kill_daemon(&home);
}

#[test]
fn respond_abort_cancels() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-abort", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "abort is cancelled → exit 1");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "cancelled");

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert!(run.review_approved_head_sha.is_none());

    kill_daemon(&home);
}

#[test]
fn respond_skip_skips_review_without_approved_sha() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-skip", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "completed");
    assert!(v["review_approved_head_sha"].is_null());

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert!(run.review_approved_head_sha.is_none());
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "review" && s.status == "skipped"),
        "steps={steps:?}"
    );

    kill_daemon(&home);
}

#[test]
fn missing_review_bin_fails_closed() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("clean");
    kill_daemon(&home);

    // Restart daemon pointing at a non-existent binary.
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let missing = fake.parent().unwrap().join("no-such-review");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, missing.as_os_str()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "5".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    commit_change(&work, "extra.txt", "x\n");
    let out = StdCommand::new("git")
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["push", "porch", "HEAD:refs/heads/feat-missing"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_eq!(run.status, "failed");
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("not found") || e.contains("review")),
        "error={:?}",
        run.error
    );
    assert!(run.review_approved_head_sha.is_none());

    kill_daemon(&home);
}

#[test]
fn review_timeout_fails_not_parks() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("hang");
    kill_daemon(&home);

    let bin = assert_cmd::cargo::cargo_bin("porch");
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", "hang".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "1".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    commit_change(&work, "extra.txt", "x\n");
    push_with_env(&work, &home, "feat-hang", &fake, "hang");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(30));
    assert_eq!(run.status, "failed");
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("timed out")),
        "error={:?}",
        run.error
    );
    assert_ne!(run.status, "parked");

    kill_daemon(&home);
}
