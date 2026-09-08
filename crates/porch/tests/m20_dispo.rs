//! Disposition history: review approve/skip/fix authority events.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{self, ActorKind, AuthorityKind, MemberRole};
use porch_gate::{
    Db, build_audit, clear_rounds_for_run, get_audit, get_run, kill_group, repo_id_for,
    round_for_decision,
};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use rusqlite::Connection;
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
: "${PORCH_HOME:?}"
# Record whether fix_requested already exists when the fixer binary starts.
python3 - <<'PY'
import os, sqlite3
home = os.environ["PORCH_HOME"]
db = sqlite3.connect(os.path.join(home, "state.sqlite"))
n = db.execute(
    "SELECT COUNT(*) FROM authority_events WHERE kind = 'fix_requested'"
).fetchone()[0]
with open(os.path.join(home, "fixer-saw-fix-requested"), "w", encoding="utf-8") as f:
    f.write(str(n))
open(os.path.join(home, "fixer-invoked"), "w", encoding="utf-8").write("1")
PY
MODE="${PORCH_FAKE_FIXER_MODE:-noop}"
case "$MODE" in
  noop)
    printf '{"summary":"noop","session_id":"sess-1"}\n'
    ;;
  drop-cite)
    # Test seam: remove durable fix_requested so standing consent cannot cite it.
    python3 - <<'PY'
import os, sqlite3
home = os.environ["PORCH_HOME"]
db = sqlite3.connect(os.path.join(home, "state.sqlite"))
db.execute(
    "DELETE FROM authority_event_members WHERE event_id IN "
    "(SELECT id FROM authority_events WHERE kind = 'fix_requested')"
)
db.execute("DELETE FROM authority_events WHERE kind = 'fix_requested'")
db.commit()
PY
    printf '{"summary":"noop-drop-cite","session_id":"sess-1"}\n'
    ;;
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
        .env(REVIEW_BIN_ENV, &fake)
        .env(FIXER_BIN_ENV, &fake_fixer)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", mode)
        .env("PORCH_FAKE_FIXER_MODE", "noop")
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
            (REVIEW_BIN_ENV, fake.as_os_str()),
            (FIXER_BIN_ENV, fake_fixer.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", mode.as_ref()),
            ("PORCH_FAKE_FIXER_MODE", "noop".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "5".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    (tmp, work, home, origin, fake)
}

fn agent_fix(
    work: &Path,
    home: &Path,
    fake: &Path,
    run_id: &str,
    extra: &[&str],
) -> std::process::Output {
    agent_fix_modes(work, home, fake, run_id, extra, "blocking", "noop")
}

fn agent_fix_modes(
    work: &Path,
    home: &Path,
    fake: &Path,
    run_id: &str,
    extra: &[&str],
    review_mode: &str,
    fixer_mode: &str,
) -> std::process::Output {
    let fake_fixer = fake.parent().unwrap().join("fake-fixer");
    let fake_gh = fake.parent().unwrap().join("fake-gh");
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut args = vec!["agent", "respond", "fix", "--run-id", run_id];
    args.extend_from_slice(extra);
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(work)
        .env("PORCH_HOME", home)
        .env(REVIEW_BIN_ENV, fake)
        .env(FIXER_BIN_ENV, &fake_fixer)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PORCH_FAKE_FIXER_MODE", fixer_mode)
        .env("PORCH_REVIEW_TIMEOUT_SECS", "20")
        .env("PORCH_FIXER_TIMEOUT_SECS", "20")
        .env("PATH", &path)
        .args(&args)
        .output()
        .unwrap()
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
fn approve_records_bulk_context_event_and_continues_certify() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-approve", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let round_id = round_for_decision(&db, &run)
        .unwrap()
        .expect("decision round");
    let instances = rounds::instances_for_round(&db, &round_id).unwrap();
    assert!(
        !instances.is_empty(),
        "parked blocking review must have instances"
    );
    let instance_ids: Vec<String> = instances.iter().map(|i| i.id.clone()).collect();

    let fake_gh = fake.parent().unwrap().join("fake-gh");
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
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
    assert_eq!(v["status"], "parked");
    assert!(v["review_approved_head_sha"].as_str().unwrap().len() >= 7);

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert!(run.review_approved_head_sha.is_some());
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        steps
            .iter()
            .rfind(|s| s.step == "compose")
            .map(|s| s.status.as_str()),
        Some("parked"),
        "certify→deliver must proceed to compose park; steps={steps:?}"
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1, "exactly one bulk approve event");
    assert_eq!(events[0].kind, AuthorityKind::ReviewApproved);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(round_id.as_str())
    );
    assert_eq!(
        events[0].reviewed_head.as_deref(),
        run.review_approved_head_sha.as_deref()
    );
    let mut member_ids: Vec<String> = events[0]
        .members
        .iter()
        .map(|m| {
            assert_eq!(m.role, MemberRole::Context);
            m.finding_instance_id.clone()
        })
        .collect();
    member_ids.sort();
    let mut expected = instance_ids;
    expected.sort();
    assert_eq!(member_ids, expected);
    assert!(
        member_ids
            .iter()
            .all(|id| !(id.starts_with('f') && id[1..].chars().all(|c| c.is_ascii_digit()))),
        "members must be instance ids, not display handles"
    );

    kill_daemon(&home);
}

