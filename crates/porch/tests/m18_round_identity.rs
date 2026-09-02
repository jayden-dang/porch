//! M18: review round identity — orchestration lifecycle and startup reconciliation.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{
    self, AssuranceCompletion, ContextApplication, ContextApplicationState, ContextSource,
    ExecutionState, OpenRoundPlan, ProducerInvocation, RoundBindings, capture_context_element,
    context_applicability_digest, sha256_hex,
};
use porch_gate::{Db, kill_group, repo_id_for, run_worktree_dir};
use porch_git::{GitDir, init_bare, worktree_add_detach};
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
        }
    }
    // Review producers spawn in their own process group; SIGTERM the daemon
    // group leaves hang-mode fakes behind. Reap anything still bound to this home.
    let marker = home.display().to_string();
    let _ = StdCommand::new("pkill")
        .args(["-9", "-f", &marker])
        .output();
    std::thread::sleep(Duration::from_millis(300));
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
COV_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else COV_JSON="$COV_JSON,"; fi
  COV_JSON="$COV_JSON{\"path\":\"$f\",\"status\":\"pass\"}"
done
COV_JSON="$COV_JSON]"
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
  missing-file)
    printf '{"comments":[],"files":[]}\n' > "$OUT"
    ;;
  files-only)
    # Presence without completion signals — must finalize incomplete.
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
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
        DaemonOpts::default(),
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

#[derive(Clone, Copy, Default)]
enum FaultHook {
    #[default]
    None,
    FailOpen,
    AbortAfterOpen,
    AbortBeforeFinalize,
    FailRecover,
}

#[derive(Clone, Copy)]
struct DaemonOpts {
    review_timeout: &'static str,
    fixer_timeout: &'static str,
    fault: FaultHook,
    wait_health: bool,
}

impl Default for DaemonOpts {
    fn default() -> Self {
        Self {
            review_timeout: "5",
            fixer_timeout: "20",
            fault: FaultHook::None,
            wait_health: true,
        }
    }
}

fn restart_daemon(
    home: &Path,
    fake_review: &Path,
    fake_fixer: &Path,
    fake_gh: &Path,
    review_mode: &str,
    path: &str,
    opts: DaemonOpts,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut env: Vec<(&str, std::ffi::OsString)> = vec![
        (REVIEW_BIN_ENV, fake_review.as_os_str().to_owned()),
        (FIXER_BIN_ENV, fake_fixer.as_os_str().to_owned()),
        (GH_BIN_ENV, fake_gh.as_os_str().to_owned()),
        ("PORCH_FAKE_REVIEW_MODE", review_mode.into()),
        ("PORCH_FAKE_FIXER_MODE", "apply".into()),
        ("PATH", path.into()),
        ("PORCH_REVIEW_TIMEOUT_SECS", opts.review_timeout.into()),
        ("PORCH_FIXER_TIMEOUT_SECS", opts.fixer_timeout.into()),
    ];
    match opts.fault {
        FaultHook::None => {}
        FaultHook::FailOpen => env.push(("PORCH_TEST_FAIL_ROUND_OPEN", "1".into())),
        FaultHook::AbortAfterOpen => env.push(("PORCH_TEST_ABORT_AFTER_ROUND_OPEN", "1".into())),
        FaultHook::AbortBeforeFinalize => {
            env.push(("PORCH_TEST_ABORT_BEFORE_ROUND_FINALIZE", "1".into()));
        }
        FaultHook::FailRecover => env.push(("PORCH_TEST_FAIL_RECOVER_STALE", "1".into())),
    }
    let env_refs: Vec<(&str, &std::ffi::OsStr)> =
        env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    porch_gate::spawn_detached_with_env(&bin, home, &env_refs).unwrap();
    if opts.wait_health {
        porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
    }
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
        DaemonOpts {
            fault: FaultHook::FailOpen,
            ..DaemonOpts::default()
        },
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
fn presence_only_coverage_finalizes_incomplete_not_complete() {
    let s = setup("files-only");
    commit_change(&s.work, "extra.txt", "x\n");
    push_branch(&s, "feat-selected-shortfall", "files-only");

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
    let coverage = rounds::coverage_for_round(&db, &rounds[0].id).unwrap();
    assert!(
        !coverage.is_empty(),
        "complete round must record coverage rows"
    );
    assert!(
        coverage
            .iter()
            .all(|row| row.state == rounds::CoverageState::Completed
                || row.state == rounds::CoverageState::Waived),
        "complete coverage must be completed/waived, got {coverage:?}"
    );
    assert!(
        coverage
            .iter()
            .filter(|row| row.state == rounds::CoverageState::Completed)
            .all(|row| row
                .completion_evidence
                .as_ref()
                .is_some_and(|e| !e.is_empty())),
        "completed rows need evidence"
    );

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

fn assert_interrupted_incomplete_no_instances_no_approval(db: &Db, run_id: &str) {
    let run = db.run_by_id(run_id).unwrap().unwrap();
    assert!(
        run.review_approved_head_sha.is_none(),
        "interrupted reconciliation must not approve: {:?}",
        run.review_approved_head_sha
    );
    let rounds = rounds::rounds_for_run(db, run_id).unwrap();
    assert_eq!(rounds.len(), 1, "expected one round, got {rounds:?}");
    let r = &rounds[0];
    assert_eq!(r.execution, ExecutionState::Interrupted);
    assert_eq!(r.assurance_completion, AssuranceCompletion::Incomplete);
    assert_eq!(r.completion_reason.as_deref(), Some("process_interrupted"));
    assert!(r.finalized_at.is_some());
    assert!(
        rounds::instances_for_round(db, &r.id).unwrap().is_empty(),
        "interrupted round must have no finding instances"
    );
    assert!(
        rounds::coverage_for_round(db, &r.id).unwrap().is_empty(),
        "interrupted round must have no coverage rows"
    );
}

fn open_stale_round(db: &Db, run_id: &str) -> rounds::RoundId {
    let inventory = b"stale-inv\n";
    let digest = sha256_hex(inventory);
    let intent = capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    let plan = OpenRoundPlan {
        run_id: run_id.to_string(),
        producers: vec![ProducerInvocation {
            descriptor_json: r#"{"adapter_kind":"porch_json_cli"}"#.into(),
            descriptor_equivalence_digest: "equiv-stale".into(),
        }],
    };
    let bindings = RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest,
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: 1,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent],
        context_applications: vec![ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: ContextApplicationState::Applied,
            effective_digest: Some(context_applicability_digest("intent", "present", inventory)),
        }],
    };
    rounds::open_round(db, &plan, &bindings).unwrap()
}

