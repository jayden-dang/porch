//! M11: install.sh, skill copy on init, setup daemon opt-in, doctor PATH hint.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn chmod_755(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn write_exe(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    chmod_755(path);
}

fn install_fake_git(bin_dir: &Path) {
    write_exe(
        &bin_dir.join("git"),
        "#!/bin/sh\n# unused in most m11 tests; real git preferred when PATH merges\necho fake-git\nexit 0\n",
    );
}

fn install_fake_claude(bin_dir: &Path) {
    write_exe(
        &bin_dir.join("claude"),
        "#!/bin/sh\necho fake-claude\nexit 0\n",
    );
}

fn install_fake_codex(bin_dir: &Path) {
    write_exe(
        &bin_dir.join("codex"),
        "#!/bin/sh\necho fake-codex\nexit 0\n",
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn git_work_tree(root: &Path) -> PathBuf {
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let st = StdCommand::new("git")
        .current_dir(&work)
        .args(["init"])
        .status()
        .unwrap();
    assert!(st.success());
    let _ = StdCommand::new("git")
        .current_dir(&work)
        .args(["config", "user.email", "porch@example.com"])
        .status();
    let _ = StdCommand::new("git")
        .current_dir(&work)
        .args(["config", "user.name", "Porch"])
        .status();
    fs::write(work.join("README"), "hi\n").unwrap();
    let _ = StdCommand::new("git")
        .current_dir(&work)
        .args(["add", "README"])
        .status();
    let _ = StdCommand::new("git")
        .current_dir(&work)
        .args(["commit", "-m", "init"])
        .status();
    work
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}

#[test]
fn install_sh_dry_run_documents_cargo_bin_and_path() {
    let script = repo_root().join("install.sh");
    assert!(script.is_file(), "missing {}", script.display());
    let tmp = TempDir::new().unwrap();
    let out = StdCommand::new("bash")
        .arg(&script)
        .env("PORCH_INSTALL_DRY_RUN", "1")
        .env("HOME", tmp.path())
        .env_remove("PORCH_PREFIX")
        .env_remove("CARGO_HOME")
        .output()
        .expect("run install.sh dry-run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".cargo/bin") || stdout.contains("cargo/bin"),
        "dry-run should mention cargo bindir, got:\n{stdout}"
    );
    assert!(
        stdout.contains("cargo install") || stdout.contains("--path"),
        "dry-run should mention cargo install --path, got:\n{stdout}"
    );
}

#[test]
fn init_copies_skill_for_detected_claude_and_codex() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let porch_home = root.join("porch-home");
    let bin = root.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    // Pre-create agent skill roots (init fails soft if missing; create so copy succeeds).
    fs::create_dir_all(user_home.join(".claude")).unwrap();
    fs::create_dir_all(user_home.join(".codex")).unwrap();
    install_fake_claude(&bin);
    install_fake_codex(&bin);
    let work = git_work_tree(&root);

    let mut path = bin.display().to_string();
    if let Some(real) = StdCommand::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Path::new(&s).parent().map(|p| p.display().to_string())
            }
        })
    {
        path = format!("{path}:{real}");
    }

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &path)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["init", "--skip-setup"])
        .assert()
        .success();

    let claude_skill = user_home.join(".claude/skills/porch/SKILL.md");
    let codex_skill = user_home.join(".codex/skills/porch/SKILL.md");
    assert!(
        claude_skill.is_file(),
        "expected {}",
        claude_skill.display()
    );
    assert!(codex_skill.is_file(), "expected {}", codex_skill.display());
    let body = fs::read_to_string(&claude_skill).unwrap();
    assert!(
        body.starts_with("---"),
        "skill must have YAML frontmatter, got prefix {:?}",
        &body[..body.len().min(40)]
    );
    assert!(body.contains("name: porch"));
    assert!(body.contains("porch agent status") || body.contains("porch agent"));
    assert!(!body.to_lowercase().contains("toon"));

    // Idempotent: second init overwrites / leaves identical.
    let before = fs::read(&claude_skill).unwrap();
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &path)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["init", "--skip-setup"])
        .assert()
        .success();
    let after = fs::read(&claude_skill).unwrap();
    assert_eq!(before, after);

    kill_daemon(&porch_home);
}