#[test]
fn skip_records_bulk_event_without_approved_head() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-skip", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let round_id = round_for_decision(&db, &run)
        .unwrap()
        .expect("decision round");
    let instances = rounds::instances_for_round(&db, &round_id).unwrap();
    let instance_ids: Vec<String> = instances.iter().map(|i| i.id.clone()).collect();

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
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
    assert!(v["review_approved_head_sha"].is_null());

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed");
    assert!(run.review_approved_head_sha.is_none());
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "review" && s.status == "skipped"),
        "steps={steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|s| s.step == "certify" && s.status == "skipped"),
        "certify must be skipped; steps={steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|s| s.step == "deliver" && s.status == "skipped"),
        "deliver must be skipped; steps={steps:?}"
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1, "exactly one bulk skip event");
    assert_eq!(events[0].kind, AuthorityKind::ReviewSkipped);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(round_id.as_str())
    );
    let mut member_ids: Vec<String> = events[0]
        .members
        .iter()
        .map(|m| {
            assert_eq!(m.role, MemberRole::Context);
            m.finding_instance_id.clone()
        })
        .collect();
    member_ids.sort();
    let mut expected = instance_ids;
    expected.sort();
    assert_eq!(member_ids, expected);

    kill_daemon(&home);
}

#[test]
fn head_moved_after_park_rejects_approve_without_event() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-stale", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let wt = run.worktree_dir.clone().expect("parked worktree");

    git(&wt, &["config", "user.email", "porch@example.com"]);
    git(&wt, &["config", "user.name", "Porch"]);
    std::fs::write(wt.join("drift.txt"), "moved\n").unwrap();
    git(&wt, &["add", "drift.txt"]);
    git(&wt, &["commit", "-m", "drift after park"]);

    let fake_gh = fake.parent().unwrap().join("fake-gh");
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "stale approve must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("drift") || err.contains("stale") || err.contains("rejected"),
        "expected drift/stale error, got {v}"
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked");
    assert!(run.review_approved_head_sha.is_none());
    assert!(
        rounds::events_for_run(&db, &run.id).unwrap().is_empty(),
        "stale approve must not write an authority event"
    );

    kill_daemon(&home);
}

#[test]
fn head_moved_after_park_rejects_skip_without_event() {
    // DISPO-2.6 / CASE-39: skip must fail closed on worktree HEAD drift like approve.
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-skip-stale", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let wt = run.worktree_dir.clone().expect("parked worktree");

    git(&wt, &["config", "user.email", "porch@example.com"]);
    git(&wt, &["config", "user.name", "Porch"]);
    std::fs::write(wt.join("drift.txt"), "moved\n").unwrap();
    git(&wt, &["add", "drift.txt"]);
    git(&wt, &["commit", "-m", "drift after park"]);

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "stale skip must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("drift") || err.contains("stale") || err.contains("rejected"),
        "expected drift/stale error, got {v}"
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked");
    assert!(run.review_approved_head_sha.is_none());
    assert!(
        rounds::events_for_run(&db, &run.id).unwrap().is_empty(),
        "stale skip must not write an authority event"
    );

    kill_daemon(&home);
}

