//! M25: the wedged, dead, and refusing-startup daemon suite (ROAD-11 / DFAULT).
//!
//! ROAD-11 asked for a suite over three daemon faults. Two of them had no
//! operator-facing vocabulary to assert against and the third could not be
//! observed at all: `rpc_call` had no read deadline, so against a daemon stopped
//! after `UnixListener::bind` every diagnostic command blocked forever. Measured
//! before the fix, `porch status`, `porch doctor`, `porch daemon status`, and
//! `porch runs` each produced no output and had not returned at twelve seconds.
//!
//! So the assertions here are about `DaemonCondition`, one of four named values,
//! and never about the absence of output within a window — asserting a hang is
//! the timer race `FAULT-1.5` forbids, and it would freeze a defect as correct
//! (`DFAULT-5.2`).
//!
//! Each condition is induced by a distinct mechanism and none of them is a
//! behaviour switch compiled into the product (`FAULT-4.2`): a live daemon for
//! ready, `kill_group` for unreachable, the existing `PORCH_TEST_FAIL_RECOVER_STALE`
//! recovery hook for refusing, and `SIGSTOP` for not-answering. `SIGSTOP` is the
//! honest instrument for a wedge — the process stays alive, so the socket file and
//! its listen backlog stay in the kernel and a client's `connect` and `write` both
//! still succeed.
//!
//! Dead-daemon coverage below is deliberately thin. `crates/porch/tests/` already
//! holds 219 `kill_daemon(` call sites across 19 files; what none of them assert is
//! what an operator's commands do once the daemon has died, which is what this file
//! adds (`DFAULT-5.8`).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::{RPC_TIMEOUT_ENV, kill_group, refusal_path};
use tempfile::TempDir;

/// Short enough that a wedged-daemon assertion costs under a second, and the
/// value every bound in this file is derived from rather than a sleep
/// (`DFAULT-5.3`).
const RPC_TIMEOUT_MS: &str = "700";

/// Generous ceiling for one command against a wedged daemon. A debug-built binary
/// plus a handful of bounded probes lives far below this; an unbounded read does
/// not finish at all.
const TERMINATES_WITHIN: Duration = Duration::from_secs(20);

fn git(dir: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(dir)
        // The fixture owns its identity, signing, and default branch. Ambient git
        // config is not a dependency: this repo has already measured a fixture go
        // from 220ms to an intermittent 30-second stall because an inherited
        // `commit.gpgsign` invoked a signing program (`DFAULT-5.6`).
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
}

fn setup() -> Setup {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    git(&root, &["init", "--bare", "-b", "main", "origin.git"]);
    git(&work, &["init", "-b", "main"]);
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);
    git(&work, &["config", "commit.gpgsign", "false"]);
    std::fs::write(work.join("README"), "base\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-m", "base"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&work, &["push", "-u", "origin", "main"]);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env("HOME", &root)
        .args(["init", "--skip-setup"])
        .assert()
        .success();

    let s = Setup {
        _tmp: tmp,
        work,
        home,
    };
    // `porch init` leaves a daemon running; every test below chooses its own
    // starting condition.
    kill_daemon(&s.home);
    s
}

/// A `porch` invocation with the fixture's environment and a short RPC deadline.
fn porch(s: &Setup) -> Command {
    let mut c = Command::cargo_bin("porch").unwrap();
    c.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env("HOME", s.home.parent().unwrap())
        .env(RPC_TIMEOUT_ENV, RPC_TIMEOUT_MS);
    c
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
/// A zombie counts as dead: the daemon is spawned by this test process and never
/// reaped, so `/proc/<pid>` outlives it and only the state field is conclusive.
fn pid_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state != "Z" && state != "X")
}

/// `/proc` state letter, so a test can prove a process is stopped rather than
/// assume the signal landed.
fn pid_state(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(str::to_string)
}

