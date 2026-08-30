//! M13: eject, rebase-park, rerun, sync, cold PATH (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::{Db, kill_group, repo_id_for, run_worktree_dir};
use porch_git::init_bare;
use porch_review::{HomeConfig, REVIEW_BIN_ENV, ToolsConfig, write_home_config};
use tempfile::TempDir;

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn git_out(work: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .current_dir(work)
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

fn kill_daemon(home: &Path) {
    if let Ok(pid) = std::fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn wait_status(db: &Db, repo_id: &str, want: &[&str], timeout: Duration) -> porch_gate::RunRow {
    let start = Instant::now();
    loop {
        let runs = db.runs_for_repo(repo_id).unwrap();
        if let Some(run) = runs.last() {
            if want.contains(&run.status.as_str()) {
                return run.clone();
            }
        }
        assert!(
            start.elapsed() <= timeout,
            "wanted {want:?}, got {:?}",
            db.runs_for_repo(repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn chmod_755(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

fn install_clean_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-review");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
OUT=""
FROM=""
TO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?}"
STATE="$PORCH_HOME/gh-pr-state"
for a in "$@"; do
  [ "$a" = "--version" ] && echo "gh version 2.50.0 (fake)" && exit 0
done
CMD=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done
case "$CMD" in
  list) if [ -f "$STATE" ]; then cat "$STATE"; else printf '[]\n'; fi ;;
  create)
    cat >/dev/null
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"t"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit) cat >/dev/null ;;
  checks) printf '[]\n' ;;
  *) echo "noop-gh: $*" >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

fn install_noop_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    std::fs::write(
        &path,
        r#"#!/bin/sh
printf '{"summary":"noop"}\n'
"#,
    )
    .unwrap();
    chmod_755(&path);
    path
}

struct Harness {
    tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    origin: PathBuf,
    bin_dir: PathBuf,
    fake_review: PathBuf,
    fake_gh: PathBuf,
    path: String,
}

fn setup_harness() -> Harness {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_review = install_clean_review(&bin_dir);
    let fake_gh = install_noop_gh(&bin_dir);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    init_bare(&origin).unwrap();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "base"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);

    let st = StdCommand::new("git")
        .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    let work = work.canonicalize().unwrap();
    git(&work, &["config", "user.email", "porch@example.com"]);
    git(&work, &["config", "user.name", "Porch"]);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake_review)
        .env(GH_BIN_ENV, &fake_gh)
        .env("PATH", &path)
        .arg("init")
        .arg("--skip-setup")
        .assert()
        .success();

    kill_daemon(&home);
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &home,
        &[
            (REVIEW_BIN_ENV, fake_review.as_os_str()),
            (GH_BIN_ENV, fake_gh.as_os_str()),
            ("PATH", path.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    Harness {
        tmp,
        work,
        home,
        origin,
        bin_dir,
        fake_review,
        fake_gh,
        path,
    }
}

fn push_branch(work: &Path, home: &Path, branch: &str) {
    let out = StdCommand::new("git")
        .current_dir(work)
        .env("PORCH_HOME", home)
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn eject_removes_remote_purge_drops_repo_state() {
    let h = setup_harness();
    let repo_id = repo_id_for(&h.work);
    let bare = h.home.join("repos").join(format!("{repo_id}.git"));
    assert!(bare.is_dir());

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["eject"])
        .assert()
        .success();

    let remotes = git_out(&h.work, &["remote"]);
    assert!(!remotes.lines().any(|l| l.trim() == "porch"));
    assert!(bare.is_dir(), "bare remains without --purge");
    let hook = std::fs::read_to_string(bare.join("hooks/post-receive")).unwrap();
    assert!(hook.contains("ejected"));

    // Re-init then purge.
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .arg("init")
        .arg("--skip-setup")
        .assert()
        .success();
    kill_daemon(&h.home);

    let other = h.home.join("repos").join("zzother.git");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("KEEP"), "x\n").unwrap();

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["eject", "--purge"])
        .assert()
        .success();

    let repo_id2 = repo_id_for(&h.work);
    let bare2 = h.home.join("repos").join(format!("{repo_id2}.git"));
    assert!(!bare2.exists());
    assert!(other.join("KEEP").is_file());
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    assert!(db.repo_by_id(&repo_id2).unwrap().is_none());
}