fn restart_clean(s: &Setup) {
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "clean",
        &s.path,
        DaemonOpts::default(),
    );
}

fn wait_file(path: &Path, timeout: Duration) {
    let start = Instant::now();
    loop {
        if path.exists() {
            return;
        }
        assert!(start.elapsed() <= timeout, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_round_interrupted(db: &Db, run_id: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        let rounds = rounds::rounds_for_run(db, run_id).unwrap();
        if rounds
            .first()
            .is_some_and(|r| r.execution == ExecutionState::Interrupted)
        {
            return;
        }
        assert!(
            start.elapsed() <= timeout,
            "round not reconciled: {rounds:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_open_pending_round(db: &Db, run_id: &str) {
    let before = rounds::rounds_for_run(db, run_id).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].execution, ExecutionState::Running);
    assert_eq!(before[0].assurance_completion, AssuranceCompletion::Pending);
}

fn boundary_post_open() {
    let s = setup("clean");
    kill_daemon(&s.home);
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "clean",
        &s.path,
        DaemonOpts {
            fault: FaultHook::AbortAfterOpen,
            ..DaemonOpts::default()
        },
    );
    commit_change(&s.work, "post-open.txt", "x\n");
    push_branch(&s, "feat-kill-post-open", "clean");
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert_open_pending_round(&db, &run.id);
    assert!(!s.home.join("review-spawned").exists());
    kill_daemon(&s.home);
    restart_clean(&s);
    assert_interrupted_incomplete_no_instances_no_approval(&db, &run.id);
    kill_daemon(&s.home);
}

fn boundary_mid_producer() {
    let s = setup("hang");
    commit_change(&s.work, "mid.txt", "x\n");
    push_branch(&s, "feat-kill-mid", "hang");
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    wait_file(&s.home.join("review-spawned"), Duration::from_secs(15));
    let run = db
        .runs_for_repo(&repo_id)
        .unwrap()
        .last()
        .expect("run")
        .clone();
    assert_open_pending_round(&db, &run.id);
    kill_daemon(&s.home);
    restart_clean(&s);
    wait_round_interrupted(&db, &run.id, Duration::from_secs(10));
    assert_interrupted_incomplete_no_instances_no_approval(&db, &run.id);
    kill_daemon(&s.home);
}

fn boundary_pre_finalize() {
    let s = setup("clean");
    kill_daemon(&s.home);
    restart_daemon(
        &s.home,
        &s.fake_review,
        &s.fake_fixer,
        &s.fake_gh,
        "clean",
        &s.path,
        DaemonOpts {
            fault: FaultHook::AbortBeforeFinalize,
            ..DaemonOpts::default()
        },
    );
    commit_change(&s.work, "pre-final.txt", "x\n");
    push_branch(&s, "feat-kill-pre-final", "clean");
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(20));
    assert!(s.home.join("review-spawned").exists());
    assert_open_pending_round(&db, &run.id);
    kill_daemon(&s.home);
    restart_clean(&s);
    assert_interrupted_incomplete_no_instances_no_approval(&db, &run.id);
    kill_daemon(&s.home);
}

