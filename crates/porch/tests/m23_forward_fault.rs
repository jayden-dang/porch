//! M23: fault injection across the forward boundary (ROAD-9 / FAULT).
//!
//! ROAD-7 made the forward boundary leave a durable record and ROAD-8 made a
//! restart read it, but both were proved by seeding rows and calling the
//! classifier directly. These tests kill a real gate at the boundary and assert
//! the durable state against two independent sources: porch's own records, and
//! the fixture `origin`, which is a local bare repository the test can read.
//!
//! The instrument is `PORCH_GIT_BIN` plus the shim below — never a hard exit
//! compiled into the forward path, and never a soft `Err`, which cannot reach
//! these states at all: an `Err` out of the phase loop terminalizes the
//! `deliver` attempt, and a terminalized attempt is not interrupted.
//!
//! Every kill waits for a marker the shim writes *after* the decisive side
//! effect, so no test races a timer and no test kills while a real push is in
//! flight (FAULT-1.5, FAULT-1.6).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::{Db, kill_group, repo_id_for, rounds};
use porch_git::{GIT_BIN_ENV, init_bare};
use porch_review::REVIEW_BIN_ENV;
use tempfile::TempDir;

/// Bounded so a leaked shim cannot wedge the suite (FAULT-6.5).
const SHIM_SLEEP_SECS: &str = "60";

fn git(dir: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = StdCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn chmod_755(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

// ---------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------

/// A `git` that passes everything through except the one call it was told to
/// interrupt.
///
/// It discriminates on argv. The forward push is
/// `--git-dir=<bare> push --no-verify [--force-with-lease=…] origin <sha>:refs/heads/<branch>`
/// and the lease observation is `--git-dir=<bare> ls-remote origin refs/heads/<branch>`,
/// so matching the subcommand plus the target ref is enough. It fires at most
/// once, guarded by a state file, and its default and fallthrough are `exec` of
/// the real `git` (FAULT-4.4).
fn install_git_shim(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("shim-git");
    let script = r#"#!/bin/sh
: "${PORCH_REAL_GIT:?PORCH_REAL_GIT required}"
MODE="${PORCH_SHIM_MODE:-off}"
if [ "$MODE" = "off" ] || [ -z "${PORCH_SHIM_REF:-}" ]; then
  exec "$PORCH_REAL_GIT" "$@"
fi
# Fire once per scenario.
if [ -n "${PORCH_SHIM_STATE:-}" ] && [ -f "$PORCH_SHIM_STATE" ]; then
  exec "$PORCH_REAL_GIT" "$@"
fi

SUB=""
HIT_REF=0
for a in "$@"; do
  case "$a" in
    --git-dir=*|-c) ;;
    push|ls-remote) if [ -z "$SUB" ]; then SUB="$a"; fi ;;
  esac
  case "$a" in
    *"$PORCH_SHIM_REF") HIT_REF=1 ;;
  esac
done

fire() {
  [ -n "${PORCH_SHIM_STATE:-}" ] && : > "$PORCH_SHIM_STATE"
  : > "$PORCH_SHIM_MARKER"
  sleep "${PORCH_SHIM_SLEEP:-60}"
}

case "$MODE" in
  hang_before_intent)
    if [ "$SUB" = "ls-remote" ] && [ "$HIT_REF" = "1" ]; then
      fire
      exit 0
    fi
    ;;
  hang_before_push)
    if [ "$SUB" = "push" ] && [ "$HIT_REF" = "1" ]; then
      # Never push: origin must not move.
      fire
      exit 0
    fi
    ;;
  push_then_hang)
    if [ "$SUB" = "push" ] && [ "$HIT_REF" = "1" ]; then
      # Let the real push complete, so origin moves and git writes the
      # remote-tracking ref, and only then signal the test. The daemon is
      # blocked waiting on this process, so the outcome record cannot commit.
      "$PORCH_REAL_GIT" "$@"
      rc=$?
      fire
      exit $rc
    fi
    ;;
