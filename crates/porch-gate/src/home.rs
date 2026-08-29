use std::env;
use std::path::PathBuf;

/// `$PORCH_HOME` or `~/.porch`.
#[must_use]
pub fn porch_home() -> PathBuf {
    if let Some(v) = env::var_os("PORCH_HOME") {
        return PathBuf::from(v);
    }
    let home = env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".porch")
}

/// Directory that holds per-repo bare remotes.
#[must_use]
pub fn repos_dir(home: &std::path::Path) -> PathBuf {
    home.join("repos")
}

/// Unix domain socket path for the daemon.
#[must_use]
pub fn socket_path(home: &std::path::Path) -> PathBuf {
    home.join("socket")
}

/// Exclusive flock path for the daemon.
#[must_use]
pub fn lock_path(home: &std::path::Path) -> PathBuf {
    home.join("daemon.lock")
}

/// Path of the daemon pid file.
#[must_use]
pub fn pid_path(home: &std::path::Path) -> PathBuf {
    home.join("daemon.pid")
}

/// Path of the `SQLite` state database.
#[must_use]
pub fn db_path(home: &std::path::Path) -> PathBuf {
    home.join("state.sqlite")
}

/// Directory for daemon log files.
#[must_use]
pub fn logs_dir(home: &std::path::Path) -> PathBuf {
    home.join("logs")
}
