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
    let dummy_bin = dummy_bin(&work);
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

fn dummy_bin(work: &std::path::Path) -> std::path::PathBuf {
    let dummy_bin = work.join("porch-dummy");
    std::fs::write(&dummy_bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dummy_bin).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dummy_bin, p).unwrap();
    }
    dummy_bin
}

#[test]
fn init_detects_default_branch_from_origin_head() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    let work = root.join("work");
    Command::new("git")
        .args(["init", "--bare", origin.to_str().unwrap()])
        .status()
        .unwrap();
    std::fs::create_dir_all(&seed).unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["init"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["config", "user.email", "porch@example.com"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["config", "user.name", "Porch"])
        .status()
        .unwrap();
    std::fs::write(seed.join("README"), "hi\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "README"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "init"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["branch", "-M", "dev"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "origin", origin.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "-u", "origin", "dev"])
        .status()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            origin.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/dev",
        ])
        .status()
        .unwrap();
    let clone = Command::new("git")
        .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(clone.success());
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

    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let bin = dummy_bin(&work);
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &bin,
        start_daemon: false,
    })
    .unwrap();
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let repo = db.repo_by_id(&result.repo_id).unwrap().unwrap();
    assert_eq!(repo.default_branch, "dev");
}

#[test]
fn notify_inserts_a_pending_run() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = dummy_bin(&work);
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let stdin = format!("0000000000000000000000000000000000000000 {sha} refs/heads/main\n");
    let ids = notify_push(&home_path, &result.bare_path, stdin.as_bytes(), None).unwrap();
    assert_eq!(ids.len(), 1);
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let runs = db.runs_for_repo(&result.repo_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].sha, sha);
    assert_eq!(runs[0].status, "pending");
    assert_eq!(runs[0].branch, "main");
}

#[test]
fn notify_cli_intent_persists_on_run() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = dummy_bin(&work);
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    let sha = "dddddddddddddddddddddddddddddddddddddddd";
    let stdin = format!("0000000000000000000000000000000000000000 {sha} refs/heads/feat\n");
    let ids = notify_push(
        &home_path,
        &result.bare_path,
        stdin.as_bytes(),
        Some("ship feat via cli"),
    )
    .unwrap();
    assert_eq!(ids.len(), 1);
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&ids[0]).unwrap().unwrap();
    assert_eq!(run.intent.as_deref(), Some("ship feat via cli"));
    assert_eq!(run.intent_source.as_deref(), Some("cli"));
}

#[test]
fn notify_empty_cli_intent_skips() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = dummy_bin(&work);
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    let sha = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let stdin = format!("0000000000000000000000000000000000000000 {sha} refs/heads/empty-intent\n");
    let ids = notify_push(&home_path, &result.bare_path, stdin.as_bytes(), Some("")).unwrap();
    assert_eq!(ids.len(), 1);
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let run = db.run_by_id(&ids[0]).unwrap().unwrap();
    assert!(run.intent.is_none());
    assert!(run.intent_source.is_none());
}

#[test]
fn notify_skips_non_heads_refs() {
    let (_keep, work) = git_repo();
    let home = TempDir::new().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let dummy_bin = dummy_bin(&work);
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home_path,
        porch_bin: &dummy_bin,
        start_daemon: false,
    })
    .unwrap();
    let sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let tag_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let stdin = format!(
        "0000000000000000000000000000000000000000 {sha} refs/heads/feature\n\
         0000000000000000000000000000000000000000 {tag_sha} refs/tags/v1.0.0\n"
    );
    let ids = notify_push(&home_path, &result.bare_path, stdin.as_bytes(), None).unwrap();
    assert_eq!(ids.len(), 1);
    let db = Db::open(&home_path.join("state.sqlite")).unwrap();
    let runs = db.runs_for_repo(&result.repo_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].branch, "feature");
    assert_eq!(runs[0].sha, sha);
}
