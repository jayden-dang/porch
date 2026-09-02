//! M4: fixer + rereview + uncertified range (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::{Db, kill_group, repo_id_for, run_fixer_dir};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use serde_json::Value;
use tempfile::TempDir;

/// Noop `gh` so deliver does not hit a real GitHub CLI (E13).
fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    let script = r#"#!/bin/sh
set -e
: "${PORCH_HOME:?}"
STATE="$PORCH_HOME/gh-pr-state"
for a in "$@"; do
  [ "$a" = "--version" ] && echo "gh version 2.50.0 (fake)" && exit 0
done
CMD=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done
case "$CMD" in
  list)
    if [ -f "$STATE" ]; then /bin/cat "$STATE"; else printf '[]\n'; fi
    ;;
  create)
    /bin/cat >/dev/null
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"t"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit)
    /bin/cat >/dev/null
    ;;
  view)
    printf '{"mergeable":"MERGEABLE","number":1,"url":"https://example.com/pull/1","title":"t","body":""}\n'
    ;;
  checks)
    printf '[]\n'
    ;;
  *) echo "noop-gh: $*" >&2; exit 1 ;;
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

fn install_fake_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-review");
    let script = r#"#!/bin/sh
set -e
OUT=""
FROM=""
TO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id)
      echo "review must not receive --session-id" >&2
      exit 1
      ;;
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
if [ -n "${PORCH_HOME:-}" ] && [ -n "$FROM" ]; then
  printf '%s\n' "$FROM" > "$PORCH_HOME/last-review-from"
fi
MODE="${PORCH_FAKE_REVIEW_MODE:-clean}"
if [ "$MODE" = "hang" ]; then
  while true; do sleep 60; done
fi
if [ "$MODE" = "rereview-hang" ]; then
  if [ -f .porch-fix-committed ]; then
    while true; do sleep 60; done
  fi
  MODE=blocking
fi
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
COV_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else COV_JSON="$COV_JSON,"; fi
  COV_JSON="$COV_JSON{\"path\":\"$f\",\"status\":\"pass\"}"
done
COV_JSON="$COV_JSON]"
# Clean if any changed file contains the substring "fixed".
HAS_FIXED=0
for f in $FILES; do
  if [ -f "$f" ] && grep -q fixed "$f" 2>/dev/null; then
    HAS_FIXED=1
    break
  fi
done
if [ "$HAS_FIXED" -eq 1 ]; then
  printf '{"comments":[],"files":%s,"coverage":%s}\n' "$FILES_JSON" "$COV_JSON" > "$OUT"
  exit 0
fi
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s,"coverage":%s}\n' "$FILES_JSON" "$COV_JSON" > "$OUT"
    ;;
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref on empty input","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s,"coverage":%s}\n' \
      "$TARGET" "$FILES_JSON" "$COV_JSON" > "$OUT"
    ;;
  two-blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"bug one","category":"bug","severity":"high","start_line":1,"end_line":1},{"path":"%s","content":"bug two","category":"bug","severity":"high","start_line":2,"end_line":2}],"files":%s,"coverage":%s}\n' \
      "$TARGET" "$TARGET" "$FILES_JSON" "$COV_JSON" > "$OUT"
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

fn install_fake_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    let script = r#"#!/bin/sh
set -e
PROMPT=""
FINDINGS=""
SESSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --prompt-file) PROMPT="$2"; shift 2 ;;
    --findings-file) FINDINGS="$2"; shift 2 ;;
    --session-id) SESSION="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ -z "$PROMPT" ] || [ ! -f "$PROMPT" ]; then
  echo "prompt file missing" >&2
  exit 1
fi
if [ -z "$FINDINGS" ] || [ ! -f "$FINDINGS" ]; then
  echo "findings file missing" >&2
  exit 1
fi
if [ -n "${PORCH_HOME:-}" ] && [ -n "$SESSION" ]; then
  printf '%s\n' "$SESSION" > "$PORCH_HOME/last-fixer-session"
fi
MODE="${PORCH_FAKE_FIXER_MODE:-noop}"
case "$MODE" in
  hang)
    while true; do sleep 60; done
    ;;
  fail)
    exit 1
    ;;
  noop)
    printf '{"summary":"noop","session_id":"sess-1"}\n'
    ;;
  apply)
    TARGET=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[0]["path"] if d else "README")' "$FINDINGS" 2>/dev/null || echo README)
    if [ ! -f "$TARGET" ]; then TARGET=README; fi
    printf 'fixed\n' >> "$TARGET"
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch add -A >/dev/null
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch commit --no-verify -m "fix: address review findings" >/dev/null
    touch .porch-fix-committed
    printf '{"summary":"address review findings","session_id":"sess-1"}\n'
    ;;
  *)
    echo "unknown PORCH_FAKE_FIXER_MODE=$MODE" >&2
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

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    _origin: PathBuf,
    fake_review: PathBuf,
    fake_fixer: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup(review_mode: &str, fixer_mode: &str) -> Setup {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_review = install_fake_review(&bin_dir);
    let fake_fixer = install_fake_fixer(&bin_dir);
    let fake_gh = install_noop_gh(&bin_dir);

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

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake_review)
        .env(FIXER_BIN_ENV, &fake_fixer)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PORCH_FAKE_FIXER_MODE", fixer_mode)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    kill_daemon(&home);
    restart_daemon(
        &home,
        &fake_review,
        &fake_fixer,
        &fake_gh,
        review_mode,
        fixer_mode,
        &path,
        "30",
        "20",
    );

    Setup {
        _tmp: tmp,
        work,
        home,
        _origin: origin,
        fake_review,
        fake_fixer,
        fake_gh,
        path,
    }
}

