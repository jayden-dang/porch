//! M19: composed review round — deterministic floor then judgment.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use porch_agent::FIXER_BIN_ENV;
use porch_deliver::GH_BIN_ENV;
use porch_gate::rounds::{
    self, AssuranceCompletion, ExecutionState, Resolution, Role, run_required_set_digest,
};
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
    /bin/cat > "$PORCH_HOME/gh-pr-body.txt"
    printf '[{"number":1,"url":"https://example.com/pull/1","title":"porch: created"}]\n' > "$STATE"
    echo "https://example.com/pull/1"
    ;;
  edit)
    /bin/cat > "$PORCH_HOME/gh-pr-body.txt"
    ;;
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
  hang)
    while true; do /bin/sleep 60; done
    ;;
  exit-fail)
    echo "forced floor failure" >&2
    exit 3
    ;;
  malformed)
    printf 'this-is-not-json\n' > "$OUT"
    ;;
  missing-file)
    printf '{{"comments":[],"files":[]}}\n' > "$OUT"
    ;;
  unstable)
    printf '{{"comments":[],"files":%s,"coverage":%s}}\n' "$FILES_JSON" "$COV_JSON" > "$OUT"
    printf 'x' >> "$0"
    ;;
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

fn install_fake_fixer(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("fake-fixer");
    write_exe(
        &path,
        r#"#!/bin/sh
set -e
PROMPT=""
FINDINGS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --prompt-file) PROMPT="$2"; shift 2 ;;
    --findings-file) FINDINGS="$2"; shift 2 ;;
    --session-id) shift 2 ;;
    *) shift ;;
  esac
done
if [ -z "$PROMPT" ] || [ ! -f "$PROMPT" ]; then
  echo "prompt file missing" >&2
  exit 1
fi
if [ -z "$FINDINGS" ] || [ ! -f "$FINDINGS" ]; then
  echo "findings file missing" >&2
  exit 1
fi
TARGET=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[0]["path"] if d else "README")' "$FINDINGS" 2>/dev/null || echo README)
if [ ! -f "$TARGET" ]; then TARGET=README; fi
printf 'fixed\n' >> "$TARGET"
git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch add -A >/dev/null
git -c core.hooksPath=/dev/null -c user.email=porch@example.com -c user.name=Porch commit --no-verify -m "fix: address review findings" >/dev/null
printf '{"summary":"address review findings","session_id":"sess-1"}\n'
"#,
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
    origin: PathBuf,
    install: PathBuf,
    porch_bin: PathBuf,
    path: String,
    fake_gh: PathBuf,
    fake_fixer: PathBuf,
    fake_agent: Option<PathBuf>,
}

fn seed_origin_and_work(root: &Path) -> (PathBuf, PathBuf) {
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
    (origin, work)
}

fn setup_engine(engine: &str, floor_mode: &str) -> Harness {
    setup_with(engine, floor_mode, "10")
}

