use std::process::Command as StdCommand;
use std::time::Duration;

use assert_cmd::Command;
use porch_gate::Db;
use tempfile::TempDir;

fn git(work: &std::path::Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test]
fn init_then_push_records_a_run() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();

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
        .arg("init")
        .assert()
        .success();

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

    // Daemon may still be flushing; sqlite WAL should be readable immediately.
    std::thread::sleep(Duration::from_millis(100));
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let id = porch_gate::repo_id_for(&work);
    let runs = db.runs_for_repo(&id).unwrap();
    assert_eq!(runs.len(), 1, "expected one run after push");
    assert_eq!(runs[0].status, "pending");
    assert_eq!(runs[0].branch, "main");

    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}

/// macOS: `/tmp` is a symlink to `/private/tmp`. Init must not store a
/// non-canonical bare path that later fails `repo_by_bare` after `GIT_DIR` is
/// canonicalized in the post-receive hook.
#[test]
fn init_then_push_with_noncanonical_porch_home_records_a_run() {
    let tmp = TempDir::new_in("/tmp").unwrap();
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    std::fs::create_dir_all(&home).unwrap();
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
        .arg("init")
        .assert()
        .success();

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

    std::thread::sleep(Duration::from_millis(100));
    let db_home = home.canonicalize().unwrap();
    let db = Db::open(&db_home.join("state.sqlite")).unwrap();
    let id = porch_gate::repo_id_for(&work);
    let runs = db.runs_for_repo(&id).unwrap();
    assert_eq!(
        runs.len(),
        1,
        "expected one run after push with non-canonical PORCH_HOME"
    );
    assert_eq!(runs[0].status, "pending");
    assert_eq!(runs[0].branch, "main");

    if let Ok(pid) = std::fs::read_to_string(db_home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}
