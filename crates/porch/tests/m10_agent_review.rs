//! M10: coding-agent review engine (PATH fakes only — no real LLM).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::{Db, kill_group, repo_id_for};
use porch_git::init_bare;
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

fn install_fake_claude(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("claude");
    let log = bin_dir.join("claude-argv.log");
    // Speaks the real `claude` argv family porch uses (-p / plan mode), plus --help for setup.
    let script = format!(
        r#"#!/bin/sh
set -e
printf '%s\n' "$*" >> "{log}"
for a in "$@"; do
  if [ "$a" = "--help" ] || [ "$a" = "-h" ]; then
    echo "fake claude help"
    exit 0
  fi
  if [ "$a" = "--session-id" ] || [ "$a" = "--resume" ]; then
    echo "fake-claude: session flags forbidden for reviewer" >&2
    exit 3
  fi
done
# Claude family: porch passes -p … and the prompt as a trailing arg; JSON on stdout.
printf '%s\n' '{{"findings":[],"coverage":[{{"path":"README","status":"pass"}}]}}'
"#,
        log = log.display()
    );
    write_exe(&path, &script);
    path
}

fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    write_exe(
        &path,
        r#"#!/bin/sh
: "${PORCH_HOME:?PORCH_HOME required}"
STATE="$PORCH_HOME/gh-pr-state"
for a in "$@"; do [ "$a" = "--version" ] && echo "gh version 2.50.0 (fake)" && exit 0; done
CMD=""; PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done
case "$CMD" in
  list)
    if [ -f "$STATE" ]; then /bin/cat "$STATE"; else printf '[]\n'; fi
    ;;
  create)
    /bin/cat >/dev/null
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit) /bin/cat >/dev/null ;;
  view) printf '{"mergeable":"MERGEABLE","number":1,"url":"https://example.com/pull/1","title":"porch: created","body":""}\n' ;;
  checks) printf '[]\n' ;;
  *) echo "noop-gh: $*" >&2; exit 1 ;;
esac
"#,
    );
    path
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn wait_status(db: &Db, repo_id: &str, want: &[&str], timeout: Duration) -> porch_gate::RunRow {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        if let Some(run) = runs.last() {
            if want.contains(&run.status.as_str()) {
                return run.clone();
            }
        }
        assert!(
            start.elapsed() <= timeout,
            "wanted {want:?}, got {:?}",
            db.runs_for_repo(repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn setup_yes_engine_agent_with_fake_claude() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_claude(&bin);

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .args(["setup", "--yes", "--engine", "agent"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["ok"], true, "{stdout}");
    assert_eq!(v["engine"], "agent");
    assert_eq!(v["verified"], true);
    assert!(v.get("wrapper").is_none() || v["wrapper"].is_null());
    assert!(
        v["agent_bin"]
            .as_str()
            .is_some_and(|s| s.contains("claude"))
    );

    let cfg = fs::read_to_string(home.join("config.yaml")).unwrap();
    assert!(
        cfg.contains("engine: agent") || cfg.contains("engine:agent"),
        "{cfg}"
    );
    assert!(cfg.contains("agent_bin:"), "{cfg}");
    assert!(!home.join("bin/review").exists());

    Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] review:"))
        .stdout(predicates::str::contains("engine=agent"));
}

#[test]
fn setup_yes_defaults_to_agent_when_claude_and_ocr_present() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    install_fake_claude(&bin);
    // Minimal ocr that supports --help / preview for verify if selected.
    write_exe(
        &bin.join("ocr"),
        r#"#!/bin/sh
if [ "$1" = "review" ] && [ "$2" = "--help" ]; then echo help; exit 0; fi
if [ "$1" = "--help" ]; then echo help; exit 0; fi
if [ "$1" = "review" ]; then
  shift
  for a in "$@"; do [ "$a" = "--help" ] && echo help && exit 0; done
  for a in "$@"; do [ "$a" = "--preview" ] && echo '{}' && exit 0; done
fi
exit 0
"#,
    );

    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PATH", path_with(&bin))
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(v["engine"], "agent", "{stdout}");
}

#[test]
fn setup_engine_ocr_still_works() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    // OCR fake (same shape as m9).
    write_exe(
        &bin.join("ocr"),
        r#"#!/bin/sh
set -e
if [ "$1" != "review" ]; then echo "expected review" >&2; exit 2; fi
shift
HELP=0; PREVIEW=0; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) HELP=1; shift ;;
    --preview|-p) PREVIEW=1; shift ;;
    --output|-o) OUT="$2"; shift 2 ;;
    --from|--to|--format) shift 2 ;;
    *) shift ;;
  esac
done
if [ "$HELP" = 1 ]; then echo "fake ocr help"; exit 0; fi
if [ "$PREVIEW" = 1 ]; then echo '{"files":[]}'; exit 0; fi
if [ -z "$OUT" ]; then echo "missing --output" >&2; exit 1; fi
printf '%s\n' '{"comments":[],"files":["README"]}' > "$OUT"
"#,
    );

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
    assert_eq!(v["engine"], "ocr", "{stdout}");
    assert!(home.join("bin/review").is_file());
}

#[test]
#[allow(clippy::too_many_lines)]
fn agent_review_push_writes_prompt_and_completes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let fake_agent = install_fake_claude(&bin);
    let fake_gh = install_noop_gh(&bin);

    init_bare(&origin).unwrap();
    let seed = root.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "base"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);

    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    let work = work.canonicalize().unwrap();
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);

    let path = path_with(&bin);
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PATH", &path)
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove("PORCH_REVIEW_AGENT_BIN")
        .args(["setup", "--yes", "--engine", "agent"])
        .assert()
        .success();

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PATH", &path)
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env("PORCH_REVIEW_AGENT_BIN", &fake_agent)
        .env(GH_BIN_ENV, &fake_gh)
        .args(["init", "--skip-setup"])
        .assert()
        .success();

    kill_daemon(&home);
    let porch_bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &porch_bin,
        &home,
        &[
            ("PORCH_REVIEW_AGENT_BIN", fake_agent.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    fs::write(work.join("README"), "changed\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "change"]);

    let out = StdCommand::new("git")
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env("PORCH_REVIEW_AGENT_BIN", &fake_agent)
        .env("PATH", &path)
        .args(["push", "porch", "HEAD:refs/heads/feat-agent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Db::open(&home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&work);
    // Clean path parks at compose after scaffold (compose resolve is Task 5).
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);

    let prompt = home
        .join("runs")
        .join(&run.id)
        .join("review")
        .join("prompt.txt");
    assert!(prompt.is_file(), "missing {}", prompt.display());
    let body = fs::read_to_string(&prompt).unwrap();
    assert!(body.contains("session-free") || body.contains("JSON"));
    assert!(body.contains("README"));

    let log = fs::read_to_string(bin.join("claude-argv.log")).unwrap();
    assert!(
        log.contains("-p") && log.contains("--permission-mode"),
        "expected claude family argv, log={log}"
    );
    assert!(!log.contains("--session-id"), "log={log}");
    assert!(!log.contains("--resume"), "log={log}");

    kill_daemon(&home);
}

#[test]
fn unknown_engine_message_lists_agent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let assert = Command::cargo_bin("porch")
        .unwrap()
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "nope"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("agent|ocr|generic") || stdout.contains("expected agent"),
        "{stdout}"
    );
}
