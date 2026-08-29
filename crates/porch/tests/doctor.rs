use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn chmod_755(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn install_fake_git(bin_dir: &Path) {
    fs::create_dir_all(bin_dir).unwrap();
    let path = bin_dir.join("git");
    fs::write(&path, "#!/bin/sh\necho fake-git\nexit 0\n").unwrap();
    chmod_755(&path);
}

#[test]
fn doctor_fails_when_git_missing_from_path() {
    let tmp = TempDir::new().unwrap();
    let empty = tmp.path().join("empty-bin");
    fs::create_dir_all(&empty).unwrap();
    let home = tmp.path().join("home");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &empty)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicates::str::contains("FAIL"))
        .stdout(predicates::str::contains("git"));
}

#[test]
fn doctor_ok_when_git_present_even_if_review_missing() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] git:"))
        .stdout(predicates::str::contains("[warn] review:"))
        .stdout(predicates::str::contains("[warn] fixer:"))
        .stdout(predicates::str::contains("PORCH_FIXER_BIN"))
        .stdout(predicates::str::contains("porch doctor"));
}

#[test]
fn init_prints_remote_and_next_steps() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    let home = root.join("home");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&home).unwrap();

    let st = std::process::Command::new("git")
        .current_dir(&work)
        .args(["init"])
        .status()
        .unwrap();
    assert!(st.success());
    let _ = std::process::Command::new("git")
        .current_dir(&work)
        .args(["config", "user.email", "porch@example.com"])
        .status();
    let _ = std::process::Command::new("git")
        .current_dir(&work)
        .args(["config", "user.name", "Porch"])
        .status();
    fs::write(work.join("README"), "hi\n").unwrap();
    let _ = std::process::Command::new("git")
        .current_dir(&work)
        .args(["add", "README"])
        .status();
    let _ = std::process::Command::new("git")
        .current_dir(&work)
        .args(["commit", "-m", "init"])
        .status();

    // No review/gh on a stripped PATH suffix → tip line may appear; remote line must.
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .arg("init")
        .assert()
        .success()
        .stdout(predicates::str::contains("porch remote ->"))
        .stdout(predicates::str::contains("repo id:"))
        .stdout(predicates::str::contains("default branch:"))
        .stdout(predicates::str::contains("PORCH_HOME:"))
        .stdout(predicates::str::contains(
            "next: git push porch HEAD:refs/heads/",
        ));

    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}