fn kill_daemon(home: &Path) -> Option<u32> {
    let pid = daemon_pid(home);
    if let Some(pid) = pid {
        kill_group(pid);
        let start = Instant::now();
        while pid_alive(pid) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "daemon {pid} did not exit"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    let _ = std::fs::remove_file(home.join("daemon.pid"));
    pid
}

/// Spawn a daemon. `fail_recover` arms the existing recovery-failure hook, which
/// makes `run_daemon` refuse before it binds the socket.
fn spawn_daemon(s: &Setup, fail_recover: bool) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut env: Vec<(&str, std::ffi::OsString)> = vec![(RPC_TIMEOUT_ENV, RPC_TIMEOUT_MS.into())];
    if fail_recover {
        env.push(("PORCH_TEST_FAIL_RECOVER_STALE", "1".into()));
    }
    let refs: Vec<(&str, &std::ffi::OsStr)> =
        env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    porch_gate::spawn_detached_with_env(&bin, &s.home, &refs).unwrap();
}

fn start_healthy_daemon(s: &Setup) -> u32 {
    spawn_daemon(s, false);
    porch_gate::wait_for_health(&s.home, Duration::from_secs(15)).unwrap();
    daemon_pid(&s.home).expect("a serving daemon records its pid")
}

/// A daemon held in `SIGSTOP` for the lifetime of the guard.
///
/// The guard exists so that a failed assertion cannot leave a stopped process
/// behind holding the state root's lock (`DFAULT-5.7`). `Drop` runs on the unwind.
struct WedgedDaemon {
    pid: u32,
}

