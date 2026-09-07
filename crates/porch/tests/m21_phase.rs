//! M21: phase-event history — status/step writes co-committed with phase events.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds;
use porch_gate::{Db, build_audit, get_run, kill_group, repo_id_for};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use rusqlite::Connection;
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

fn chmod_755(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

fn install_fake_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-review");
    std::fs::write(
        &path,
        r#"#!/bin/sh
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
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

fn install_fake_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    std::fs::write(
        &path,
        r#"#!/bin/sh
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
MODE="${PORCH_FAKE_FIXER_MODE:-noop}"
case "$MODE" in
  noop)
    printf '{"summary":"noop","session_id":"sess-1"}\n'
    ;;
  *)
    echo "unknown PORCH_FAKE_FIXER_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

#[allow(clippy::too_many_lines)]
fn install_fake_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
LOG="$PORCH_HOME/gh-argv.log"
{
  printf '+'
  for a in "$@"; do
    printf ' %s' "$a"
  done
  printf '\n'
} >> "$LOG"
for a in "$@"; do
  if [ "$a" = "--version" ]; then
    echo "gh version 2.50.0 (fake)"
    exit 0
  fi
done
STATE="$PORCH_HOME/gh-pr-state"
BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
TITLE_FILE="$PORCH_HOME/gh-pr-title.txt"
CMD=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then
    CMD="$a"
    break
  fi
  PREV="$a"
done
case "$CMD" in
  list)
    if [ -f "$STATE" ]; then cat "$STATE"; else printf '[]\n'; fi
    exit 0
    ;;
  create)
    cat > "$BODY_FILE"
    TITLE=""
    PREV=""
    for a in "$@"; do
      if [ "$PREV" = "--title" ]; then TITLE="$a"; break; fi
      PREV="$a"
    done
    printf '%s\n' "$TITLE" > "$TITLE_FILE"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"%s"}]\n' "$TITLE" > "$STATE"
    echo "https://example.com/pull/1"
    exit 0
    ;;
  edit)
    HAS_BODY=0
    PREV=""
    for a in "$@"; do
      if [ "$a" = "--body-file" ]; then HAS_BODY=1; fi
      if [ "$PREV" = "--title" ]; then printf '%s\n' "$a" > "$TITLE_FILE"; fi
      PREV="$a"
    done
    if [ "$HAS_BODY" -eq 1 ]; then cat > "$BODY_FILE"; fi
    exit 0
    ;;
  view)
    BODY=""
    if [ -f "$BODY_FILE" ]; then BODY=$(cat "$BODY_FILE"); fi
    TITLE="porch: existing"
    if [ -f "$TITLE_FILE" ]; then TITLE=$(cat "$TITLE_FILE"); fi
    BODY_ESC=$(printf '%s' "$BODY" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null || printf '%s' "$BODY" | sed 's/"/\\"/g' | tr '\n' ' ')
    if echo "$*" | grep -q mergeable; then
      printf '{"mergeable":"MERGEABLE","title":"%s","body":"%s"}\n' "$TITLE" "$BODY_ESC"
    else
      printf '{"title":"%s","body":"%s","url":"https://example.com/pull/1","number":1}\n' "$TITLE" "$BODY_ESC"
    fi
    exit 0
    ;;
  checks)
    printf '[{"name":"lint","state":"success","bucket":"pass"}]\n'
    exit 0
    ;;
  *)
    echo "fake-gh: unhandled: $*" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
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
    review_mode: String,
}

fn setup() -> Setup {
    setup_with_review_mode("clean")
}

