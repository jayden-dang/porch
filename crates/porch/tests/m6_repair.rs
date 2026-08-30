//! M6 deliver repair: mechanical allowlisted fix → restart at review → certify → re-push.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
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
# After a deliver repair commit, optional flip to blocking for park tests.
if [ "$MODE" = "clean_then_blocking" ]; then
  if [ -n "${PORCH_HOME:-}" ] && [ -f "$PORCH_HOME/.porch-repair-committed" ]; then
    MODE=blocking
  else
    MODE=clean
  fi
fi
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

#[allow(clippy::too_many_lines)]
fn install_fake_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    let script = r#"#!/bin/sh
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
    BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
    cat > "$BODY_FILE"
    # Handshake: hold create until the test helper diverges origin/main after
    # seeing this argv line, so pr view observes CONFLICTING against a live tip.
    if [ -f "$PORCH_HOME/wait-main-diverge" ]; then
      i=0
      while [ ! -f "$PORCH_HOME/main-diverged" ]; do
        i=$((i + 1))
        if [ "$i" -gt 200 ]; then
          echo "fake-gh: timed out waiting for main-diverged" >&2
          exit 1
        fi
        sleep 0.05
      done
    fi
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    exit 0
    ;;
  edit)
    BODY_FILE="$PORCH_HOME/gh-pr-body.txt"
    cat > "$BODY_FILE"
    if [ ! -f "$STATE" ]; then
      printf '[{"number":42,"url":"https://example.com/pull/42","title":"porch: existing"}]\n' > "$STATE"
    fi
    exit 0
    ;;
  view)
    # One CONFLICTING after helper sets the marker (first view only); later MERGEABLE.
    VIEW_N="$PORCH_HOME/gh-view-count"
    n=0
    if [ -f "$VIEW_N" ]; then
      n=$(cat "$VIEW_N")
    fi
    n=$((n + 1))
    printf '%s\n' "$n" > "$VIEW_N"
    if [ -f "$PORCH_HOME/gh-mergeable-conflicting" ] && [ "$n" -eq 1 ]; then
      printf '{"mergeable":"CONFLICTING"}\n'
    else
      printf '{"mergeable":"MERGEABLE"}\n'
    fi
    exit 0
    ;;
  checks)
    case "$MODE" in
      lint_fail_then_ok)
        if [ -f "$PORCH_HOME/.porch-repair-committed" ]; then
          printf '[{"name":"lint","state":"success","bucket":"pass","link":"https://example.com/lint"}]\n'
          exit 0
        fi
        printf '[{"name":"lint","state":"failure","bucket":"fail","link":"https://example.com/lint-fail"}]\n'
        exit 1
        ;;
      lint_fail)
        printf '[{"name":"lint","state":"failure","bucket":"fail","link":"https://example.com/lint-fail"},{"name":"e2e","state":"success","bucket":"pass"}]\n'
        exit 1
        ;;
      lint_cancelled)
        printf '[{"name":"lint","state":"cancelled","bucket":"cancel"}]\n'
        exit 1
        ;;
      lint_timed_out)
        # Real gh: timed_out rows use bucket=fail.
        printf '[{"name":"lint","state":"timed_out","bucket":"fail"}]\n'
        exit 1
        ;;
      lint_ok)
        printf '[{"name":"lint","state":"success","bucket":"pass"},{"name":"e2e","state":"failure","bucket":"fail"}]\n'
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

fn install_fake_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    let script = r#"#!/bin/sh
set -e
PROMPT=""
FINDINGS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --prompt-file) PROMPT="$2"; shift 2 ;;
    --findings-file) FINDINGS="$2"; shift 2 ;;
    --session-id) shift 2 ;;
    *) shift ;;
  esac
done
if [ -z "$PROMPT" ] || [ ! -f "$PROMPT" ]; then
  echo "prompt file missing" >&2
  exit 1
fi
if [ -z "$FINDINGS" ] || [ ! -f "$FINDINGS" ]; then
  echo "findings file missing" >&2
  exit 1
fi
# Count spawns for budget assertions.
if [ -n "${PORCH_HOME:-}" ]; then
  COUNTER="$PORCH_HOME/fixer-spawn-count"
  n=0
  if [ -f "$COUNTER" ]; then
    n=$(cat "$COUNTER")
  fi
  n=$((n + 1))
  printf '%s\n' "$n" > "$COUNTER"
