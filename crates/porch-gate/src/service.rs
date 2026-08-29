//! OS-managed daemon service definitions (launchd / systemd / Task Scheduler).
//!
//! M8 ships `KeepAlive` / `Restart=always` managed units plus the existing detached
//! `ensure_daemon` fallback. True socket activation (`LISTEN_FDS`) remains later.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::Result;
use crate::db::Db;
use crate::home::{db_path, lock_path, logs_dir, pid_path, socket_path};
use crate::rpc;

const SKIP_LOAD_ENV: &str = "PORCH_SERVICE_SKIP_LOAD";

/// Test hook so unit tests can skip launchctl without `env::set_var` (forbidden unsafe).
static FORCE_SKIP_LOAD: AtomicBool = AtomicBool::new(false);

/// First 8 hex of sha256 of the canonical `PORCH_HOME` path.
#[must_use]
pub fn daemon_service_suffix(home: &Path) -> String {
    let abs = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(abs.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..4])
}

/// Paths and labels for the current platform's service definition.
#[derive(Debug, Clone)]
pub struct ServicePaths {
    pub suffix: String,
    pub label: String,
    pub definition_path: PathBuf,
}

/// Resolve where the service definition file lives for this OS / home.
#[must_use]
pub fn service_paths(home: &Path, user_home: &Path) -> ServicePaths {
    let suffix = daemon_service_suffix(home);
    #[cfg(target_os = "macos")]
    {
        let label = format!("ai.porch.daemon.{suffix}");
        let definition_path = user_home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        ServicePaths {
            suffix,
            label,
            definition_path,
        }
    }
    #[cfg(target_os = "linux")]
    {
        let label = format!("porch-daemon-{suffix}");
        let definition_path = user_home
            .join(".config/systemd/user")
            .join(format!("{label}.service"));
        ServicePaths {
            suffix,
            label,
            definition_path,
        }
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
    {
        let label = format!("porch-daemon-{suffix}");
        let definition_path = user_home
            .join(".porch-services")
            .join(format!("{label}.cmd"));
        ServicePaths {
            suffix,
            label,
            definition_path,
        }
    }
}

/// Render a macOS `LaunchAgent` plist (pure string; no I/O).
#[must_use]
pub fn render_launchd_plist(
    label: &str,
    porch_exe: &Path,
    porch_home: &Path,
    path_env: &str,
) -> String {
    let exe = porch_exe.display();
    let home = porch_home.display();
    let log = porch_home.join("logs").join("daemon.log");
    let log_s = log.display();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PORCH_HOME</key>
    <string>{home}</string>
    <key>PATH</key>
    <string>{path_env}</string>
  </dict>
  <key>WorkingDirectory</key>
  <string>{home}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log_s}</string>
  <key>StandardErrorPath</key>
  <string>{log_s}</string>
</dict>
</plist>
"#
    )
}

