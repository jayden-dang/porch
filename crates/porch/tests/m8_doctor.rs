//! M8: `porch doctor` PATH / home / daemon report.

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
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] git:"))
        .stdout(predicates::str::contains("[warn] review:"))
        .stdout(predicates::str::contains("porch setup"))
        .stdout(predicates::str::contains("[warn] fixer:"))
        .stdout(predicates::str::contains("PORCH_FIXER_BIN"));
}

#[test]
fn doctor_ok_floor_sibling_is_distinct_from_judgment_review() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");
    let _quality = assert_cmd::cargo::cargo_bin("porch-quality");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] floor:"))
        .stdout(predicates::str::contains("porch-quality"))
        .stdout(predicates::str::contains("deterministic floor"))
        .stdout(predicates::str::contains("[warn] review:"))
        .stdout(predicates::str::contains("rules"));
}

fn review_line(stdout: &str) -> String {
    stdout
        .lines()
        .find(|l| l.contains("] review:"))
        .unwrap_or("")
        .to_string()
}

#[test]
fn doctor_review_line_names_voice_phrase_and_winning_key() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("config.yaml"),
        "review:\n  assurance: rules-only\n  engine: quality\n",
    )
    .unwrap();

    let out = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let review = review_line(&stdout);
    assert!(
        review.contains(porch_review::voice_phrase(true)),
        "expected rules-only phrase: {review} full={stdout}"
    );
    assert!(
        review.contains("assurance"),
        "expected winning key assurance: {review}"
    );
    assert!(stdout.contains("[ok  ] floor:"), "{stdout}");
    assert!(stdout.contains("identity="), "{stdout}");
    assert!(stdout.contains("porch-quality"), "{stdout}");
    assert!(
        !home.join("daemon.pid").exists(),
        "doctor must not spawn a daemon"
    );
}

#[test]
fn doctor_review_line_names_engine_when_assurance_absent() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("config.yaml"), "review:\n  engine: quality\n").unwrap();

    let out = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let review = review_line(&stdout);
    assert!(
        review.contains(porch_review::voice_phrase(true)),
        "expected rules-only phrase: {review} full={stdout}"
    );
    assert!(
        review.contains("engine"),
        "expected winning key engine: {review}"
    );
    assert!(
        !home.join("daemon.pid").exists(),
        "doctor must not spawn a daemon"
    );
}

#[test]
fn doctor_warns_when_floor_sibling_is_missing() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");
    let isolated = tmp.path().join("isolated");
    fs::create_dir_all(&isolated).unwrap();
    let porch_src = assert_cmd::cargo::cargo_bin("porch");
    let porch_copy = isolated.join("porch");
    fs::copy(&porch_src, &porch_copy).unwrap();
    chmod_755(&porch_copy);

    let out = std::process::Command::new(&porch_copy)
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[warn] floor:"), "{stdout}");
    assert!(stdout.contains("porch-quality"), "{stdout}");
    assert!(
        stdout.contains("not PATH") || stdout.contains("sibling"),
        "{stdout}"
    );
    assert!(stdout.contains("[warn] review:"), "{stdout}");
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
        .args(["init", "-b", "main"])
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
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .arg("init")
        .arg("--skip-setup")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("porch remote ->"), "{stdout}");
    assert!(stdout.contains("repo id:"), "{stdout}");
    assert!(stdout.contains("default branch:"), "{stdout}");
    assert!(stdout.contains("PORCH_HOME:"), "{stdout}");
    assert!(
        stdout.contains("next: git push porch HEAD:refs/heads/"),
        "{stdout}"
    );
    assert!(
        stdout.contains("rules") || stdout.contains("reader"),
        "init tip must name rules/reader: {stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("judgment engine"),
        "init tip must not label wiring as engine/quality: {stdout}"
    );

    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}

/// The `git` binary override is reported, so it cannot act invisibly.
///
/// Proven through a subprocess because the workspace forbids `unsafe_code` and
/// `std::env::set_var` is unsafe on edition 2024, so an in-process test cannot
/// set the variable at all.
#[test]
fn doctor_reports_the_git_binary_override() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let shim = tmp.path().join("shim-git");
    fs::write(&shim, "#!/bin/sh\nexec git \"$@\"\n").unwrap();
    chmod_755(&shim);
    let home = tmp.path().join("home");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env(porch_git::GIT_BIN_ENV, &shim)
        .arg("doctor")
        .assert()
        .stdout(predicates::str::contains("[ok  ] git:"))
        .stdout(predicates::str::contains("shim-git"))
        .stdout(predicates::str::contains(porch_git::GIT_BIN_ENV));
}

/// An override naming a binary that is not there fails the check, rather than
/// silently falling back to `git` on `PATH`.
#[test]
fn doctor_fails_when_the_git_override_is_missing() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    install_fake_git(&bin);
    let home = tmp.path().join("home");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", &bin)
        .env("PORCH_HOME", &home)
        .env("HOME", tmp.path())
        .env(porch_git::GIT_BIN_ENV, tmp.path().join("absent-git"))
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicates::str::contains("FAIL"))
        .stdout(predicates::str::contains("absent-git"));
}
