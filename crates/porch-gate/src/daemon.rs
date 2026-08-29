use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::Result;
use crate::db::Db;
use crate::executor::RunExecutor;
use crate::home::{db_path, lock_path, logs_dir, pid_path, socket_path};
use crate::rpc::{self, Request};

struct Inflight {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct DaemonState {
    inflight: HashMap<String, Inflight>,
}

/// Hold the lock, recover stale runs, bind the socket, serve JSON-RPC.
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
    if let Err(e) = executor.recover_stale(home) {
        return Err(crate::Error::Other(format!("recover stale runs: {e}")));
    }
    let state = Arc::new(Mutex::new(DaemonState {
        inflight: HashMap::new(),
    }));
    let sock = socket_path(home);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    std::fs::write(pid_path(home), std::process::id().to_string())?;
    tracing::info!(path = %sock.display(), "daemon listening");

    let home_buf = home.to_path_buf();
    kick_pending(&home_buf, &db, executor, &state);

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("accept: {e}");
                continue;
            }
        };
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            continue;
        }
        match handle_rpc_line(&line, &home_buf, &db, executor, &state) {
            Ok(resp) => {
                let _ = writeln!(stream, "{resp}");
            }
            Err(e) => tracing::warn!("rpc: {e}"),
        }
    }
    Ok(())
}

fn handle_rpc_line(
    line: &str,
    home: &Path,
    db: &Arc<Db>,
    executor: &Arc<dyn RunExecutor>,
    state: &Arc<Mutex<DaemonState>>,
) -> Result<String> {
    let req: Request =
        serde_json::from_str(line.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;
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
        other => serde_json::json!({"error": format!("unknown method {other}")}),
    };
    let resp = rpc::Response {
        jsonrpc: "2.0".into(),
        result,
        id: req.id,
    };
    serde_json::to_string(&resp).map_err(|e| crate::Error::Other(e.to_string()))
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

    let prior = db.in_flight_same_branch(&run.repo_id, &run.branch, run_id)?;
    let mut wait_for: Vec<JoinHandle<()>> = Vec::new();
    let mut orphan_worktrees: Vec<(String, PathBuf)> = Vec::new();
    {
        let mut guard = state.lock().expect("daemon state");
        for old in &prior {
            let _ = db.set_run_status(&old.id, "cancelled", Some("superseded by new push"));
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
    crate::spawn_detached(porch_bin, home)?;
    wait_for_health(home, Duration::from_secs(5))
}
