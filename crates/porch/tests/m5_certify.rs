//! M5: cheap certify adapters from trusted `.porch.yaml` (PATH fakes only).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_gate::{Db, kill_group, repo_id_for};
use porch_git::init_bare;
use porch_review::REVIEW_BIN_ENV;
use tempfile::TempDir;

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
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

fn install_fake_review(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-review");
    let script = r#"#!/bin/sh
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
MODE="${PORCH_FAKE_REVIEW_MODE:-clean}"
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
case "$MODE" in
  clean)
    printf '{"comments":[],"files":%s}\n' "$FILES_JSON" > "$OUT"
    ;;
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{"comments":[{"path":"%s","content":"null deref","category":"bug","severity":"high","start_line":1,"end_line":2}],"files":%s}\n' \
      "$TARGET" "$FILES_JSON" > "$OUT"
    ;;
  *)
    echo "unknown PORCH_FAKE_REVIEW_MODE=$MODE" >&2
    exit 1
    ;;
esac
"#;
    std::fs::write(&path, script).unwrap();
    chmod_755(&path);
    path
}

fn install_certify_fakes(bin_dir: &Path) {
    let format = bin_dir.join("porch-fake-format");
    std::fs::write(
        &format,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
echo format-ran >> "$PORCH_HOME/certify-format.ran"
"#,
    )
    .unwrap();
    chmod_755(&format);

    let lint = bin_dir.join("porch-fake-lint");
    std::fs::write(
        &lint,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
echo lint-ran >> "$PORCH_HOME/certify-lint.ran"
"#,
    )
    .unwrap();
    chmod_755(&lint);

    let lint_fail = bin_dir.join("porch-fake-lint-fail");
    std::fs::write(
        &lint_fail,
        r#"#!/bin/sh
: "${PORCH_HOME:?PORCH_HOME required}"
echo lint-fail >> "$PORCH_HOME/certify-lint-fail.ran"
echo "fake lint diagnostic: boom" >&2
exit 1
"#,
    )
    .unwrap();
    chmod_755(&lint_fail);

    let hostile = bin_dir.join("porch-hostile-format");
    std::fs::write(
        &hostile,
        r#"#!/bin/sh
: "${PORCH_HOME:?PORCH_HOME required}"
echo hostile >> "$PORCH_HOME/certify-hostile.ran"
"#,
    )
    .unwrap();
    chmod_755(&hostile);

    let format_dirty = bin_dir.join("porch-fake-format-dirty");
    std::fs::write(
        &format_dirty,
        r#"#!/bin/sh
set -e
: "${PORCH_HOME:?PORCH_HOME required}"
echo format-dirty >> "$PORCH_HOME/certify-format-dirty.ran"
echo formatted >> dirty.txt
"#,
    )
    .unwrap();
    chmod_755(&format_dirty);
}

struct Setup {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    fake_review: PathBuf,
    path: String,
}

/// Seed origin/main; optional trusted `.porch.yaml` body on the default branch.
fn setup(trusted_yaml: Option<&str>, review_mode: &str) -> Setup {
    setup_with_opts(trusted_yaml, review_mode, false)
}

/// When `isolate_git_identity`, daemon gets empty `HOME` + `GIT_CONFIG_NOSYSTEM`
/// and the gate bare sets `user.useConfigOnly=true` so correction commits cannot
/// guess an author.
fn setup_with_opts(
    trusted_yaml: Option<&str>,
    review_mode: &str,
    isolate_git_identity: bool,
) -> Setup {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    let empty_home = root.join("empty-home");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&empty_home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_review = install_fake_review(&bin_dir);
    install_certify_fakes(&bin_dir);

    init_bare(&origin).unwrap();

    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    if let Some(yaml) = trusted_yaml {
        std::fs::write(seed.join(".porch.yaml"), yaml).unwrap();
        git(&seed, &["add", ".porch.yaml"]);
    }
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

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PORCH_HOME", &home)
        .env(REVIEW_BIN_ENV, &fake_review)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PATH", &path)
        .arg("init")
        .assert()
        .success();

    if isolate_git_identity {
        harden_gate_bare_no_identity(&home);
    }

    kill_daemon(&home);
    restart_certify_daemon(
        &home,
        &fake_review,
        review_mode,
        &path,
        isolate_git_identity.then_some(empty_home.as_path()),
    );

    let _ = origin;
    Setup {
        _tmp: tmp,
        work,
        home,
        fake_review,
        path,
    }
}

fn gate_bare_dir(home: &Path) -> PathBuf {
    std::fs::read_dir(home.join("repos"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|x| x == "git"))
        .map(|e| e.path())
        .expect("gate bare")
}