fi
MODE="${PORCH_FAKE_FIXER_MODE:-noop}"
case "$MODE" in
  noop)
    printf '{"summary":"noop"}\n'
    ;;
  apply)
    printf 'repaired\n' >> README
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch add -A >/dev/null
    git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch \
      commit --no-verify -m "porch: repair allowlisted checks" >/dev/null
    if [ -n "${PORCH_HOME:-}" ]; then
      touch "$PORCH_HOME/.porch-repair-committed"
    fi
    touch .porch-repair-committed
    printf '{"summary":"repair allowlisted checks"}\n'
    ;;
  *)
    echo "unknown PORCH_FAKE_FIXER_MODE=$MODE" >&2
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
    fake_fixer: PathBuf,
    path: String,
}

fn setup(
    trusted_yaml: Option<&str>,
    review_mode: &str,
    gh_mode: &str,
    fixer_mode: Option<&str>,
) -> Setup {
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
    let fake_fixer = install_fake_fixer(&bin_dir);

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
    restart_repair_daemon(
        &home,
        &fake_review,
        &fake_gh,
        &fake_fixer,
        review_mode,
        gh_mode,
        fixer_mode,
        &path,
    );

    Setup {
        _tmp: tmp,
        work,
        home,
        origin,
        fake_review,
        fake_gh,
        fake_fixer,
        path,
    }
}

#[allow(clippy::too_many_arguments)]
fn restart_repair_daemon(
    home: &Path,
    fake_review: &Path,
    fake_gh: &Path,
    fake_fixer: &Path,
    review_mode: &str,
    gh_mode: &str,
    fixer_mode: Option<&str>,
    path: &str,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut extra: Vec<(&str, &std::ffi::OsStr)> = vec![
        (REVIEW_BIN_ENV, fake_review.as_os_str()),
        (GH_BIN_ENV, fake_gh.as_os_str()),
        ("PORCH_FAKE_REVIEW_MODE", review_mode.as_ref()),
        ("PORCH_FAKE_GH_MODE", gh_mode.as_ref()),
        ("PATH", path.as_ref()),
        ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ("PORCH_GH_TIMEOUT_SECS", "10".as_ref()),
        ("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3".as_ref()),
        ("PORCH_DELIVER_CHECK_POLL_SECS", "1".as_ref()),
        ("PORCH_FIXER_TIMEOUT_SECS", "10".as_ref()),
    ];
    if let Some(mode) = fixer_mode {
        extra.push((FIXER_BIN_ENV, fake_fixer.as_os_str()));
        extra.push(("PORCH_FAKE_FIXER_MODE", mode.as_ref()));
    }
    porch_gate::spawn_detached_with_env(&bin, home, &extra).unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

fn push_with_env(s: &Setup, branch: &str, review_mode: &str, gh_mode: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
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

fn compose_skip(s: &Setup, run_id: &str, gh_mode: &str) -> std::process::Output {
    let mut cmd = Command::cargo_bin("porch").unwrap();
    cmd.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env(FIXER_BIN_ENV, &s.fake_fixer)
        .env("PORCH_FAKE_REVIEW_MODE", "clean")
        .env("PORCH_FAKE_GH_MODE", gh_mode)
        .env("PATH", &s.path)
        .env("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3")
        .env("PORCH_DELIVER_CHECK_POLL_SECS", "1")
        .args(["agent", "respond", "skip", "--run-id", run_id]);
    cmd.output().unwrap()
}

fn gh_argv_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("gh-argv.log")).unwrap_or_default()
}

fn fixer_spawn_count(home: &Path) -> u32 {
    std::fs::read_to_string(home.join("fixer-spawn-count"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

enum MainDiverge {
    /// Non-conflicting file on origin/main (repair rebase moves HEAD).
    NonConflicting,
    /// Conflicting README rewrite on origin/main (repair rebase aborts).
    ConflictingReadme,
}

/// After fake-gh logs `pr create`, diverge origin/main and unblock create.
///
/// Requires `$PORCH_HOME/wait-main-diverge` so create holds until `main-diverged`.
fn spawn_diverge_main_after_pr_create(
    home: PathBuf,
    other: PathBuf,
    diverge: MainDiverge,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            let log = std::fs::read_to_string(home.join("gh-argv.log")).unwrap_or_default();
            if log.contains("pr create") {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out waiting for pr create in gh-argv.log"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        match diverge {
            MainDiverge::NonConflicting => {
                commit_change(&other, "main-only.txt", "from main after pr\n");
            }
            MainDiverge::ConflictingReadme => {
                std::fs::write(other.join("README"), "remote main conflict after pr\n").unwrap();
                git(&other, &["add", "README"]);
                git(&other, &["commit", "-m", "main diverges after pr"]);
            }
        }
        git(&other, &["push", "origin", "HEAD:refs/heads/main"]);
        std::fs::write(home.join("gh-mergeable-conflicting"), "1").unwrap();
        std::fs::write(home.join("main-diverged"), "1").unwrap();
    })
}

fn prepare_origin_clone(origin: &Path, other: &Path) {
    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), other.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    git(other, &["config", "user.email", "porch@example.com"]);
    git(other, &["config", "user.name", "Porch"]);
}

