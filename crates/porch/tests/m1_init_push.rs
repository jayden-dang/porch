//! M1: init + push into a dead gate.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::Db;
use tempfile::TempDir;

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
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

/// Noop `gh` so a future fixture that reaches deliver cannot hit real GitHub (E13).
fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(bin_dir).unwrap();
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
    chmod_755(&path);
    path
}

fn wait_run_recorded(db: &Db, repo_id: &str, timeout: Duration) -> porch_gate::RunRow {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        if let Some(run) = runs.first() {
            if run.status != "pending" && run.status != "running" {
                return run.clone();
            }
        }
        assert!(
            start.elapsed() <= timeout,
            "no terminal run for {repo_id}: {:?}",
            db.runs_for_repo(repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn restart_with_noop_gh(home: &Path, fake_gh: &Path, path: &str) {
    kill_daemon(home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        home,
        &[(GH_BIN_ENV, fake_gh.as_os_str()), ("PATH", path.as_ref())],
    )
    .unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

#[test]
fn init_then_push_records_a_run() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let fake_gh = install_noop_gh(&bin_dir);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    git(&work, &["init"]);
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);
    std::fs::write(work.join("README"), "hi\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "init"]);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    restart_with_noop_gh(&home, &fake_gh, &path);

    let mut push = StdCommand::new("git");
    push.current_dir(&work).env("PORCH_HOME", &home).args([
        "push",
        "porch",
        "HEAD:refs/heads/main",
    ]);
    let out = push.output().unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let id = porch_gate::repo_id_for(&work);
    let run = wait_run_recorded(&db, &id, Duration::from_secs(10));
    assert_eq!(run.branch, "main");
    // Without origin on the author clone, rebase fetch fails closed.
    assert!(
        matches!(run.status.as_str(), "completed" | "failed" | "cancelled"),
        "status={}",
        run.status
    );

    kill_daemon(&home);
}

/// macOS: `/tmp` is a symlink to `/private/tmp`. Init must not store a
/// non-canonical bare path that later fails `repo_by_bare` after `GIT_DIR` is
/// canonicalized in the post-receive hook.
#[test]
fn init_then_push_with_noncanonical_porch_home_records_a_run() {
    let tmp = TempDir::new_in("/tmp").unwrap();
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let fake_gh = install_noop_gh(&bin_dir);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // Intentionally do not canonicalize `home` — keep the `/tmp/...` form.
    let canon_home = home.canonicalize().unwrap();
    if home.as_os_str() == canon_home.as_os_str() {
        // e.g. Linux where /tmp is not a symlink; nothing to prove here.
        return;
    }

    git(&work, &["init"]);
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);
    std::fs::write(work.join("README"), "hi\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "init"]);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    // Restart against canonical home (daemon lock/socket live there after init).
    restart_with_noop_gh(&canon_home, &fake_gh, &path);

    let mut push = StdCommand::new("git");
    push.current_dir(&work).env("PORCH_HOME", &home).args([
        "push",
        "porch",
        "HEAD:refs/heads/main",
    ]);
    let out = push.output().unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db_home = home.canonicalize().unwrap();
    let db = Db::open(&db_home.join("state.sqlite")).unwrap();
    let id = porch_gate::repo_id_for(&work);
    let run = wait_run_recorded(&db, &id, Duration::from_secs(10));
    assert_eq!(run.branch, "main");
    assert!(
        matches!(run.status.as_str(), "completed" | "failed" | "cancelled"),
        "status={}",
        run.status
    );

    kill_daemon(&db_home);
}
