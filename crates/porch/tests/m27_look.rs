//! M27: daemon-free inspect (`LOOK` / ROAD-10 second wave).
//!
//! `porch status` and `porch runs` must not spawn. Custody tips are git on the
//! layout-derived bare. Forward facts are tested in `porch-gate` (`look.rs`).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::kill_group;
use tempfile::TempDir;

fn git_at(git_dir: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git --git-dir {} {args:?}", git_dir.display());
}

fn git(dir: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

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
    kill_daemon(&s.home);
    s
}

fn porch(s: &Setup) -> Command {
    let mut c = Command::cargo_bin("porch").unwrap();
    c.current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env("HOME", s.home.parent().unwrap());
    c
}

fn daemon_pid(home: &Path) -> Option<u32> {
    std::fs::read_to_string(home.join("daemon.pid"))
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
}

fn pid_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state != "Z" && state != "X")
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

fn repo_id(s: &Setup) -> String {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["config", "--get", "porch.repo-id"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn head_sha(s: &Setup) -> String {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn pin_recovery(s: &Setup, run_id: &str, sha: &str) {
    let bare = s.home.join("repos").join(format!("{}.git", repo_id(s)));
    git_at(
        &bare,
        &[
            "fetch",
            s.work.to_str().unwrap(),
            &format!("{sha}:refs/heads/look-keep"),
        ],
    );
    git_at(
        &bare,
        &["update-ref", &format!("refs/porch/recover/{run_id}"), sha],
    );
}

#[test]
fn status_and_runs_do_not_spawn_when_the_daemon_is_dead() {
    let s = setup();
    assert!(daemon_pid(&s.home).is_none());
    let before = std::fs::read_dir(s.home.join("repos")).unwrap().count();

    let status = porch(&s).args(["status", "--json"]).output().unwrap();
    assert!(status.status.success(), "{status:?}");
    let v: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(v["condition"], "unreachable");
    assert_eq!(v["daemon_healthy"], false);
    assert!(daemon_pid(&s.home).is_none());

    let runs = porch(&s).arg("runs").output().unwrap();
    assert!(runs.status.success(), "{runs:?}");
    assert!(daemon_pid(&s.home).is_none());
    let after = std::fs::read_dir(s.home.join("repos")).unwrap().count();
    assert_eq!(before, after, "inspect must not create a new bare");
}

#[test]
fn status_lists_every_recover_ref_not_only_the_latest_run() {
    let s = setup();
    let sha = head_sha(&s);
    pin_recovery(&s, "run-old", &sha);
    pin_recovery(&s, "run-new", &sha);

    let out = porch(&s).args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let tips = v["recovery_tips"].as_array().unwrap();
    let names: Vec<&str> = tips.iter().filter_map(|t| t["ref"].as_str()).collect();
    assert!(names.iter().any(|n| n.ends_with("/run-old")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with("/run-new")), "{names:?}");
}

#[test]
fn inspect_uses_porch_repo_id_not_the_path_hash() {
    let s = setup();
    let sha = head_sha(&s);
    let custom = "preset-id";
    let custom_bare = s.home.join("repos").join(format!("{custom}.git"));
    git(
        &s.home.join("repos"),
        &["init", "--bare", "-b", "main", &format!("{custom}.git")],
    );
    git(
        &s.work,
        &[
            "push",
            custom_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    git(&s.work, &["config", "porch.repo-id", custom]);
    git_at(
        &custom_bare,
        &["update-ref", "refs/porch/recover/from-preset", &sha],
    );

    let out = porch(&s).args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["repo_id"], custom);
    let tips = v["recovery_tips"].as_array().unwrap();
    assert!(
        tips.iter()
            .any(|t| t["ref"].as_str() == Some("refs/porch/recover/from-preset")),
        "{tips:?}"
    );
}