/// Render a systemd user unit (pure string; no I/O).
#[must_use]
pub fn render_systemd_unit(porch_exe: &Path, porch_home: &Path, path_env: &str) -> String {
    let exe = porch_exe.display();
    let home = porch_home.display();
    let log = porch_home.join("logs").join("daemon.log");
    let log_s = log.display();
    format!(
        "[Unit]\n\
Description=Porch local git gate daemon\n\
After=default.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={exe} daemon run\n\
WorkingDirectory={home}\n\
Environment=PORCH_HOME={home}\n\
Environment=PATH={path_env}\n\
Restart=always\n\
StandardOutput=append:{log_s}\n\
StandardError=append:{log_s}\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

/// Render a Windows Task Scheduler-style command string (no schtasks required).
#[must_use]
pub fn render_windows_task_command(porch_exe: &Path, porch_home: &Path) -> String {
    format!("\"{}\" daemon run", porch_exe.display())
        + &format!(" (PORCH_HOME={})", porch_home.display())
}

/// Write the OS service definition. Does not require the daemon to be running.
///
/// When `PORCH_SERVICE_SKIP_LOAD=1` (or the manager binary is missing), only the
/// file is written — launchctl/systemctl are not invoked.
///
/// # Errors
///
/// Returns I/O errors if directories or the definition file cannot be written.
pub fn install_service(
    porch_exe: &Path,
    porch_home: &Path,
    user_home: &Path,
) -> Result<ServicePaths> {
    let paths = service_paths(porch_home, user_home);
    if let Some(parent) = paths.definition_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(logs_dir(porch_home))?;

    let path_env = env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/local/bin".into());
    let body = {
        #[cfg(target_os = "macos")]
        {
            render_launchd_plist(&paths.label, porch_exe, porch_home, &path_env)
        }
        #[cfg(target_os = "linux")]
        {
            render_systemd_unit(porch_exe, porch_home, &path_env)
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
        {
            render_windows_task_command(porch_exe, porch_home)
        }
    };
    std::fs::write(&paths.definition_path, body)?;

    if skip_load() {
        return Ok(paths);
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("launchctl")
            .args([
                "bootstrap",
                &format!("gui/{}", uid_string()),
                paths.definition_path.to_str().unwrap_or(""),
            ])
            .status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = Command::new("systemctl")
            .args(["--user", "enable", "--now", &paths.label])
            .status();
    }

    Ok(paths)
}

/// Stop if possible and remove the service definition file.
///
/// # Errors
///
/// Returns I/O errors if the definition cannot be removed.
pub fn uninstall_service(porch_home: &Path, user_home: &Path) -> Result<ServicePaths> {
    let paths = service_paths(porch_home, user_home);
    if !skip_load() {
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{}", uid_string()), &paths.label])
                .status();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", &paths.label])
                .status();
        }
    }
    stop_process(porch_home);
    if paths.definition_path.exists() {
        std::fs::remove_file(&paths.definition_path)?;
    }
    Ok(paths)
}

/// Try the OS manager, then fall back to detached [`crate::ensure_daemon`].
///
/// # Errors
///
/// Returns an error only when both the manager path and detached spawn fail.
pub fn start_service(porch_exe: &Path, porch_home: &Path, user_home: &Path) -> Result<String> {
    let paths = service_paths(porch_home, user_home);
    let mut manager_err: Option<String> = None;

    if paths.definition_path.exists() && !skip_load() {
        #[cfg(target_os = "macos")]
        {
            let st = Command::new("launchctl")
                .args([
                    "bootstrap",
                    &format!("gui/{}", uid_string()),
                    paths.definition_path.to_str().unwrap_or(""),
                ])
                .status();
            match st {
                Ok(s) if s.success() => {
                    if crate::wait_for_health(porch_home, std::time::Duration::from_secs(5)).is_ok()
                    {
                        return Ok(format!("started via launchd ({})", paths.label));
                    }
                    manager_err = Some("launchd bootstrap ok but health timeout".into());
                }
                Ok(s) => {
                    // Already loaded: kickstart
                    let kick = Command::new("launchctl")
                        .args([
                            "kickstart",
                            "-k",
                            &format!("gui/{}/{}", uid_string(), paths.label),
                        ])
                        .status();
                    if kick.as_ref().is_ok_and(std::process::ExitStatus::success)
                        && crate::wait_for_health(porch_home, std::time::Duration::from_secs(5))
                            .is_ok()
                    {
                        return Ok(format!("kickstarted via launchd ({})", paths.label));
                    }
                    manager_err = Some(format!("launchctl exit {s}"));
                }
                Err(e) => manager_err = Some(format!("launchctl: {e}")),
            }
        }
        #[cfg(target_os = "linux")]
        {
            let st = Command::new("systemctl")
                .args(["--user", "start", &paths.label])
                .status();
            match st {
                Ok(s) if s.success() => {
                    if crate::wait_for_health(porch_home, std::time::Duration::from_secs(5)).is_ok()
                    {
                        return Ok(format!("started via systemd ({})", paths.label));
                    }
                    manager_err = Some("systemd start ok but health timeout".into());
                }
                Ok(s) => manager_err = Some(format!("systemctl exit {s}")),
                Err(e) => manager_err = Some(format!("systemctl: {e}")),
            }
        }
    }

    match crate::ensure_daemon(porch_exe, porch_home) {
        Ok(()) => {
            if let Some(e) = manager_err {
                tracing::warn!("service manager failed ({e}); used detached ensure_daemon");
                Ok(format!("started detached (manager failed: {e})"))
            } else {
                Ok("started detached via ensure_daemon".into())
            }
        }
        Err(e) => {
            let msg = match manager_err {
                Some(m) => format!("manager: {m}; detached: {e}"),
                None => format!("detached: {e}"),
            };
            Err(crate::Error::Other(msg))
        }
    }
}

/// Stop the daemon process. Leaves the service definition installed.
///
/// Refuses when there are active pending/running/parked runs unless `force`.
///
/// # Errors
///
/// Returns an error if active runs block stop, or the process cannot be signalled.
pub fn stop_daemon(porch_home: &Path, force: bool) -> Result<()> {
    if !force {
        let db_file = db_path(porch_home);
        if db_file.exists() {
            let db = Db::open(&db_file)?;
            let active = db.active_runs(None, None)?;
            if !active.is_empty() {
                let ids: Vec<_> = active.iter().map(|r| r.id.as_str()).collect();
                return Err(crate::Error::Other(format!(
                    "refuse stop: active runs {ids:?}; pass --force to override"
                )));
            }
        }
    }
    stop_process(porch_home);
    Ok(())
}

fn stop_process(porch_home: &Path) {
    let pid_file = pid_path(porch_home);
    if let Ok(pid_s) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_s.trim().parse::<u32>() {
            crate::kill_group(pid);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    let _ = std::fs::remove_file(socket_path(porch_home));
    let _ = std::fs::remove_file(pid_file);
    let _ = std::fs::remove_file(lock_path(porch_home));
}

/// Operator-facing daemon / service status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub socket_healthy: bool,
    pub service_file: PathBuf,
    pub service_file_exists: bool,
    pub label: String,
    pub porch_home: PathBuf,
}

/// Collect status for `porch daemon status`.
///
/// # Errors
///
/// Currently infallible; wrapped in Result for API symmetry.
pub fn service_status(porch_home: &Path, user_home: &Path) -> Result<ServiceStatus> {
    let paths = service_paths(porch_home, user_home);
    let socket_healthy = rpc::health_check(porch_home).ok() == Some(true);
    let pid = std::fs::read_to_string(pid_path(porch_home))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    Ok(ServiceStatus {
        running: socket_healthy || pid.is_some(),
        pid,
        socket_healthy,
        service_file: paths.definition_path.clone(),
        service_file_exists: paths.definition_path.exists(),
        label: paths.label,
        porch_home: porch_home.to_path_buf(),
    })
}

fn skip_load() -> bool {
    if FORCE_SKIP_LOAD.load(Ordering::SeqCst) {
        return true;
    }
    matches!(env::var(SKIP_LOAD_ENV), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Force-skip OS manager load (unit tests). Prefer `PORCH_SERVICE_SKIP_LOAD=1` on child processes.
pub fn set_skip_service_load_for_tests(skip: bool) {
    FORCE_SKIP_LOAD.store(skip, Ordering::SeqCst);
}

#[cfg(unix)]
fn uid_string() -> String {
    // Prefer getuid via libc-free approach: parse id -u; fall back to "501".
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn suffix_is_eight_hex_and_differs_for_two_homes() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let sa = daemon_service_suffix(a.path());
        let sb = daemon_service_suffix(b.path());
        assert_eq!(sa.len(), 8);
        assert!(sa.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(sa, sb);
    }

    #[test]
    fn rendered_macos_plist_contains_label_args_keepalive() {
        let home = PathBuf::from("/tmp/porch-home-a");
        let suffix = "abcd1234";
        let label = format!("ai.porch.daemon.{suffix}");
        let exe = PathBuf::from("/usr/local/bin/porch");
        let plist = render_launchd_plist(&label, &exe, &home, "/usr/bin:/bin");
        assert!(plist.contains(&format!("<string>{label}</string>")));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<string>/tmp/porch-home-a</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("/usr/local/bin/porch"));
    }

    #[test]
    fn rendered_systemd_unit_contains_restart_and_exec() {
        let home = PathBuf::from("/tmp/porch-home-b");
        let exe = PathBuf::from("/usr/bin/porch");
        let unit = render_systemd_unit(&exe, &home, "/usr/bin");
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("ExecStart=/usr/bin/porch daemon run"));
        assert!(unit.contains("Environment=PORCH_HOME=/tmp/porch-home-b"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn install_writes_definition_under_temp_home() {
        let tmp = TempDir::new().unwrap();
        let user_home = tmp.path().join("user");
        let porch_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).unwrap();
        std::fs::create_dir_all(&porch_home).unwrap();
        let exe = tmp.path().join("bin").join("porch");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();

        set_skip_service_load_for_tests(true);
        let paths = install_service(&exe, &porch_home, &user_home).unwrap();
        set_skip_service_load_for_tests(false);
        assert!(paths.definition_path.is_file());
        let body = std::fs::read_to_string(&paths.definition_path).unwrap();
        assert!(body.contains("daemon"));
        assert!(body.contains("run"));
        assert_eq!(paths.suffix.len(), 8);
    }

    #[test]
    fn stop_refuses_active_runs_unless_force() {
        set_skip_service_load_for_tests(true);
        let tmp = TempDir::new().unwrap();
        let porch_home = tmp.path().join("home");
        std::fs::create_dir_all(&porch_home).unwrap();

        let db = crate::Db::open(&crate::home::db_path(&porch_home)).unwrap();
        db.upsert_repo("repo1", &porch_home, &porch_home.join("bare.git"), "main")
            .unwrap();
        let parked = db
            .insert_run("repo1", "feat-parked", "aaa", None, None)
            .unwrap();
        db.set_run_status(&parked.id, "parked", None).unwrap();
        let pending = db
            .insert_run("repo1", "feat-pending", "bbb", None, None)
            .unwrap();
        assert_eq!(pending.status, "pending");

        let err = stop_daemon(&porch_home, false).expect_err("active runs must block stop");
        let msg = err.to_string();
        assert!(
            msg.contains("refuse stop") && msg.contains("--force"),
            "unexpected err: {msg}"
        );

        stop_daemon(&porch_home, true).expect("force stop must succeed");
        set_skip_service_load_for_tests(false);
    }
}
