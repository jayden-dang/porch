use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::Result;
use crate::db::Db;
use crate::home::{db_path, lock_path, logs_dir, pid_path, socket_path};
use crate::rpc;

/// Hold the lock, bind the socket, serve health JSON-RPC until SIGTERM.
///
/// # Errors
///
/// Returns an error if the home dirs cannot be created, the lock is held by
/// another daemon, the database or socket cannot be opened, or an accept/read
/// I/O failure escapes the accept loop setup.
pub fn run_daemon(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::create_dir_all(logs_dir(home))?;
    let lock_file = File::create(lock_path(home))?;
    lock_file
        .try_lock_exclusive()
        .map_err(|e| crate::Error::Other(format!("daemon already running: {e}")))?;
    let _db = Db::open(&db_path(home))?;
    let sock = socket_path(home);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    std::fs::write(pid_path(home), std::process::id().to_string())?;
    tracing::info!(path = %sock.display(), "daemon listening");
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
        match rpc::handle_line(&line) {
            Ok(resp) => {
                let _ = writeln!(stream, "{resp}");
            }
            Err(e) => tracing::warn!("rpc: {e}"),
        }
    }
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