#[test]
fn rebase_conflict_parks_with_phase_rebase() {
    let h = setup_harness();

    // Divergent edits on the same line → rebase conflict.
    std::fs::write(h.work.join("README"), "author-line\n").unwrap();
    git(&h.work, &["add", "README"]);
    git(&h.work, &["commit", "-m", "author edit"]);
    git(&h.work, &["checkout", "-b", "feat-conflict"]);

    // Advance origin/main with conflicting change via a fresh clone.
    let tmp_clone = h.tmp.path().join("origin-edit");
    let st = StdCommand::new("git")
        .args([
            "clone",
            h.origin.to_str().unwrap(),
            tmp_clone.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let tmp_clone = tmp_clone.canonicalize().unwrap();
    git(&tmp_clone, &["config", "user.email", "porch@example.com"]);
    git(&tmp_clone, &["config", "user.name", "Porch"]);
    std::fs::write(tmp_clone.join("README"), "origin-line\n").unwrap();
    git(&tmp_clone, &["add", "README"]);
    git(&tmp_clone, &["commit", "-m", "origin edit"]);
    git(&tmp_clone, &["push", "origin", "main"]);

    push_branch(&h.work, &h.home, "feat-conflict");
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(15),
    );
    assert_eq!(run.status, "parked", "expected park not fail: {run:?}");
    assert!(
        run.worktree_dir.as_ref().is_some_and(|p| p.exists()),
        "worktree kept on rebase park"
    );
    let steps = db.step_results_for_run(&run.id).unwrap();
    let rebase = steps.iter().rev().find(|s| s.step == "rebase").unwrap();
    assert_eq!(rebase.status, "parked");

    let status = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(status.status.success());
    let body = String::from_utf8_lossy(&status.stdout);
    assert!(body.contains("\"phase\": \"rebase\""), "{body}");

    // approve refused on rebase park
    let approve = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(approve.status.code(), Some(2));

    let fixer = install_noop_fixer(&h.bin_dir);
    let abort = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PORCH_FIXER_BIN", &fixer)
        .args(["agent", "respond", "abort", "--run-id", &run.id])
        .output()
        .unwrap();
    // D11: cancelled → exit 1 with JSON status.
    assert_eq!(abort.status.code(), Some(1));
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
}

#[test]
fn rebase_park_fix_rewrites_tip_and_continues() {
    let h = setup_harness();

    std::fs::write(h.work.join("README"), "author-line\n").unwrap();
    git(&h.work, &["add", "README"]);
    git(&h.work, &["commit", "-m", "author edit"]);
    git(&h.work, &["checkout", "-b", "feat-rebase-fix"]);

    let tmp_clone = h.tmp.path().join("origin-edit-fix");
    let st = StdCommand::new("git")
        .args([
            "clone",
            h.origin.to_str().unwrap(),
            tmp_clone.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let tmp_clone = tmp_clone.canonicalize().unwrap();
    git(&tmp_clone, &["config", "user.email", "porch@example.com"]);
    git(&tmp_clone, &["config", "user.name", "Porch"]);
    std::fs::write(tmp_clone.join("README"), "origin-line\n").unwrap();
    git(&tmp_clone, &["add", "README"]);
    git(&tmp_clone, &["commit", "-m", "origin edit"]);
    git(&tmp_clone, &["push", "origin", "main"]);

    push_branch(&h.work, &h.home, "feat-rebase-fix");
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(15),
    );
    assert_eq!(run.status, "parked", "expected park not fail: {run:?}");
    let base = run.base_sha.clone().expect("rebase park records base_sha");

    // Fake fixer: re-parent tip onto recorded base so retry rebase is a no-op.
    let fixer = h.bin_dir.join("fake-rebase-fixer");
    std::fs::write(
        &fixer,
        format!(
            r#"#!/bin/sh
set -e
BASE="{base}"
git reset --soft "$BASE"
printf 'resolved\n' > README
git add README
git commit -m "rebase fix" >/dev/null
printf '{{"summary":"rewrote tip onto base"}}\n'
"#
        ),
    )
    .unwrap();
    chmod_755(&fixer);

    let fix = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PORCH_FIXER_BIN", &fixer)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .args(["agent", "respond", "fix", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        fix.status.success() || fix.status.code() == Some(1),
        "fix stdout={} stderr={}",
        String::from_utf8_lossy(&fix.stdout),
        String::from_utf8_lossy(&fix.stderr)
    );

    let start = Instant::now();
    let final_run = loop {
        let r = db.run_by_id(&run.id).unwrap().unwrap();
        let steps = db.step_results_for_run(&r.id).unwrap();
        let rebase_done = steps
            .iter()
            .any(|s| s.step == "rebase" && s.status == "completed");
        if rebase_done || matches!(r.status.as_str(), "completed" | "failed" | "cancelled") {
            break r;
        }
        assert!(
            start.elapsed() < Duration::from_secs(25),
            "fix did not finish: {r:?} steps={steps:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let steps = db.step_results_for_run(&final_run.id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "rebase" && s.status == "completed"),
        "expected rebase completed after fix: {steps:?} run={final_run:?}"
    );
    assert_ne!(final_run.status, "failed", "run={final_run:?}");
}

#[test]
fn rerun_enqueues_fresh_run_id() {
    let h = setup_harness();
    git(&h.work, &["checkout", "-b", "feat-rerun"]);
    std::fs::write(h.work.join("extra.txt"), "x\n").unwrap();
    git(&h.work, &["add", "extra.txt"]);
    git(&h.work, &["commit", "-m", "feat"]);
    push_branch(&h.work, &h.home, "feat-rerun");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let first = wait_status(
        &db,
        &repo_id,
        &["completed", "failed", "parked"],
        Duration::from_secs(20),
    );
    let first_id = first.id.clone();
    let first_wt = run_worktree_dir(&h.home, &repo_id, &first_id);

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env(REVIEW_BIN_ENV, &h.fake_review)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env("PATH", &h.path)
        .args(["rerun", "--run-id", &first_id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("rerun started:"));
    let second = wait_status(
        &db,
        &repo_id,
        &["completed", "failed", "parked", "running", "pending"],
        Duration::from_secs(20),
    );
    // Wait until we have two terminal-ish runs or a second distinct id.
    let start = Instant::now();
    let second_id = loop {
        let runs = db.runs_for_repo(&repo_id).unwrap();
        if let Some(r) = runs.iter().rev().find(|r| r.id != first_id) {
            if matches!(
                r.status.as_str(),
                "completed" | "failed" | "parked" | "cancelled"
            ) || start.elapsed() > Duration::from_secs(2)
            {
                break r.id.clone();
            }
        }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(first_id, second_id);
    let second_wt = run_worktree_dir(&h.home, &repo_id, &second_id);
    assert_ne!(first_wt, second_wt);
    let _ = second;
}

#[test]
fn agent_sync_reports_fetch_hint_json() {
    let h = setup_harness();
    git(&h.work, &["checkout", "-b", "feat-sync"]);
    std::fs::write(h.work.join("s.txt"), "1\n").unwrap();
    git(&h.work, &["add", "s.txt"]);
    git(&h.work, &["commit", "-m", "s"]);
    push_branch(&h.work, &h.home, "feat-sync");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let _ = wait_status(
        &db,
        &repo_id,
        &["completed", "failed", "parked"],
        Duration::from_secs(20),
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "sync"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("fetch_hint"), "{body}");
    assert!(body.contains("git fetch porch"), "{body}");
    assert!(!body.contains("origin push"), "{body}");
}

#[test]
fn agent_sync_recover_ff_and_diverge_refuse() {
    let h = setup_harness();
    git(&h.work, &["checkout", "-b", "feat-recover"]);
    std::fs::write(h.work.join("r.txt"), "1\n").unwrap();
    git(&h.work, &["add", "r.txt"]);
    git(&h.work, &["commit", "-m", "r1"]);
    let submit = git_out(&h.work, &["rev-parse", "HEAD"]);
    push_branch(&h.work, &h.home, "feat-recover");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed", "parked"],
        Duration::from_secs(20),
    );

    // Unpublished pipeline tip ahead of the author clone.
    std::fs::write(h.work.join("r.txt"), "2\n").unwrap();
    git(&h.work, &["add", "r.txt"]);
    git(&h.work, &["commit", "-m", "r2-pipeline"]);
    let pipeline = git_out(&h.work, &["rev-parse", "HEAD"]);
    let bare = h.home.join("repos").join(format!("{repo_id}.git"));
    git(
        &h.work,
        &[
            "push",
            bare.to_str().unwrap(),
            &format!("{pipeline}:refs/porch/recover/{}", run.id),
        ],
    );
    git(&h.work, &["reset", "--hard", &submit]);

    let hint = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "sync", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        hint.status.success(),
        "{}",
        String::from_utf8_lossy(&hint.stderr)
    );
    let hint_body = String::from_utf8_lossy(&hint.stdout);
    assert!(
        hint_body.contains("porch agent sync --recover"),
        "recoverable hint: {hint_body}"
    );

    let recover = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "sync", "--recover", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        recover.status.success(),
        "{}",
        String::from_utf8_lossy(&recover.stderr)
    );
    let body = String::from_utf8_lossy(&recover.stdout);
    assert!(
        body.contains("custody_returned") || body.contains("\"recovered\": true"),
        "{body}"
    );
    assert_eq!(git_out(&h.work, &["rev-parse", "HEAD"]), pipeline);

    // Divergent sibling — refuse, keep recovery ref.
    git(&h.work, &["reset", "--hard", &submit]);
    std::fs::write(h.work.join("r.txt"), "sibling\n").unwrap();
    git(&h.work, &["add", "r.txt"]);
    git(&h.work, &["commit", "-m", "sibling"]);
    let refuse = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "sync", "--recover", "--run-id", &run.id])
        .output()
        .unwrap();
    assert_eq!(refuse.status.code(), Some(1));
    let refuse_body = String::from_utf8_lossy(&refuse.stdout);
    assert!(
        refuse_body.contains("recovery refused") || refuse_body.contains("not an ancestor"),
        "{refuse_body}"
    );
    let rec_sha = git_out(
        &h.work,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            &format!("refs/porch/recover/{}", run.id),
        ],
    );
    assert_eq!(rec_sha, pipeline);
}