fn harden_gate_bare_no_identity(home: &Path) {
    let gate_bare = gate_bare_dir(home);
    let bare = gate_bare.to_str().unwrap();
    let st = StdCommand::new("git")
        .args(["--git-dir", bare, "config", "user.useConfigOnly", "true"])
        .status()
        .unwrap();
    assert!(st.success());
    let _ = StdCommand::new("git")
        .args(["--git-dir", bare, "config", "--unset-all", "user.email"])
        .status();
    let _ = StdCommand::new("git")
        .args(["--git-dir", bare, "config", "--unset-all", "user.name"])
        .status();
}

fn restart_certify_daemon(
    home: &Path,
    fake_review: &Path,
    review_mode: &str,
    path: &str,
    empty_home: Option<&Path>,
) {
    let bin = assert_cmd::cargo::cargo_bin("porch");
    let mut extra: Vec<(&str, &std::ffi::OsStr)> = vec![
        (REVIEW_BIN_ENV, fake_review.as_os_str()),
        ("PORCH_FAKE_REVIEW_MODE", review_mode.as_ref()),
        ("PATH", path.as_ref()),
        ("PORCH_REVIEW_TIMEOUT_SECS", "10".as_ref()),
        ("PORCH_CERTIFY_TIMEOUT_SECS", "30".as_ref()),
    ];
    if let Some(empty) = empty_home {
        extra.push(("HOME", empty.as_os_str()));
        extra.push(("GIT_CONFIG_NOSYSTEM", std::ffi::OsStr::new("1")));
    }
    porch_gate::spawn_detached_with_env(&bin, home, &extra).unwrap();
    porch_gate::wait_for_health(home, Duration::from_secs(5)).unwrap();
}

fn push_with_env(s: &Setup, branch: &str, review_mode: &str) {
    let out = StdCommand::new("git")
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env(REVIEW_BIN_ENV, &s.fake_review)
        .env("PORCH_FAKE_REVIEW_MODE", review_mode)
        .env("PATH", &s.path)
        .args(["push", "porch", &format!("HEAD:refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_change(work: &Path, name: &str, body: &str) {
    std::fs::write(work.join(name), body).unwrap();
    git(work, &["add", name]);
    git(work, &["commit", "-m", name]);
}

fn last_step<'a>(
    steps: &'a [porch_gate::StepResultRow],
    name: &str,
) -> Option<&'a porch_gate::StepResultRow> {
    steps.iter().rfind(|s| s.step == name)
}

const TRUSTED_OK: &str = r"
commands:
  format: porch-fake-format
  lint: porch-fake-lint
";

#[test]
fn trusted_format_lint_run_hostile_pushed_commands_do_not() {
    let s = setup(Some(TRUSTED_OK), "clean");

    // Hostile executing commands on the feature branch must not run.
    std::fs::write(
        s.work.join(".porch.yaml"),
        r"
commands:
  format: porch-hostile-format
  lint: porch-hostile-format
",
    )
    .unwrap();
    git(&s.work, &["add", ".porch.yaml"]);
    git(&s.work, &["commit", "-m", "hostile yaml"]);
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-trust", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("completed")
    );
    assert_eq!(
        last_step(&steps, "deliver").map(|s| s.status.as_str()),
        Some("completed")
    );

    assert!(
        s.home.join("certify-format.ran").is_file(),
        "trusted format should have run"
    );
    assert!(
        s.home.join("certify-lint.ran").is_file(),
        "trusted lint should have run"
    );
    assert!(
        !s.home.join("certify-hostile.ran").is_file(),
        "hostile pushed commands must not run"
    );

    kill_daemon(&s.home);
}

#[test]
fn lint_nonzero_fails_certify_and_run() {
    let yaml = r"
commands:
  format: porch-fake-format
  lint: porch-fake-lint-fail
";
    let s = setup(Some(yaml), "clean");
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-lint-fail", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let err = run.error.as_deref().unwrap_or("");
    assert!(
        err.contains("fake lint diagnostic: boom"),
        "certify error should include truncated stderr: {err}"
    );

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("failed")
    );
    let certify_err = last_step(&steps, "certify")
        .and_then(|s| s.error.as_deref())
        .unwrap_or("");
    assert!(
        certify_err.contains("fake lint diagnostic: boom"),
        "step_results error should include stderr: {certify_err}"
    );
    assert!(
        last_step(&steps, "deliver").is_none_or(|s| s.status != "completed"),
        "deliver must not complete after certify failure: {steps:?}"
    );
    assert!(s.home.join("certify-format.ran").is_file());
    assert!(s.home.join("certify-lint-fail.ran").is_file());

    kill_daemon(&s.home);
}

#[test]
fn missing_trusted_yaml_completes_without_spawn() {
    let s = setup(None, "clean");
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-no-yaml", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("completed")
    );
    assert!(!s.home.join("certify-format.ran").is_file());
    assert!(!s.home.join("certify-lint.ran").is_file());

    kill_daemon(&s.home);
}