#[test]
fn fix_freezes_selected_target_instance_ids() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("two-blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(
        &work,
        &home,
        "feat-dispo-fix-targets",
        &fake,
        "two-blocking",
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let round_id = round_for_decision(&db, &run)
        .unwrap()
        .expect("decision round");
    let instances = rounds::instances_for_round(&db, &round_id).unwrap();
    assert!(
        instances.len() >= 2,
        "two-blocking park needs ≥2 instances; got {}",
        instances.len()
    );
    let expected_f0 = instances[0].id.clone();

    let out = agent_fix(&work, &home, &fake, &run.id, &["--findings", "f0"]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1, "exactly one fix_requested event");
    assert_eq!(events[0].kind, AuthorityKind::FixRequested);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(round_id.as_str())
    );
    assert_eq!(events[0].members.len(), 1);
    assert_eq!(events[0].members[0].role, MemberRole::Target);
    assert_eq!(events[0].members[0].finding_instance_id, expected_f0);
    assert!(
        !(events[0].members[0].finding_instance_id.starts_with('f')
            && events[0].members[0].finding_instance_id[1..]
                .chars()
                .all(|c| c.is_ascii_digit())),
        "members must be instance ids, not display handles"
    );

    kill_daemon(&home);
}

#[test]
fn empty_fix_selection_usage_exits_without_event() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-fix-empty", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = agent_fix(&work, &home, &fake, &run.id, &["--findings", ""]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "empty selection must usage-exit; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["code"], "usage");
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("no findings selected") || err.contains("findings"),
        "expected empty-selection usage copy, got {v}"
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked");
    assert!(
        rounds::events_for_run(&db, &run.id).unwrap().is_empty(),
        "empty selection must not write fix_requested"
    );
    assert!(
        !home.join("fixer-invoked").is_file(),
        "fixer must not spawn on empty selection"
    );

    kill_daemon(&home);
}

#[test]
fn fix_persists_event_before_fixer_and_drift_skips_spawn() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-fix-order", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = agent_fix(&work, &home, &fake, &run.id, &[]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let saw = std::fs::read_to_string(home.join("fixer-saw-fix-requested")).unwrap();
    assert_eq!(
        saw.trim(),
        "1",
        "fixer must observe fix_requested already committed at spawn"
    );
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, AuthorityKind::FixRequested);
    assert!(!events[0].members.is_empty());
    assert!(
        events[0]
            .members
            .iter()
            .all(|m| m.role == MemberRole::Target)
    );

    let (_tmp2, work2, home2, _origin2, fake2) = setup_with_origin_and_fake("blocking");
    commit_change(&work2, "bug.txt", "boom\n");
    push_with_env(&work2, &home2, "feat-dispo-fix-drift", &fake2, "blocking");
    let db2 = Db::open(&home2.join("state.sqlite")).unwrap();
    let repo_id2 = repo_id_for(&work2);
    let run2 = wait_status(&db2, &repo_id2, &["parked"], Duration::from_secs(20));
    let wt = run2.worktree_dir.clone().expect("parked worktree");
    git(&wt, &["config", "user.email", "porch@example.com"]);
    git(&wt, &["config", "user.name", "Porch"]);
    std::fs::write(wt.join("drift.txt"), "moved\n").unwrap();
    git(&wt, &["add", "drift.txt"]);
    git(&wt, &["commit", "-m", "drift after park"]);

    let out = agent_fix(&work2, &home2, &fake2, &run2.id, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stale fix must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("drift") || err.contains("stale") || err.contains("rejected"),
        "expected drift/stale error, got {v}"
    );
    let run2 = db2.run_by_id(&run2.id).unwrap().unwrap();
    assert_eq!(run2.status, "parked");
    assert!(
        rounds::events_for_run(&db2, &run2.id).unwrap().is_empty(),
        "drifted fix must not write an authority event"
    );
    assert!(
        !home2.join("fixer-invoked").is_file(),
        "fixer must not spawn after HEAD drift"
    );

    kill_daemon(&home);
    kill_daemon(&home2);
}

