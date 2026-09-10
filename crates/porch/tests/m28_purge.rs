//! M28: `--purge` refuses over unforwarded custody tips (`PURGE` / ROAD-24).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::kill_group;
use tempfile::TempDir;

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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

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
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pid
}

fn pin_unforwarded(s: &Setup) -> (PathBuf, String) {
    let repo_id = git_out(&s.work, &["config", "--get", "porch.repo-id"]);
    let bare = s.home.join("repos").join(format!("{repo_id}.git"));
    std::fs::write(s.work.join("extra"), "porch-authored\n").unwrap();
    git(&s.work, &["add", "extra"]);
    git(&s.work, &["commit", "-m", "extra"]);
    let sha = git_out(&s.work, &["rev-parse", "HEAD"]);
    git_at(
        &bare,
        &[
            "fetch",
            s.work.to_str().unwrap(),
            "+HEAD:refs/porch/recover/01UNFORWARDED",
        ],
    );
    git(&s.work, &["reset", "--hard", "HEAD~1"]);
    (bare, sha)
}

#[test]
fn purge_refuses_unforwarded_recover_ref_and_status_still_lists_it() {
    let s = setup();
    let (bare, sha) = pin_unforwarded(&s);

    porch(&s)
        .args(["eject", "--purge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("refuse --purge"))
        .stderr(predicates::str::contains(&sha));

    let remotes = git_out(&s.work, &["remote"]);
    assert!(
        remotes.lines().any(|l| l.trim() == "porch"),
        "still attached: {remotes}"
    );
    assert!(bare.is_dir());

    porch(&s)
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("01UNFORWARDED"))
        .stdout(predicates::str::contains(&sha));
}

#[test]
fn abandon_without_purge_is_invalid() {
    let s = setup();
    porch(&s)
        .args(["eject", "--abandon"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--purge"));
    let remotes = git_out(&s.work, &["remote"]);
    assert!(remotes.lines().any(|l| l.trim() == "porch"));
}

#[test]
fn purge_abandon_deletes_an_unforwarded_recover_ref() {
    let s = setup();
    let (bare, _sha) = pin_unforwarded(&s);

    porch(&s)
        .args(["eject", "--purge", "--abandon"])
        .assert()
        .success()
        .stdout(predicates::str::contains("purged"));

    assert!(!bare.exists());
    let abandoned = s.home.join("abandoned");
    let n = std::fs::read_dir(&abandoned)
        .expect("abandon record")
        .count();
    assert_eq!(n, 1);
}

#[test]
fn clean_purge_without_abandon_still_purges() {
    let s = setup();
    let repo_id = git_out(&s.work, &["config", "--get", "porch.repo-id"]);
    let bare = s.home.join("repos").join(format!("{repo_id}.git"));
    porch(&s).args(["eject", "--purge"]).assert().success();
    assert!(!bare.exists());
}