fn setup_with_review_mode(review_mode: &str) -> Setup {
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
    let fake_gh = install_fake_gh(&bin_dir);
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
        .env("PORCH_FAKE_FIXER_MODE", "noop")
        .env("PORCH_FAKE_GH_MODE", "ok")
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();
    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake_review.as_os_str()),
            (FIXER_BIN_ENV, fake_fixer.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", review_mode.as_ref()),
            ("PORCH_FAKE_FIXER_MODE", "noop".as_ref()),
            ("PORCH_FAKE_GH_MODE", "ok".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
            ("PORCH_FIXER_TIMEOUT_SECS", "10".as_ref()),
            ("PORCH_GH_TIMEOUT_SECS", "10".as_ref()),
            ("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3".as_ref()),
            ("PORCH_DELIVER_CHECK_POLL_SECS", "1".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();
    Setup {
        _tmp: tmp,
        work,
        home,
        fake_review,
        fake_fixer,
        fake_gh,
        path,
        review_mode: review_mode.to_string(),
    }
}

fn push_feat(s: &Setup, branch: &str, intent: Option<&str>) {
    let mut cmd = StdCommand::new("git");
    cmd.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", &s.review_mode)
        .env("PORCH_FAKE_FIXER_MODE", "noop")
        .env("PORCH_FAKE_GH_MODE", "ok")
        .env("PATH", &s.path);
    if let Some(intent) = intent {
        cmd.env("PORCH_INTENT", intent);
    }
    let out = cmd
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

fn agent_cmd(s: &Setup) -> Command {
    let mut cmd = Command::cargo_bin("porch").unwrap();
    cmd.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", &s.review_mode)
        .env("PORCH_FAKE_FIXER_MODE", "noop")
        .env("PORCH_FAKE_GH_MODE", "ok")
        .env("PORCH_REVIEW_TIMEOUT_SECS", "20")
        .env("PORCH_FIXER_TIMEOUT_SECS", "20")
        .env("PATH", &s.path);
    cmd
}

fn park_compose_run(s: &Setup, branch: &str) -> porch_gate::RunRow {
    git(&s.work, &["checkout", "-b", branch]);
    commit_change(&s.work, &format!("{branch}.txt"), "x\n");
    push_feat(s, branch, Some("phase events beside status"));
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    run
}

fn park_review_run(s: &Setup, branch: &str) -> porch_gate::RunRow {
    git(&s.work, &["checkout", "-b", branch]);
    commit_change(&s.work, &format!("{branch}.txt"), "x\n");
    push_feat(s, branch, Some("review park phase terminal"));
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_some(),
        "blocking review park must leave an open review attempt"
    );
    run
}

#[test]
fn parking_for_compose_leaves_matching_phase_event_beside_status() {
    let s = setup();
    let run = park_compose_run(&s, "feat-phase-park");
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    assert_eq!(run.status, "parked");
    assert_eq!(run.error.as_deref(), Some("awaiting compose"));

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    assert!(
        !events.is_empty(),
        "compose park must leave phase events: {events:?}"
    );
    let attempts = rounds::phase::attempts_for_run(&db, &run.id).unwrap();
    let compose = attempts
        .iter()
        .find(|a| a.operation_kind == Some(rounds::phase::OperationKind::Compose))
        .expect("nested compose attempt");
    assert!(
        events.iter().any(|e| {
            e.attempt_id == compose.id && e.kind == rounds::phase::PhaseEventKind::Started
        }),
        "compose started event missing: {events:?}"
    );
    let open = rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Deliver)
        .unwrap()
        .expect("deliver stays nonterminal while compose parked");
    assert_eq!(compose.parent_attempt_id.as_ref(), Some(&open.id));
    kill_daemon(&s.home);
}

#[test]
fn cancelling_on_agent_abort_leaves_matching_phase_event_beside_status() {
    let s = setup();
    let run = park_compose_run(&s, "feat-phase-abort");
    let out = agent_cmd(&s)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.error.as_deref(), Some("agent abort"));

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    let terminals: Vec<_> = events
        .iter()
        .filter(|e| e.kind == rounds::phase::PhaseEventKind::Terminal)
        .collect();
    assert!(
        terminals.len() >= 2,
        "compose abort must terminal nested compose and deliver: {events:?}"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Deliver)
            .unwrap()
            .is_none()
    );
    kill_daemon(&s.home);
}

