use std::process::Command;

use porch_gate::{Db, InitOptions, init, notify_push, repo_id_for};
use tempfile::TempDir;

fn git_repo() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["init"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["config", "user.email", "porch@example.com"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["config", "user.name", "Porch"])
        .status()
        .unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["add", "README"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["commit", "-m", "init"])
        .status()
        .unwrap();
    (tmp, work)
}

#[test]
fn repo_id_is_twelve_hex_chars() {
    let tmp = TempDir::new().unwrap();
    let id = repo_id_for(&tmp.path().canonicalize().unwrap());
    assert_eq!(id.len(), 12);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn init_without_daemon_installs_bare_remote_and_hooks() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = work.join("porch-dummy");
    std::fs::write(&dummy_bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dummy_bin).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dummy_bin, p).unwrap();
    }
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    assert!(result.bare_path.is_dir());
    let remotes = Command::new("git")
        .current_dir(&work)
        .args(["remote"])
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&remotes.stdout);
    assert!(names.lines().any(|l| l.trim() == "porch"));
    assert!(result.bare_path.join("hooks/pre-receive").is_file());
    assert!(result.bare_path.join("hooks/post-receive").is_file());
}

#[test]
fn notify_inserts_a_pending_run() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = work.join("porch-dummy");
    std::fs::write(&dummy_bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dummy_bin).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dummy_bin, p).unwrap();
    }
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let stdin = format!("0000000000000000000000000000000000000000 {sha} refs/heads/main\n");
    let ids = notify_push(&home_path, &result.bare_path, stdin.as_bytes()).unwrap();
    assert_eq!(ids.len(), 1);
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let runs = db.runs_for_repo(&result.repo_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].sha, sha);
    assert_eq!(runs[0].status, "pending");
    assert_eq!(runs[0].branch, "main");
}
