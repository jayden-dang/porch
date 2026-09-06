//! Disposition history: review approve/skip/fix authority events.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{self, AuthorityKind, MemberRole};
use porch_gate::{Db, kill_group, repo_id_for, round_for_decision};
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
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
        .env("PORCH_FAKE_FIXER_MODE", "noop")
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

    // Move live worktree HEAD after park so rev-parse diverges from reviewed HEAD.
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

    // Fresh park for drift: move worktree HEAD after park, then fix must fail closed.
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