#[test]
fn completing_delivery_leaves_matching_phase_event_beside_status() {
    let s = setup();
    let run = park_compose_run(&s, "feat-phase-complete");
    agent_cmd(&s)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
        .assert()
        .success();

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == rounds::phase::PhaseEventKind::Terminal
                && e.outcome.as_deref() == Some("completed")),
        "delivery completion must leave a completed terminal: {events:?}"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Deliver)
            .unwrap()
            .is_none()
    );
    kill_daemon(&s.home);
}

#[test]
fn superseding_by_a_new_push_leaves_matching_phase_event_beside_status() {
    let s = setup();
    let first = park_compose_run(&s, "feat-phase-supersede");
    let first_id = first.id.clone();

    commit_change(&s.work, "second.txt", "y\n");
    push_feat(&s, "feat-phase-supersede", Some("supersede prior"));

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let start = Instant::now();
    let cancelled = loop {
        let run = db.run_by_id(&first_id).unwrap().unwrap();
        if run.status == "cancelled" {
            break run;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "expected first run cancelled by supersede, got {run:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        cancelled.error.as_deref(),
        Some("superseded by new push"),
        "{cancelled:?}"
    );
    let events = rounds::phase::events_for_run(&db, &first_id).unwrap();
    assert!(
        events.iter().any(|e| {
            e.kind == rounds::phase::PhaseEventKind::Terminal
                && (e.outcome.as_deref() == Some("cancelled")
                    || e.cause.as_deref() == Some("superseded by new push"))
        }),
        "supersede must leave a matching phase terminal beside cancelled status: {events:?}"
    );
    kill_daemon(&s.home);
}

#[test]
fn review_park_approve_terminals_review_attempt_beside_status() {
    let s = setup_with_review_mode("blocking");
    let run = park_review_run(&s, "feat-phase-review-approve");
    agent_cmd(&s)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .assert()
        .success();

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert!(
        run.status == "parked" || run.status == "completed",
        "approve should leave parked compose or completed: {run:?}"
    );
    assert!(run.review_approved_head_sha.is_some());

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    let attempts = rounds::phase::attempts_for_run(&db, &run.id).unwrap();
    let review = attempts
        .iter()
        .find(|a| a.phase == rounds::phase::PhaseName::Review && a.parent_attempt_id.is_none())
        .expect("review attempt");
    assert!(
        events.iter().any(|e| {
            e.attempt_id == review.id && e.kind == rounds::phase::PhaseEventKind::Terminal
        }),
        "review approve must terminal the review attempt beside status: events={events:?}"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none(),
        "review must not stay open after approve"
    );
    kill_daemon(&s.home);
}

#[test]
fn review_park_abort_terminals_review_attempt_beside_cancelled_status() {
    let s = setup_with_review_mode("blocking");
    let run = park_review_run(&s, "feat-phase-review-abort");
    let out = agent_cmd(&s)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.error.as_deref(), Some("agent abort"));

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    let attempts = rounds::phase::attempts_for_run(&db, &run.id).unwrap();
    let review = attempts
        .iter()
        .find(|a| a.phase == rounds::phase::PhaseName::Review && a.parent_attempt_id.is_none())
        .expect("review attempt");
    assert!(
        events.iter().any(|e| {
            e.attempt_id == review.id
                && e.kind == rounds::phase::PhaseEventKind::Terminal
                && e.outcome.as_deref() == Some("cancelled")
        }),
        "review abort must terminal the review attempt beside cancelled status: events={events:?}"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none(),
        "review must not stay open after abort"
    );
    kill_daemon(&s.home);
}