#[test]
fn killed_at_each_boundary_reconciles_to_interrupted_incomplete() {
    boundary_post_open();
    boundary_mid_producer();
    boundary_pre_finalize();
}

#[test]
fn reconcile_stale_uses_at_most_one_committed_write_per_round() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
        .unwrap();
    let mut round_ids = Vec::new();
    for i in 0..3 {
        let run = db
            .insert_run("repo1", &format!("feat-{i}"), "deadbeef", None, None)
            .unwrap();
        round_ids.push(open_stale_round(&db, &run.id));
    }

    rounds::reset_committed_write_count();
    let n = rounds::reconcile_stale(&db).unwrap();
    assert_eq!(n, 3);
    let writes = rounds::take_committed_write_count();
    assert_eq!(
        writes, 3,
        "expected one committed write per stale round, got {writes}"
    );
    for id in &round_ids {
        let loaded = rounds::get_round(&db, id).unwrap().unwrap();
        assert_eq!(loaded.execution, ExecutionState::Interrupted);
        assert_eq!(loaded.assurance_completion, AssuranceCompletion::Incomplete);
        assert!(rounds::instances_for_round(&db, id).unwrap().is_empty());
    }

    rounds::reset_committed_write_count();
    let n2 = rounds::reconcile_stale(&db).unwrap();
    assert_eq!(n2, 0);
    assert_eq!(rounds::take_committed_write_count(), 0);
}