#[test]
fn noop_fix_still_opens_new_round_and_keeps_prior_events() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-noop-round", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let first_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("first decision round");
    let first_instances = rounds::instances_for_round(&db, &first_round).unwrap();
    let first_ids: Vec<String> = first_instances.iter().map(|i| i.id.clone()).collect();
    let first_head = rounds::get_round(&db, &first_round)
        .unwrap()
        .expect("first round")
        .to_sha;

    let out = agent_fix(&work, &home, &fake, &run.id, &[]);
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
    let rounds_list = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert!(
        rounds_list.len() >= 2,
        "unchanged HEAD must still open a fresh round; got {}",
        rounds_list.len()
    );
    let second_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("post-fix decision round");
    assert_ne!(second_round.as_str(), first_round.as_str());
    let second = rounds::get_round(&db, &second_round)
        .unwrap()
        .expect("second round row");
    assert_eq!(
        second.to_sha, first_head,
        "noop fixer keeps HEAD; rereview still binds that SHA"
    );
    let second_instances = rounds::instances_for_round(&db, &second_round).unwrap();
    assert!(!second_instances.is_empty());
    for inst in &second_instances {
        assert!(
            !first_ids.contains(&inst.id),
            "fingerprint must not reuse prior instance ids"
        );
    }

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, AuthorityKind::FixRequested);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(first_round.as_str())
    );
    assert!(
        events[0]
            .members
            .iter()
            .all(|m| first_ids.contains(&m.finding_instance_id)),
        "prior fix targets must remain on the old round"
    );
    assert!(
        events[0].members.iter().all(|m| !second_instances
            .iter()
            .any(|i| i.id == m.finding_instance_id)),
        "new instances must not inherit old target membership"
    );

    kill_daemon(&home);
}

#[test]
fn noop_fix_reruns_producers_without_second_automatic_fix() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-noop-producers", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let first_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("first decision round");
    let first_producers = rounds::producers_for_round(&db, &first_round).unwrap();
    assert!(
        !first_producers.is_empty(),
        "initial park must record producers"
    );

    let out = agent_fix(&work, &home, &fake, &run.id, &[]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "parked", "exactly one rereview then park again");
    assert!(run.review_approved_head_sha.is_none());
    let second_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("post-fix decision round");
    let second_producers = rounds::producers_for_round(&db, &second_round).unwrap();
    assert_eq!(
        second_producers.len(),
        first_producers.len(),
        "required producers must run again on the unchanged SHA"
    );
    let first_ids: Vec<&str> = first_producers.iter().map(|p| p.id.as_str()).collect();
    for p in &second_producers {
        assert!(
            !first_ids.contains(&p.id.as_str()),
            "rereview producer ids must be fresh"
        );
    }
    assert_eq!(
        std::fs::read_to_string(home.join("fixer-invoked"))
            .unwrap()
            .trim(),
        "1",
        "no automatic second fix loop"
    );
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == AuthorityKind::FixRequested)
            .count(),
        1
    );

    kill_daemon(&home);
}

#[test]
fn clean_rereview_completes_without_bulk_approve_event() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-clean-rereview", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = agent_fix_modes(&work, &home, &fake, &run.id, &[], "blocking", "apply");
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["status"] == "parked" || v["status"] == "completed",
        "clean rereview must continue; got {v}"
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(
        events.len(),
        1,
        "clean rereview must not synthesize bulk approve; got {events:?}"
    );
    assert_eq!(events[0].kind, AuthorityKind::FixRequested);
    assert!(
        events
            .iter()
            .all(|e| e.kind != AuthorityKind::ReviewApproved),
        "no review_approved when rereview has no blocking findings"
    );

    kill_daemon(&home);
}

#[test]
fn yes_after_noop_fix_records_porch_approve_citing_fix_event() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-yes", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let first_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("first decision round");
    let first_instances = rounds::instances_for_round(&db, &first_round).unwrap();
    let first_ids: Vec<String> = first_instances.iter().map(|i| i.id.clone()).collect();

    let out = agent_fix(&work, &home, &fake, &run.id, &["--yes"]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["review_approved_head_sha"].as_str().unwrap().len() >= 7,
        "standing consent must set approved HEAD; got {v}"
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(
        events.len(),
        2,
        "fix_requested then porch review_approved; got {events:?}"
    );
    assert_eq!(events[0].kind, AuthorityKind::FixRequested);
    assert_eq!(events[1].kind, AuthorityKind::ReviewApproved);
    assert_eq!(events[1].actor_kind, ActorKind::Porch);
    assert_eq!(
        events[1].authority_event_id.as_deref(),
        Some(events[0].id.as_str()),
        "porch approve must cite the fix_requested event"
    );
    assert_eq!(
        events[1].head_changed,
        Some(false),
        "noop fixer must record head_changed=false"
    );

    let second_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("post-fix decision round");
    assert_ne!(
        second_round.as_str(),
        first_round.as_str(),
        "rereview must mint a new round"
    );
    assert_eq!(
        events[1].review_round_id.as_deref(),
        Some(second_round.as_str())
    );
    let second_instances = rounds::instances_for_round(&db, &second_round).unwrap();
    let mut second_ids: Vec<String> = second_instances.iter().map(|i| i.id.clone()).collect();
    second_ids.sort();
    assert!(
        !second_ids.is_empty(),
        "new round must have fresh instances"
    );
    for id in &second_ids {
        assert!(
            !first_ids.contains(id),
            "new-round instance {id} must not reuse pre-fix ids"
        );
    }
    let mut member_ids: Vec<String> = events[1]
        .members
        .iter()
        .map(|m| {
            assert_eq!(m.role, MemberRole::Context);
            m.finding_instance_id.clone()
        })
        .collect();
    member_ids.sort();
    assert_eq!(
        member_ids, second_ids,
        "standing consent freezes the new-round instance set"
    );
    assert!(
        events[0]
            .members
            .iter()
            .all(|m| first_ids.contains(&m.finding_instance_id)),
        "prior fix_requested membership must remain on the old round"
    );

    kill_daemon(&home);
}