#[test]
fn standing_consent_yes_terminals_review_attempt_beside_status() {
    let s = setup_with_review_mode("blocking");
    let run = park_review_run(&s, "feat-phase-standing-yes");
    agent_cmd(&s)
        .args(["agent", "respond", "fix", "--yes", "--run-id", &run.id])
        .assert()
        .success();

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert!(
        run.status == "parked" || run.status == "completed",
        "standing consent should leave parked compose or completed: {run:?}"
    );
    assert!(run.review_approved_head_sha.is_some());

    let events = rounds::phase::events_for_run(&db, &run.id).unwrap();
    let attempts = rounds::phase::attempts_for_run(&db, &run.id).unwrap();
    let review = attempts
        .iter()
        .find(|a| a.phase == rounds::phase::PhaseName::Review && a.parent_attempt_id.is_none())
        .expect("review attempt");
    assert!(
        events.iter().any(|e| {
            e.attempt_id == review.id
                && e.kind == rounds::phase::PhaseEventKind::Terminal
                && e.outcome.as_deref() == Some("completed")
        }),
        "standing consent --yes must terminal the review attempt beside status: events={events:?}"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none(),
        "review must not stay open after standing consent --yes"
    );
    kill_daemon(&s.home);
}

fn seed_running_with_open_review(home: &Path) -> (Db, String) {
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-crash", "deadbeef", None, None)
        .unwrap();
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run.id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();
    let review = rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
        .unwrap()
        .expect("review open");
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: review.id,
            kind: rounds::phase::OperationKind::Fixer,
        },
        rounds::RunEffects::none(),
    )
    .unwrap();
    (db, run.id)
}

#[test]
fn killed_mid_phase_after_restart_reconciles_attempts_and_status() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let (db, run_id) = seed_running_with_open_review(home);

    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_some(),
        "precondition: review left nonterminal mid-phase"
    );

    let closed = rounds::phase::reconcile_interrupted(&db).expect("reconcile");
    assert!(
        closed >= 2,
        "must terminal nested fixer and review, got {closed}"
    );

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("daemon restarted")),
        "status error must name restart, got {:?}",
        run.error
    );

    for phase in [
        rounds::phase::PhaseName::Intent,
        rounds::phase::PhaseName::Rebase,
        rounds::phase::PhaseName::Review,
        rounds::phase::PhaseName::Certify,
        rounds::phase::PhaseName::Deliver,
    ] {
        assert!(
            rounds::phase::nonterminal_attempt(&db, &run_id, phase)
                .unwrap()
                .is_none(),
            "canonical phase {phase:?} must not stay nonterminal after reconcile"
        );
    }

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    let interrupted: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == rounds::phase::PhaseEventKind::Terminal
                && e.outcome.as_deref() == Some("interrupted")
        })
        .collect();
    assert!(
        interrupted.len() >= 2,
        "interrupted terminals must cover nested and parent: {events:?}"
    );
}

#[test]
fn interrupted_terminal_rolls_back_with_status_when_txn_fails() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let (db, run_id) = seed_running_with_open_review(home);
    let before_events = rounds::phase::events_for_run(&db, &run_id).unwrap().len();
    let before = db.run_by_id(&run_id).unwrap().unwrap();

    {
        let conn = rusqlite::Connection::open(home.join("state.sqlite")).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_stale_status BEFORE UPDATE ON runs
            BEGIN
                SELECT RAISE(ABORT, 'forced mid-txn write failure');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = rounds::phase::reconcile_interrupted(&db);
    assert!(
        poisoned.is_err(),
        "injected status write failure must abort reconcile"
    );

    let after = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(after.error, before.error);
    assert_eq!(
        rounds::phase::events_for_run(&db, &run_id).unwrap().len(),
        before_events,
        "rolled-back txn must leave no interrupted terminal"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_some(),
        "review must stay nonterminal when reconcile rolls back"
    );
}

#[test]
fn parked_run_reports_phase_from_the_log() {
    let s = setup_with_review_mode("blocking");
    let run = park_review_run(&s, "feat-phase-snap-review");
    let open = {
        let db = Db::open(&s.home.join("state.sqlite")).unwrap();
        rounds::phase::nonterminal_attempt(&db, &run.id, rounds::phase::PhaseName::Review)
            .unwrap()
            .expect("review park leaves a nonterminal review attempt")
    };

    let snap = get_run(&s.home, &run.id).unwrap();
    let phase = snap
        .phase
        .as_ref()
        .expect("parked run with a nonterminal attempt must expose compact phase");
    assert_eq!(phase.phase, "review");
    assert_eq!(phase.ordinal, open.ordinal);
    assert!(
        phase.operation.is_none(),
        "review park has no nested operation"
    );

    let out = agent_cmd(&s)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["phase"], "review", "{status}");
    kill_daemon(&s.home);
}

