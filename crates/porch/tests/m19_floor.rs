//! M19: composed review round — deterministic floor then judgment.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{self, Resolution, Role};
use porch_gate::{Db, kill_group, repo_id_for};
use porch_git::init_bare;
use porch_review::REVIEW_AGENT_BIN_ENV;
use tempfile::TempDir;

fn chmod_755(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn write_exe(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    chmod_755(path);
}

fn path_with(bin_dir: &Path) -> String {
    let real = StdCommand::new("which")
        .arg("git")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                Path::new(&p).parent().map(|d| d.display().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "/usr/bin".into());
    format!("{}:{real}", bin_dir.display())
}

fn git(work: &Path, args: &[&str]) {
    let st = StdCommand::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn kill_daemon(home: &Path) {
    if let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            kill_group(pid);
        }
    }
    let marker = home.display().to_string();
    let _ = StdCommand::new("pkill")
        .args(["-9", "-f", &marker])
        .output();
    std::thread::sleep(Duration::from_millis(300));
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

fn install_noop_gh(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-gh");
    write_exe(
        &path,
        r#"#!/bin/sh
: "${PORCH_HOME:?PORCH_HOME required}"
STATE="$PORCH_HOME/gh-pr-state"
for a in "$@"; do [ "$a" = "--version" ] && echo "gh version 2.50.0 (fake)" && exit 0; done
CMD=""; PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done
case "$CMD" in
  list)
    if [ -f "$STATE" ]; then /bin/cat "$STATE"; else printf '[]\n'; fi
    ;;
  create)
    /bin/cat >/dev/null
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit) /bin/cat >/dev/null ;;
  view) printf '{"mergeable":"MERGEABLE","number":1,"url":"https://example.com/pull/1","title":"porch: created","body":""}\n' ;;
  checks) printf '[]\n' ;;
  *) echo "noop-gh: $*" >&2; exit 1 ;;
esac
"#,
    );
    path
}

fn install_fake_claude(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("claude");
    let log = bin_dir.join("claude-argv.log");
    write_exe(
        &path,
        &format!(
            r#"#!/bin/sh
set -e
printf '%s\n' "$*" >> "{log}"
for a in "$@"; do
  if [ "$a" = "--help" ] || [ "$a" = "-h" ]; then
    echo "fake claude help"
    exit 0
  fi
  if [ "$a" = "--session-id" ] || [ "$a" = "--resume" ]; then
    echo "fake-claude: session flags forbidden for reviewer" >&2
    exit 3
  fi
done
if [ -n "${{PORCH_HOME:-}}" ]; then
  printf 'judgment-start\n' >> "$PORCH_HOME/producer-order"
fi
printf '%s\n' '{{"findings":[],"coverage":[{{"path":"README","status":"pass"}}]}}'
"#,
            log = log.display()
        ),
    );
    path
}

fn install_fake_floor(install: &Path, mode: &str) -> PathBuf {
    let path = install.join(format!("porch-quality{}", std::env::consts::EXE_SUFFIX));
    write_exe(
        &path,
        &format!(
            r#"#!/bin/sh
set -e
HELP=0
OUT=""
FROM=""
TO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) HELP=1; shift ;;
    --output) OUT="$2"; shift 2 ;;
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --format) shift 2 ;;
    *) shift ;;
  esac
done
if [ "$HELP" = 1 ]; then echo "fake porch-quality help"; exit 0; fi
if [ -z "$OUT" ]; then echo "missing --output" >&2; exit 1; fi
if [ -n "${{PORCH_HOME:-}}" ]; then
  printf 'floor-start\n' >> "$PORCH_HOME/producer-order"
fi
FILES=$(git diff --name-only "$FROM" "$TO" 2>/dev/null || true)
if [ -z "$FILES" ]; then FILES="README"; fi
FILES_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else FILES_JSON="$FILES_JSON,"; fi
  FILES_JSON="$FILES_JSON\"$f\""
done
FILES_JSON="$FILES_JSON]"
COV_JSON="["
FIRST=1
for f in $FILES; do
  if [ $FIRST -eq 1 ]; then FIRST=0; else COV_JSON="$COV_JSON,"; fi
  COV_JSON="$COV_JSON{{\"path\":\"$f\",\"status\":\"pass\"}}"