esac

exec "$PORCH_REAL_GIT" "$@"
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
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
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
COV_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; COV_JSON="$COV_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
  COV_JSON="$COV_JSON{\"path\":\"$f\",\"status\":\"pass\"}"
done
FILES_JSON="$FILES_JSON]"
COV_JSON="$COV_JSON]"
printf '{"comments":[],"files":%s,"coverage":%s}\n' "$FILES_JSON" "$COV_JSON" > "$OUT"
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

/// A `gh` that logs every invocation to an append-only file.
///
/// The log is what "no second pull request" is asserted on: `create` overwrites
/// the state file, so a two-run scenario asserting on state would pass by
/// ordering accident (FAULT-3.1).
fn install_fake_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    let script = r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
{
  printf '+'
  for a in "$@"; do printf ' %s' "$a"; done
  printf '\n'
} >> "$PORCH_HOME/gh-argv.log"

for a in "$@"; do
  if [ "$a" = "--version" ]; then
    echo "gh version 2.50.0 (fake)"
    exit 0
  fi
done

STATE="$PORCH_HOME/gh-pr-state"
CMD=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done

case "$CMD" in
  list)
    if [ -f "$STATE" ]; then cat "$STATE"; else printf '[]\n'; fi
    exit 0
    ;;
  create)
    cat > "$PORCH_HOME/gh-pr-body.txt"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    exit 0
    ;;
  edit)
    cat > "$PORCH_HOME/gh-pr-body.txt"
    if [ ! -f "$STATE" ]; then
      printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    fi
    exit 0
    ;;
  view)
    printf '{"mergeable":"MERGEABLE"}\n'
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
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    origin: PathBuf,
    fake_review: PathBuf,
    fake_gh: PathBuf,
    shim: PathBuf,
    real_git: PathBuf,
    path: String,
}

/// How a daemon is launched: with the shim armed, or with the real `git`.
#[derive(Clone)]
enum Arm {
    /// No override at all — the restarted gate runs the real `git`, so no
    /// verdict in this suite is derived through the instrument (FAULT-4.5).
    RealGit,
    Shim {
        mode: &'static str,
        refname: String,
        marker: PathBuf,
        state: PathBuf,
    },
}

fn setup() -> Setup {
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
    let shim = install_git_shim(&bin_dir);
    let real_git = which_git();

    init_bare(&origin).unwrap();
    // The fixture owns its default branch, identity, and signing; ambient git
    // configuration is not a dependency of these tests (FAULT-6.6).
    git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    seed_repo(&seed, &origin);

    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    let work = work.canonicalize().unwrap();
    configure_identity(&work);

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
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    let s = Setup {
        _tmp: tmp,
        work,
        home,
        origin,
        fake_review,
        fake_gh,
        shim,
        real_git,
        path,
    };
    kill_daemon(&s.home);
    s
}

