use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::Result;
use crate::db::Db;
use crate::events::{EventHub, clear_event_hub, install_event_hub};
use crate::executor::RunExecutor;
use crate::home::{db_path, lock_path, logs_dir, pid_path, socket_path};
use crate::rpc::{self, Request};

struct Inflight {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct DaemonState {
    inflight: HashMap<String, Inflight>,
    hub: Arc<EventHub>,
}

/// Hold the lock, recover stale runs, bind the socket, serve JSON-RPC.
///
/// Each accepted connection is handled on its own thread so a long-lived
/// `subscribe` does not stall `health` / `start_run`.
///
/// # Errors
///
/// Returns an error if the home dirs cannot be created, the lock is held by
/// another daemon, the database or socket cannot be opened, recovery fails, or
/// an accept/read I/O failure escapes the accept loop setup.
pub fn run_daemon(home: &Path, executor: &Arc<dyn RunExecutor>) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::create_dir_all(logs_dir(home))?;
    let lock_file = File::create(lock_path(home))?;
    lock_file
        .try_lock_exclusive()
        .map_err(|e| crate::Error::Other(format!("daemon already running: {e}")))?;
    let db = Arc::new(Db::open(&db_path(home))?);
    if let Err(e) = crate::rounds::reconcile_stale(&db) {
        return Err(crate::Error::Other(format!("reconcile stale rounds: {e}")));
    }
    if let Err(e) = executor.recover_stale(home) {
        return Err(crate::Error::Other(format!("recover stale runs: {e}")));
    }
    let hub = Arc::new(EventHub::new());
    install_event_hub(Arc::clone(&hub));
    let state = Arc::new(Mutex::new(DaemonState {
        inflight: HashMap::new(),
        hub: Arc::clone(&hub),
    }));
    let sock = socket_path(home);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    std::fs::write(pid_path(home), std::process::id().to_string())?;
    tracing::info!(path = %sock.display(), "daemon listening");

