//! M14: agent run, intent CLI, skill loop docs (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
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
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
    ;;
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s}\n' \
      "$TARGET" "$FILES_JSON" > "$OUT"
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

fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?}"
STATE="$PORCH_HOME/gh-pr-state"
BODYLOG="$PORCH_HOME/gh-pr-body.log"
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
  list) if [ -f "$STATE" ]; then cat "$STATE"; else printf '[]\n'; fi ;;
  create)
    cat > "$BODYLOG"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"t"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit)
    cat > "$BODYLOG"
    ;;
  checks) printf '[]\n' ;;
  *) echo "noop-gh: $*" >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

struct Harness {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    fake_review: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup(mode: &str) -> Harness {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_review = install_fake_review(&bin_dir);
    let fake_gh = install_noop_gh(&bin_dir);
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
        .env(REVIEW_BIN_ENV, &fake_review)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
        .env("PORCH_FAKE_REVIEW_MODE", mode)
        .args(["init", "--skip-setup"])
        .assert()
        .success();

    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake_review.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PATH", path.as_ref()),
            ("PORCH_FAKE_REVIEW_MODE", mode.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    Harness {
        _tmp: tmp,
        work,
        home,
        fake_review,
        fake_gh,
        path,
    }
}

fn commit_change(work: &Path, name: &str) {
    std::fs::write(work.join(name), format!("{name}\n")).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
}

#[test]
fn agent_run_wait_stops_at_park() {
    let h = setup("blocking");
    git(&h.work, &["checkout", "-b", "feat-park"]);
    commit_change(&h.work, "park.txt");

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
        .args(["agent", "run", "--wait", "--timeout", "45"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let last = stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("jsonl line");
    let v: Value = serde_json::from_str(last).unwrap();
    assert_eq!(v["status"], "parked", "stdout={stdout}");
    assert!(v.get("run_id").and_then(|x| x.as_str()).is_some());
    assert!(v.get("findings").and_then(|x| x.as_array()).is_some());

    kill_daemon(&h.home);
}

#[test]
fn agent_run_intent_persists_on_push() {
    let h = setup("clean");
    git(&h.work, &["checkout", "-b", "feat-intent"]);
    commit_change(&h.work, "intent.txt");

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .args([
            "agent",
            "run",
            "--intent",
            "ship intent via agent run",
            "--wait",
            "--timeout",
            "45",
        ])
        .assert()
        .success();

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(&db, &repo_id, &["completed"], Duration::from_secs(5));
    assert_eq!(run.branch, "feat-intent");
    assert_eq!(run.intent.as_deref(), Some("ship intent via agent run"));
    assert_eq!(run.intent_source.as_deref(), Some("env"));

    kill_daemon(&h.home);
}

#[test]
fn agent_run_second_push_waits_for_new_run() {
    let h = setup("clean");
    git(&h.work, &["checkout", "-b", "feat-second"]);
    commit_change(&h.work, "first.txt");

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .args(["agent", "run", "--wait", "--timeout", "45"])
        .assert()
        .success();

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let first = wait_status(&db, &repo_id, &["completed"], Duration::from_secs(5));
    assert_eq!(first.branch, "feat-second");

    commit_change(&h.work, "second.txt");
    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .args(["agent", "run", "--wait", "--timeout", "45"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let last = stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("jsonl line");
    let v: Value = serde_json::from_str(last).unwrap();
    let second_id = v["run_id"].as_str().expect("run_id");
    assert_ne!(
        second_id, first.id,
        "second push must attach the new run, not the prior completed one; stdout={stdout}"
    );
    assert_eq!(v["status"], "completed", "stdout={stdout}");
    assert!(
        second_id > first.id.as_str(),
        "new run id should be newer ULID: {second_id} vs {}",
        first.id
    );

    kill_daemon(&h.home);
}

#[test]
fn agent_run_timeout_requires_wait() {
    let h = setup("clean");
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "run", "--timeout", "5"])
        .assert()
        .failure()
        .code(2);
    kill_daemon(&h.home);
}

#[test]
fn agent_run_intent_warns_on_attach_without_run_id() {
    let h = setup("blocking");
    git(&h.work, &["checkout", "-b", "feat-intent-warn"]);
    commit_change(&h.work, "warn.txt");

    let out = StdCommand::new("git")
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["push", "porch", "HEAD:refs/heads/feat-intent-warn"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(30));

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .args(["agent", "run", "--intent", "ignored on attach"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("--intent ignored"), "stderr={stderr}");
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["run_id"], run.id);

    kill_daemon(&h.home);
}

#[test]
fn agent_run_attach_run_id() {
    let h = setup("blocking");
    git(&h.work, &["checkout", "-b", "feat-attach"]);
    commit_change(&h.work, "attach.txt");

    let out = StdCommand::new("git")
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["push", "porch", "HEAD:refs/heads/feat-attach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(30));

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .args(["agent", "run", "--run-id", &run.id])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["run_id"], run.id);
    assert_eq!(v["status"], "parked");

    // --intent with --run-id is usage
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "run", "--run-id", &run.id, "--intent", "nope"])
        .assert()
        .failure()
        .code(2);

    kill_daemon(&h.home);
}

#[test]
fn init_intent_prints_tip() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init"]);
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);
    std::fs::write(work.join("README"), "x\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "init"]);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["init", "--skip-setup", "--intent", "remember this for push"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("porch agent run --intent"),
        "stdout={stdout}"
    );
    assert!(stdout.contains("remember this for push"), "stdout={stdout}");
    kill_daemon(&home);
}

#[test]
fn deliver_pr_body_includes_intent_and_review_summary() {
    let h = setup("clean");
    git(&h.work, &["checkout", "-b", "feat-body"]);
    commit_change(&h.work, "body.txt");

    let out = StdCommand::new("git")
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PORCH_INTENT", "document the body enrichment")
        .args(["push", "porch", "HEAD:refs/heads/feat-body"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(&db, &repo_id, &["completed"], Duration::from_secs(45));
    assert_eq!(run.intent.as_deref(), Some("document the body enrichment"));

    let body = std::fs::read_to_string(h.home.join("gh-pr-body.log")).expect("gh body log");
    assert!(body.contains("## Intent"), "{body}");
    assert!(body.contains("document the body enrichment"), "{body}");
    assert!(body.contains("## What Changed"), "{body}");
    assert!(body.contains("body.txt"), "{body}");
    assert!(body.contains("## Review"), "{body}");
    assert!(body.contains("## Certify"), "{body}");
    assert!(body.contains("porch-attestation"), "{body}");

    kill_daemon(&h.home);
}

#[test]
fn skill_documents_agent_run_and_never_merge() {
    let md = porch_gate::skill_markdown();
    assert!(md.contains("porch agent run"));
    assert!(md.contains("porch agent status"));
    assert!(md.contains("porch agent respond"));
    assert!(md.contains("porch agent sync"));
    assert!(md.to_lowercase().contains("never merge"));
    assert!(md.contains("babysit deploy"));
    assert!(md.contains("fix --yes"));
    assert!(md.contains("one fix round") || md.contains("one") && md.contains("round"));
}