impl WedgedDaemon {
    fn new(s: &Setup) -> Self {
        let pid = start_healthy_daemon(s);
        signal(pid, "-STOP");
        let start = Instant::now();
        while pid_state(pid).as_deref() != Some("T") {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "daemon {pid} did not stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { pid }
    }
}

impl Drop for WedgedDaemon {
    fn drop(&mut self) {
        // A stopped process cannot act on SIGTERM, so continue it first.
        signal(self.pid, "-CONT");
        kill_group(self.pid);
    }
}

fn signal(pid: u32, sig: &str) {
    let _ = StdCommand::new("kill")
        .args([sig, &pid.to_string()])
        .status();
}

/// Poll for a file, bounded. A marker on disk rather than a sleep, so nothing here
/// races a timer.
fn wait_for_file(path: &Path, within: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < within,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `condition` field of `porch daemon status --json`, plus how long the
/// command took to say it.
fn reported_condition(s: &Setup) -> (serde_json::Value, Duration) {
    let start = Instant::now();
    let out = porch(s)
        .args(["daemon", "status", "--json"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("status json: {e}: {text}"));
    (v["condition"].clone(), elapsed)
}

fn condition_label(s: &Setup) -> String {
    reported_condition(s).0["condition"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// The four conditions
// ---------------------------------------------------------------------------

#[test]
fn a_serving_daemon_reports_ready() {
    let s = setup();
    let pid = start_healthy_daemon(&s);

    let (cond, _) = reported_condition(&s);
    assert_eq!(cond["condition"], "ready");
    assert_eq!(cond["pid"], pid);

    porch(&s)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[ok  ] daemon: ready"));

    kill_daemon(&s.home);
}

#[test]
fn a_dead_daemon_reports_unreachable_and_names_the_socket() {
    let s = setup();
    start_healthy_daemon(&s);
    kill_daemon(&s.home);

    let (cond, elapsed) = reported_condition(&s);
    assert_eq!(cond["condition"], "unreachable");
    assert!(
        cond["reason"].as_str().unwrap().contains("socket"),
        "reason should name the socket: {cond}"
    );
    assert!(elapsed < TERMINATES_WITHIN, "took {elapsed:?}");

    // Not running is the ordinary state of a checkout whose daemon has exited, so
    // it is information rather than a failure — and the advice must name a command
    // that actually starts one (`DFAULT-4.3`).
    porch(&s)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("[info] daemon: not running"))
        .stdout(predicates::str::contains("porch daemon start"))
        .stdout(predicates::str::contains("push notify").not());
}

#[test]
fn a_refused_startup_reports_refusing_and_names_the_barrier() {
    let s = setup();
    spawn_daemon(&s, true);
    // The refusal record is the marker: it is written on the way out of
    // `run_daemon`, before the error is returned.
    wait_for_file(&refusal_path(&s.home), Duration::from_secs(15));

    let (cond, elapsed) = reported_condition(&s);
    assert_eq!(
        cond["condition"], "refusing",
        "a refusal must not read as a dead daemon: {cond}"
    );
    assert_eq!(cond["barrier"], porch_gate::BARRIER_RECOVER_STALE);
    assert_eq!(cond["cause"], "test recover_stale failure");
    assert!(elapsed < TERMINATES_WITHIN, "took {elapsed:?}");

    // A daemon that will not serve is a fault the operator has to act on.
    porch(&s)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicates::str::contains("daemon: refusing to start"))
        .stdout(predicates::str::contains("test recover_stale failure"));
}

#[test]
fn a_refusal_cause_survives_a_later_start_attempt() {
    let s = setup();
    spawn_daemon(&s, true);
    wait_for_file(&refusal_path(&s.home), Duration::from_secs(15));
    let first = std::fs::read_to_string(refusal_path(&s.home)).unwrap();

    // The operator's instinct is to retry. Before the record existed, the retry
    // truncated `logs/daemon.log` and took the only copy of the cause with it
    // (`DFAULT-2.2`).
    spawn_daemon(&s, true);
    wait_for_file(&refusal_path(&s.home), Duration::from_secs(15));
    porch(&s).args(["daemon", "status"]).assert().success();

    let after = std::fs::read_to_string(refusal_path(&s.home)).unwrap();
    assert!(
        after.contains("test recover_stale failure"),
        "the cause must survive a retry: {after}"
    );
    assert_eq!(condition_label(&s), "refusing");
    assert!(!first.is_empty());

    // And a start that gets through clears it, so a stale refusal is never
    // reported as a current one (`DFAULT-2.3`).
    start_healthy_daemon(&s);
    assert_eq!(condition_label(&s), "ready");
    assert!(
        !refusal_path(&s.home).exists(),
        "a serving daemon must clear the refusal record"
    );
    kill_daemon(&s.home);
}

#[test]
fn a_wedged_daemon_reports_not_answering_rather_than_hanging() {
    let s = setup();
    let wedged = WedgedDaemon::new(&s);

    let (cond, elapsed) = reported_condition(&s);
    assert_eq!(
        cond["condition"], "not-answering",
        "a wedged daemon must not read as healthy or as absent: {cond}"
    );
    assert_eq!(cond["pid"], wedged.pid, "the operator needs the pid");
    assert_eq!(
        cond["waited_ms"],
        RPC_TIMEOUT_MS.parse::<u64>().unwrap(),
        "the wait must be the configured deadline, not a wall-clock guess"
    );
    assert!(elapsed < TERMINATES_WITHIN, "took {elapsed:?}");

    // `running` and `socket_healthy` cannot name this state between them: a pid
    // file plus an unanswering socket reads the same as a daemon that died between
    // the pid write and the bind (`DFAULT-4.4`).
    porch(&s)
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("condition=not-answering"))
        .stdout(predicates::str::contains("porch daemon stop --force"));
}

#[test]
fn every_diagnostic_command_terminates_against_a_wedged_daemon() {
    let s = setup();
    let _wedged = WedgedDaemon::new(&s);

    // Measured before the RPC gained a deadline, each of these produced no output
    // and had not returned at twelve seconds.
    for args in [
        vec!["doctor"],
        vec!["daemon", "status"],
        vec!["status"],
        vec!["runs"],
    ] {
        let start = Instant::now();
        let out = porch(&s).args(&args).output().unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < TERMINATES_WITHIN,
            "porch {args:?} took {elapsed:?}"
        );
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !all.trim().is_empty(),
            "porch {args:?} returned without saying anything"
        );
    }
}

