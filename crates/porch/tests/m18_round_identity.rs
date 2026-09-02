//! M18: review round identity — orchestration lifecycle (Task 9).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{self, AssuranceCompletion, ExecutionState};
use porch_gate::{Db, kill_group, repo_id_for};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use serde_json::Value;
use tempfile::TempDir;

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
    chmod_755(&path);
    path
}

fn chmod_755(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
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
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
if [ -n "${PORCH_HOME:-}" ]; then
  printf 'spawned\n' >> "$PORCH_HOME/review-spawned"
  if [ -n "$OUT" ]; then
    printf '%s\n' "$OUT" >> "$PORCH_HOME/review-outputs"
  fi
  if [ -n "$FROM" ]; then
    printf '%s\n' "$FROM" > "$PORCH_HOME/last-review-from"
  fi
fi
MODE="${PORCH_FAKE_REVIEW_MODE:-clean}"
if [ "$MODE" = "hang" ]; then
  while true; do sleep 60; done
fi
if [ "$MODE" = "exit-fail" ]; then
  echo "forced producer failure" >&2
  exit 3
fi
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
HAS_FIXED=0
for f in $FILES; do
  if [ -f "$f" ] && grep -q fixed "$f" 2>/dev/null; then
    HAS_FIXED=1
    break
  fi
done
if [ "$HAS_FIXED" -eq 1 ]; then
  printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
  exit 0
fi
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
    ;;
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref on empty input","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s}\n' \
      "$TARGET" "$FILES_JSON" > "$OUT"
    ;;
  missing-file)
    printf '{"comments":[],"files":[]}\n' > "$OUT"
    ;;
  malformed)
    printf 'this-is-not-json\n' > "$OUT"
    ;;
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

fn install_fake_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    let script = r#"#!/bin/sh
set -e
PROMPT=""
FINDINGS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --prompt-file) PROMPT="$2"; shift 2 ;;
    --findings-file) FINDINGS="$2"; shift 2 ;;
    --session-id) shift 2 ;;
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
MODE="${PORCH_FAKE_FIXER_MODE:-apply}"
case "$MODE" in
  apply)
    TARGET=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[0]["path"] if d else "README")' "$FINDINGS" 2>/dev/null || echo README)
    if [ ! -f "$TARGET" ]; then TARGET=README; fi
    printf 'fixed\n' >> "$TARGET"
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch add -A >/dev/null
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch commit --no-verify -m "fix: address review findings" >/dev/null
    printf '{"summary":"address review findings","session_id":"sess-1"}\n'
    ;;
  *)
    echo "unknown PORCH_FAKE_FIXER_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    fake_review: PathBuf,
    fake_fixer: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup(review_mode: &str) -> Setup {
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
        .env("PORCH_FAKE_FIXER_MODE", "apply")
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
        &path,
        "5",
        "20",
        false,
    );

    Setup {
        _tmp: tmp,
        work,
        home,
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
    path: &str,
    review_timeout: &str,
    fixer_timeout: &str,
    fail_open: bool,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut env: Vec<(&str, std::ffi::OsString)> = vec![
        (REVIEW_BIN_ENV, fake_review.as_os_str().to_owned()),
        (FIXER_BIN_ENV, fake_fixer.as_os_str().to_owned()),
        (GH_BIN_ENV, fake_gh.as_os_str().to_owned()),
        ("PORCH_FAKE_REVIEW_MODE", review_mode.into()),
        ("PORCH_FAKE_FIXER_MODE", "apply".into()),
        ("PATH", path.into()),
        ("PORCH_REVIEW_TIMEOUT_SECS", review_timeout.into()),
        ("PORCH_FIXER_TIMEOUT_SECS", fixer_timeout.into()),
    ];
    if fail_open {
        env.push(("PORCH_TEST_FAIL_ROUND_OPEN", "1".into()));
    }
    let env_refs: Vec<(&str, &std::ffi::OsStr)> =
        env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    porch_gate::spawn_detached_with_env(&bin, home, &env_refs).unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

fn commit_change(work: &Path, name: &str, body: &str) {
    std::fs::write(work.join(name), body).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
}

fn push_branch(s: &Setup, branch: &str, mode: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env("PORCH_FAKE_REVIEW_MODE", mode)
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

fn assert_finished_incomplete(db: &Db, run_id: &str, reason: &str) {
    let rounds = rounds::rounds_for_run(db, run_id).unwrap();
    assert_eq!(rounds.len(), 1, "expected one round, got {rounds:?}");
    let r = &rounds[0];
    assert_eq!(r.execution, ExecutionState::Finished);
    assert_eq!(r.assurance_completion, AssuranceCompletion::Incomplete);
    assert_eq!(r.completion_reason.as_deref(), Some(reason));
    assert!(r.finalized_at.is_some());
}

#[test]
fn failed_round_open_aborts_before_producer_spawn() {
    let s = setup("clean");
    kill_daemon(&s.home);
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "clean",
        &s.path,
        "5",
        "20",
        true,
    );

    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-open-fail", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_eq!(run.status, "failed");
    assert!(
        !s.home.join("review-spawned").exists(),
        "producer must not spawn when open fails"
    );
    assert!(
        rounds::rounds_for_run(&db, &run.id).unwrap().is_empty(),
        "failed open must leave no committed round"
    );

    kill_daemon(&s.home);
}

#[test]
fn timeout_finalizes_finished_incomplete_with_distinct_reason() {
    let s = setup("hang");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-timeout", "hang");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(30));
    assert_finished_incomplete(&db, &run.id, "producer_timeout");
    assert!(s.home.join("review-spawned").exists());

    kill_daemon(&s.home);
}

#[test]
fn unsuccessful_exit_finalizes_finished_incomplete() {
    let s = setup("exit-fail");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-exit", "exit-fail");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_finished_incomplete(&db, &run.id, "producer_exit");

    kill_daemon(&s.home);
}

#[test]
fn malformed_output_finalizes_finished_incomplete() {
    let s = setup("malformed");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-malformed", "malformed");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_finished_incomplete(&db, &run.id, "malformed_output");

    kill_daemon(&s.home);
}

#[test]
fn coverage_shortfall_finalizes_finished_incomplete() {
    let s = setup("missing-file");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-coverage", "missing-file");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_finished_incomplete(&db, &run.id, "coverage_shortfall");

    kill_daemon(&s.home);
}

#[test]
fn clean_run_finalizes_complete_and_records_approved_sha() {
    let s = setup("clean");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-clean", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(20),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(run.review_approved_head_sha.is_some());

    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].execution, ExecutionState::Finished);
    assert_eq!(
        rounds[0].assurance_completion,
        AssuranceCompletion::Complete
    );
    assert!(rounds[0].completion_reason.is_none());

    kill_daemon(&s.home);
}

