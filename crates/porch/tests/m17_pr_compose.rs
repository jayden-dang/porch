//! M17: PR compose — scaffold after lease-push, park compose (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::{GH_BIN_ENV, MANAGED_BEGIN, MANAGED_END};
use porch_gate::{Db, kill_group, repo_id_for, run_artifact_dir};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use tempfile::TempDir;

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
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

fn chmod_755(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

fn install_fake_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-review");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
OUT=""
FROM=""
TO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
MODE="${PORCH_FAKE_REVIEW_MODE:-clean}"
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
    ;;
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

#[allow(clippy::too_many_lines)] // shell fake mirrors m6 argv/body/title paths
fn install_fake_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
LOG="$PORCH_HOME/gh-argv.log"
{
  printf '+'
  for a in "$@"; do
    printf ' %s' "$a"
  done
  printf '\n'
} >> "$LOG"

for a in "$@"; do
  if [ "$a" = "--version" ]; then
    echo "gh version 2.50.0 (fake)"
    exit 0
  fi
done

MODE="${PORCH_FAKE_GH_MODE:-ok}"
STATE="$PORCH_HOME/gh-pr-state"
BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
TITLE_FILE="$PORCH_HOME/gh-pr-title.txt"

CMD=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then
    CMD="$a"
    break
  fi
  PREV="$a"
done

case "$CMD" in
  list)
    if [ -f "$STATE" ] || [ "$MODE" = "existing_pr" ]; then
      if [ -f "$STATE" ]; then
        cat "$STATE"
      else
        printf '[{"number":42,"url":"https://example.com/pull/42","title":"porch: existing"}]\n'
      fi
    else
      printf '[]\n'
    fi
    exit 0
    ;;
  create)
    cat > "$BODY_FILE"
    TITLE=""
    PREV=""
    for a in "$@"; do
      if [ "$PREV" = "--title" ]; then TITLE="$a"; break; fi
      PREV="$a"
    done
    printf '%s\n' "$TITLE" > "$TITLE_FILE"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"%s"}]\n' "$TITLE" > "$STATE"
    echo "https://example.com/pull/1"
    exit 0
    ;;
  edit)
    HAS_BODY=0
    HAS_TITLE=0
    PREV=""
    for a in "$@"; do
      if [ "$a" = "--body-file" ]; then HAS_BODY=1; fi
      if [ "$PREV" = "--title" ]; then
        printf '%s\n' "$a" > "$TITLE_FILE"
        HAS_TITLE=1
      fi
      PREV="$a"
    done
    if [ "$HAS_BODY" -eq 1 ]; then
      cat > "$BODY_FILE"
    fi
    if [ ! -f "$STATE" ]; then
      TITLE=$(cat "$TITLE_FILE" 2>/dev/null || echo "porch: existing")
      printf '[{"number":42,"url":"https://example.com/pull/42","title":"%s"}]\n' "$TITLE" > "$STATE"
    elif [ "$HAS_TITLE" -eq 1 ]; then
      TITLE=$(cat "$TITLE_FILE")
      # Refresh title in state JSON (simple rewrite).
      printf '[{"number":42,"url":"https://example.com/pull/42","title":"%s"}]\n' "$TITLE" > "$STATE"
    fi
    exit 0
    ;;
  view)
    BODY=""
    if [ -f "$BODY_FILE" ]; then
      BODY=$(cat "$BODY_FILE")
    fi
    TITLE="porch: existing"
    if [ -f "$TITLE_FILE" ]; then
      TITLE=$(cat "$TITLE_FILE")
    elif [ -f "$STATE" ]; then
      TITLE=$(sed -n 's/.*"title":"\([^"]*\)".*/\1/p' "$STATE" | head -n1)
      [ -n "$TITLE" ] || TITLE="porch: existing"
    fi
    # Escape body for JSON string (minimal).
    BODY_ESC=$(printf '%s' "$BODY" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null || printf '%s' "$BODY" | sed 's/\\/\\\\/g; s/"/\\"/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    if echo "$*" | grep -q mergeable; then
      printf '{"mergeable":"MERGEABLE","title":"%s","body":"%s"}\n' "$TITLE" "$BODY_ESC"
    else
      printf '{"title":"%s","body":"%s","url":"https://example.com/pull/42","number":42}\n' "$TITLE" "$BODY_ESC"
    fi
    exit 0
    ;;
  checks)
    printf '[{"name":"lint","state":"success","bucket":"pass"}]\n'
    exit 0
    ;;
  *)
    echo "fake-gh: unhandled args: $*" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    fake_review: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup(trusted_yaml: Option<&str>) -> Setup {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_review = install_fake_review(&bin_dir);
    let fake_gh = install_fake_gh(&bin_dir);

    init_bare(&origin).unwrap();

    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    if let Some(yaml) = trusted_yaml {
        std::fs::write(seed.join(".porch.yaml"), yaml).unwrap();
        git(&seed, &["add", ".porch.yaml"]);
    }
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

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake_review)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .env("PORCH_FAKE_GH_MODE", "ok")
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake_review.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", "clean".as_ref()),
            ("PORCH_FAKE_GH_MODE", "ok".as_ref()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
            ("PORCH_GH_TIMEOUT_SECS", "10".as_ref()),
            ("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3".as_ref()),
            ("PORCH_DELIVER_CHECK_POLL_SECS", "1".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    Setup {
        _tmp: tmp,
        work,
        home,
        fake_review,
        fake_gh,
        path,
    }
}

fn push_feat(s: &Setup, branch: &str, intent: Option<&str>) {
    let mut cmd = StdCommand::new("git");
    cmd.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .env("PORCH_FAKE_GH_MODE", "ok")
        .env("PATH", &s.path);
    if let Some(intent) = intent {
        cmd.env("PORCH_INTENT", intent);
    }
    let out = cmd
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_change(work: &Path, name: &str, body: &str) {
    std::fs::write(work.join(name), body).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
}

fn last_step<'a>(
    steps: &'a [porch_gate::StepResultRow],
    name: &str,
) -> Option<&'a porch_gate::StepResultRow> {
    steps.iter().rfind(|s| s.step == name)
}

