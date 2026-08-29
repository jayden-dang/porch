//! M6: GitHub deliver — lease-push, gh PR, allowlisted checks (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::{Db, kill_group, repo_id_for};
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
    let script = r#"#!/bin/sh
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
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s}\n' \
      "$TARGET" "$FILES_JSON" > "$OUT"
    ;;
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

fn install_fake_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    let script = r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
LOG="$PORCH_HOME/gh-argv.log"
# Log argv as a single line for easy assertions.
{
  printf '+'
  for a in "$@"; do
    printf ' %s' "$a"
  done
  printf '\n'
} >> "$LOG"

# ensure_gh_runnable
for a in "$@"; do
  if [ "$a" = "--version" ]; then
    echo "gh version 2.50.0 (fake)"
    exit 0
  fi
done

MODE="${PORCH_FAKE_GH_MODE:-ok}"
STATE="$PORCH_HOME/gh-pr-state"

# Detect pr subcommand position.
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
    # Read body from stdin when --body-file -
    BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
    cat > "$BODY_FILE"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    exit 0
    ;;
  edit)
    BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
    cat > "$BODY_FILE"
    # Keep existing state / seed one if MODE=existing_pr
    if [ ! -f "$STATE" ]; then
      printf '[{"number":42,"url":"https://example.com/pull/42","title":"porch: existing"}]\n' > "$STATE"
    fi
    exit 0
    ;;
  view)
    printf '{"mergeable":"MERGEABLE"}\n'
    exit 0
    ;;
  checks)
    case "$MODE" in
      lint_fail)
        printf '[{"name":"lint","state":"failure","bucket":"fail"},{"name":"e2e","state":"success","bucket":"pass"}]\n'
        exit 1
        ;;
      lint_ok)
        printf '[{"name":"lint","state":"success","bucket":"pass"},{"name":"spend-money","state":"failure","bucket":"fail"}]\n'
        exit 0
        ;;
      lint_pending)
        printf '[{"name":"lint","state":"pending","bucket":"pending"}]\n'
        exit 0
        ;;
      *)
        printf '[{"name":"lint","state":"success","bucket":"pass"}]\n'
        exit 0
        ;;
    esac
    ;;
  *)
    echo "fake-gh: unhandled args: $*" >&2
    exit 1
    ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    origin: PathBuf,
    fake_review: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup(trusted_yaml: Option<&str>, review_mode: &str, gh_mode: &str) -> Setup {
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
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PORCH_FAKE_GH_MODE", gh_mode)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    kill_daemon(&home);
    restart_deliver_daemon(&home, &fake_review, &fake_gh, review_mode, gh_mode, &path);

    Setup {
        _tmp: tmp,
        work,
        home,
        origin,
        fake_review,
        fake_gh,
        path,
    }
}