    let home_buf = home.to_path_buf();
    kick_pending(&home_buf, &db, executor, &state);

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("accept: {e}");
                continue;
            }
        };
        let home_t = home_buf.clone();
        let db_t = Arc::clone(&db);
        let exec_t = Arc::clone(executor);
        let state_t = Arc::clone(&state);
        let hub_t = Arc::clone(&hub);
        std::thread::spawn(move || {
            if let Err(e) = handle_connection(stream, &home_t, &db_t, &exec_t, &state_t, &hub_t) {
                tracing::warn!("connection: {e}");
            }
        });
    }
    clear_event_hub();
    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    home: &Path,
    db: &Arc<Db>,
    executor: &Arc<dyn RunExecutor>,
    state: &Arc<Mutex<DaemonState>>,
    hub: &Arc<EventHub>,
) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let req: Request =
        serde_json::from_str(line.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;

    if req.method == "subscribe" {
        return handle_subscribe(stream, &req, hub);
    }

    let result = match req.method.as_str() {
        "health" => serde_json::json!({"ok": true, "pid": std::process::id()}),
        "start_run" => {
            let run_id = req
                .params
                .as_ref()
                .and_then(|p| p.get("run_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::Error::Other("start_run requires params.run_id".into()))?;
            match start_run(home, db, executor, state, run_id) {
                Ok(()) => serde_json::json!({"ok": true, "run_id": run_id}),
                Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
            }
        }
        "list_runs" => {
            let repo_id = req
                .params
                .as_ref()
                .and_then(|p| p.get("repo_id"))
                .and_then(|v| v.as_str());
            let limit = req
                .params
                .as_ref()
                .and_then(|p| p.get("limit"))
                .and_then(serde_json::Value::as_u64)
                .map_or(20, |n| usize::try_from(n).unwrap_or(20));
            match rpc::list_runs_result(db, repo_id, limit) {
                Ok(v) => v,
                Err(e) => serde_json::json!({"error": e.to_string()}),
            }
        }
        "get_run" => {
            let run_id = req
                .params
                .as_ref()
                .and_then(|p| p.get("run_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::Error::Other("get_run requires params.run_id".into()))?;
            match rpc::get_run_result(db, hub, run_id) {
                Ok(v) => v,
                Err(e) => serde_json::json!({"error": e.to_string()}),
            }
        }
        "get_finding_hunk" => {
            let run_id = req
                .params
                .as_ref()
                .and_then(|p| p.get("run_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::Error::Other("get_finding_hunk requires params.run_id".into())
                })?;
            let finding_id = req
                .params
                .as_ref()
                .and_then(|p| p.get("finding_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::Error::Other("get_finding_hunk requires params.finding_id".into())
                })?;
            match rpc::get_finding_hunk_result(db, run_id, finding_id) {
                Ok(v) => v,
                Err(e) => serde_json::json!({"error": e.to_string()}),
            }
        }
        other => serde_json::json!({"error": format!("unknown method {other}")}),
    };
    let resp = rpc::Response {
        jsonrpc: "2.0".into(),
        result,
        id: req.id,
    };
    let out = serde_json::to_string(&resp).map_err(|e| crate::Error::Other(e.to_string()))?;
    writeln!(stream, "{out}")?;
    Ok(())
}

fn handle_subscribe(mut stream: UnixStream, req: &Request, hub: &Arc<EventHub>) -> Result<()> {
    let run_id = req
        .params
        .as_ref()
        .and_then(|p| p.get("run_id"))
        .and_then(|v| v.as_str());
    let ack = rpc::Response {
        jsonrpc: "2.0".into(),
        result: serde_json::json!({"ok": true, "subscribed": true}),
        id: req.id,
    };
    let out = serde_json::to_string(&ack).map_err(|e| crate::Error::Other(e.to_string()))?;
    writeln!(stream, "{out}")?;

    let sub = hub.subscribe(run_id);
    // Stream until the client hangs up (write fails) or we idle for a long time.
    // Write errors end the loop; Drop unregisters the subscriber.
    loop {
        match sub.recv_timeout(Duration::from_secs(30)) {
            Some(ev) => {
                let line =
                    serde_json::to_string(&ev).map_err(|e| crate::Error::Other(e.to_string()))?;
                if writeln!(stream, "{line}").is_err() {
                    break;
                }
            }
            None => {
                // Empty write often succeeds after peer close; a newline fails with
                // EPIPE. Client skips blank lines (see subscribe_events).
                if stream.write_all(b"\n").is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn kick_pending(
    home: &Path,
    db: &Arc<Db>,
    executor: &Arc<dyn RunExecutor>,
    state: &Arc<Mutex<DaemonState>>,
) {
    let pending = match db.pending_runs() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("list pending: {e}");
            return;
        }
    };
    for run in pending {
        if let Err(e) = start_run(home, db, executor, state, &run.id) {
            tracing::warn!(run_id = %run.id, "start pending: {e}");
        }
    }
}

fn start_run(
    home: &Path,
    db: &Arc<Db>,
    executor: &Arc<dyn RunExecutor>,
    state: &Arc<Mutex<DaemonState>>,
    run_id: &str,
) -> Result<()> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| crate::Error::Other(format!("unknown run {run_id}")))?;
    if run.status != "pending" && run.status != "running" {
        return Ok(());
    }
    {
        let guard = state.lock().expect("daemon state");
        if guard.inflight.contains_key(run_id) {
            return Ok(());
        }
    }

    let prior = db.in_flight_same_branch(&run.repo_id, &run.branch, run_id)?;
    let mut wait_for: Vec<JoinHandle<()>> = Vec::new();
    let mut orphan_worktrees: Vec<(String, PathBuf)> = Vec::new();
    {
        let mut guard = state.lock().expect("daemon state");
        for old in &prior {
            let _ = db.set_run_status(&old.id, "cancelled", Some("superseded by new push"));
            guard.hub.publish_state(&old.id);
            guard
                .hub
                .publish_activity(&old.id, "status=cancelled superseded");
            if let Some(inf) = guard.inflight.remove(&old.id) {
                inf.cancel.store(true, Ordering::SeqCst);
                wait_for.push(inf.handle);
            } else if let Some(wt) = old.worktree_dir.clone() {
                // Parked runs have no inflight handle; sweep their worktrees.
                orphan_worktrees.push((old.repo_id.clone(), wt));
            }
        }
    }
    for handle in wait_for {
        let _ = handle.join();
    }
    for (repo_id, wt) in orphan_worktrees {
        if let Ok(Some(repo)) = db.repo_by_id(&repo_id) {
            if let Ok(bare) = porch_git::GitDir::new(&repo.bare_path) {
                let _ = porch_git::worktree_remove_force(&bare, &wt);
            }
        }
        let _ = std::fs::remove_dir_all(&wt);
    }

    // Re-check: may have been cancelled by a newer start_run while we waited.
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| crate::Error::Other(format!("unknown run {run_id}")))?;
    if run.status == "cancelled" {
        return Ok(());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = Arc::clone(&cancel);
    let exec = Arc::clone(executor);
    let home_buf: PathBuf = home.to_path_buf();
    let run_id_owned = run_id.to_string();
    let state_clear = Arc::clone(state);
    let handle = std::thread::spawn(move || {
        exec.execute(&home_buf, &run_id_owned, &cancel_thread);
        if let Ok(mut guard) = state_clear.lock() {
            guard.inflight.remove(&run_id_owned);
        }
    });

    state
        .lock()
        .expect("daemon state")
        .inflight
        .insert(run_id.to_string(), Inflight { cancel, handle });
    Ok(())
}

/// Poll until the daemon answers health, or `timeout` elapses.
///
/// # Errors
///
/// Returns [`crate::Error::Other`] when the daemon never becomes healthy.
pub fn wait_for_health(home: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if rpc::health_check(home).ok() == Some(true) {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(crate::Error::Other("daemon did not become healthy".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start the daemon if it is not already healthy, then wait for health.
///
/// # Errors
///
/// Returns an error if the daemon cannot be spawned or does not become healthy.
pub fn ensure_daemon(porch_bin: &Path, home: &Path) -> Result<()> {
    if rpc::health_check(home).ok() == Some(true) {
        return Ok(());
    }
    let porch_env = crate::collect_porch_env();
    let extra: Vec<(&str, &std::ffi::OsStr)> = porch_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect();
    crate::spawn_detached_with_env(porch_bin, home, &extra)?;
    wait_for_health(home, Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::RunExecutor;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    struct NoopExecutor;

    impl RunExecutor for NoopExecutor {
        fn execute(&self, _home: &Path, _run_id: &str, _cancel: &AtomicBool) {}

        fn recover_stale(&self, _home: &Path) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    fn start_test_daemon(home: &Path) -> JoinHandle<()> {
        let home = home.to_path_buf();
        let exec: Arc<dyn RunExecutor> = Arc::new(NoopExecutor);
        std::thread::spawn(move || {
            let _ = run_daemon(&home, &exec);
        })
    }

    #[test]
    fn health_list_get_subscribe_with_thread_per_connection() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
            .unwrap();
        let run = db
            .insert_run("repo1", "feat", "abc123", Some("intent"), Some("cli"))
            .unwrap();
        db.set_run_status(&run.id, "parked", None).unwrap();
        db.insert_step_result(&run.id, "review", "parked", None)
            .unwrap();
        db.set_findings_json(&run.id, Some(r#"[{"id":"f0"}]"#))
            .unwrap();

        let _handle = start_test_daemon(&home);
        wait_for_health(&home, Duration::from_secs(5)).unwrap();

        assert!(rpc::health_check(&home).unwrap());

        let listed = rpc::list_runs(&home, Some("repo1"), Some(10)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["id"], run.id);
        assert_eq!(listed[0]["branch"], "feat");
        assert_eq!(listed[0]["status"], "parked");

        let snap = rpc::get_run(&home, &run.id).unwrap();
        assert_eq!(snap.run_id, run.id);
        assert_eq!(snap.status, "parked");
        assert_eq!(snap.branch, "feat");
        assert!(snap.findings.is_array());
        assert_eq!(snap.steps.len(), 1);
        assert_eq!(snap.steps[0].step, "review");

        let hunk_missing_wt = rpc::get_finding_hunk(&home, &run.id, "f0").unwrap();
        assert!(
            hunk_missing_wt.get("error").is_some(),
            "without worktree, hunk RPC should error: {hunk_missing_wt}"
        );

        // Subscribe: first event is stream_gap; publish_state yields state or gap.
        let hub = crate::event_hub().expect("hub installed");
        let run_id = run.id.clone();
        let home2 = home.clone();
        let join = std::thread::spawn(move || {
            let mut n = 0u32;
            let mut saw_gap = false;
            let mut saw_state_or_gap_after = false;
            let _ = rpc::subscribe_events(&home2, Some(&run_id), |ev| {
                n += 1;
                match &ev {
                    crate::events::Event::StreamGap { .. } if n == 1 => {
                        saw_gap = true;
                        true
                    }
                    crate::events::Event::State { .. } | crate::events::Event::StreamGap { .. } => {
                        saw_state_or_gap_after = true;
                        false
                    }
                    crate::events::Event::Activity { .. } => n < 20,
                }
            });
            (saw_gap, saw_state_or_gap_after)
        });
        std::thread::sleep(Duration::from_millis(100));
        hub.publish_state(&run.id);
        let (gap, after) = join.join().unwrap();
        assert!(gap, "subscribe must open with stream_gap");
        assert!(after, "publish_state must deliver state or gap");

        // Cleanup: remove socket so daemon accept may fail eventually; kill via pid.
        if let Ok(pid) = std::fs::read_to_string(pid_path(&home)) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                crate::kill_group(pid);
            }
        }
    }

    #[test]
    fn get_finding_hunk_result_reads_capped_snippet_from_worktree() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let wt = home.join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let mut body = String::new();
        for i in 1..=20 {
            use std::fmt::Write as _;
            writeln!(body, "line {i} content").unwrap();
        }
        std::fs::write(wt.join("src/a.rs"), &body).unwrap();

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
            .unwrap();
        let run = db
            .insert_run("repo1", "feat", "abc123", None, None)
            .unwrap();
        db.set_worktree_dir(&run.id, &wt).unwrap();
        db.set_findings_json(
            &run.id,
            Some(
                r#"[{"id":"f0","path":"src/a.rs","message":"bug","severity":"warning","action":"ask-user","start_line":5,"end_line":7}]"#,
            ),
        )
        .unwrap();

        let hunk = rpc::get_finding_hunk_result(&db, &run.id, "f0").unwrap();
        assert!(hunk.get("error").is_none(), "hunk={hunk}");
        let text = hunk["hunk"].as_str().unwrap();
        assert!(text.contains("line 5 content"), "hunk={text}");
        assert!(text.contains("line 7 content"), "hunk={text}");
        assert_eq!(hunk["path"], "src/a.rs");
        assert_eq!(hunk["truncated"], false);
    }

    #[test]
    fn get_finding_hunk_result_rejects_path_escape() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let wt = home.join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join("src/a.rs"), "fn ok() {}\n").unwrap();

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
            .unwrap();
        let run = db
            .insert_run("repo1", "feat", "abc123", None, None)
            .unwrap();
        db.set_worktree_dir(&run.id, &wt).unwrap();

        for (id, path) in [
            ("f_dotdot", "../etc/passwd"),
            ("f_abs", "/etc/passwd"),
            ("f_nested", "src/../../etc/passwd"),
        ] {
            db.set_findings_json(
                &run.id,
                Some(&format!(
                    r#"[{{"id":"{id}","path":"{path}","message":"escape","severity":"warning","action":"ask-user","start_line":1,"end_line":1}}]"#
                )),
            )
            .unwrap();
            let hunk = rpc::get_finding_hunk_result(&db, &run.id, id).unwrap();
            let err = hunk.get("error").and_then(|e| e.as_str()).unwrap_or("");
            assert!(
                err.contains("escape") || err.contains("relative"),
                "path={path} hunk={hunk}"
            );
            assert!(
                hunk.get("hunk").is_none(),
                "must not return hunk for {path}"
            );
        }
    }
}