fn which_git() -> PathBuf {
    let out = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git must be on PATH");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

fn configure_identity(dir: &Path) {
    git(dir, &["config", "user.email", "porch@example.com"]);
    git(dir, &["config", "user.name", "Porch"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn seed_repo(seed: &Path, origin: &Path) {
    git(seed, &["init", "-b", "main"]);
    configure_identity(seed);
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(seed, &["add", "README"]);
    git(seed, &["commit", "-m", "base"]);
    git(seed, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(seed, &["push", "-u", "origin", "main"]);
}

// ---------------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------------

fn daemon_pid(home: &Path) -> Option<u32> {
    std::fs::read_to_string(home.join("daemon.pid"))
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
}

/// True while `pid` is still running.
///
/// A zombie counts as dead. The daemon is spawned by this test process and
/// never reaped, so `/proc/<pid>` outlives it; only the state field says whether
/// it is gone. Its file descriptors — including the state root's exclusive lock —
/// are released on exit, which is what the restart actually waits for.
fn pid_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) state …`, and comm may contain spaces and parentheses.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state != "Z" && state != "X")
}

/// SIGTERM the daemon's group, reap anything still bound to this home, then
/// wait for the pid to be gone.
///
/// Waiting rather than sleeping matters: a new daemon takes an exclusive flock
/// and exits with `daemon already running` if the old one has not released it,
/// after which `wait_for_health` would burn its budget and fail with a message
/// that does not name the cause.
fn kill_daemon(home: &Path) -> Option<u32> {
    let pid = daemon_pid(home);
    if let Some(pid) = pid {
        kill_group(pid);
    }
    let marker = home.display().to_string();
    let _ = StdCommand::new("pkill")
        .args(["-9", "-f", &marker])
        .output();
    if let Some(pid) = pid {
        let start = Instant::now();
        while pid_alive(pid) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "daemon {pid} did not exit"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    pid
}

fn start_daemon(s: &Setup, arm: &Arm) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut env: Vec<(&str, std::ffi::OsString)> = vec![
        (REVIEW_BIN_ENV, s.fake_review.clone().into()),
        (GH_BIN_ENV, s.fake_gh.clone().into()),
        ("PATH", s.path.clone().into()),
        ("PORCH_REVIEW_TIMEOUT_SECS", "20".into()),
        ("PORCH_GH_TIMEOUT_SECS", "20".into()),
        ("PORCH_DELIVER_CHECK_TIMEOUT_SECS", "3".into()),
        ("PORCH_DELIVER_CHECK_POLL_SECS", "1".into()),
    ];
    if let Arm::Shim {
        mode,
        refname,
        marker,
        state,
    } = arm
    {
        env.push((GIT_BIN_ENV, s.shim.clone().into()));
        env.push(("PORCH_REAL_GIT", s.real_git.clone().into()));
        env.push(("PORCH_SHIM_MODE", (*mode).into()));
        env.push(("PORCH_SHIM_REF", refname.clone().into()));
        env.push(("PORCH_SHIM_MARKER", marker.clone().into()));
        env.push(("PORCH_SHIM_STATE", state.clone().into()));
        env.push(("PORCH_SHIM_SLEEP", SHIM_SLEEP_SECS.into()));
    }
    let refs: Vec<(&str, &std::ffi::OsStr)> =
        env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    porch_gate::spawn_detached_with_env(&bin, &s.home, &refs).unwrap();
    porch_gate::wait_for_health(&s.home, Duration::from_secs(15)).unwrap();
}

/// Restart with the real `git` and prove the gate really died.
///
/// A successful health response proves only that *some* daemon answers on the
/// socket, so the pid change is what makes the startup barrier argument valid
/// (FAULT-6.4). After it, reconciliation has already completed: it runs inside
/// `Db::open` → `reconcile_stale` → `recover_stale` before `UnixListener::bind`,
/// and either recovery error refuses to serve — so every verdict row, run
/// status, and error is durable with no polling (FAULT-6.3).
fn restart_and_reconcile(s: &Setup, killed: Option<u32>) {
    start_daemon(s, &Arm::RealGit);
    let now = daemon_pid(&s.home).expect("a restarted daemon records its pid");
    if let Some(killed) = killed {
        assert_ne!(
            now, killed,
            "the gate under test really died and a new one reconciled"
        );
    }
}

fn push_branch(s: &Setup, branch: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PATH", &s.path)
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push porch {branch}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The documented recovery for an interrupted run: a new run and a fresh
/// worktree over the same commit.
fn rerun(s: &Setup, run_id: &str) {
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env(GH_BIN_ENV, &s.fake_gh)
        .env("PATH", &s.path)
        .args(["rerun", "--run-id", run_id])
        .assert()
        .success();
}

fn commit_change(work: &Path, name: &str, body: &str) -> String {
    std::fs::write(work.join(name), body).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
    git_out(work, &["rev-parse", "HEAD"]).expect("HEAD")
}

fn wait_file(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "marker {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_status(db: &Db, repo_id: &str, want: &[&str], timeout: Duration) -> porch_gate::RunRow {
    wait_status_after(db, repo_id, want, None, timeout)
}

/// Like [`wait_status`], ignoring `not_run`.
///
/// The interrupted run is already terminal when a retry starts, so waiting on
/// the latest terminal run would match it before the retry's run exists.
fn wait_status_after(
    db: &Db,
    repo_id: &str,
    want: &[&str],
    not_run: Option<&str>,
    timeout: Duration,
) -> porch_gate::RunRow {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        if let Some(run) = runs
            .last()
            .filter(|r| not_run.is_none_or(|skip| r.id != skip))
        {
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

// ---------------------------------------------------------------------------
// Ground truth
// ---------------------------------------------------------------------------

fn db_of(s: &Setup) -> Db {
    Db::open(&s.home.join("state.sqlite")).unwrap()
}

fn latest_run(s: &Setup) -> porch_gate::RunRow {
    let db = db_of(s);
    let repo_id = repo_id_for(&s.work);
    db.runs_for_repo(&repo_id)
        .unwrap()
        .last()
        .cloned()
        .expect("a run")
}

/// What `origin` actually holds for a branch — read directly, not inferred.
fn origin_sha(s: &Setup, branch: &str) -> Option<String> {
    git_out(&s.origin, &["rev-parse", &format!("refs/heads/{branch}")])
}

/// The gate repository, which recovery does not remove — unlike the worktree.
fn bare_path(s: &Setup) -> PathBuf {
    s.home
        .join("repos")
        .join(format!("{}.git", repo_id_for(&s.work)))
}

/// The gate repository's remote-tracking ref, which is `RECON-2.2`'s
/// discriminator.
fn tracking_sha(s: &Setup, branch: &str) -> Option<String> {
    git_out(
        &bare_path(s),
        &["rev-parse", &format!("refs/remotes/origin/{branch}")],
    )
}

fn forward_kinds(db: &Db, run_id: &str) -> Vec<String> {
    rounds::forward::records_for_run(db, run_id)
        .unwrap()
        .into_iter()
        .map(|r| r.kind.as_str().to_string())
        .collect()
}

fn verdicts(db: &Db, run_id: &str) -> Vec<rounds::VerdictRow> {
    rounds::reconcile::verdicts_for_run(db, run_id).unwrap()
}

fn gh_log(s: &Setup) -> String {
    std::fs::read_to_string(s.home.join("gh-argv.log")).unwrap_or_default()
}

/// A reached-origin or undetermined message must never claim a pull request is
/// absent: the durable evidence for a crash before the `gh` call and for one
/// after it is byte-identical (RECON-1.7).
fn assert_claims_nothing_about_pr_absence(err: &str) {
    for forbidden in [
        "no pull request",
        "without a pull request",
        "pull request was not",
        "pull request is absent",
    ] {
        assert!(
            !err.to_lowercase().contains(forbidden),
            "message must not assert a pull request is absent ({forbidden}): {err}"
        );
    }
}

fn conclusion_count(err: &str) -> usize {
    err.matches("Re-push to continue").count()
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// The window ROAD-7 left open and ROAD-8's discriminator exists to close: the
/// push landed, the outcome record did not commit, the gate died.
///
/// This is also the only end-to-end proof that a forward push through the
/// production path writes the gate's remote-tracking ref (FAULT-2.3). If it did
/// not, `Evidence::TrackingRefMatched` would be unreachable in production and
/// the store-level test that covers it would be proving nothing.
#[test]
fn forward_landed_then_killed_reconciles_to_reached_origin() {
    let s = setup();
    let branch = "feat-landed";
    let marker = s.home.join("shim-fired");
    let arm = Arm::Shim {
        mode: "push_then_hang",
        refname: format!("refs/heads/{branch}"),
        marker: marker.clone(),
        state: s.home.join("shim-state"),
    };
    start_daemon(&s, &arm);
    let head = commit_change(&s.work, "landed.txt", "x\n");
    push_branch(&s, branch);

    wait_file(&marker, Duration::from_secs(90));
    let killed = kill_daemon(&s.home);

    // Ground truth, read before porch gets a second chance to touch anything.
    assert_eq!(
        origin_sha(&s, branch).as_deref(),
        Some(head.as_str()),
        "the shim let the real push complete, so origin moved"
    );
    assert_eq!(
        tracking_sha(&s, branch).as_deref(),
        Some(head.as_str()),
        "a real forward push writes refs/remotes/origin/<branch> in the gate repo; \
         RECON-2.2's discriminator rests on this"
    );

    restart_and_reconcile(&s, killed);

    let run = latest_run(&s);
    let db = db_of(&s);
    assert_eq!(
        forward_kinds(&db, &run.id),
        vec!["intent".to_string()],
        "the outcome record never committed: the gate was blocked in the push"
    );

    let v = verdicts(&db, &run.id);
    assert_eq!(v.len(), 1, "one conclusion per interrupted forward: {v:?}");
    assert_eq!(v[0].verdict, rounds::Verdict::ReachedOrigin);
    assert_eq!(v[0].evidence, "tracking_ref_matched");
    assert_eq!(v[0].authorized_sha, head);
    assert_eq!(v[0].observed_tracking_sha.as_deref(), Some(head.as_str()));

    let err = run.error.clone().unwrap_or_default();
    assert!(
        err.starts_with("daemon restarted"),
        "the interruption phrase stays a prefix (RECON-5.3): {err}"
    );
    assert!(
        err.contains("carries the authorized commit on origin"),
        "the operator is told the branch landed: {err}"
    );
    assert!(
        err.contains("pull request state is unrecorded"),
        "and that the pull request state is not known: {err}"
    );
    assert_eq!(
        conclusion_count(&err),
        1,
        "exactly one conclusion, not one per repair handoff: {err}"
    );
    assert_claims_nothing_about_pr_absence(&err);
}

/// The intent committed, the push never moved anything, the gate died. Porch
/// cannot tell "about to push" from "pushing", and says so.
#[test]
fn killed_before_the_push_reconciles_to_indeterminate() {
    let s = setup();
    let branch = "feat-unpushed";
    let marker = s.home.join("shim-fired");
    let arm = Arm::Shim {
        mode: "hang_before_push",
        refname: format!("refs/heads/{branch}"),
        marker: marker.clone(),
        state: s.home.join("shim-state"),
    };
    start_daemon(&s, &arm);
    let head = commit_change(&s.work, "unpushed.txt", "x\n");
    push_branch(&s, branch);

    wait_file(&marker, Duration::from_secs(90));
    let killed = kill_daemon(&s.home);

    assert_eq!(
        origin_sha(&s, branch),
        None,
        "the shim swallowed the push, so origin never saw the branch"
    );
    assert_eq!(tracking_sha(&s, branch), None);

    restart_and_reconcile(&s, killed);

    let run = latest_run(&s);
    let db = db_of(&s);
    assert_eq!(
        forward_kinds(&db, &run.id),
        vec!["intent".to_string()],
        "the intent commits before the pushing command runs (FWDAUTH)"
    );

    let v = verdicts(&db, &run.id);
    assert_eq!(v.len(), 1, "one conclusion: {v:?}");
    assert_eq!(v[0].verdict, rounds::Verdict::Indeterminate);
    assert_eq!(v[0].evidence, "intent_only");
    assert_eq!(v[0].authorized_sha, head);
    assert_eq!(v[0].observed_tracking_sha, None);

    let err = run.error.clone().unwrap_or_default();
    assert!(
        err.contains("did not record its result"),
        "porch says it does not know, rather than guessing: {err}"
    );
    assert!(
        err.contains("Re-push to continue"),
        "the remedy is stated, not only the diagnosis: {err}"
    );
    assert!(
        !err.contains("carries the authorized commit"),
        "no claim that the branch landed: {err}"
    );
    assert!(
        !err.to_lowercase().contains("unchanged"),
        "and no claim that origin is unchanged, which porch cannot know: {err}"
    );
    assert_eq!(conclusion_count(&err), 1, "exactly one conclusion: {err}");
    assert_claims_nothing_about_pr_absence(&err);
}

/// Killed before the lease observes: nothing external happened, so there is
/// nothing to conclude. `RECON-8.1` reached by a real kill rather than by an
/// absent fixture.
#[test]
fn killed_before_intent_leaves_no_conclusion() {
    let s = setup();
    let branch = "feat-nointent";
    let marker = s.home.join("shim-fired");
    let arm = Arm::Shim {
        mode: "hang_before_intent",
        refname: format!("refs/heads/{branch}"),
        marker: marker.clone(),
        state: s.home.join("shim-state"),
    };
    start_daemon(&s, &arm);
    commit_change(&s.work, "nointent.txt", "x\n");
    push_branch(&s, branch);

    wait_file(&marker, Duration::from_secs(90));
    let killed = kill_daemon(&s.home);
    assert_eq!(origin_sha(&s, branch), None, "origin untouched");

    restart_and_reconcile(&s, killed);

    let run = latest_run(&s);
    let db = db_of(&s);
    assert!(
        forward_kinds(&db, &run.id).is_empty(),
        "a refusal before the lease observes writes no forward record"
    );
    assert!(
        verdicts(&db, &run.id).is_empty(),
        "no record means no conclusion to persist"
    );
    assert_eq!(run.status, "failed", "RECON-8.1 is unchanged");
    assert_eq!(
        conclusion_count(&run.error.clone().unwrap_or_default()),
        0,
        "and the operator is told nothing about a forward that never started"
    );
    kill_daemon(&s.home);
}

/// `origin` refuses the push. No kill: the run fails closed on its own path,
/// which is why it receives no conclusion — `RECON-1.1` scopes conclusions to
/// interrupted runs, and a refusal is not an interruption.
///
/// What this asserts is the record shape production writes for a real refusal,
/// which is the shape the store-level classifier tests were written against.
#[test]
fn refused_push_records_failure_and_leaves_origin_unchanged() {
    let s = setup();
    let branch = "feat-refused";
    let hook = s.origin.join("hooks").join("pre-receive");
    std::fs::create_dir_all(s.origin.join("hooks")).unwrap();
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'origin declines this push' >&2\nexit 1\n",
    )
    .unwrap();
    chmod_755(&hook);

    start_daemon(&s, &Arm::RealGit);
    commit_change(&s.work, "refused.txt", "x\n");
    push_branch(&s, branch);

    let db = db_of(&s);
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["failed"], Duration::from_secs(120));

    assert_eq!(
        origin_sha(&s, branch),
        None,
        "a declined push leaves origin unchanged"
    );
    let kinds = forward_kinds(&db, &run.id);
    assert!(
        kinds.contains(&"intent".to_string()),
        "the intent commits before the push (FWDAUTH): {kinds:?}"
    );
    assert!(
        kinds.contains(&"push_failed".to_string()),
        "and the refusal is recorded as porch's own command result: {kinds:?}"
    );
    assert!(
        verdicts(&db, &run.id).is_empty(),
        "a run that reported its own error is not reconciled (FAULT-5.3)"
    );
    let err = run.error.clone().unwrap_or_default();
    assert!(
        !err.to_lowercase().contains("origin is unchanged"),
        "porch reports its own command's failure, never the remote's state: {err}"
    );
    kill_daemon(&s.home);
}

/// Recovering from a crash must not cost a second pull request.
#[test]
fn retry_after_reached_origin_adopts_the_pull_request() {
    let s = setup();
    let branch = "feat-adopt";
    let marker = s.home.join("shim-fired");
    let arm = Arm::Shim {
        mode: "push_then_hang",
        refname: format!("refs/heads/{branch}"),
        marker: marker.clone(),
        state: s.home.join("shim-state"),
    };
    start_daemon(&s, &arm);
    let head = commit_change(&s.work, "adopt.txt", "x\n");
    push_branch(&s, branch);
    wait_file(&marker, Duration::from_secs(90));
    let killed = kill_daemon(&s.home);
    assert_eq!(origin_sha(&s, branch).as_deref(), Some(head.as_str()));

    restart_and_reconcile(&s, killed);
    let first = latest_run(&s);
    {
        let db = db_of(&s);
        let v = verdicts(&db, &first.id);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].verdict, rounds::Verdict::ReachedOrigin);
    }
    // The interrupted run never reached the pull request adapter.
    assert!(
        !gh_log(&s).contains(" pr create"),
        "the gate died before the pull request call: {}",
        gh_log(&s)
    );

    // Follow the remedy. A re-push is not available here and the message says
    // so: the branch already matches the gate ref, so `git push porch` reports
    // everything up to date and creates no run. `porch rerun` is the documented
    // recovery for exactly that.
    let err = first.error.clone().unwrap_or_default();
    assert!(
        err.contains("porch rerun"),
        "the remedy names a command that does something when the branch has not \
         moved: {err}"
    );
    rerun(&s, &first.id);

    let db = db_of(&s);
    let repo_id = repo_id_for(&s.work);
    let second = wait_status_after(
        &db,
        &repo_id,
        &["completed", "failed", "parked"],
        Some(&first.id),
        Duration::from_secs(120),
    );
    assert_ne!(second.id, first.id, "the retry is a new run");

    assert_eq!(
        origin_sha(&s, branch).as_deref(),
        Some(head.as_str()),
        "origin already carried the authorized SHA and did not move"
    );
    let kinds = forward_kinds(&db, &second.id);
    assert!(
        kinds.contains(&"already_current".to_string()),
        "the retry observed the remote already at the authorized SHA: {kinds:?}"
    );
    let intents = kinds.iter().filter(|k| k.as_str() == "intent").count();
    assert_eq!(
        intents, 1,
        "the retry records its own intent under its own deliver attempt \
         (FWDAUTH-2.7): {kinds:?}"
    );

    let log = gh_log(&s);
    assert_eq!(
        log.matches(" pr create").count(),
        1,
        "exactly one pull request was ever opened across both runs: {log}"
    );
    kill_daemon(&s.home);
}

/// Repeated restarts are idempotent: a conclusion is an append, not a
/// reclassification, and re-deriving one does not add a row (RECON-3.7).
#[test]
fn two_restarts_leave_one_conclusion() {
    let s = setup();
    let branch = "feat-twice";
    let marker = s.home.join("shim-fired");
    let arm = Arm::Shim {
        mode: "hang_before_push",
        refname: format!("refs/heads/{branch}"),
        marker: marker.clone(),
        state: s.home.join("shim-state"),
    };
    start_daemon(&s, &arm);
    commit_change(&s.work, "twice.txt", "x\n");
    push_branch(&s, branch);
    wait_file(&marker, Duration::from_secs(90));
    let killed = kill_daemon(&s.home);

    restart_and_reconcile(&s, killed);
    let run = latest_run(&s);
    let after_first = {
        let db = db_of(&s);
        verdicts(&db, &run.id)
    };
    assert_eq!(after_first.len(), 1);

    let killed = kill_daemon(&s.home);
    restart_and_reconcile(&s, killed);

    let db = db_of(&s);
    let after_second = verdicts(&db, &run.id);
    assert_eq!(
        after_second.len(),
        1,
        "the second restart re-derives nothing: {after_second:?}"
    );
    assert_eq!(after_second[0].id, after_first[0].id, "the same row");
    assert!(
        rounds::reconcile::attempts_awaiting_verdict(&db)
            .unwrap()
            .is_empty(),
        "and nothing is left awaiting a conclusion (FAULT-5.5)"
    );
    kill_daemon(&s.home);
}
