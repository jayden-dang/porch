//! Toy-repo smoke for the `porch-quality` binary (M16 dogfood unit).

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn binary_range_review_on_toy_repo() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    git(root, &["init"]);
    git(root, &["config", "user.email", "porch@example.com"]);
    git(root, &["config", "user.name", "Porch"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn ok() -> i32 { 1 }\n").unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "c1"]);
    let from = git_stdout(root, &["rev-parse", "HEAD"]);

    fs::write(
        root.join("src/lib.rs"),
        "pub fn ok() -> i32 { danger().unwrap() }\n",
    )
    .unwrap();
    fs::write(root.join("web.js"), "console.log(x)\nvar y = 1\n").unwrap();
    fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "c2"]);
    let to = git_stdout(root, &["rev-parse", "HEAD"]);

    let out = root.join("result.json");
    Command::cargo_bin("porch-quality")
        .unwrap()
        .current_dir(root)
        .args([
            "--from",
            &from,
            "--to",
            &to,
            "--format",
            "json",
            "--output",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let v: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let files = v["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().filter_map(|x| x.as_str()).collect();
    assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
    assert!(paths.contains(&"web.js"), "{paths:?}");
    assert!(paths.contains(&"Cargo.lock"), "{paths:?}");

    let coverage = v["coverage"].as_array().unwrap();
    let lock = coverage
        .iter()
        .find(|c| c["path"] == "Cargo.lock")
        .expect("lock coverage");
    assert_eq!(lock["status"], "skip");
    assert_eq!(lock["reason"], "lockfile");

    let comments = v["comments"].as_array().unwrap();
    assert!(
        comments.iter().any(|c| c["content"]
            .as_str()
            .unwrap_or("")
            .contains("unwrap-in-lib")),
        "{comments:?}"
    );
}

#[test]
fn help_exits_zero() {
    Command::cargo_bin("porch-quality")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("--from"));
}