done
COV_JSON="$COV_JSON]"
MODE="{mode}"
case "$MODE" in
  blocking)
    TARGET=$(printf '%s\n' $FILES | head -n1)
    if [ -z "$TARGET" ]; then TARGET="README"; fi
    printf '{{"comments":[{{"path":"%s","content":"floor blocker on empty input","category":"bug","severity":"high","start_line":1,"end_line":1}}],"files":%s,"coverage":%s}}\n' \
      "$TARGET" "$FILES_JSON" "$COV_JSON" > "$OUT"
    ;;
  *)
    printf '{{"comments":[],"files":%s,"coverage":%s}}\n' "$FILES_JSON" "$COV_JSON" > "$OUT"
    ;;
esac
/bin/sleep 0.2
if [ -n "${{PORCH_HOME:-}}" ]; then
  printf 'floor-end\n' >> "$PORCH_HOME/producer-order"
fi
"#,
        ),
    );
    path
}

fn copy_porch_launch(install: &Path) -> PathBuf {
    fs::create_dir_all(install).unwrap();
    let src = assert_cmd::cargo::cargo_bin("porch");
    let dst = install.join(format!("porch{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&src, &dst).unwrap();
    chmod_755(&dst);
    dst
}

struct Harness {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
}

fn seed_origin_and_work(root: &Path) -> PathBuf {
    let origin = root.join("origin.git");
    let work = root.join("work");
    init_bare(&origin).unwrap();
    let seed = root.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    fs::write(seed.join("README"), "base\n").unwrap();
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
    work
}

fn setup_engine(engine: &str, floor_mode: &str) -> Harness {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    let install = root.join("install");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();

    let porch_bin = copy_porch_launch(&install);
    install_fake_floor(&install, floor_mode);
    let fake_gh = install_noop_gh(&bin);
    let fake_agent = if engine == "agent" {
        Some(install_fake_claude(&bin))
    } else {
        None
    };
    let path = format!("{}:{}", install.display(), path_with(&bin));
    let work = seed_origin_and_work(&root);

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&work)
        .env("PATH", &path)
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env_remove(REVIEW_AGENT_BIN_ENV)
        .args(["setup", "--yes", "--engine", engine])
        .assert()
        .success();

    let mut init = Command::cargo_bin("porch").unwrap();
    init.current_dir(&work)
        .env("PATH", &path)
        .env("PORCH_HOME", &home)
        .env_remove("PORCH_REVIEW_BIN")
        .env(GH_BIN_ENV, &fake_gh)
        .args(["init", "--skip-setup"]);
    if let Some(agent) = &fake_agent {
        init.env(REVIEW_AGENT_BIN_ENV, agent);
    } else {
        init.env_remove(REVIEW_AGENT_BIN_ENV);
    }
    init.assert().success();

    kill_daemon(&home);
    let timeout: &std::ffi::OsStr = "10".as_ref();
    let mut extra: Vec<(&str, &std::ffi::OsStr)> = vec![
        (GH_BIN_ENV, fake_gh.as_os_str()),
        ("PATH", path.as_ref()),
        ("PORCH_REVIEW_TIMEOUT_SECS", timeout),
    ];
    if let Some(agent) = &fake_agent {
        extra.push((REVIEW_AGENT_BIN_ENV, agent.as_os_str()));
    }
    porch_gate::spawn_detached_with_env(&porch_bin, &home, &extra).unwrap();
    porch_gate::wait_for_health(&home, Duration::from_secs(5)).unwrap();

    Harness {
        _tmp: tmp,
        work,
        home,
    }
}

fn setup_agent(floor_mode: &str) -> Harness {
    setup_engine("agent", floor_mode)
}

fn setup_quality(floor_mode: &str) -> Harness {
    setup_engine("quality", floor_mode)
}

fn push_branch(work: &Path, home: &Path, branch: &str) {
    fs::write(work.join("README"), "changed\n").unwrap();
    git(work, &["add", "README"]);
    git(work, &["commit", "-m", "change"]);
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

fn recorded_requirements(db: &Db, run_id: &str) -> Vec<porch_gate::rounds::RequirementRow> {
    let rounds = rounds::rounds_for_run(db, run_id).unwrap();
    assert_eq!(rounds.len(), 1, "expected one round, got {rounds:?}");
    rounds::requirements_for_round(db, &rounds[0].id).unwrap()
}

#[test]
fn agent_engine_records_floor_and_judgment_and_runs_floor_first() {
    let h = setup_agent("clean");
    push_branch(&h.work, &h.home, "feat-agent-floor");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_ne!(run.status, "failed", "err={:?}", run.error);

    let reqs = recorded_requirements(&db, &run.id);
    assert_eq!(reqs.len(), 2, "expected floor then judgment, got {reqs:?}");
    assert_eq!(reqs[0].slot, 0);
    assert_eq!(reqs[0].role, Role::Floor);
    assert_eq!(reqs[0].resolution, Resolution::Resolved);
    assert!(reqs[0].producer_invocation_id.is_some());
    assert!(reqs[0].expected_equivalence_digest.is_some());
    assert_eq!(reqs[1].slot, 1);
    assert_eq!(reqs[1].role, Role::Judgment);
    assert_eq!(reqs[1].resolution, Resolution::Resolved);
    assert!(reqs[1].producer_invocation_id.is_some());

    let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
    let floor_end = order
        .lines()
        .position(|l| l == "floor-end")
        .expect("floor invocation must finish");
    let judgment_start = order
        .lines()
        .position(|l| l == "judgment-start")
        .expect("judgment spawn must be recorded");
    assert!(
        floor_end < judgment_start,
        "floor must finish before judgment spawn, order={order}"
    );

    kill_daemon(&h.home);
}

#[test]
fn quality_engine_records_floor_alone_and_still_forwards() {
    let h = setup_quality("clean");
    push_branch(&h.work, &h.home, "feat-quality-floor");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_ne!(run.status, "failed", "err={:?}", run.error);
    assert!(
        run.review_approved_head_sha
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
        "floor-only round must remain eligible to authorize a forward: {run:?}"
    );

    let reqs = recorded_requirements(&db, &run.id);
    assert_eq!(reqs.len(), 1, "expected floor alone, got {reqs:?}");
    assert_eq!(reqs[0].slot, 0);
    assert_eq!(reqs[0].role, Role::Floor);
    assert_eq!(reqs[0].resolution, Resolution::Resolved);
    assert!(reqs[0].producer_invocation_id.is_some());

    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    let producers = rounds::producers_for_round(&db, &rounds[0].id).unwrap();
    assert_eq!(
        producers.len(),
        1,
        "quality must not spawn a judgment producer"
    );

    let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
    assert!(
        order.lines().any(|l| l == "floor-end"),
        "floor must run, order={order}"
    );
    assert!(
        !order.lines().any(|l| l == "judgment-start"),
        "quality must not spawn judgment, order={order}"
    );

    kill_daemon(&h.home);
}

#[test]
fn blocking_floor_findings_still_run_judgment_and_park() {
    let h = setup_agent("blocking");
    push_branch(&h.work, &h.home, "feat-floor-block");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    assert!(run.review_approved_head_sha.is_none());

    let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
    let floor_end = order
        .lines()
        .position(|l| l == "floor-end")
        .expect("floor invocation must finish");
    let judgment_start = order
        .lines()
        .position(|l| l == "judgment-start")
        .expect("judgment must still spawn after blocking floor findings");
    assert!(
        floor_end < judgment_start,
        "floor must finish before judgment spawn, order={order}"
    );

    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .args(["agent", "status", "--run-id", &run.id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "parked");
    let findings = v["findings"].as_array().expect("findings");
    assert!(
        !findings.is_empty(),
        "merged findings must park the run: {v}"
    );

    kill_daemon(&h.home);
}

#[test]
fn judgment_context_applications_exclude_floor_output() {
    let h = setup_agent("clean");
    push_branch(&h.work, &h.home, "feat-no-floor-output");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_ne!(run.status, "failed", "err={:?}", run.error);

    let reqs = recorded_requirements(&db, &run.id);
    let judgment = reqs
        .iter()
        .find(|r| r.role == Role::Judgment)
        .expect("judgment requirement");
    let judgment_id = judgment
        .producer_invocation_id
        .as_deref()
        .expect("resolved judgment invocation");

    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    let apps = rounds::context_applications_for_round(&db, &rounds[0].id).unwrap();
    let judgment_apps: Vec<_> = apps
        .iter()
        .filter(|a| a.producer_invocation_id == judgment_id)
        .collect();
    assert!(
        judgment_apps
            .iter()
            .all(|a| a.element_name != "floor-output" && !a.element_name.contains("floor")),
        "judgment context must not include floor output: {judgment_apps:?}"
    );

    kill_daemon(&h.home);
}