fn restart_deliver_daemon(
    home: &Path,
    fake_review: &Path,
    fake_gh: &Path,
    review_mode: &str,
    gh_mode: &str,
    path: &str,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let extra: Vec<(&str, &std::ffi::OsStr)> = vec![
        (REVIEW_BIN_ENV, fake_review.as_os_str()),
        (GH_BIN_ENV, fake_gh.as_os_str()),
        ("PORCH_FAKE_REVIEW_MODE", review_mode.as_ref()),
        ("PORCH_FAKE_GH_MODE", gh_mode.as_ref()),
        ("PATH", path.as_ref()),
        ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ("PORCH_GH_TIMEOUT_SECS", "10".as_ref()),
        ("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3".as_ref()),
        ("PORCH_DELIVER_CHECK_POLL_SECS", "1".as_ref()),
    ];
    porch_gate::spawn_detached_with_env(&bin, home, &extra).unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

fn push_with_env(s: &Setup, branch: &str, review_mode: &str, gh_mode: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PORCH_FAKE_GH_MODE", gh_mode)
        .env("PATH", &s.path)
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

fn origin_branch_sha(origin: &Path, branch: &str) -> Option<String> {
    let out = StdCommand::new("git")
        .args([
            "--git-dir",
            origin.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn gh_argv_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("gh-argv.log")).unwrap_or_default()
}

#[test]
fn lease_push_exact_sha_and_pr_create() {
    let s = setup(None, "clean", "ok");
    git(&s.work, &["checkout", "-b", "feat-lease"]);
    commit_change(&s.work, "feat.txt", "hello\n");
    let local_head = {
        let out = StdCommand::new("git")
            .current_dir(&s.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    push_with_env(&s, "feat-lease", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("completed")
    );

    let remote = origin_branch_sha(&s.origin, "feat-lease").expect("origin branch");
    let certified = run.head_sha.as_deref().unwrap_or(&local_head);
    assert_eq!(remote, certified, "origin tip must equal certified HEAD");
    assert_eq!(run.pr_url.as_deref(), Some("https://example.com/pull/1"));

    let log = gh_argv_log(&s.home);
    assert!(log.contains("pr create"), "expected pr create in {log}");
    assert!(!log.contains("run rerun"), "must never gh run rerun: {log}");
}

#[test]
fn incorporate_refuse_leaves_origin_tip() {
    let s = setup(None, "clean", "ok");
    git(&s.work, &["checkout", "-b", "feat-refuse"]);
    commit_change(&s.work, "local.txt", "local\n");

    // Divergent tip on origin before porch deliver.
    let other = s.work.parent().unwrap().join("other-refuse");
    let st = StdCommand::new("git")
        .args(["clone", s.origin.to_str().unwrap(), other.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    git(&other, &["config", "user.email", "porch@example.com"]);
    git(&other, &["config", "user.name", "Porch"]);
    git(&other, &["checkout", "-b", "feat-refuse"]);
    commit_change(&other, "remote-only.txt", "remote\n");
    git(&other, &["push", "origin", "HEAD:refs/heads/feat-refuse"]);
    let remote_before = origin_branch_sha(&s.origin, "feat-refuse").unwrap();

    push_with_env(&s, "feat-refuse", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("failed")
    );
    let remote_after = origin_branch_sha(&s.origin, "feat-refuse").unwrap();
    assert_eq!(
        remote_after, remote_before,
        "origin tip must not be overwritten"
    );
    let log = gh_argv_log(&s.home);
    assert!(
        !log.contains("pr create"),
        "should fail before PR after refuse; log={log}"
    );
}

#[test]
fn find_existing_pr_edits_not_creates() {
    let s = setup(None, "clean", "existing_pr");
    git(&s.work, &["checkout", "-b", "feat-edit"]);
    commit_change(&s.work, "edit.txt", "x\n");
    push_with_env(&s, "feat-edit", "clean", "existing_pr");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert_eq!(run.pr_url.as_deref(), Some("https://example.com/pull/42"));
    let log = gh_argv_log(&s.home);
    assert!(log.contains("pr edit"), "expected pr edit in {log}");
    assert!(
        !log.contains("pr create"),
        "must not create duplicate: {log}"
    );
}

#[test]
fn trusted_watch_checks_ignores_hostile_and_never_reruns() {
    let trusted = r"
deliver:
  github:
    watch_checks: [lint]
    rerun_transient: 0
";
    let s = setup(Some(trusted), "clean", "lint_ok");

    // Hostile yaml on feature must not expand allowlist / trigger rerun.
    std::fs::write(
        s.work.join(".porch.yaml"),
        r"
deliver:
  github:
    watch_checks: [spend-money, e2e]
    rerun_transient: 5
",
    )
    .unwrap();
    git(&s.work, &["add", ".porch.yaml"]);
    git(&s.work, &["commit", "-m", "hostile deliver yaml"]);
    commit_change(&s.work, "feat-watch.txt", "y\n");
    push_with_env(&s, "feat-watch", "clean", "lint_ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    let log = gh_argv_log(&s.home);
    assert!(log.contains("pr checks"), "expected checks poll: {log}");
    assert!(!log.contains("run rerun"), "must never gh run rerun: {log}");
    assert!(
        !log.contains("spend-money"),
        "hostile check name must not appear in argv: {log}"
    );
}

#[test]
fn non_empty_allowlist_lint_fail_after_push_and_pr() {
    let trusted = r"
deliver:
  github:
    watch_checks: [lint]
";
    let s = setup(Some(trusted), "clean", "lint_fail");
    git(&s.work, &["checkout", "-b", "feat-lint-fail"]);
    commit_change(&s.work, "bad.txt", "z\n");
    push_with_env(&s, "feat-lint-fail", "clean", "lint_fail");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("failed")
    );
    // Push+PR then fail watch.
    assert!(
        origin_branch_sha(&s.origin, "feat-lint-fail").is_some(),
        "branch should have been lease-pushed before watch fail"
    );
    assert!(run.pr_url.is_some(), "PR should exist before watch fail");
    let log = gh_argv_log(&s.home);
    assert!(!log.contains("run rerun"), "{log}");
}

#[test]
fn empty_diff_skips_deliver_no_push_no_gh() {
    let s = setup(None, "clean", "ok");
    // Push main tip again as a new branch name that rebases to empty?
    // Better: create branch at main without new commits — empty vs origin/main.
    git(&s.work, &["checkout", "-b", "feat-empty"]);
    push_with_env(&s, "feat-empty", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("skipped")
    );
    assert!(
        origin_branch_sha(&s.origin, "feat-empty").is_none(),
        "empty-diff must not push feature branch"
    );
    assert!(
        !s.home.join("gh-argv.log").exists() || !gh_argv_log(&s.home).contains("pr "),
        "gh must not be used for PR on skip"
    );
}

#[test]
fn agent_skip_skips_deliver_no_push_no_gh() {
    let s = setup(None, "blocking", "ok");
    git(&s.work, &["checkout", "-b", "feat-skip"]);
    commit_change(&s.work, "park.txt", "p\n");
    push_with_env(&s, "feat-skip", "blocking", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(45));
    assert_eq!(run.status, "parked");

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PATH", &s.path)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
        .assert()
        .success();

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed");
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("skipped")
    );
    assert!(origin_branch_sha(&s.origin, "feat-skip").is_none());
    assert!(
        !s.home.join("gh-argv.log").exists() || !gh_argv_log(&s.home).contains("pr "),
        "no gh PR on agent skip"
    );
}

#[test]
fn gh_missing_fails_before_push() {
    let s = setup(None, "clean", "ok");
    // Point PORCH_GH_BIN at nonexistent and restart daemon.
    kill_daemon(&s.home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let missing = s.home.join("no-such-gh-binary");
    porch_gate::spawn_detached_with_env(
        &bin,
        &s.home,
        &[
            (REVIEW_BIN_ENV, s.fake_review.as_os_str()),
            (GH_BIN_ENV, missing.as_os_str()),
            ("PORCH_FAKE_REVIEW_MODE", "clean".as_ref()),
            ("PATH", s.path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&s.home, Duration::from_secs(5)).unwrap();

    git(&s.work, &["checkout", "-b", "feat-no-gh"]);
    commit_change(&s.work, "nogh.txt", "n\n");
    push_with_env(&s, "feat-no-gh", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert!(
        origin_branch_sha(&s.origin, "feat-no-gh").is_none(),
        "must fail before push when gh missing"
    );
}

#[test]
fn supersede_during_check_watch_cancels_not_fails() {
    let trusted = r"
deliver:
  github:
    watch_checks: [lint]
";
    let s = setup(Some(trusted), "clean", "lint_pending");
    git(&s.work, &["checkout", "-b", "feat-watch-cancel"]);
    commit_change(&s.work, "w1.txt", "one\n");
    push_with_env(&s, "feat-watch-cancel", "clean", "lint_pending");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);

    // Wait until deliver opened a PR and is babysitting pending checks.
    let start = Instant::now();
    let first_id = loop {
        let runs = db.runs_for_repo(&repo_id).unwrap();
        if let Some(run) = runs.last() {
            if run.status == "running" && run.pr_url.is_some() {
                break run.id.clone();
            }
            assert!(
                run.status != "failed" && run.status != "completed",
                "first run left watch early: status={} err={:?}",
                run.status,
                run.error
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for watch: {:?}",
            db.runs_for_repo(&repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    // Second push on same branch should supersede promptly.
    commit_change(&s.work, "w2.txt", "two\n");
    let supersede_at = Instant::now();
    push_with_env(&s, "feat-watch-cancel", "clean", "lint_pending");

    let start = Instant::now();
    let first = loop {
        let run = db.run_by_id(&first_id).unwrap().unwrap();
        if run.status != "running" && run.status != "pending" {
            break run;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "supersede stuck behind watch poll: {run:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        first.status, "cancelled",
        "supersede must win over watch failure; err={:?}",
        first.error
    );
    assert!(
        supersede_at.elapsed() < Duration::from_secs(5),
        "cancel must be prompt, not wait full poll timeout"
    );

    // New run should be progressing (not blocked forever by old watch join).
    let _ = wait_status(
        &db,
        &repo_id,
        &["running", "completed", "failed", "cancelled"],
        Duration::from_secs(15),
    );
}

#[test]
fn lease_updates_ancestor_tip_on_origin() {
    let s = setup(None, "clean", "ok");
    git(&s.work, &["checkout", "-b", "feat-lease-upd"]);
    commit_change(&s.work, "a.txt", "a\n");
    // Seed ancestor tip on origin (not via porch deliver).
    git(
        &s.work,
        &["push", "origin", "HEAD:refs/heads/feat-lease-upd"],
    );
    let ancestor = origin_branch_sha(&s.origin, "feat-lease-upd").unwrap();

    commit_change(&s.work, "b.txt", "b\n");
    let local_head = {
        let out = StdCommand::new("git")
            .current_dir(&s.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_ne!(local_head, ancestor);

    push_with_env(&s, "feat-lease-upd", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let remote = origin_branch_sha(&s.origin, "feat-lease-upd").unwrap();
    let certified = run.head_sha.as_deref().unwrap_or(&local_head);
    assert_eq!(remote, certified);
    assert_ne!(remote, ancestor, "origin tip must move past ancestor");
}

#[test]
fn pr_base_branch_from_trusted_yaml_in_pr_create() {
    // Trusted yaml on origin/main; team PR base is `dev` (klynt-shaped).
    let trusted = r"
pr:
  base_branch: dev
review:
  path_instructions:
    - path: crates/enclave/**
      instructions: Treat TEE as ask-user.
";
    let s = setup(Some(trusted), "clean", "ok");
    // Ensure origin/dev exists for rebase onto (same tip as main is fine).
    let main_sha = origin_branch_sha(&s.origin, "main").unwrap();
    let st = StdCommand::new("git")
        .args([
            "--git-dir",
            s.origin.to_str().unwrap(),
            "update-ref",
            "refs/heads/dev",
            &main_sha,
        ])
        .status()
        .unwrap();
    assert!(st.success());

    git(&s.work, &["checkout", "-b", "feat-base-dev"]);
    std::fs::create_dir_all(s.work.join("crates/enclave")).unwrap();
    commit_change(&s.work, "crates/enclave/x.rs", "fn x() {}\n");
    push_with_env(&s, "feat-base-dev", "clean", "ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let log = gh_argv_log(&s.home);
    assert!(
        log.contains("pr create") && log.contains("--base") && log.contains("dev"),
        "expected pr create --base dev in {log}"
    );

    let main_tip = origin_branch_sha(&s.origin, "main").unwrap();
    assert_eq!(
        run.trusted_config_sha.as_deref(),
        Some(main_tip.as_str()),
        "trusted_config_sha must pin origin/main (yaml-bearing default tip)"
    );

    let pi = s
        .home
        .join("runs")
        .join(&run.id)
        .join("path_instructions.json");
    assert!(
        pi.is_file(),
        "path_instructions.json should be persisted under runs/<id>/"
    );
    let raw = std::fs::read_to_string(&pi).unwrap();
    assert!(
        raw.contains("crates/enclave/**"),
        "persisted instructions: {raw}"
    );
}