#[test]
fn parked_without_nonterminal_reports_phase_unavailable() {
    let s = setup();
    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = db
        .insert_run(&repo_id, "feat-phase-unavailable", "deadbeef", None, None)
        .unwrap();
    Connection::open(s.home.join("state.sqlite"))
        .unwrap()
        .execute(
            "UPDATE runs SET status = 'parked', error = 'forced park without phase log' WHERE id = ?1",
            [&run.id],
        )
        .unwrap();

    let snap = get_run(&s.home, &run.id).unwrap();
    assert_eq!(snap.status, "parked");
    assert!(
        snap.phase.is_none(),
        "parked run with no nonterminal attempt must leave phase unavailable, got {:?}",
        snap.phase
    );

    let out = agent_cmd(&s)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(
        status["phase"], "review",
        "must not invent review when the log has no nonterminal: {status}"
    );
    assert_eq!(status["phase"], "unavailable", "{status}");
    kill_daemon(&s.home);
}

#[test]
fn agent_status_keeps_shape_while_phase_comes_from_snapshot_field() {
    let s = setup();
    let run = park_compose_run(&s, "feat-phase-status-shape");

    let snap = get_run(&s.home, &run.id).unwrap();
    let phase = snap
        .phase
        .as_ref()
        .expect("compose park must expose compact phase from the log");
    assert_eq!(phase.phase, "deliver");
    assert_eq!(phase.operation.as_deref(), Some("compose"));

    let out = agent_cmd(&s)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for key in [
        "run_id",
        "repo_id",
        "branch",
        "status",
        "phase",
        "head_sha",
        "base_sha",
        "review_approved_head_sha",
        "findings",
        "assurance_record",
    ] {
        assert!(
            status.get(key).is_some(),
            "missing frozen key {key}: {status}"
        );
    }
    assert_eq!(status["phase"], "compose", "{status}");
    assert_eq!(status["status"], "parked");
    assert!(
        status.get("steps").is_none(),
        "agent status must stay compact: {status}"
    );
    kill_daemon(&s.home);
}

/// Seed a running deliver attempt with `repairs` nested `deliver_repair` starts.
/// When `finish_all` is false, the last nested start is left nonterminal (mid-flight).
fn seed_deliver_with_repairs(home: &Path, repairs: usize, finish_all: bool) -> (Db, String) {
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-repair-budget", "deadbeef", None, None)
        .unwrap();
    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run.id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();
    for i in 0..repairs {
        let nested = rounds::phase::persist_phase_transition(
            &db,
            rounds::phase::PhaseTransition::NestedStart {
                parent: deliver.clone(),
                kind: rounds::phase::OperationKind::DeliverRepair,
            },
            rounds::RunEffects::none(),
        )
        .unwrap();
        let finish = finish_all || i + 1 < repairs;
        if finish {
            rounds::phase::persist_phase_transition(
                &db,
                rounds::phase::PhaseTransition::NestedTerminal {
                    attempt: nested,
                    outcome: "completed".into(),
                    cause: Some(format!("attempt {}", i + 1)),
                },
                rounds::RunEffects::none(),
            )
            .unwrap();
        }
    }
    (db, run.id)
}

