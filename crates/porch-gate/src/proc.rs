use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::home::logs_dir;

/// Spawn `porch daemon run` in its own process group. Returns the child pid.
///
/// # Errors
///
/// Returns an I/O error if the log directory or process cannot be created.
pub fn spawn_detached(porch_bin: &Path, home: &Path) -> Result<u32> {
    std::fs::create_dir_all(logs_dir(home))?;
    let log = File::create(logs_dir(home).join("daemon.log"))?;
    let log2 = log.try_clone()?;
    let mut cmd = Command::new(porch_bin);
    cmd.args(["daemon", "run"]);
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
}