#[test]
fn abort_records_bulk_context_event_and_cancels() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-abort", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let round_id = round_for_decision(&db, &run)
        .unwrap()
        .expect("decision round");
    let round = rounds::get_round(&db, &round_id)
        .unwrap()
        .expect("round row");
    let instances = rounds::instances_for_round(&db, &round_id).unwrap();
    assert!(
        !instances.is_empty(),
        "parked blocking review must have instances"
    );
    let instance_ids: Vec<String> = instances.iter().map(|i| i.id.clone()).collect();
    let wt = run.worktree_dir.clone().expect("parked worktree");
    assert!(wt.exists(), "worktree present before abort");

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "abort is cancelled → exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "cancelled");
    assert!(v["review_approved_head_sha"].is_null());

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.error.as_deref(), Some("agent abort"));
    assert!(run.review_approved_head_sha.is_none());
    assert!(
        !wt.exists(),
        "worktree cleaned only after abort commit succeeds"
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1, "exactly one bulk abort event");
    assert_eq!(events[0].kind, AuthorityKind::ReviewAborted);
    assert_ne!(
        events[0].kind,
        AuthorityKind::ReviewSkipped,
        "abort must stay distinct from skip"
    );
    assert_ne!(
        run.error.as_deref(),
        Some("superseded by new push"),
        "operator abort must stay distinct from push supersession"
    );
    assert!(!events[0].identity_unavailable);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(round_id.as_str())
    );
    assert_eq!(
        events[0].reviewed_head.as_deref(),
        Some(round.to_sha.as_str())
    );
    let mut member_ids: Vec<String> = events[0]
        .members
        .iter()
        .map(|m| {
            assert_eq!(m.role, MemberRole::Context);
            m.finding_instance_id.clone()
        })
        .collect();
    member_ids.sort();
    let mut expected = instance_ids;
    expected.sort();
    assert_eq!(member_ids, expected);
    assert!(
        member_ids
            .iter()
            .all(|id| !(id.starts_with('f') && id[1..].chars().all(|c| c.is_ascii_digit()))),
        "members must be instance ids, not display handles"
    );

    kill_daemon(&home);
}

#[test]
fn abort_txn_failure_leaves_parked_worktree_then_cleanup_after_commit() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-abort-txn", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let wt = run.worktree_dir.clone().expect("parked worktree");
    assert!(wt.exists());

    {
        let conn = Connection::open(home.join("state.sqlite")).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_abort_effects BEFORE UPDATE ON runs
            BEGIN
                SELECT RAISE(ABORT, 'forced mid-txn write failure');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        poisoned.status.code(),
        Some(1),
        "poisoned abort must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&poisoned.stdout),
        String::from_utf8_lossy(&poisoned.stderr)
    );

    let parked = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(parked.status, "parked");
    assert!(
        rounds::events_for_run(&db, &run.id).unwrap().is_empty(),
        "rolled-back abort must leave no authority event"
    );
    assert!(
        wt.exists(),
        "worktree must remain when abort txn fails before commit"
    );

    {
        let conn = Connection::open(home.join("state.sqlite")).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS poison_abort_effects;")
            .unwrap();
    }

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "abort is cancelled → exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let cancelled = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, AuthorityKind::ReviewAborted);
    assert!(
        !wt.exists(),
        "worktree cleanup runs only after abort txn commits"
    );

    kill_daemon(&home);
}