#[test]
fn budgeted_started_repairs_refuse_another_with_exhausted_cause() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_deliver_with_repairs(tmp.path(), 3, true);

    // Column left at default 0 — enforcement must not read it.
    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.deliver_repair_attempts, 0);
    assert_eq!(
        rounds::phase::repair_attempts_started(&db, &run_id).unwrap(),
        3,
        "budget is the count of nested deliver_repair started events"
    );

    let deliver =
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Deliver)
            .unwrap()
            .expect("deliver still open when budget blocks another repair");
    let cause = "deliver repair budget exhausted (3)";
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Terminal {
            attempt: deliver.id,
            outcome: "failed".into(),
            cause: Some(cause.into()),
        },
        rounds::RunEffects {
            status: Some("failed".into()),
            error: Some(cause.into()),
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();

    assert_eq!(
        rounds::phase::repair_attempts_started(&db, &run_id).unwrap(),
        3,
        "refuse must not start a fourth nested repair"
    );
    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("budget exhausted")),
        "run must carry budget-exhausted cause, got {:?}",
        run.error
    );
    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert!(
        events.iter().any(|e| {
            e.kind == rounds::phase::PhaseEventKind::Terminal
                && e.outcome.as_deref() == Some("failed")
                && e.cause
                    .as_deref()
                    .is_some_and(|c| c.contains("budget exhausted"))
        }),
        "deliver terminal must carry budget-exhausted cause: {events:?}"
    );
}

#[test]
fn kill_between_repair_count_and_finish_never_exceeds_budget_after_restart() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let (db, run_id) = seed_deliver_with_repairs(home, 3, false);

    assert_eq!(
        rounds::phase::repair_attempts_started(&db, &run_id).unwrap(),
        3,
        "unfinished started repairs still count toward the budget"
    );
    assert_eq!(
        db.run_by_id(&run_id)
            .unwrap()
            .unwrap()
            .deliver_repair_attempts,
        0,
        "column must not be the budget source"
    );

    drop(db);
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.fail_stale_running("daemon restarted while run was in progress")
        .unwrap();

    assert_eq!(
        rounds::phase::repair_attempts_started(&db, &run_id).unwrap(),
        3,
        "restart must not permit more than the budget of started repairs"
    );
    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    let repair_starts = attempts
        .iter()
        .filter(|a| a.operation_kind == Some(rounds::phase::OperationKind::DeliverRepair))
        .count();
    assert_eq!(repair_starts, 3);
}

fn seed_deliver_with_open_compose(home: &Path) -> (Db, String) {
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-audit-tree", "deadbeef", None, None)
        .unwrap();
    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run.id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("parked".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: deliver,
            kind: rounds::phase::OperationKind::Compose,
        },
        rounds::RunEffects::none(),
    )
    .unwrap();
    (db, run.id)
}

#[test]
fn audit_phase_tree_names_event_source_and_nests_operations() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_deliver_with_open_compose(tmp.path());

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.schema_version, 2);
    assert_eq!(doc.phase.kind, "phase_events");
    assert_eq!(
        doc.phase.attempts.len(),
        1,
        "top-level roots only: {:?}",
        doc.phase.attempts
    );
    let deliver = &doc.phase.attempts[0];
    assert_eq!(deliver.phase, "deliver");
    assert_eq!(deliver.ordinal, 1);
    assert!(deliver.operation.is_none());
    assert!(deliver.parent_id.is_none());
    assert!(
        deliver.terminal.is_none(),
        "deliver stays nonterminal while compose is open"
    );
    assert_eq!(deliver.children.len(), 1, "compose nested under deliver");
    let compose = &deliver.children[0];
    assert_eq!(compose.phase, "deliver");
    assert_eq!(compose.operation.as_deref(), Some("compose"));
    assert_eq!(compose.parent_id.as_deref(), Some(deliver.id.as_str()));
    assert!(compose.children.is_empty());
}

#[test]
fn audit_phase_without_events_is_explicitly_unavailable() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-audit-unavailable", "deadbeef", None, None)
        .unwrap();
    // Compatibility step_results alone must not synthesize a phase tree.
    let raw = Connection::open(home.join("state.sqlite")).unwrap();
    raw.execute(
        "INSERT INTO step_results (id, run_id, step, status, error, created_at)
         VALUES ('s1', ?1, 'review', 'completed', NULL, '1')",
        [&run.id],
    )
    .unwrap();
    drop(raw);

    let doc = build_audit(&db, &run.id).unwrap();
    assert_eq!(doc.schema_version, 2);
    assert_eq!(doc.phase.kind, "unavailable");
    assert!(
        doc.phase.attempts.is_empty(),
        "unavailable must not invent attempts: {:?}",
        doc.phase.attempts
    );
    assert!(
        doc.phase.steps.is_empty(),
        "unavailable must not invent steps: {:?}",
        doc.phase.steps
    );
}