#[test]
fn certify_sees_biome_from_config_tools_with_thin_path() {
    let h = setup_harness();

    // Recorded biome lives outside the thin PATH.
    let tools_dir = h.tmp.path().join("tools-only");
    std::fs::create_dir_all(&tools_dir).unwrap();
    let biome = tools_dir.join("biome");
    std::fs::write(
        &biome,
        r"#!/bin/sh
echo biome-ok
",
    )
    .unwrap();
    chmod_755(&biome);

    let cfg = HomeConfig {
        tools: ToolsConfig {
            biome: Some(biome.to_string_lossy().into_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    write_home_config(&h.home, &cfg).unwrap();

    // Put biome-using certify command on trusted main.
    let edit = h.tmp.path().join("edit-main");
    let st = StdCommand::new("git")
        .args(["clone", h.origin.to_str().unwrap(), edit.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    let edit = edit.canonicalize().unwrap();
    git(&edit, &["config", "user.email", "porch@example.com"]);
    git(&edit, &["config", "user.name", "Porch"]);
    std::fs::write(
        edit.join(".porch.yaml"),
        "commands:\n  format: biome --version\n",
    )
    .unwrap();
    git(&edit, &["add", ".porch.yaml"]);
    git(&edit, &["commit", "-m", "trusted certify biome"]);
    git(&edit, &["push", "origin", "main"]);

    // Restart daemon with thin PATH (no tools-only dir).
    kill_daemon(&h.home);
    let thin = format!("/usr/bin:/bin:{}", h.bin_dir.display());
    let bin = assert_cmd::cargo::cargo_bin("porch");
    porch_gate::spawn_detached_with_env(
        &bin,
        &h.home,
        &[
            (REVIEW_BIN_ENV, h.fake_review.as_os_str()),
            (GH_BIN_ENV, h.fake_gh.as_os_str()),
            ("PATH", thin.as_ref()),
            ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ],
    )
    .unwrap();
    porch_gate::wait_for_health(&h.home, Duration::from_secs(5)).unwrap();

    // Refresh author origin + new feature.
    git(&h.work, &["fetch", "origin"]);
    git(&h.work, &["checkout", "main"]);
    git(&h.work, &["pull", "--ff-only", "origin", "main"]);
    git(&h.work, &["checkout", "-b", "feat-biome"]);
    std::fs::write(h.work.join("n.txt"), "n\n").unwrap();
    git(&h.work, &["add", "n.txt"]);
    git(&h.work, &["commit", "-m", "feat"]);
    push_branch(&h.work, &h.home, "feat-biome");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    // Compose parks after certify+scaffold; park proves certify reached deliver.
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed"],
        Duration::from_secs(25),
    );
    assert_eq!(
        run.status, "parked",
        "certify should find biome via tools PATH then park compose: {:?}",
        run.error
    );
    assert!(
        run.pr_url.is_some(),
        "scaffold PR proves certify completed before compose park"
    );
}