fn setup_with(engine: &str, floor_mode: &str, review_timeout: &str) -> Harness {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let bin = root.join("bin");
    let install = root.join("install");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();

    let porch_bin = copy_porch_launch(&install);
    if floor_mode == "missing" {
        install_fake_floor(&bin, "clean");
    } else {
        install_fake_floor(&install, floor_mode);
    }
    let fake_gh = install_noop_gh(&bin);
    let fake_fixer = install_fake_fixer(&bin);
    let fake_agent = if engine == "agent" {
        Some(install_fake_claude(&bin))
    } else {
        None
    };
    let path = format!("{}:{}", install.display(), path_with(&bin));
    let (origin, work) = seed_origin_and_work(&root);

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
    let timeout: &std::ffi::OsStr = review_timeout.as_ref();
    let mut extra: Vec<(&str, &std::ffi::OsStr)> = vec![
        (GH_BIN_ENV, fake_gh.as_os_str()),
        (FIXER_BIN_ENV, fake_fixer.as_os_str()),
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
        origin,
        install,
        porch_bin,
        path,
        fake_gh,
        fake_fixer,
        fake_agent,
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

fn origin_has_branch(origin: &Path, branch: &str) -> bool {
    let out = StdCommand::new("git")
        .args([
            "--git-dir",
            origin.to_str().unwrap(),
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .unwrap();
    out.success()
}

fn assert_failed_closed(db: &Db, run: &porch_gate::RunRow) {
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    assert!(
        steps.iter().all(|s| s.status != "parked"),
        "unsatisfied floor must not park: {steps:?}"
    );
    assert!(run.review_approved_head_sha.is_none(), "run={run:?}");
}

fn assert_incomplete_naming_floor(db: &Db, run_id: &str) -> porch_gate::rounds::RoundRecord {
    let rounds = rounds::rounds_for_run(db, run_id).unwrap();
    assert_eq!(rounds.len(), 1, "expected one round, got {rounds:?}");
    let round = rounds[0].clone();
    assert_eq!(round.execution, ExecutionState::Finished);
    assert_eq!(round.assurance_completion, AssuranceCompletion::Incomplete);
    let reason = round.completion_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("floor"),
        "completion reason must name the floor, got {reason:?}"
    );
    round
}

fn set_home_engine(home: &Path, engine: &str) {
    let path = home.join("config.yaml");
    let body = fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    let mut replaced = false;
    for line in body.lines() {
        if !replaced && line.trim_start().starts_with("engine:") {
            let indent_len = line.len() - line.trim_start().len();
            out.push_str(&line[..indent_len]);
            out.push_str("engine: ");
            out.push_str(engine);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(replaced, "config.yaml missing engine:\n{body}");
    fs::write(&path, out).unwrap();
}

fn respond_fix(
    h: &Harness,
    run_id: &str,
    extra: &[(&str, &std::ffi::OsStr)],
) -> std::process::Output {
    let mut cmd = StdCommand::new(&h.porch_bin);
    cmd.current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .env(FIXER_BIN_ENV, &h.fake_fixer)
        .env(GH_BIN_ENV, &h.fake_gh)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["agent", "respond", "fix", "--run-id", run_id]);
    if let Some(agent) = &h.fake_agent {
        cmd.env(REVIEW_AGENT_BIN_ENV, agent);
    } else {
        cmd.env_remove(REVIEW_AGENT_BIN_ENV);
    }
    for (key, value) in extra {
        cmd.env(*key, *value);
    }
    cmd.output().unwrap()
}

fn park_with_blocking_floor(h: &Harness, branch: &str) -> (Db, porch_gate::RunRow) {
    push_branch(&h.work, &h.home, branch);
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_eq!(run.status, "parked", "err={:?}", run.error);
    (db, run)
}

fn agent_status_json(h: &Harness, run_id: &str) -> serde_json::Value {
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .args(["agent", "status", "--run-id", run_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "status json: {e}; exit={:?} stdout={stdout} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn assert_shape_mismatch(
    db: &Db,
    run: &porch_gate::RunRow,
    pinned_shape: &str,
    attempted_shape: &str,
) {
    let run = db.run_by_id(&run.id).unwrap().expect("run");
    assert_eq!(run.status, "failed", "err={:?}", run.error);
    let steps = db.step_results_for_run(&run.id).unwrap();
    let review = steps
        .iter()
        .rev()
        .find(|s| s.step == "review")
        .expect("review step");
    assert_eq!(review.status, "failed", "steps={steps:?}");
    let payload: serde_json::Value =
        serde_json::from_str(review.error.as_deref().expect("mismatch payload")).unwrap();
    assert_eq!(payload["kind"], "assurance_shape_mismatch");
    assert_eq!(payload["pinned_shape"], pinned_shape);
    assert_eq!(payload["attempted_shape"], attempted_shape);
    let pinned = payload["pinned_digest"].as_str().expect("pinned_digest");
    let attempted = payload["attempted_digest"]
        .as_str()
        .expect("attempted_digest");
    assert_ne!(pinned, attempted, "payload={payload}");
    assert_eq!(run.error.as_deref(), review.error.as_deref());
    let rounds = rounds::rounds_for_run(db, &run.id).unwrap();
    assert_eq!(
        rounds.len(),
        1,
        "a shape mismatch must not open a later round: {rounds:?}"
    );
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

#[test]
fn unresolvable_floor_finalizes_incomplete_and_fails_closed() {
    let h = setup_quality("missing");
    let branch = "feat-floor-unresolved";
    push_branch(&h.work, &h.home, branch);

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(20),
    );
    assert_failed_closed(&db, &run);
    let round = assert_incomplete_naming_floor(&db, &run.id);
    let reqs = rounds::requirements_for_round(&db, &round.id).unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "expected the floor requirement, got {reqs:?}"
    );
    assert_eq!(reqs[0].role, Role::Floor);
    assert_eq!(reqs[0].resolution, Resolution::Unresolved);
    assert!(reqs[0].producer_invocation_id.is_none());
    assert!(reqs[0].expected_equivalence_digest.is_none());
    assert!(
        reqs[0]
            .reason
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty()),
        "unresolved row must carry a reason, got {:?}",
        reqs[0].reason
    );

    let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
    assert!(
        !order.lines().any(|l| l == "judgment-start"),
        "unresolved floor must not spawn judgment, order={order}"
    );

    kill_daemon(&h.home);
}

#[test]
fn floor_operational_faults_finalize_incomplete_and_skip_judgment() {
    let cases = [
        ("hang", "feat-floor-timeout", "2"),
        ("exit-fail", "feat-floor-exit", "10"),
        ("malformed", "feat-floor-malformed", "10"),
        ("missing-file", "feat-floor-coverage", "10"),
        ("unstable", "feat-floor-artifact", "10"),
    ];
    for (mode, branch, timeout) in cases {
        let h = setup_with("agent", mode, timeout);
        push_branch(&h.work, &h.home, branch);

        let db = Db::open(&h.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&h.work);
        let run = wait_status(
            &db,
            &repo_id,
            &["parked", "failed", "completed"],
            Duration::from_secs(25),
        );
        assert_failed_closed(&db, &run);
        assert_incomplete_naming_floor(&db, &run.id);

        let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
        assert!(
            !order.lines().any(|l| l == "judgment-start"),
            "{mode}: judgment must not spawn after floor fault, order={order}"
        );

        kill_daemon(&h.home);
    }
}

#[test]
fn unsatisfied_floor_keeps_records_and_does_not_forward() {
    let h = setup_quality("missing");
    let branch = "feat-floor-no-forward";
    push_branch(&h.work, &h.home, branch);

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(20),
    );
    assert_failed_closed(&db, &run);
    assert_incomplete_naming_floor(&db, &run.id);

    let reread = db
        .run_by_id(&run.id)
        .unwrap()
        .expect("failed run must remain");
    assert_eq!(reread.status, "failed");
    assert_eq!(reread.id, run.id);
    assert!(
        !origin_has_branch(&h.origin, branch),
        "unsatisfied floor must not forward {branch}"
    );
    assert!(
        !h.home.join("gh-pr-state").exists(),
        "unsatisfied floor must not open a pull request"
    );

    kill_daemon(&h.home);
}

#[test]
fn rerun_after_unsatisfied_floor_starts_independent_run() {
    let h = setup_quality("missing");
    let branch = "feat-floor-rerun";
    push_branch(&h.work, &h.home, branch);

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let first = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(20),
    );
    assert_failed_closed(&db, &first);
    assert_incomplete_naming_floor(&db, &first.id);
    let first_id = first.id.clone();

    install_fake_floor(&h.install, "clean");
    let out = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .args(["rerun", "--run-id", &first_id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rerun started:"),
        "stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let start = Instant::now();
    let second = loop {
        let runs = db.runs_for_repo(&repo_id).unwrap();
        if let Some(run) = runs.iter().rev().find(|r| r.id != first_id) {
            if matches!(
                run.status.as_str(),
                "completed" | "failed" | "parked" | "cancelled"
            ) {
                break run.clone();
            }
        }
        assert!(
            start.elapsed() <= Duration::from_secs(30),
            "rerun did not finish: {:?}",
            db.runs_for_repo(&repo_id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(second.id, first_id);
    assert_ne!(second.status, "failed", "err={:?}", second.error);

    let first_again = db.run_by_id(&first_id).unwrap().expect("prior run");
    assert_eq!(first_again.status, "failed");
    assert!(first_again.review_approved_head_sha.is_none());
    assert_incomplete_naming_floor(&db, &first_id);

    let reqs = recorded_requirements(&db, &second.id);
    assert_eq!(
        reqs.len(),
        1,
        "expected floor alone on the new run, got {reqs:?}"
    );
    assert_eq!(reqs[0].role, Role::Floor);
    assert_eq!(reqs[0].resolution, Resolution::Resolved);
    assert!(reqs[0].producer_invocation_id.is_some());
    assert!(
        second
            .review_approved_head_sha
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
        "new run must authorize independently: {second:?}"
    );

    kill_daemon(&h.home);
}

#[test]
fn matching_later_round_proceeds_and_a_shape_change_fails_closed() {
    {
        let h = setup_agent("blocking");
        let (db, run) = park_with_blocking_floor(&h, "feat-pin-match");
        let pinned = run_required_set_digest(&db, &run.id)
            .unwrap()
            .expect("first round pins the run");

        let out = respond_fix(&h, &run.id, &[]);
        assert!(
            out.status.success(),
            "matching later review failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let after = db.run_by_id(&run.id).unwrap().expect("run");
        let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
        assert_eq!(
            rounds.len(),
            2,
            "a matching later review must open another round: status={} err={:?} rounds={rounds:?}",
            after.status,
            after.error
        );
        assert_eq!(
            run_required_set_digest(&db, &run.id).unwrap().as_deref(),
            Some(pinned.as_str())
        );
        kill_daemon(&h.home);
    }

    {
        let h = setup_agent("blocking");
        let (db, run) = park_with_blocking_floor(&h, "feat-pin-weaken");
        set_home_engine(&h.home, "quality");
        let _ = respond_fix(&h, &run.id, &[]);
        assert_shape_mismatch(&db, &run, "floor+judgment", "floor-only");
        kill_daemon(&h.home);
    }

    {
        let h = setup_quality("blocking");
        let (db, run) = park_with_blocking_floor(&h, "feat-pin-strengthen");
        set_home_engine(&h.home, "agent");
        let agent = install_fake_claude(&h.install);
        let extra = [(REVIEW_AGENT_BIN_ENV, agent.as_os_str())];
        let _ = respond_fix(&h, &run.id, &extra);
        assert_shape_mismatch(&db, &run, "floor-only", "floor+judgment");
        kill_daemon(&h.home);
    }
}

#[test]
fn changed_producer_artifact_is_a_mismatch_when_configuration_is_unchanged() {
    let h = setup_agent("blocking");
    let (db, run) = park_with_blocking_floor(&h, "feat-pin-artifact");
    let floor = h
        .install
        .join(format!("porch-quality{}", std::env::consts::EXE_SUFFIX));
    let mut body = fs::read(&floor).unwrap();
    body.push(b'x');
    fs::write(&floor, body).unwrap();
    chmod_755(&floor);
    let _ = respond_fix(&h, &run.id, &[]);
    assert_shape_mismatch(&db, &run, "floor+judgment", "floor+judgment");
    kill_daemon(&h.home);
}

#[test]
fn missing_judgment_is_incomplete_not_a_floor_only_round() {
    let h = setup_agent("clean");
    let agent = h.fake_agent.as_ref().expect("agent bin");
    fs::remove_file(agent).unwrap();
    push_branch(&h.work, &h.home, "feat-missing-judgment");

    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(20),
    );
    assert_failed_closed(&db, &run);
    let rounds = rounds::rounds_for_run(&db, &run.id).unwrap();
    assert_eq!(rounds.len(), 1, "expected one round, got {rounds:?}");
    let round = &rounds[0];
    assert_eq!(round.execution, ExecutionState::Finished);
    assert_eq!(round.assurance_completion, AssuranceCompletion::Incomplete);
    let reqs = rounds::requirements_for_round(&db, &round.id).unwrap();
    assert_eq!(
        reqs.len(),
        2,
        "missing judgment must not collapse to floor-only, got {reqs:?}"
    );
    assert_eq!(reqs[0].role, Role::Floor);
    assert_eq!(reqs[0].resolution, Resolution::Resolved);
    assert_eq!(reqs[1].role, Role::Judgment);
    assert_eq!(reqs[1].resolution, Resolution::Unresolved);
    assert!(reqs[1].producer_invocation_id.is_none());
    assert!(
        reqs[1]
            .reason
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty()),
        "unresolved judgment must carry a reason, got {:?}",
        reqs[1].reason
    );
    let order = fs::read_to_string(h.home.join("producer-order")).unwrap_or_default();
    assert!(
        !order.lines().any(|l| l == "judgment-start"),
        "missing judgment must not spawn, order={order}"
    );
    kill_daemon(&h.home);
}

#[test]
fn a_pre_floor_client_cannot_create_or_approve_on_an_upgraded_state_root() {
    let h = setup_quality("clean");
    let path = h.home.join("state.sqlite");
    let repo_id = repo_id_for(&h.work);
    let db = Db::open(&path).unwrap();
    let run = db
        .insert_run(&repo_id, "feat-fence", "deadbeef", None, None)
        .expect("a current binary can still create a run");
    db.set_review_approved_head_sha(&run.id, Some("approved-sha"))
        .expect("a current binary can still write an approval");
    drop(db);

    let conn = rusqlite::Connection::open(&path).unwrap();
    let insert_err = conn
        .execute(
            "INSERT INTO runs (id, repo_id, branch, sha, status, created_at)
             VALUES ('old-client-run', ?1, 'feat-old', 'deadbeef', 'pending', '9')",
            rusqlite::params![repo_id],
        )
        .expect_err("a pre-floor client must not create a run on an upgraded state root");
    let insert_msg = insert_err.to_string();
    assert!(
        insert_msg.contains("porch_writer_protocol"),
        "absence must fail closed, got {insert_msg}"
    );

    let approve_err = conn
        .execute(
            "UPDATE runs SET review_approved_head_sha = 'sneak' WHERE id = ?1",
            [&run.id],
        )
        .expect_err("a pre-floor client must not approve a run on an upgraded state root");
    let approve_msg = approve_err.to_string();
    assert!(
        approve_msg.contains("porch_writer_protocol"),
        "absence must fail closed, got {approve_msg}"
    );

    kill_daemon(&h.home);
}

#[test]
fn run_status_states_the_recorded_assurance_shape() {
    {
        let h = setup_quality("clean");
        push_branch(&h.work, &h.home, "feat-shape-floor-only");
        let db = Db::open(&h.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&h.work);
        let run = wait_status(
            &db,
            &repo_id,
            &["parked", "failed", "completed"],
            Duration::from_secs(30),
        );
        assert_ne!(run.status, "failed", "err={:?}", run.error);
        let snap = porch_gate::get_run(&h.home, &run.id).unwrap();
        let rec = serde_json::to_value(&snap.assurance_record).unwrap();
        assert_eq!(rec["kind"], "round", "{rec}");
        assert_eq!(rec["assurance_shape"], "floor-only", "{rec}");
        let v = agent_status_json(&h, &run.id);
        assert_eq!(v["assurance_record"]["kind"], "round");
        assert_eq!(v["assurance_record"]["assurance_shape"], "floor-only");
        kill_daemon(&h.home);
    }

    {
        let h = setup_agent("clean");
        push_branch(&h.work, &h.home, "feat-shape-floor-judgment");
        let db = Db::open(&h.home.join("state.sqlite")).unwrap();
        let repo_id = repo_id_for(&h.work);
        let run = wait_status(
            &db,
            &repo_id,
            &["parked", "failed", "completed"],
            Duration::from_secs(30),
        );
        assert_ne!(run.status, "failed", "err={:?}", run.error);
        let snap = porch_gate::get_run(&h.home, &run.id).unwrap();
        let rec = serde_json::to_value(&snap.assurance_record).unwrap();
        assert_eq!(rec["kind"], "round", "{rec}");
        assert_eq!(rec["assurance_shape"], "floor+judgment", "{rec}");
        let v = agent_status_json(&h, &run.id);
        assert_eq!(v["assurance_record"]["kind"], "round");
        assert_eq!(v["assurance_record"]["assurance_shape"], "floor+judgment");
        kill_daemon(&h.home);
    }
}

#[test]
fn delivered_attestation_states_the_assurance_shape() {
    let h = setup_quality("clean");
    push_branch(&h.work, &h.home, "feat-attest-shape");
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(30),
    );
    assert_ne!(run.status, "failed", "err={:?}", run.error);
    assert!(run.pr_url.is_some(), "run must open a PR: {run:?}");
    let body = fs::read_to_string(h.home.join("gh-pr-body.txt")).expect("gh body");
    assert!(body.contains("<!-- porch-attestation"), "{body}");
    let start = body
        .find("<!-- porch-attestation")
        .and_then(|i| body[i..].find('{').map(|j| i + j))
        .expect("attestation json");
    let end = body[start..]
        .find("-->")
        .map(|i| start + i)
        .expect("attestation close");
    let json: serde_json::Value = serde_json::from_str(body[start..end].trim()).unwrap();
    assert_eq!(json["assurance_shape"], "floor-only", "{json}");
    let head = json["head_sha"].as_str().unwrap_or("");
    assert!(!head.is_empty(), "attestation must keep head_sha: {json}");
    assert_eq!(
        Some(head),
        run.head_sha.as_deref(),
        "attestation head_sha must bind the delivered tip: {json}"
    );
    kill_daemon(&h.home);
}

#[test]
fn unsatisfied_floor_exposes_no_response_and_rerun_diagnostics() {
    let h = setup_quality("missing");
    push_branch(&h.work, &h.home, "feat-floor-diag");
    let db = Db::open(&h.home.join("state.sqlite")).unwrap();
    let repo_id = repo_id_for(&h.work);
    let run = wait_status(
        &db,
        &repo_id,
        &["parked", "failed", "completed"],
        Duration::from_secs(20),
    );
    assert_failed_closed(&db, &run);

    let v = agent_status_json(&h, &run.id);
    assert_eq!(v["status"], "failed");
    let actions = v["allowed_actions"].as_array();
    assert!(
        actions.is_none_or(Vec::is_empty),
        "failed floor-blocked run must expose no response verbs: {v}"
    );
    if let Some(actions) = actions {
        for verb in ["approve", "skip", "fix", "respond", "abort"] {
            assert!(
                !actions.iter().any(|x| x.as_str() == Some(verb)),
                "failed floor-blocked run exposed {verb}: {v}"
            );
        }
    }
    let blob = v.to_string();
    let rerun = format!("porch rerun --run-id {}", run.id);
    assert!(
        blob.contains(&rerun),
        "diagnostics must carry copyable {rerun}: {v}"
    );
    let lower = blob.to_ascii_lowercase();
    assert!(
        lower.contains("restart") && lower.contains("daemon"),
        "resolution failure must advise restarting the daemon: {v}"
    );

    let respond = Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .args(["agent", "respond", "approve", "--run-id", &run.id])
        .output()
        .unwrap();
    let respond_v: serde_json::Value = serde_json::from_slice(&respond.stdout)
        .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&respond.stdout) }));
    assert_ne!(
        respond_v.get("status").and_then(|s| s.as_str()),
        Some("parked")
    );
    assert_ne!(
        respond_v.get("status").and_then(|s| s.as_str()),
        Some("completed")
    );
    let after = db.run_by_id(&run.id).unwrap().expect("run");
    assert_eq!(after.status, "failed");
    assert!(after.review_approved_head_sha.is_none());

    kill_daemon(&h.home);
}

#[test]
fn pin_mismatch_reports_both_shapes_and_setup_is_unchanged() {
    let h = setup_agent("blocking");
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--verify"])
        .assert()
        .success();

    let (db, run) = park_with_blocking_floor(&h, "feat-pin-report");
    set_home_engine(&h.home, "quality");
    let _ = respond_fix(&h, &run.id, &[]);
    assert_shape_mismatch(&db, &run, "floor+judgment", "floor-only");

    let v = agent_status_json(&h, &run.id);
    let err = v["error"].as_str().unwrap_or("");
    assert!(
        err.contains("floor+judgment") && err.contains("floor-only"),
        "mismatch must name both shapes: {v}"
    );
    assert!(
        err.contains("pinned") && err.contains("attempted"),
        "mismatch must label pinned vs attempted: {v}"
    );

    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--yes", "--engine", "quality"])
        .assert()
        .success();
    Command::cargo_bin("porch")
        .unwrap()
        .current_dir(&h.work)
        .env("PORCH_HOME", &h.home)
        .env("PATH", &h.path)
        .env_remove("PORCH_REVIEW_BIN")
        .args(["setup", "--verify"])
        .assert()
        .success();

    kill_daemon(&h.home);
}
