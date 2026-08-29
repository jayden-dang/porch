use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::home::logs_dir;

/// Collect every `PORCH_*` variable from an env-like key/value iterator.
#[must_use]
pub fn collect_porch_env_from<K, V, I>(vars: I) -> Vec<(String, OsString)>
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
    I: IntoIterator<Item = (K, V)>,
{
    vars.into_iter()
        .filter_map(|(k, v)| {
            let key = k.as_ref().to_str()?.to_string();
            if key.starts_with("PORCH_") {
                Some((key, v.as_ref().to_os_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Collect every `PORCH_*` variable from the current process environment.
#[must_use]
pub fn collect_porch_env() -> Vec<(String, OsString)> {
    collect_porch_env_from(std::env::vars_os())
}

/// Spawn `porch daemon run` in its own process group. Returns the child pid.
///
/// # Errors
///
/// Returns an I/O error if the log directory or process cannot be created.
pub fn spawn_detached(porch_bin: &Path, home: &Path) -> Result<u32> {
    spawn_detached_with_env(porch_bin, home, &[])
}

/// Like [`spawn_detached`], with extra environment variables for the daemon.
///
/// `PORCH_HOME` is always set to `home` after `extra_env` so the explicit home wins.
///
/// # Errors
///
/// Returns an I/O error if the log directory or process cannot be created.
pub fn spawn_detached_with_env(
    porch_bin: &Path,
    home: &Path,
    extra_env: &[(&str, &OsStr)],
) -> Result<u32> {
    std::fs::create_dir_all(logs_dir(home))?;
    let log = File::create(logs_dir(home).join("daemon.log"))?;
    let log2 = log.try_clone()?;
    let mut cmd = Command::new(porch_bin);
    cmd.args(["daemon", "run"]);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.env("PORCH_HOME", home);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(log));
    cmd.stderr(Stdio::from(log2));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// Best-effort terminate of a process group spawned by [`spawn_detached`].
pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        // Unix pids fit in pid_t / i32; refuse absurd values instead of wrapping.
        let Ok(raw) = i32::try_from(pid) else {
            return;
        };
        let _ = killpg(Pid::from_raw(raw), Signal::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_wrapper_runs_true() {
        let mut cmd = Command::new("true");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let status = cmd.status().unwrap();
        assert!(status.success());
    }

    #[test]
    fn collect_porch_env_from_keeps_only_porch_prefix() {
        let collected = collect_porch_env_from([
            ("PATH", "/usr/bin"),
            ("PORCH_HOME", "/tmp/home"),
            ("PORCH_REVIEW_BIN", "fake-review"),
            ("OTHER", "nope"),
        ]);
        assert_eq!(collected.len(), 2);
        assert!(
            collected
                .iter()
                .any(|(k, v)| k == "PORCH_HOME" && v.to_string_lossy() == "/tmp/home")
        );
        assert!(
            collected
                .iter()
                .any(|(k, v)| k == "PORCH_REVIEW_BIN" && v.to_string_lossy() == "fake-review")
        );
    }
}