fn gh_argv_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("gh-argv.log")).unwrap_or_default()
}

#[test]
fn deliver_scaffolds_pr_and_parks_compose() {
    let s = setup(None);
    git(&s.work, &["checkout", "-b", "feat-compose"]);
    commit_change(&s.work, "compose.txt", "hello\n");
    push_feat(&s, "feat-compose", Some("document the compose scaffold"));

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert_eq!(run.pr_url.as_deref(), Some("https://example.com/pull/1"));

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "compose").map(|s| s.status.as_str()),
        Some("parked"),
        "steps={steps:?}"
    );
    assert!(
        last_step(&steps, "deliver").is_none_or(|s| {
            s.status.as_str() != "completed" && s.status.as_str() != "parked"
        }),
        "deliver must not be completed or parked; steps={steps:?}"
    );
    // parked_phase driver is compose+parked only
    let parked = steps.iter().rev().find(|s| s.status == "parked").unwrap();
    assert_eq!(parked.step, "compose");

    let body = std::fs::read_to_string(s.home.join("gh-pr-body.txt")).expect("gh body");
    assert!(body.contains(MANAGED_BEGIN), "{body}");
    assert!(body.contains(MANAGED_END), "{body}");
    assert!(body.contains("## Summary"), "{body}");
    assert!(body.contains("porch-attestation"), "{body}");
    assert!(!body.contains("## Intent"), "{body}");
    assert!(!body.contains("## Review"), "{body}");
    assert!(!body.contains("## Certify"), "{body}");
    assert!(!body.contains("## Pipeline"), "{body}");
    assert!(!body.contains("## What Changed"), "{body}");

    let packet_path = run_artifact_dir(&s.home, &run.id).join("compose-packet.json");
    assert!(
        packet_path.is_file(),
        "missing compose packet at {}",
        packet_path.display()
    );
    let packet: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&packet_path).unwrap()).unwrap();
    assert_eq!(packet["schema_version"], 1);
    assert_eq!(packet["run_id"], run.id);
    assert_eq!(packet["repo_id"], repo_id);
    assert_eq!(packet["branch"], "feat-compose");
    assert!(packet["base_sha"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(packet["head_sha"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(packet["pr_url"], "https://example.com/pull/1");
    assert_eq!(packet["pr_number"], 1);
    assert_eq!(packet["intent"], "document the compose scaffold");
    assert!(
        packet["title_scaffold"]
            .as_str()
            .is_some_and(|t| t.contains("document the compose scaffold") || t.starts_with("porch:")),
        "{packet}"
    );
    assert!(
        packet["body_scaffold"]
            .as_str()
            .is_some_and(|b| b.contains("## Summary") && b.contains("porch-attestation")),
        "{packet}"
    );
    assert_eq!(packet["template_source"], "porch_default");
    assert!(packet["template_path"].is_null());
    assert!(packet["change_summary"].as_str().is_some());
    assert!(
        packet["theater_reject_rules"].is_object() || packet["theater_reject_rules"].is_array()
    );
    assert_eq!(packet["porch_managed_markers"]["begin"], MANAGED_BEGIN);
    assert_eq!(packet["porch_managed_markers"]["end"], MANAGED_END);

    let log = gh_argv_log(&s.home);
    assert!(log.contains("pr create"), "expected pr create in {log}");
    assert!(
        !log.contains("pr checks"),
        "must not watch checks while compose parked: {log}"
    );

    // Worktree kept for agent respond.
    assert!(
        run.worktree_dir.as_ref().is_some_and(|p| p.exists()),
        "worktree should remain while compose parked"
    );

    kill_daemon(&s.home);
}

#[test]
fn deliver_with_watch_checks_does_not_poll_while_compose_parked() {
    let trusted = r"
deliver:
  github:
    watch_checks: [lint]
    rerun_transient: 0
";
    let s = setup(Some(trusted));
    git(&s.work, &["checkout", "-b", "feat-no-watch"]);
    commit_change(&s.work, "nowatch.txt", "x\n");
    push_feat(&s, "feat-no-watch", None);

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);

    let log = gh_argv_log(&s.home);
    assert!(log.contains("pr create"), "{log}");
    assert!(
        !log.contains("pr checks"),
        "watch must wait until compose resolves: {log}"
    );

    kill_daemon(&s.home);
}