#[test]
fn legacy_abort_records_identity_unavailable_without_display_members() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-legacy-abort", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let wt = run.worktree_dir.clone().expect("parked worktree");

    db.set_findings_json(
        &run.id,
        Some(
            r#"[{"id":"f0","path":"bug.txt","message":"legacy","severity":"warning","action":"ask-user","start_line":1,"end_line":1}]"#,
        ),
    )
    .unwrap();
    clear_rounds_for_run(&db, &run.id).unwrap();
    assert!(
        round_for_decision(&db, &db.run_by_id(&run.id).unwrap().unwrap())
            .unwrap()
            .is_none(),
        "legacy park must have no decision round"
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "legacy abort is cancelled → exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.error.as_deref(), Some("agent abort"));
    assert!(run.review_approved_head_sha.is_none());
    assert!(
        !wt.exists(),
        "legacy abort still cleans worktree after commit"
    );

    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1, "legacy abort still records an event");
    assert_eq!(events[0].kind, AuthorityKind::ReviewAborted);
    assert!(events[0].identity_unavailable);
    assert!(events[0].review_round_id.is_none());
    assert!(events[0].reviewed_head.is_none());
    assert!(
        events[0].members.is_empty(),
        "legacy abort must not synthesize members from display fN"
    );
    assert!(
        events[0]
            .members
            .iter()
            .all(|m| m.finding_instance_id != "f0"),
        "display f0 must never appear as a member id"
    );

    kill_daemon(&home);
}

#[test]
fn audit_snapshot_includes_events_instances_and_related_occurrences() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-audit-related", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    let first_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("first decision round");
    let first_instances = rounds::instances_for_round(&db, &first_round).unwrap();
    assert_eq!(first_instances.len(), 1);
    let first_id = first_instances[0].id.clone();
    let first_fp = first_instances[0].fingerprint.clone();
    let first_fp_ver = first_instances[0].fingerprint_version;

    let out = agent_fix(&work, &home, &fake, &run.id, &[]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    let second_round = round_for_decision(&db, &run)
        .unwrap()
        .expect("post-fix decision round");
    let second_instances = rounds::instances_for_round(&db, &second_round).unwrap();
    assert_eq!(second_instances.len(), 1);
    let second_id = second_instances[0].id.clone();
    assert_ne!(first_id, second_id);
    assert_eq!(
        second_instances[0].fingerprint, first_fp,
        "noop rereview must reuse fingerprint for related_occurrences"
    );
    assert_eq!(second_instances[0].fingerprint_version, first_fp_ver);

    let doc = build_audit(&db, &run.id).unwrap();
    assert_eq!(doc.schema_version, 2);
    assert_eq!(doc.run_id, run.id);
    assert!(
        doc.rounds.len() >= 2,
        "audit must list both rounds; got {}",
        doc.rounds.len()
    );
    assert!(
        doc.instances.iter().any(|i| i.id == first_id),
        "first instance missing from audit"
    );
    assert!(
        doc.instances.iter().any(|i| i.id == second_id),
        "second instance missing from audit"
    );
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        doc.events
            .iter()
            .any(|e| e.id == events[0].id && e.kind == "fix_requested"),
        "committed fix_requested must appear in audit events: {:?}",
        doc.events
    );
    assert!(
        !doc.related_occurrences.is_empty(),
        "matching fingerprints across rounds must form a related group"
    );
    let group = doc
        .related_occurrences
        .iter()
        .find(|g| g.fingerprint == first_fp && g.fingerprint_version == first_fp_ver)
        .expect("related group for shared fingerprint");
    assert_eq!(
        group.instance_ids,
        vec![first_id.clone(), second_id.clone()],
        "related_occurrences ordered by round ordinal then instance id"
    );
    assert!(
        !serde_json::to_value(&doc)
            .unwrap()
            .to_string()
            .contains("lineage"),
        "audit must not emit lineage edges"
    );

    let rpc_doc = get_audit(&home, &run.id).unwrap();
    assert_eq!(rpc_doc.run_id, doc.run_id);
    assert_eq!(rpc_doc.watermark.audit_rev, doc.watermark.audit_rev);
    assert_eq!(rpc_doc.related_occurrences, doc.related_occurrences);

    kill_daemon(&home);
}

