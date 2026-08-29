use std::process::Command;

use porch_git::{GitDir, init_bare, run, run_c, stdout_trim};
use tempfile::TempDir;

fn write_commit(work: &std::path::Path) {
    Command::new("git")
        .current_dir(work)
        .args(["init"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["config", "user.email", "porch@example.com"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["config", "user.name", "Porch"])
        .status()
        .unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["add", "README"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["commit", "-m", "init"])
        .status()
        .unwrap();
}

#[test]
fn git_dir_rejects_relative_path() {
    let err = GitDir::new("relative/path").unwrap_err();
    assert!(matches!(err, porch_git::Error::GitDirNotAbsolute(_)));
}

#[test]
fn rev_parse_head_uses_absolute_git_dir() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    write_commit(&work);
    let git_dir = GitDir::new(work.join(".git")).unwrap();
    let out = run(&git_dir, &["rev-parse", "HEAD"]).unwrap();
    let sha = stdout_trim(&out);
    assert_eq!(sha.len(), 40, "expected full SHA, got {sha:?}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn init_bare_creates_repository_at_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let bare = tmp.path().canonicalize().unwrap().join("gate.git");
    let git_dir = init_bare(&bare).unwrap();
    let out = run(&git_dir, &["rev-parse", "--is-bare-repository"]).unwrap();
    assert_eq!(stdout_trim(&out), "true");
}

#[test]
fn run_c_rejects_relative_work_tree() {
    let err = run_c(std::path::Path::new("rel"), &["status"]).unwrap_err();
    assert!(matches!(err, porch_git::Error::GitDirNotAbsolute(_)));
}
