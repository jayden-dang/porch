//! M16: quality engine setup / doctor / JSON contract (PATH fakes; no LLM/gh).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str as pred_str;
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

fn path_with(bin_dir: &Path) -> String {
    let real_git = StdCommand::new("which")
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
    format!("{}:{real_git}", bin_dir.display())
}

/// Fake `porch-quality` that speaks M3 argv and passes setup range smoke.
fn install_fake_porch_quality(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("porch-quality");
    let log = bin_dir.join("pq-argv.log");
    write_exe(
        &path,
        &format!(
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
if [ "$HELP" = 1 ]; then echo "fake porch-quality help"; exit 0; fi
if [ -z "$OUT" ]; then echo "missing --output" >&2; exit 1; fi
# Cover README for setup tempfile smoke; callers may overwrite.
printf '%s\n' '{{"comments":[],"files":["README"],"coverage":[{{"path":"README","status":"pass"}}],"groups":[]}}' > "$OUT"
"#,
            log = log.display()
        ),
    );
    path
}

#[test]
fn setup_yes_engine_quality_writes_wrapper() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_porch_quality(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .args(["setup", "--yes", "--engine", "quality"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], true, "{stdout}");
    assert_eq!(v["engine"], "quality");
    assert_eq!(v["verified"], true);
    let wrap = PathBuf::from(v["wrapper"].as_str().unwrap());
    assert!(wrap.starts_with(&home), "wrapper={wrap:?}");
    let body = fs::read_to_string(&wrap).unwrap();
    assert!(
        body.contains("porch-quality") || body.contains("exec "),
        "body={body}"
    );
    let cfg = fs::read_to_string(home.join("config.yaml")).unwrap();
    assert!(cfg.contains("quality"), "{cfg}");
}

#[test]
fn doctor_reports_quality_engine() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_porch_quality(&bin);

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .args(["setup", "--yes", "--engine", "quality"])
        .assert()
        .success();

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("engine=quality"));
}

#[test]
fn quality_wrapper_forwards_m3_argv() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_porch_quality(&bin);
    let log = bin.join("pq-argv.log");

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "quality"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], true, "{stdout}");

    // Clear argv log after setup smoke, then invoke wrapper with porch argv.
    let _ = fs::write(&log, "");
    let wrap = PathBuf::from(v["wrapper"].as_str().unwrap());
    let out = root.join("out.json");
    let st = StdCommand::new(&wrap)
        .args([
            "--from",
            "aaa",
            "--to",
            "bbb",
            "--format",
            "json",
            "--output",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let log_body = fs::read_to_string(&log).unwrap();
    assert!(
        log_body.contains("--from")
            && log_body.contains("--to")
            && log_body.contains("--format")
            && log_body.contains("--output"),
        "log={log_body}"
    );
}

#[test]
fn porch_review_accepts_quality_json_shape() {
    let raw = br#"{
      "comments":[{"path":"a.rs","content":"[rust/unwrap-in-lib] x","category":"bug","severity":"medium","start_line":1,"end_line":1}],
      "files":["a.rs","Cargo.lock"],
      "coverage":[
        {"path":"a.rs","status":"pass"},
        {"path":"Cargo.lock","status":"skip","reason":"lockfile"}
      ],
      "groups":[{"label":"rust:src-0","files":["a.rs"]}]
    }"#;
    let out = porch_review::parse_review_json(raw).unwrap();
    assert_eq!(out.covered_files, vec!["a.rs", "Cargo.lock"]);
    assert_eq!(out.findings.len(), 1);
    porch_review::assert_coverage(&["a.rs".into(), "Cargo.lock".into()], &out.covered_files)
        .unwrap();
}

#[test]
fn cargo_install_porch_package_includes_quality_bin() {
    Command::cargo_bin("porch-quality")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(pred_str::contains("--from"));
}