#[test]
fn parked_audit_is_as_of_with_audit_rev_watermark_and_inferred_phase() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-audit-asof", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let snap = get_run(&home, &run.id).unwrap();
    let doc = get_audit(&home, &run.id).unwrap();
    assert_eq!(doc.completeness, "as_of");
    assert_eq!(doc.run_status, "parked");
    let raw = serde_json::to_value(&doc).unwrap();
    assert!(
        raw["watermark"].get("state_rev").is_none(),
        "watermark must not carry EventHub state_rev"
    );
    assert_ne!(
        raw["watermark"]["audit_rev"],
        serde_json::json!(snap.state_rev),
        "audit watermark key is audit_rev, not state_rev={}",
        snap.state_rev
    );
    let conn = Connection::open(home.join("state.sqlite")).unwrap();
    let audit_rev: i64 = conn
        .query_row(
            "SELECT audit_rev FROM runs WHERE id = ?1",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    let history_rev: i64 = conn
        .query_row(
            "SELECT review_history_revision FROM runs WHERE id = ?1",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(doc.watermark.audit_rev, audit_rev);
    assert_eq!(doc.watermark.review_history_revision, history_rev);
    assert_eq!(doc.phase.kind, "phase_events");
    assert!(
        !doc.phase.attempts.is_empty(),
        "parked run with phase log must expose an attempt tree"
    );
    assert!(
        !doc.phase.steps.is_empty(),
        "phase slice steps rebuild from durable phase events"
    );

    kill_daemon(&home);
}

#[test]
fn inconsistent_parked_audit_sets_anomaly_but_still_succeeds() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-audit-anomaly", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    clear_rounds_for_run(&db, &run.id).unwrap();
    db.set_findings_json(&run.id, None).unwrap();
    assert!(
        round_for_decision(&db, &db.run_by_id(&run.id).unwrap().unwrap())
            .unwrap()
            .is_none()
    );
    assert!(rounds::events_for_run(&db, &run.id).unwrap().is_empty());

    let doc = get_audit(&home, &run.id).unwrap();
    assert_eq!(doc.completeness, "as_of");
    assert_eq!(doc.run_status, "parked");
    let anomaly = doc.anomaly.expect("inconsistent parked must set anomaly");
    assert!(!anomaly.code.is_empty());
    assert!(!anomaly.detail.is_empty());

    kill_daemon(&home);
}

#[test]
fn get_run_stays_compact_with_display_handles_and_may_advertise_audit() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-audit-get-run", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let snap = get_run(&home, &run.id).unwrap();
    let findings = snap.findings.as_array().expect("findings array");
    assert!(!findings.is_empty());
    assert_eq!(findings[0]["id"], "f0");
    assert!(findings[0].get("fingerprint").is_none());
    assert!(findings[0].get("criterion_id").is_none());
    assert!(
        !snap.steps.is_empty(),
        "compact snapshot must carry steps[] as live state"
    );
    assert!(
        snap.steps.iter().any(|s| s.step == "review"),
        "steps[] should include the review row: {:?}",
        snap.steps
    );
    assert!(
        snap.audit_available,
        "compact snapshot may advertise audit_available"
    );
    let raw = serde_json::to_value(&snap).unwrap();
    assert!(raw.get("related_occurrences").is_none());
    assert!(raw.get("events").is_none());
    assert!(raw.get("schema_version").is_none());
    assert!(raw.get("watermark").is_none());
    assert!(raw.get("completeness").is_none());

    kill_daemon(&home);
}