const WATCH_LINT: &str = r"
deliver:
  github:
    watch_checks: [lint]
";

#[test]
#[ignore = "allowlist repair still outside compose-resume watch path"]
fn red_lint_fixer_rereview_certify_second_lease_push() {
    let s = setup(
        Some(WATCH_LINT),
        "clean",
        "lint_fail_then_ok",
        Some("apply"),
    );
    git(&s.work, &["checkout", "-b", "feat-repair"]);
    commit_change(&s.work, "feat.txt", "hello\n");
    let pre_push_head = {
        let out = StdCommand::new("git")
            .current_dir(&s.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    push_with_env(&s, "feat-repair", "clean", "lint_fail_then_ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed", "parked"],
        Duration::from_secs(60),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert_eq!(run.deliver_repair_attempts, 1);
    assert_eq!(fixer_spawn_count(&s.home), 1);

    let remote = origin_branch_sha(&s.origin, "feat-repair").expect("origin tip");
    assert_ne!(remote, pre_push_head, "origin must move to post-repair SHA");
    assert_eq!(
        run.head_sha.as_deref(),
        Some(remote.as_str()),
        "run head must equal post-repair origin tip"
    );
    assert!(
        run.review_approved_head_sha.as_deref() == Some(remote.as_str()),
        "review_approved_head_sha rebound after rereview; got {:?}",
        run.review_approved_head_sha
    );

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("completed")
    );
    assert!(
        steps
            .iter()
            .any(|s| s.step == "deliver" && s.status == "failed"),
        "expected an earlier deliver failed row: {steps:?}"
    );
    let log = gh_argv_log(&s.home);
    assert!(
        log.contains("pr edit") || log.matches("pr create").count() >= 1,
        "expected PR update or create after repair: {log}"
    );
    assert!(!log.contains("run rerun"), "{log}");

    let repair_dir = s.home.join("runs").join(&run.id).join("deliver-repair");
    let prompt = std::fs::read_to_string(repair_dir.join("prompt.txt")).unwrap();
    assert!(
        prompt.contains("allowlisted") || prompt.contains("checks failed"),
        "{prompt}"
    );
    let findings = std::fs::read_to_string(repair_dir.join("findings.json")).unwrap();
    assert!(
        findings.contains("\"name\":\"lint\"") && findings.contains("\"state\":\"failure\""),
        "findings must carry name/state: {findings}"
    );
    assert!(
        findings.contains("\"link\":\"https://example.com/lint-fail\""),
        "findings must retain check link: {findings}"
    );
}

#[test]
#[ignore = "allowlist repair still outside compose-resume watch path"]
fn budget_exhaust_no_fourth_fixer_spawn() {
    let s = setup(Some(WATCH_LINT), "clean", "lint_fail", Some("noop"));
    git(&s.work, &["checkout", "-b", "feat-budget"]);
    commit_change(&s.work, "b.txt", "b\n");
    push_with_env(&s, "feat-budget", "clean", "lint_fail");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed"],
        Duration::from_secs(60),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("budget exhausted")),
        "err={:?}",
        run.error
    );
    assert_eq!(run.deliver_repair_attempts, 3);
    assert_eq!(fixer_spawn_count(&s.home), 3);
}

#[test]
#[ignore = "allowlist repair still outside compose-resume watch path"]
fn missing_fixer_bin_fails_closed() {
    let s = setup(Some(WATCH_LINT), "clean", "lint_fail", None);
    git(&s.work, &["checkout", "-b", "feat-no-fixer"]);
    commit_change(&s.work, "n.txt", "n\n");
    push_with_env(&s, "feat-no-fixer", "clean", "lint_fail");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed"],
        Duration::from_secs(45),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert_ne!(run.status, "completed");
    assert_eq!(fixer_spawn_count(&s.home), 0);
    assert!(run.deliver_repair_attempts >= 1);
}