#[test]
fn empty_diff_skips_certify_without_spawn() {
    let s = setup(Some(TRUSTED_OK), "clean");
    // Push main tip: empty after rebase → skip remaining (including certify).
    push_with_env(&s, "main", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);

    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("skipped")
    );
    assert!(!s.home.join("certify-format.ran").is_file());
    assert!(!s.home.join("certify-lint.ran").is_file());

    kill_daemon(&s.home);
}

#[test]
fn respond_approve_runs_trusted_certify() {
    let s = setup(Some(TRUSTED_OK), "blocking");
    // Hostile executing commands on the parked tip must still not run.
    std::fs::write(
        s.work.join(".porch.yaml"),
        r"
commands:
  format: porch-hostile-format
  lint: porch-hostile-format
",
    )
    .unwrap();
    git(&s.work, &["add", ".porch.yaml"]);
    git(&s.work, &["commit", "-m", "hostile yaml"]);
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-approve-certify", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(30));

    // Approve runs certify in this CLI process (not the daemon), so PATH must
    // include the format/lint fakes here.
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .env("PATH", &s.path)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("completed")
    );
    assert!(
        s.home.join("certify-format.ran").is_file(),
        "approve path must run trusted format"
    );
    assert!(
        s.home.join("certify-lint.ran").is_file(),
        "approve path must run trusted lint"
    );
    assert!(
        !s.home.join("certify-hostile.ran").is_file(),
        "hostile pushed commands must not run on approve"
    );

    kill_daemon(&s.home);
}

#[test]
fn agent_skip_skips_certify_without_spawn() {
    let s = setup(Some(TRUSTED_OK), "blocking");
    commit_change(&s.work, "bug.txt", "boom\n");
    push_with_env(&s, "feat-skip", "blocking");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(&db, &repo_id, &["parked"], Duration::from_secs(30));

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&s.work)
        .env("PORCH_HOME", &s.home)
        .args(["agent", "respond", "skip", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    assert_eq!(run.status, "completed");
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("skipped")
    );
    assert!(!s.home.join("certify-format.ran").is_file());
    assert!(!s.home.join("certify-lint.ran").is_file());

    kill_daemon(&s.home);
}

#[test]
fn unparseable_trusted_yaml_fails_closed() {
    let s = setup(Some("commands: [not-a-map\n"), "clean");
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-bad-yaml", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert_eq!(
        last_step(&steps, "certify").map(|s| s.status.as_str()),
        Some("failed")
    );
    assert!(!s.home.join("certify-format.ran").is_file());

    kill_daemon(&s.home);
}

#[test]
fn format_dirty_tree_gets_correction_commit() {
    let yaml = r"
commands:
  format: porch-fake-format-dirty
  lint: porch-fake-lint
";
    let s = setup(Some(yaml), "clean");
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-dirty", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "completed", "err={:?}", run.error);
    assert!(s.home.join("certify-format-dirty.ran").is_file());
    assert!(s.home.join("certify-lint.ran").is_file());

    // Correction commit is on the run worktree tip (gate bare), not forwarded yet.
    let run = db.run_by_id(&run.id).unwrap().unwrap();
    let head = run.head_sha.expect("head_sha");
    let gate_bare = gate_bare_dir(&s.home);
    let subj = StdCommand::new("git")
        .args([
            "--git-dir",
            gate_bare.to_str().unwrap(),
            "log",
            "-5",
            "--format=%s",
            &head,
        ])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&subj.stdout);
    assert!(
        log.lines().any(|l| l.contains("porch: apply format")),
        "expected format correction commit in log:\n{log}\nsubj_err={}",
        String::from_utf8_lossy(&subj.stderr)
    );

    kill_daemon(&s.home);
}

#[test]
fn correction_commit_sets_identity_under_use_config_only() {
    let yaml = r"
commands:
  format: porch-fake-format-dirty
  lint: porch-fake-lint
";
    let s = setup_with_opts(Some(yaml), "clean", true);
    commit_change(&s.work, "feat.txt", "x\n");
    push_with_env(&s, "feat-identity", "clean");

    let db = Db::open(&s.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&s.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["completed", "failed"],
        Duration::from_secs(30),
    );
    assert_eq!(
        run.status, "completed",
        "correction commit must supply porch identity under useConfigOnly; err={:?}",
        run.error
    );

    let run = db.run_by_id(&run.id).unwrap().unwrap();
    let head = run.head_sha.expect("head_sha");
    let gate_bare = gate_bare_dir(&s.home);
    let out = StdCommand::new("git")
        .args([
            "--git-dir",
            gate_bare.to_str().unwrap(),
            "log",
            "-1",
            "--format=%an <%ae> %s",
            &head,
        ])
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&out.stdout);
    assert!(
        line.contains("Porch <porch@example.com>"),
        "expected porch-managed author, got: {line}"
    );
    assert!(
        line.contains("porch: apply format"),
        "expected format correction subject, got: {line}"
    );

    kill_daemon(&s.home);
}