#[test]
fn audit_document_exposes_head_changed_false_on_porch_approve() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(
        &work,
        &home,
        "feat-dispo-audit-head-changed",
        &fake,
        "blocking",
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let out = agent_fix(&work, &home, &fake, &run.id, &["--yes"]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let doc = get_audit(&home, &run.id).unwrap();
    let approve = doc
        .events
        .iter()
        .find(|e| e.kind == "review_approved" && e.actor_kind == "porch")
        .expect("porch review_approved in audit");
    assert_eq!(
        approve.head_changed,
        Some(false),
        "audit must expose stored head_changed=false"
    );

    kill_daemon(&home);
}

#[test]
fn agent_audit_prints_builder_json_while_status_stays_compact() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(
        &work,
        &home,
        "feat-dispo-agent-audit-cli",
        &fake,
        "blocking",
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let expected = get_audit(&home, &run.id).unwrap();
    let expected_json = serde_json::to_value(&expected).unwrap();

    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let audit_out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, fake)
        .env("PATH", &path)
        .args(["agent", "audit", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        audit_out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&audit_out.stdout),
        String::from_utf8_lossy(&audit_out.stderr)
    );
    let printed: Value = serde_json::from_slice(&audit_out.stdout).unwrap();
    assert_eq!(printed["schema_version"], expected_json["schema_version"]);
    assert_eq!(printed["run_id"], run.id);
    assert_eq!(printed["watermark"], expected_json["watermark"]);
    assert_eq!(printed["events"], expected_json["events"]);
    assert_eq!(
        printed["related_occurrences"],
        expected_json["related_occurrences"]
    );
    assert!(
        printed.get("schema_version").is_some(),
        "agent audit must emit the typed audit document"
    );

    let status_out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        status_out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&status_out.stdout),
        String::from_utf8_lossy(&status_out.stderr)
    );
    let status: Value = serde_json::from_slice(&status_out.stdout).unwrap();
    assert_eq!(status["run_id"], run.id);
    assert_eq!(status["status"], "parked");
    assert!(status.get("schema_version").is_none());
    assert!(status.get("events").is_none());
    assert!(status.get("related_occurrences").is_none());
    assert!(status.get("watermark").is_none());
    let findings = status["findings"].as_array().expect("compact findings");
    assert!(!findings.is_empty());
    assert_eq!(findings[0]["id"], "f0");

    assert_eq!(
        porch_gate::MAILBOX_CAP,
        64,
        "subscribe mailbox cap must stay frozen"
    );

    kill_daemon(&home);
}

#[test]
fn modern_yes_without_fix_cite_does_not_authorize() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-missing-cite", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    assert!(
        round_for_decision(&db, &run).unwrap().is_some(),
        "modern decision round required"
    );

    let out = agent_fix_modes(
        &work,
        &home,
        &fake,
        &run.id,
        &["--yes"],
        "blocking",
        "drop-cite",
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing cite must fail closed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("fix_requested") || err.contains("standing consent") || err.contains("cite"),
        "expected missing-cite error, got {v}"
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert!(
        run.review_approved_head_sha.is_none(),
        "must not authorize without citing fix_requested; got {:?}",
        run.review_approved_head_sha
    );
    let events = rounds::events_for_run(&db, &run.id).unwrap();
    assert!(
        events
            .iter()
            .all(|e| e.kind != AuthorityKind::ReviewApproved),
        "must not write review_approved without cite; got {events:?}"
    );

    kill_daemon(&home);
}

#[test]
fn human_audit_cli_prints_same_builder_document() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-human-audit", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let expected = get_audit(&home, &run.id).unwrap();
    let expected_json = serde_json::to_value(&expected).unwrap();

    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let audit_out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake)
        .env("PATH", &path)
        .args(["audit", "--json", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        audit_out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&audit_out.stdout),
        String::from_utf8_lossy(&audit_out.stderr)
    );
    let printed: Value = serde_json::from_slice(&audit_out.stdout).unwrap();
    assert_eq!(printed["schema_version"], expected_json["schema_version"]);
    assert_eq!(printed["run_id"], run.id);
    assert_eq!(printed["watermark"], expected_json["watermark"]);
    assert_eq!(printed["events"], expected_json["events"]);

    kill_daemon(&home);
}

#[test]
fn audit_watermark_survives_daemon_restart_without_writes() {
    let (_tmp, work, home, _origin, fake) = setup_with_origin_and_fake("blocking");
    commit_change(&work, "bug.txt", "boom\n");
    push_with_env(&work, &home, "feat-dispo-audit-restart", &fake, "blocking");

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));

    let before = get_audit(&home, &run.id).unwrap();
    let before_rev = before.watermark.audit_rev;
    let before_history = before.watermark.review_history_revision;

    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let fake_fixer = fake.parent().unwrap().join("fake-fixer");
    let fake_gh = fake.parent().unwrap().join("fake-gh");
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
            (FIXER_BIN_ENV, fake_fixer.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", "blocking".as_ref()),
            ("PORCH_FAKE_FIXER_MODE", "noop".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "5".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    let after = get_audit(&home, &run.id).unwrap();
    assert_eq!(
        after.watermark.audit_rev, before_rev,
        "audit_rev must be unchanged across restart with no writes"
    );
    assert_eq!(
        after.watermark.review_history_revision, before_history,
        "review_history_revision must be unchanged across restart with no writes"
    );
    assert_eq!(after.run_id, before.run_id);
    assert_eq!(after.completeness, before.completeness);

    kill_daemon(&home);
}