#[test]
fn cancelled_allowlisted_check_no_fixer() {
    let s = setup(Some(WATCH_LINT), "clean", "lint_cancelled", Some("apply"));
    git(&s.work, &["checkout", "-b", "feat-cancel-check"]);
    commit_change(&s.work, "c.txt", "c\n");
    push_with_env(&s, "feat-cancel-check", "clean", "lint_cancelled");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let parked = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(parked.status, "parked", "err={:?}", parked.error);
    let out = compose_skip(&s, &parked.id, "lint_cancelled");
    assert_eq!(out.status.code(), Some(1));

    let run = db.run_by_id(&parked.id).unwrap().unwrap();
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert_eq!(fixer_spawn_count(&s.home), 0);
    assert_eq!(run.deliver_repair_attempts, 0);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("non-repairable") || e.contains("cancelled")),
        "err={:?}",
        run.error
    );
}

#[test]
fn timed_out_allowlisted_check_no_fixer() {
    let s = setup(Some(WATCH_LINT), "clean", "lint_timed_out", Some("apply"));
    git(&s.work, &["checkout", "-b", "feat-timeout-check"]);
    commit_change(&s.work, "t.txt", "t\n");
    push_with_env(&s, "feat-timeout-check", "clean", "lint_timed_out");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let parked = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(parked.status, "parked", "err={:?}", parked.error);
    let out = compose_skip(&s, &parked.id, "lint_timed_out");
    assert_eq!(out.status.code(), Some(1));

    let run = db.run_by_id(&parked.id).unwrap().unwrap();
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert_eq!(fixer_spawn_count(&s.home), 0);
    assert_eq!(run.deliver_repair_attempts, 0);
}

#[test]
fn unlisted_e2e_failure_with_lint_green_completes_no_fixer() {
    let s = setup(Some(WATCH_LINT), "clean", "lint_ok", Some("apply"));
    git(&s.work, &["checkout", "-b", "feat-unlisted"]);
    commit_change(&s.work, "u.txt", "u\n");
    push_with_env(&s, "feat-unlisted", "clean", "lint_ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let parked = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(45),
    );
    assert_eq!(parked.status, "parked", "err={:?}", parked.error);
    let out = compose_skip(&s, &parked.id, "lint_ok");
    assert!(
        out.status.success(),
        "compose skip failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&parked.id).unwrap().unwrap();
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert_eq!(fixer_spawn_count(&s.home), 0);
    assert_eq!(run.deliver_repair_attempts, 0);
}

#[test]
#[ignore = "allowlist repair still outside compose-resume watch path"]
fn rereview_parks_no_second_lease_push_of_unreviewed_sha() {
    let s = setup(
        Some(WATCH_LINT),
        "clean_then_blocking",
        "lint_fail_then_ok",
        Some("apply"),
    );
    git(&s.work, &["checkout", "-b", "feat-park-repair"]);
    commit_change(&s.work, "p.txt", "p\n");
    let local_pre = {
        let out = StdCommand::new("git")
            .current_dir(&s.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    push_with_env(
        &s,
        "feat-park-repair",
        "clean_then_blocking",
        "lint_fail_then_ok",
    );

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(60),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert_eq!(run.deliver_repair_attempts, 1);
    assert!(
        run.review_approved_head_sha.is_none(),
        "approved SHA cleared on repair then park: {:?}",
        run.review_approved_head_sha
    );
    assert!(
        db.get_uncertified_pipeline_range(&repo_id, "feat-park-repair")
            .unwrap()
            .is_none(),
        "deliver repair must not upsert uncertified_pipeline_ranges"
    );

    // First lease-push used pre-repair SHA; no second push of unreviwed repair.
    let remote = origin_branch_sha(&s.origin, "feat-park-repair").unwrap();
    assert_eq!(
        remote, local_pre,
        "origin must remain at pre-repair tip (no unreviwed push)"
    );
    assert!(run.pr_url.is_some(), "PR kept across park");
    assert!(
        run.worktree_dir.as_ref().is_some_and(|p| p.exists()),
        "worktree kept when parked"
    );
}

#[test]
fn mergeable_conflicting_rebase_conflict_fails_closed() {
    // Main stays compatible through initial rebase + first lease-push. After PR
    // create, helper diverges origin/main with a conflicting README; deliver
    // repair rebase must abort (not the pipeline's initial rebase).
    let s = setup(Some(WATCH_LINT), "clean", "lint_ok", Some("apply"));
    std::fs::write(s.home.join("wait-main-diverge"), "1").unwrap();

    let other = s.work.parent().unwrap().join("other-conflict");
    prepare_origin_clone(&s.origin, &other);
    let helper =
        spawn_diverge_main_after_pr_create(s.home.clone(), other, MainDiverge::ConflictingReadme);

    git(&s.work, &["checkout", "-b", "feat-conflict"]);
    std::fs::write(s.work.join("README"), "feature conflict\n").unwrap();
    git(&s.work, &["add", "README"]);
    git(&s.work, &["commit", "-m", "feature readme"]);
    push_with_env(&s, "feat-conflict", "clean", "lint_ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["failed", "completed", "parked"],
        Duration::from_secs(60),
    );
    helper.join().unwrap();
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.contains("rebase conflict")),
        "deliver repair rebase must conflict, got {:?}",
        run.error
    );
    assert_eq!(run.deliver_repair_attempts, 1);
    assert_eq!(fixer_spawn_count(&s.home), 0);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "deliver" && s.status == "failed"),
        "expected deliver failed before repair abort: {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|s| s.step == "deliver_repair" && s.status == "completed"),
        "repair rebase abort must not complete deliver_repair: {steps:?}"
    );
    assert_ne!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("completed"),
        "must not complete a second deliver after abort"
    );
}