#[test]
fn a_wedged_daemon_is_not_replaced_by_a_second_one() {
    let s = setup();
    let wedged = WedgedDaemon::new(&s);

    // `porch rerun` still calls `ensure_daemon` (a work command). Inspect
    // commands no longer spawn (`LOOK-1.1`). Bounding the RPC turned the
    // former infinite hang into a reachable spawn, and the spawn used to
    // succeed: the single-instance guard discarded `fs4`'s `Ok(false)` for a
    // contended lock, so a second daemon bound the socket and served the same
    // state root while the first was alive (`DFAULT-6.1`, `DFAULT-6.2`).
    let out = porch(&s).arg("rerun").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not answering"),
        "the real fault should be reported, not a lock error: {stderr}"
    );

    assert_eq!(
        daemon_pid(&s.home),
        Some(wedged.pid),
        "no replacement daemon may take over the pid file"
    );
    assert_eq!(
        pid_state(wedged.pid).as_deref(),
        Some("T"),
        "the wedged daemon is still the one holding the state root"
    );
}

#[test]
fn a_second_daemon_cannot_bind_while_the_first_holds_the_lock() {
    let s = setup();
    let first = start_healthy_daemon(&s);

    spawn_daemon(&s, false);
    // The loser writes its error to `logs/daemon.log`, which its own spawn
    // truncated, so the file is its own output.
    let log = s.home.join("logs").join("daemon.log");
    let start = Instant::now();
    let refused = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("daemon already running") {
            break true;
        }
        if start.elapsed() > Duration::from_secs(15) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        refused,
        "a second daemon must refuse the lock: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );

    assert_eq!(
        daemon_pid(&s.home),
        Some(first),
        "the pid file must still name the daemon that holds the lock"
    );
    assert_eq!(condition_label(&s), "ready");
    kill_daemon(&s.home);
}

// ---------------------------------------------------------------------------
// GOAL-4: the daemon-free surfaces, in every condition
// ---------------------------------------------------------------------------

/// `ESCAPE` made detach need neither a healthy daemon nor a readable database, and
/// proved it with `porch-gate` unit tests. This is that property through the
/// binary, with no daemon answering (`DFAULT-5.4`).
#[test]
fn eject_detaches_in_every_condition() {
    // Dead.
    {
        let s = setup();
        start_healthy_daemon(&s);
        kill_daemon(&s.home);
        porch(&s)
            .arg("eject")
            .assert()
            .success()
            .stdout(predicates::str::contains("PORCH_HOME left intact"));
    }
    // Refusing to start.
    {
        let s = setup();
        spawn_daemon(&s, true);
        wait_for_file(&refusal_path(&s.home), Duration::from_secs(15));
        porch(&s)
            .arg("eject")
            .assert()
            .success()
            .stdout(predicates::str::contains("ejected repo"));
    }
    // Wedged — the state in which every diagnostic command used to hang.
    {
        let s = setup();
        let _wedged = WedgedDaemon::new(&s);
        porch(&s)
            .arg("eject")
            .assert()
            .success()
            .stdout(predicates::str::contains("ejected repo"));
    }
}

#[test]
fn agent_sync_answers_without_a_daemon() {
    let s = setup();
    let wedged = WedgedDaemon::new(&s);

    // Reads the state database directly. It need not find a run — it must answer
    // rather than block, which is the half of GOAL-4 that says "inspect state".
    let start = Instant::now();
    let out = porch(&s).args(["agent", "sync"]).output().unwrap();
    let elapsed = start.elapsed();
    assert!(elapsed < TERMINATES_WITHIN, "took {elapsed:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|e| panic!("agent sync must answer with JSON: {e}: {stdout}"));

    assert_eq!(
        pid_state(wedged.pid).as_deref(),
        Some("T"),
        "asking for state must not have disturbed the daemon"
    );
}

use predicates::prelude::PredicateBooleanExt;