#[test]
fn blocking_findings_park_and_still_finalize_complete() {
    let s = setup("blocking");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_branch(&s, "feat-block", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    assert!(run.review_approved_head_sha.is_none());

    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].execution, ExecutionState::Finished);
    assert_eq!(
        rounds[0].assurance_completion,
        AssuranceCompletion::Complete
    );
    assert!(
        !rounds::instances_for_round(&db, &rounds[0].id)
            .unwrap()
            .is_empty()
    );

    kill_daemon(&s.home);
}

#[test]
fn two_rounds_keep_separate_invocation_artifact_namespaces() {
    let s = setup("blocking");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_branch(&s, "feat-two-rounds", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let first_rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(first_rounds.len(), 1);
    let first_id = first_rounds[0].id.as_str().to_string();
    let first_producer = rounds::producers_for_round(&db, &first_rounds[0].id).unwrap()[0]
        .id
        .clone();
    let first_art = s
        .home
        .join("runs")
        .join(&run.id)
        .join("rounds")
        .join(&first_id)
        .join("producers")
        .join(&first_producer);
    assert!(
        first_art.join("result.json").is_file(),
        "missing first invocation artifact under {}",
        first_art.display()
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
        .env("PORCH_FAKE_FIXER_MODE", "apply")
        .env("PATH", &s.path)
        .args(["agent", "respond", "fix", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "completed", "failed"],
        Duration::from_secs(40),
    );
    let all = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(all.len(), 2, "expected two rounds after fix+rereview");
    assert_ne!(all[0].id.as_str(), all[1].id.as_str());
    let second = all.iter().find(|r| r.ordinal == 2).unwrap();
    let second_producer = rounds::producers_for_round(&db, &second.id).unwrap()[0]
        .id
        .clone();
    let second_art = s
        .home
        .join("runs")
        .join(&run.id)
        .join("rounds")
        .join(second.id.as_str())
        .join("producers")
        .join(&second_producer);
    assert!(
        second_art.join("result.json").is_file(),
        "missing second invocation artifact under {}",
        second_art.display()
    );
    assert_ne!(first_art, second_art);

    kill_daemon(&s.home);
}

#[test]
fn approve_records_sha_skip_leaves_unrecorded_post_fix_from_sha_unchanged() {
    // Approve path
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug.txt", "boom\n");
        push_branch(&s, "feat-approve", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        assert!(run.review_approved_head_sha.is_none());

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .env(GH_BIN_ENV, &s.fake_gh)
            .env("PATH", &s.path)
            .args(["agent", "respond", "approve", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["review_approved_head_sha"].as_str().unwrap().len() >= 7);
        kill_daemon(&s.home);
    }

    // Skip path
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug2.txt", "boom\n");
        push_branch(&s, "feat-skip", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .args(["agent", "respond", "skip", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(out.status.success());
        let run = db.run_by_id(&run.id).unwrap().unwrap();
        assert!(run.review_approved_head_sha.is_none());
        kill_daemon(&s.home);
    }

    // Post-fix from_sha resolves from the uncertified pipeline range (pre-fix HEAD).
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug3.txt", "boom\n");
        push_branch(&s, "feat-from-sha", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        let pre_fix_head = run.head_sha.clone().expect("parked run has head_sha");

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .env(REVIEW_BIN_ENV, &s.fake_review)
            .env(FIXER_BIN_ENV, &s.fake_fixer)
            .env(GH_BIN_ENV, &s.fake_gh)
            .env("PORCH_FAKE_REVIEW_MODE", "blocking")
            .env("PORCH_FAKE_FIXER_MODE", "apply")
            .env("PATH", &s.path)
            .args(["agent", "respond", "fix", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fix failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = wait_status(
            &db,
            &repo_id,
            &["parked", "completed", "failed"],
            Duration::from_secs(40),
        );
        let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
        assert!(rounds.len() >= 2);
        let second = rounds.iter().find(|r| r.ordinal == 2).unwrap();
        assert_eq!(
            second.from_sha, pre_fix_head,
            "post-fix from_sha must be the uncertified range start (pre-fix HEAD)"
        );
        let last_from = std::fs::read_to_string(s.home.join("last-review-from")).unwrap();
        assert_eq!(last_from.trim(), pre_fix_head);
        kill_daemon(&s.home);
    }
}