#[test]
fn startup_recovers_stale_runs_and_refuses_when_recovery_fails() {
    // Still recovers stale running runs (and their open rounds).
    {
        let s = setup("clean");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        // Ensure the bare has objects for worktree_add_detach.
        commit_change(&s.work, "seed-stale.txt", "s\n");
        push_branch(&s, "feat-seed-stale", "clean");
        let _ = wait_status(
            &db,
            &repo_id,
            &["parked", "completed", "failed"],
            Duration::from_secs(20),
        );

        let repo = db.repo_by_id(&repo_id).unwrap().unwrap();
        let sha = {
            let out = StdCommand::new("git")
                .current_dir(&s.work)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let run = db
            .insert_run(&repo_id, "stale-running", &sha, None, None)
            .unwrap();
        let wt = run_worktree_dir(&s.home, &repo_id, &run.id);
        db.set_worktree_dir(&run.id, &wt).unwrap();
        db.set_run_status(&run.id, "running", None).unwrap();
        worktree_add_detach(&GitDir::new(&repo.bare_path).unwrap(), &wt, &sha).unwrap();
        let round_id = open_stale_round(&db, &run.id);
        assert!(wt.exists());

        kill_daemon(&s.home);
        restart_daemon(
            &s.home,
            &s.fake_review,
            &s.fake_fixer,
            &s.fake_gh,
            "clean",
            &s.path,
            DaemonOpts::default(),
        );

        let start = Instant::now();
        let failed = loop {
            let r = db.run_by_id(&run.id).unwrap().unwrap();
            if r.status == "failed" {
                break r;
            }
            assert!(
                start.elapsed() <= Duration::from_secs(10),
                "stale run not recovered: {} {:?}",
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
        assert!(!wt.exists(), "stale worktree must be removed");
        let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
        assert_eq!(loaded.execution, ExecutionState::Interrupted);
        assert_eq!(loaded.assurance_completion, AssuranceCompletion::Incomplete);
        kill_daemon(&s.home);
    }

    // Refuse to serve when recover_stale fails.
    {
        let s = setup("clean");
        kill_daemon(&s.home);
        let sock = s.home.join("daemon.sock");
        let _ = std::fs::remove_file(&sock);
        restart_daemon(
            &s.home,
            &s.fake_review,
            &s.fake_fixer,
            &s.fake_gh,
            "clean",
            &s.path,
            DaemonOpts {
                fault: FaultHook::FailRecover,
                wait_health: false,
                ..DaemonOpts::default()
            },
        );
        let health = porch_gate::wait_for_health(&s.home, Duration::from_secs(2));
        assert!(
            health.is_err(),
            "daemon must refuse to serve when recovery fails"
        );
        kill_daemon(&s.home);
    }
}

#[test]
fn parked_round_serves_findings_from_applicable_round() {
    let s = setup("blocking");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_branch(&s, "feat-round-read", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(rounds.len(), 1);
    let round_id = rounds[0].id.as_str().to_string();
    let instances = rounds::instances_for_round(&db, &rounds[0].id).unwrap();
    assert!(!instances.is_empty());
    assert!(
        run.findings_json.is_none(),
        "finalized rounds must not write findings_json: {:?}",
        run.findings_json
    );

    let snap = porch_gate::get_run(&s.home, &run.id).unwrap();
    assert_eq!(snap.assurance_record.kind_str(), "round");
    assert_eq!(
        snap.assurance_record.review_round_id().unwrap(),
        round_id.as_str()
    );
    assert!(snap.assurance_record.audit_identity_available());

    let findings = snap.findings.as_array().expect("findings array");
    assert_eq!(findings.len(), instances.len());
    assert_eq!(findings[0]["id"], "f0");
    assert_eq!(findings[0]["path"], instances[0].path);
    assert_eq!(findings[0]["message"], instances[0].evidence);
    assert!(findings[0].get("criterion_id").is_none());
    assert!(findings[0].get("fingerprint").is_none());

    let hunk = porch_gate::get_finding_hunk(&s.home, &run.id, "f0").unwrap();
    assert!(hunk.get("error").is_none(), "hunk={hunk}");
    assert_eq!(hunk["path"], instances[0].path);

    let status = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(status.status.success());
    let v: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(v["assurance_record"]["kind"], "round");
    assert_eq!(v["assurance_record"]["review_round_id"], round_id);
    assert!(!v["findings"].as_array().unwrap().is_empty());

    kill_daemon(&s.home);
}

#[test]
fn inapplicable_finalized_round_is_not_served() {
    let s = setup("blocking");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_branch(&s, "feat-inapplicable-serve", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(
        rounds[0].assurance_completion,
        AssuranceCompletion::Complete
    );
    let before = porch_gate::get_run(&s.home, &run.id).unwrap();
    assert_eq!(before.assurance_record.kind_str(), "round");

    // Drift trusted_config on the run so reconstructed bindings no longer match.
    db.set_trusted_config_sha(&run.id, &"0".repeat(40)).unwrap();
    let after = porch_gate::get_run(&s.home, &run.id).unwrap();
    assert_ne!(
        after.assurance_record.kind_str(),
        "round",
        "inapplicable complete round must not back assurance_record: {:?}",
        after.assurance_record
    );
    assert!(
        after
            .findings
            .as_array()
            .is_none_or(std::vec::Vec::is_empty),
        "must not project instances from an inapplicable round: {:?}",
        after.findings
    );

    kill_daemon(&s.home);
}

fn strip_rounds_for_run(db: &Db, run_id: &str) {
    porch_gate::clear_rounds_for_run(db, run_id).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn legacy_parked_run_answers_actions_and_unreviewed_is_none() {
    // Pre-migration shape: parked review findings in findings_json, no round rows.
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug.txt", "boom\n");
        push_branch(&s, "feat-legacy-label", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        let instances = {
            let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
            rounds::instances_for_round(&db, &rounds[0].id).unwrap()
        };
        let legacy_json = serde_json::json!([{
            "id": "f0",
            "path": instances[0].path,
            "message": instances[0].evidence,
            "severity": instances[0].severity,
            "action": instances[0].action,
            "start_line": 1,
            "end_line": 2
        }])
        .to_string();
        strip_rounds_for_run(&db, &run.id);
        db.set_findings_json(&run.id, Some(&legacy_json)).unwrap();
        assert!(rounds::rounds_for_run(&db, &run.id).unwrap().is_empty());

        let snap = porch_gate::get_run(&s.home, &run.id).unwrap();
        assert_eq!(snap.assurance_record.kind_str(), "legacy_snapshot");
        assert!(snap.assurance_record.review_round_id().is_none());
        assert!(!snap.assurance_record.audit_identity_available());
        assert_eq!(snap.findings[0]["message"], instances[0].evidence);
        assert!(snap.findings[0].get("criterion_id").is_none());

        let hunk = porch_gate::get_finding_hunk(&s.home, &run.id, "f0").unwrap();
        assert!(hunk.get("error").is_none(), "hunk={hunk}");

        porch_gate::set_finding_note(&s.home, &run.id, "f0", "operator note").unwrap();
        let notes = porch_gate::load_finding_notes(&s.home, &run.id).unwrap();
        assert_eq!(notes.get("f0").map(String::as_str), Some("operator note"));

        let abort = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .args(["agent", "respond", "abort", "--run-id", &run.id])
            .output()
            .unwrap();
        let abort_v: Value = serde_json::from_slice(&abort.stdout).unwrap();
        assert_eq!(
            abort_v["status"],
            "cancelled",
            "{}",
            String::from_utf8_lossy(&abort.stdout)
        );
        assert_eq!(db.run_by_id(&run.id).unwrap().unwrap().status, "cancelled");
        kill_daemon(&s.home);
    }

    // Approve on a legacy parked run.
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug.txt", "boom\n");
        push_branch(&s, "feat-legacy-approve", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        db.set_findings_json(
            &run.id,
            Some(
                r#"[{"id":"f0","path":"bug.txt","message":"legacy","severity":"warning","action":"ask-user"}]"#,
            ),
        )
        .unwrap();
        strip_rounds_for_run(&db, &run.id);

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .env(GH_BIN_ENV, &s.fake_gh)
            .env("PATH", &s.path)
            .args(["agent", "respond", "approve", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        kill_daemon(&s.home);
    }

    // Skip on a legacy parked run.
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug.txt", "boom\n");
        push_branch(&s, "feat-legacy-skip", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        db.set_findings_json(
            &run.id,
            Some(
                r#"[{"id":"f0","path":"bug.txt","message":"legacy","severity":"warning","action":"ask-user"}]"#,
            ),
        )
        .unwrap();
        strip_rounds_for_run(&db, &run.id);

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .args(["agent", "respond", "skip", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            db.run_by_id(&run.id)
                .unwrap()
                .unwrap()
                .review_approved_head_sha
                .is_none()
        );
        kill_daemon(&s.home);
    }

    // Fix on a legacy parked run.
    {
        let s = setup("blocking");
        commit_change(&s.work, "bug.txt", "boom\n");
        push_branch(&s, "feat-legacy-fix", "blocking");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
        db.set_findings_json(
            &run.id,
            Some(
                r#"[{"id":"f0","path":"bug.txt","message":"legacy","severity":"warning","action":"ask-user","start_line":1,"end_line":1}]"#,
            ),
        )
        .unwrap();
        strip_rounds_for_run(&db, &run.id);

        let out = Command::cargo_bin("porch")
            .unwrap()
            .current_dir(&s.work)
            .env("PORCH_HOME", &s.home)
            .env(REVIEW_BIN_ENV, &s.fake_review)
            .env(FIXER_BIN_ENV, &s.fake_fixer)
            .env(GH_BIN_ENV, &s.fake_gh)
            .env("PORCH_FAKE_REVIEW_MODE", "clean")
            .env("PORCH_FAKE_FIXER_MODE", "apply")
            .env("PATH", &s.path)
            .args(["agent", "respond", "fix", "--run-id", &run.id])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fix failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        kill_daemon(&s.home);
    }

    // Unreviewed run → none
    {
        let s = setup("clean");
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&s.work);
        let run = db
            .insert_run(&repo_id, "feat-none", "deadbeef", None, None)
            .unwrap();
        let (record, findings) = porch_gate::resolve_run_assurance(&db, &run).unwrap();
        assert_eq!(record.kind_str(), "none");
        assert!(record.review_round_id().is_none());
        assert!(!record.audit_identity_available());
        assert!(findings.is_empty());
        kill_daemon(&s.home);
    }
}

#[test]
fn legacy_finding_dto_ignores_enriched_fields() {
    let raw = r#"{"id":"f0","path":"a.rs","message":"x","severity":"warning","action":"ask-user","criterion_id":"rust/unwrap-in-lib","evidence":"e","fingerprint":"fp"}"#;
    let dto: porch_gate::LegacyFindingDto = serde_json::from_str(raw).unwrap();
    let v = serde_json::to_value(&dto).unwrap();
    assert_eq!(v["id"], "f0");
    assert_eq!(v["path"], "a.rs");
    assert_eq!(v["message"], "x");
    assert!(v.get("criterion_id").is_none());
    assert!(v.get("evidence").is_none());
    assert!(v.get("fingerprint").is_none());
    assert!(v.get("consequence").is_none());
    assert!(v.get("provenance").is_none());
    assert!(v.get("confidence").is_none());
    assert!(v.get("anchor_kind").is_none());
}

#[test]
fn repo_id_for_is_stable_for_same_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("repo");
    std::fs::create_dir_all(&path).unwrap();
    let a = repo_id_for(&path);
    let b = repo_id_for(&path);
    assert_eq!(a, b);
    assert_eq!(a.len(), 12);
    let abs = path.canonicalize().unwrap();
    assert_eq!(repo_id_for(&abs), a);
}
