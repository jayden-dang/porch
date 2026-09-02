//! M8: operator UX — bare porch, runs, non-TTY attach, daemon install, agent JSON unchanged.

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
    if [ -f "$STATE" ]; then cat "$STATE"; else printf '[]\n'; fi
    ;;
  create)
    cat >/dev/null
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"t"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit)
    cat >/dev/null
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

fn setup_parked() -> (TempDir, PathBuf, PathBuf, String) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake = install_fake_review(&bin_dir);
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
        .env(GH_BIN_ENV, &fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
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
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", "blocking".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "5".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    git(&work, &["checkout", "-b", "feat-m8"]);
    std::fs::write(work.join("README"), "base\nchange\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "change"]);

    let out = StdCommand::new("git")
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake)
        .env("PORCH_FAKE_REVIEW_MODE", "blocking")
        .env("PATH", &path)
        .args(["push", "porch", "HEAD:refs/heads/feat-m8"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let repo_id = repo_id_for(&work);
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(20));
    assert_eq!(run.branch, "feat-m8");

    (tmp, work, home, run.id)
}

#[test]
fn bare_porch_outside_git_repo_fails_without_daemon_lock() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("porch-home");
    // Not a git repo.
    let assert = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(tmp.path())
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        err.contains("not a git work tree") || err.contains("git"),
        "stderr={err}"
    );
    assert!(
        !home.join("daemon.lock").exists(),
        "must not create daemon lock outside a git repo"
    );
}

#[test]
fn non_tty_porch_and_runs_list_parked_agent_status_unchanged() {
    let (_tmp, work, home, run_id) = setup_parked();

    // Non-TTY bare `porch` should print the parked run (assert_cmd has no TTY).
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains(run_id.as_str()) || text.contains("parked") || text.contains("feat-m8"),
        "stdout={text}"
    );

    let runs_out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["runs"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let runs: Value = serde_json::from_slice(&runs_out).unwrap();
    let arr = runs.as_array().expect("runs json array");
    assert!(!arr.is_empty());
    assert!(
        arr.iter()
            .any(|r| r.get("id").and_then(|v| v.as_str()) == Some(run_id.as_str()))
    );

    let agent = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["agent", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&agent).unwrap();
    assert_eq!(status["status"], "parked");
    assert_eq!(status["run_id"], run_id);
    assert!(status.get("findings").and_then(|v| v.as_array()).is_some());
    assert!(status.get("branch").is_some());
    assert!(status.get("phase").is_some());

    // attach non-TTY prints snapshot
    let attach = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .args(["attach", "--run-id", &run_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let attach_text = String::from_utf8_lossy(&attach);
    assert!(attach_text.contains(&run_id));
    assert!(attach_text.contains("parked"));

    kill_daemon(&home);
}

#[test]
fn daemon_install_writes_definition_with_skip_load() {
    let tmp = TempDir::new().unwrap();
    let user_home = tmp.path().join("user");
    let porch_home = tmp.path().join("home");
    std::fs::create_dir_all(&user_home).unwrap();
    std::fs::create_dir_all(&porch_home).unwrap();

    Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["daemon", "install"])
        .assert()
        .success();

    let suffix = porch_gate::daemon_service_suffix(&porch_home);
    #[cfg(target_os = "macos")]
    {
        let plist = user_home
            .join("Library/LaunchAgents")
            .join(format!("ai.porch.daemon.{suffix}.plist"));
        assert!(plist.is_file(), "missing {}", plist.display());
        let body = std::fs::read_to_string(&plist).unwrap();
        assert!(body.contains("KeepAlive"));
        assert!(body.contains("daemon"));
        assert!(body.contains("run"));
    }
    #[cfg(target_os = "linux")]
    {
        let unit = user_home
            .join(".config/systemd/user")
            .join(format!("porch-daemon-{suffix}.service"));
        assert!(unit.is_file());
        let body = std::fs::read_to_string(&unit).unwrap();
        assert!(body.contains("Restart=always"));
    }
}