#[test]
fn mergeable_conflicting_clean_rebase_rereview_second_lease_push() {
    // Main compatible through first lease-push+PR. Helper then adds a
    // non-conflicting origin/main commit so deliver-time repair rebase moves HEAD.
    let s = setup(Some(WATCH_LINT), "clean", "lint_ok", Some("apply"));
    std::fs::write(s.home.join("wait-main-diverge"), "1").unwrap();

    let other = s.work.parent().unwrap().join("other-ff");
    prepare_origin_clone(&s.origin, &other);
    let helper =
        spawn_diverge_main_after_pr_create(s.home.clone(), other, MainDiverge::NonConflicting);

    git(&s.work, &["checkout", "-b", "feat-rebase-ok"]);
    commit_change(&s.work, "feat-only.txt", "from feat\n");
    // First lease-push tip (pre deliver-repair rebase).
    let pre_lease = {
        let out = StdCommand::new("git")
            .current_dir(&s.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    push_with_env(&s, "feat-rebase-ok", "clean", "lint_ok");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    // After merge-conflict repair + rereview, second deliver parks compose
    // (compose resolve / watch completion is Task 5/6).
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(60),
    );
    helper.join().unwrap();
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(
        run.deliver_repair_attempts >= 1,
        "attempts={}",
        run.deliver_repair_attempts
    );
    assert_eq!(fixer_spawn_count(&s.home), 0, "rebase path needs no fixer");

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "deliver_repair" && s.status == "completed"),
        "deliver repair rebase must record deliver_repair: {steps:?}"
    );
    assert!(
        steps.iter().any(|s| {
            s.step == "review"
                && s.status == "completed"
                && s.error.as_deref() == Some("deliver_repair")
        }),
        "post-repair rereview row required: {steps:?}"
    );
    assert_eq!(
        last_step(&steps, "compose").map(|s| s.status.as_str()),
        Some("parked"),
        "second deliver must park compose, not complete: {steps:?}"
    );

    let remote = origin_branch_sha(&s.origin, "feat-rebase-ok").expect("origin tip");
    assert_ne!(
        remote, pre_lease,
        "origin must move to post-repair-rebase HEAD (not the first lease tip)"
    );
    assert_eq!(run.head_sha.as_deref(), Some(remote.as_str()));
    assert_eq!(
        run.review_approved_head_sha.as_deref(),
        Some(remote.as_str()),
        "review_approved_head_sha rebound after rereview"
    );
    let log = gh_argv_log(&s.home);
    assert!(!log.contains("run rerun"), "{log}");
    assert!(
        !run.error.as_deref().is_some_and(|e| e.contains("refuse")),
        "must not incorporate-refuse after clean rebase: {:?}",
        run.error
    );
}