#[allow(clippy::too_many_arguments)]
fn restart_daemon(
    home: &Path,
    fake_review: &Path,
    fake_fixer: &Path,
    fake_gh: &Path,
    review_mode: &str,
    fixer_mode: &str,
    path: &str,
    review_timeout: &str,
    fixer_timeout: &str,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        home,
        &[
            (REVIEW_BIN_ENV, fake_review.as_os_str()),
            (FIXER_BIN_ENV, fake_fixer.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", review_mode.as_ref()),
            ("PORCH_FAKE_FIXER_MODE", fixer_mode.as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", review_timeout.as_ref()),
            ("PORCH_FIXER_TIMEOUT_SECS", fixer_timeout.as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

fn push_with_env(s: &Setup, branch: &str, review_mode: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PATH", &s.path)
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

fn agent_fix(s: &Setup, run_id: &str, extra: &[&str], fixer_mode: &str) -> std::process::Output {
    let mut args = vec!["agent", "respond", "fix", "--run-id", run_id];
    args.extend_from_slice(extra);
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
        .env("PORCH_FAKE_FIXER_MODE", fixer_mode)
        .env("PORCH_REVIEW_TIMEOUT_SECS", "20")
        .env("PORCH_FIXER_TIMEOUT_SECS", "20")
        .env("PATH", &s.path)
        .args(&args)
        .output()
        .unwrap()
}

#[test]
fn fix_then_clean_rereview_completes_with_new_approved_sha() {
    let s = setup("blocking", "apply");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-fix-clean", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));
    let parked_head = run.head_sha.clone().expect("parked head");

    let out = agent_fix(&s, &run.id, &[], "apply");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    // Clean rereview certifies then parks compose (Task 5 resolves).
    assert_eq!(v["status"], "parked");
    let approved = v["review_approved_head_sha"].as_str().unwrap();
    assert_ne!(approved, parked_head);
    assert!(approved.len() >= 7);

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked");
    assert!(
        run.worktree_dir.as_ref().is_some_and(|p| p.exists()),
        "worktree kept while compose parked"
    );
    assert!(
        db.get_uncertified_pipeline_range(&repo_id, "feat-fix-clean")
            .unwrap()
            .is_none()
    );

    kill_daemon(&s.home);
}

#[test]
fn fix_noop_still_blocking_parks_again() {
    let s = setup("blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-noop", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    let out = agent_fix(&s, &run.id, &[], "noop");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "parked");
    assert!(v["review_approved_head_sha"].is_null());

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked");
    assert!(run.review_approved_head_sha.is_none());
    assert!(run.worktree_dir.as_ref().is_some_and(|p| p.exists()));

    kill_daemon(&s.home);
}

#[test]
fn fix_yes_approves_remaining_after_one_round() {
    let s = setup("blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-yes", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    let out = agent_fix(&s, &run.id, &["--yes"], "noop");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "parked");
    assert!(v["review_approved_head_sha"].as_str().unwrap().len() >= 7);

    kill_daemon(&s.home);
}

#[test]
fn rereview_timeout_after_commit_fails_and_persists_uncertified() {
    let s = setup("blocking", "apply");
    kill_daemon(&s.home);
    // Daemon keeps a generous review timeout for the initial park; the short
    // timeout applies only to the CLI-side rereview after fix.
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "rereview-hang",
        "apply",
        &s.path,
        "15",
        "10",
    );

    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-hang", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "rereview-hang")
        .env("PORCH_FAKE_FIXER_MODE", "apply")
        .env("PORCH_REVIEW_TIMEOUT_SECS", "1")
        .env("PATH", &s.path)
        .args(["agent", "respond", "fix", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "failed");
    assert!(v["review_approved_head_sha"].is_null());

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert_ne!(run.status, "parked");
    assert!(run.review_approved_head_sha.is_none());

    let rng = db
        .get_uncertified_pipeline_range(&repo_id, "feat-hang")
        .unwrap()
        .expect("uncertified range");
    assert_ne!(rng.from_sha, rng.to_sha);

    kill_daemon(&s.home);
}

#[test]
fn next_initial_review_uses_uncertified_from_sha() {
    let s = setup("clean", "noop");
    commit_change(&s.work, "a.txt", "one\n");
    push_with_env(&s, "feat-bind", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let _ = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    // Seed an uncertified range covering HEAD as tip with an earlier from_sha.
    let head = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let parent = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["rev-parse", "HEAD~1"])
        .output()
        .unwrap();
    let parent = String::from_utf8_lossy(&parent.stdout).trim().to_string();
    db.upsert_uncertified_pipeline_range(&repo_id, "feat-bind", &parent, &head, "seed-run")
        .unwrap();

    kill_daemon(&s.home);
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "clean",
        "noop",
        &s.path,
        "30",
        "20",
    );

    commit_change(&s.work, "b.txt", "two\n");
    push_with_env(&s, "feat-bind", "clean");
    let _ = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    let last_from = std::fs::read_to_string(s.home.join("last-review-from")).unwrap();
    let last_from = last_from.trim();
    assert_eq!(
        last_from, parent,
        "initial review should bind uncertified from_sha"
    );

    kill_daemon(&s.home);
}

#[test]
fn completed_review_clears_uncertified_range() {
    let s = setup("clean", "noop");
    commit_change(&s.work, "c.txt", "c\n");
    // Push will complete; seed range with to_sha = upcoming HEAD after push is awkward.
    // Instead: push clean, then upsert range whose to_sha is that completed HEAD, then
    // push another clean commit and assert clear — but clear happens on the run that
    // certifies. Seed before push with parent..current after first commit, then push.
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);

    let head = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let parent = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["rev-parse", "HEAD~1"])
        .output()
        .unwrap();
    let parent = String::from_utf8_lossy(&parent.stdout).trim().to_string();
    db.upsert_uncertified_pipeline_range(&repo_id, "feat-clear", &parent, &head, "seed")
        .unwrap();

    push_with_env(&s, "feat-clear", "clean");
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(
        db.get_uncertified_pipeline_range(&repo_id, "feat-clear")
            .unwrap()
            .is_none(),
        "uncertified should be cleared"
    );

    kill_daemon(&s.home);
}

#[test]
fn fixer_session_passed_on_second_fix() {
    let s = setup("blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-sess", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    let out = agent_fix(&s, &run.id, &[], "noop");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "parked");

    // Second fix applies; should receive sess-1 from first round.
    let out = agent_fix(&s, &run.id, &[], "apply");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let last = std::fs::read_to_string(s.home.join("last-fixer-session")).unwrap();
    assert_eq!(last.trim(), "sess-1");

    kill_daemon(&s.home);
}

#[test]
fn review_cli_never_receives_session_id() {
    // Fake review fails if it sees --session-id; clean/blocking paths must still work.
    let s = setup("blocking", "apply");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-nosess", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);

    let out = agent_fix(&s, &run.id, &[], "apply");
    assert!(
        out.status.success(),
        "rereview must not pass session-id: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    kill_daemon(&s.home);
}

#[test]
fn missing_fixer_bin_fails_closed() {
    let s = setup("blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-nobin", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env_remove(FIXER_BIN_ENV)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env("PATH", &s.path)
        .args(["agent", "respond", "fix", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let err = v["error"].as_str().unwrap_or("");
    assert!(
        err.contains("PORCH_FIXER_BIN") || err.contains("not set") || v["status"] == "failed",
        "v={v}"
    );

    kill_daemon(&s.home);
}

#[test]
fn findings_flag_selects_subset() {
    let s = setup("two-blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-subset", "two-blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));
    let snap = porch_gate::get_run(&s.home, &run.id).unwrap();
    let findings = snap.findings.as_array().expect("findings array");
    assert!(findings.len() >= 2, "findings={findings:?}");

    let out = agent_fix(&s, &run.id, &["--findings", "f0"], "noop");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let findings_path = run_fixer_dir(&s.home, &run.id).join("findings.json");
    let raw = std::fs::read_to_string(&findings_path).unwrap();
    let selected: Vec<Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["id"], "f0");

    kill_daemon(&s.home);
}

#[test]
fn prompt_file_is_outside_worktree() {
    let s = setup("blocking", "noop");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-prompt", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));
    let wt = run.worktree_dir.clone().unwrap();

    let out = agent_fix(&s, &run.id, &[], "noop");
    assert!(out.status.success());

    let prompt = run_fixer_dir(&s.home, &run.id).join("prompt.txt");
    assert!(prompt.exists());
    let prompt_abs = prompt.canonicalize().unwrap();
    let home_abs = s.home.canonicalize().unwrap();
    let wt_abs = wt.canonicalize().unwrap_or(wt);
    assert!(prompt_abs.starts_with(&home_abs));
    assert!(!prompt_abs.starts_with(&wt_abs));

    kill_daemon(&s.home);
}