#[test]
fn audit_phase_steps_rebuild_from_terminal_and_evidence_events() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-audit-steps", "deadbeef", None, None)
        .unwrap();

    let review = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run.id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Evidence {
            attempt: review.clone(),
            cause: "AllowlistFailed".into(),
        },
        rounds::RunEffects {
            status: None,
            error: None,
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "review".into(),
                status: "failed".into(),
                error: Some("AllowlistFailed".into()),
            }],
        },
    )
    .unwrap();
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Terminal {
            attempt: review,
            outcome: "completed".into(),
            cause: Some("operator_approve".into()),
        },
        rounds::RunEffects {
            status: Some("completed".into()),
            error: None,
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "review".into(),
                status: "completed".into(),
                error: Some("operator_approve".into()),
            }],
        },
    )
    .unwrap();

    let doc = build_audit(&db, &run.id).unwrap();
    assert_eq!(doc.phase.kind, "phase_events");
    assert_eq!(
        doc.phase.steps,
        vec![
            porch_gate::AuditStep {
                step: "review".into(),
                status: "evidence".into(),
                error: Some("AllowlistFailed".into()),
            },
            porch_gate::AuditStep {
                step: "review".into(),
                status: "completed".into(),
                error: Some("operator_approve".into()),
            },
        ],
        "steps project terminal/evidence events in seq order"
    );
}

#[test]
fn active_run_audit_is_partial_as_of_watermark() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_deliver_with_open_compose(tmp.path());

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.run_status, "parked");
    assert_eq!(
        doc.completeness, "as_of",
        "active run must be labelled partial as-of its watermark"
    );
    assert!(
        doc.watermark.audit_rev >= 0,
        "as-of document carries durable audit_rev"
    );
    assert_eq!(doc.phase.kind, "phase_events");
}

#[test]
fn audit_build_for_200_phase_events_is_fast_and_index_backed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-audit-perf", "deadbeef", None, None)
        .unwrap();

    // Deliver start + 100 nested start/terminal pairs ≥ 200 phase events.
    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run.id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .unwrap();
    for i in 0..100 {
        let nested = rounds::phase::persist_phase_transition(
            &db,
            rounds::phase::PhaseTransition::NestedStart {
                parent: deliver.clone(),
                kind: rounds::phase::OperationKind::DeliverRepair,
            },
            rounds::RunEffects::none(),
        )
        .unwrap();
        rounds::phase::persist_phase_transition(
            &db,
            rounds::phase::PhaseTransition::NestedTerminal {
                attempt: nested,
                outcome: "completed".into(),
                cause: Some(format!("seed {i}")),
            },
            rounds::RunEffects::none(),
        )
        .unwrap();
    }
    let event_count = rounds::phase::events_for_run(&db, &run.id).unwrap().len();
    assert!(
        event_count >= 200,
        "need ≥200 phase events for the latency target, got {event_count}"
    );

    // Warm the page cache, then measure.
    let _ = build_audit(&db, &run.id).unwrap();
    let start = Instant::now();
    let doc = build_audit(&db, &run.id).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(doc.phase.kind, "phase_events");
    assert!(
        elapsed < Duration::from_millis(100),
        "build_audit for {event_count} events took {elapsed:?}, want <100ms"
    );

    let raw = Connection::open(home.join("state.sqlite")).unwrap();
    let mut stmt = raw
        .prepare("EXPLAIN QUERY PLAN SELECT id, run_id, attempt_id, seq, kind, outcome, cause, created_at FROM phase_events WHERE run_id = ?1 ORDER BY seq, id")
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map([&run.id], |row| row.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let plan_text = plan.join(" | ");
    assert!(
        plan_text.contains("phase_events_run")
            || plan_text.to_ascii_lowercase().contains("using index"),
        "phase_events query must be index-backed, plan={plan_text:?}"
    );
}