#[test]
fn init_skill_copy_warns_soft_when_agent_dirs_missing() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user-empty");
    let porch_home = root.join("porch-home");
    let bin = root.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    install_fake_claude(&bin);
    let work = git_work_tree(&root);

    // No ~/.claude directory at all — init must still succeed (fail soft).
    let mut path = bin.display().to_string();
    if let Ok(o) = StdCommand::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if let Some(parent) = Path::new(&s).parent() {
            path = format!("{path}:{}", parent.display());
        }
    }

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &path)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["init", "--skip-setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("porch remote ->"))
        .stderr(
            predicates::str::contains("skill:")
                .and(predicates::str::contains(".claude"))
                .and(predicates::str::contains("missing")),
        );

    assert!(
        !user_home.join(".claude/skills/porch/SKILL.md").is_file(),
        "must not create agent home when missing"
    );
    kill_daemon(&porch_home);
}

#[test]
fn setup_yes_default_does_not_install_daemon_service() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let porch_home = root.join("porch-home");
    let bin = root.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    install_fake_git(&bin);
    // Minimal agent that passes setup --help check.
    write_exe(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ] || [ \"$1\" = \"-h\" ]; then echo help; exit 0; fi\necho fake\nexit 0\n",
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &bin)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["setup", "--yes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert!(
        v.get("daemon_service").is_none()
            || v["daemon_service"].is_null()
            || v["daemon_service"] == Value::Null,
        "default setup must not install daemon, got {v}"
    );

    // No launch agent / systemd unit under user home.
    let launch = user_home.join("Library/LaunchAgents");
    let systemd = user_home.join(".config/systemd/user");
    let has_plist = launch.read_dir().is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("porch"))
    });
    let has_unit = systemd.read_dir().is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("porch"))
    });
    assert!(
        !has_plist && !has_unit,
        "detached default must not write service file"
    );
}

#[test]
fn setup_install_daemon_flag_writes_service_definition() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let porch_home = root.join("porch-home");
    let bin = root.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    write_exe(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ] || [ \"$1\" = \"-h\" ]; then echo help; exit 0; fi\necho fake\nexit 0\n",
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &bin)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["setup", "--yes", "--install-daemon"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["ok"], true);
    let svc = v["daemon_service"]
        .as_str()
        .expect("daemon_service path in JSON");
    assert!(
        Path::new(svc).is_file(),
        "service definition missing at {svc}"
    );
}

#[test]
fn doctor_warns_when_cargo_bin_porch_exists_but_not_on_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let porch_home = root.join("porch-home");
    let path_bin = root.join("path-bin");
    // Honor CARGO_HOME (same bindir install.sh uses), not only ~/.cargo.
    let cargo_home = root.join("custom-cargo");
    let cargo_bin = cargo_home.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&path_bin).unwrap();
    fs::create_dir_all(&cargo_bin).unwrap();

    // Real-looking porch in cargo bin, but cargo bin NOT on PATH.
    write_exe(&cargo_bin.join("porch"), "#!/bin/sh\necho porch\nexit 0\n");
    // git on PATH so doctor does not hard-fail.
    write_exe(&path_bin.join("git"), "#!/bin/sh\necho git\nexit 0\n");

    Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("CARGO_HOME", &cargo_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &path_bin)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_GH_BIN")
        .env_remove("PORCH_FIXER_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(
            predicates::str::contains("warn").and(predicates::str::contains(
                cargo_bin.to_string_lossy().as_ref(),
            )),
        )
        .stdout(predicates::str::contains("PATH"));
}

#[test]
fn setup_verify_does_not_write_daemon_even_with_install_daemon() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let porch_home = root.join("porch-home");
    let bin = root.join("bin");
    fs::create_dir_all(&user_home).unwrap();
    fs::create_dir_all(&porch_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    write_exe(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ] || [ \"$1\" = \"-h\" ]; then echo help; exit 0; fi\necho fake\nexit 0\n",
    );

    // Seed a valid setup so --verify can succeed.
    Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &bin)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["setup", "--yes"])
        .assert()
        .success();

    let out = Command::cargo_bin("porch")
        .unwrap()
        .env("HOME", &user_home)
        .env("PORCH_HOME", &porch_home)
        .env("PATH", &bin)
        .env("PORCH_SERVICE_SKIP_LOAD", "1")
        .args(["setup", "--verify", "--install-daemon"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert!(
        v.get("daemon_service").is_none()
            || v["daemon_service"].is_null()
            || v["daemon_service"] == Value::Null,
        "--verify must not install daemon, got {v}"
    );

    let launch = user_home.join("Library/LaunchAgents");
    let systemd = user_home.join(".config/systemd/user");
    let has_plist = launch.read_dir().is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("porch"))
    });
    let has_unit = systemd.read_dir().is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("porch"))
    });
    assert!(
        !has_plist && !has_unit,
        "--verify must not write service file even with --install-daemon"
    );
}
