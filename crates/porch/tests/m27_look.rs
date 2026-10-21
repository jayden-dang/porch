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

fn floor_only_plan(run_id: &str) -> porch_gate::rounds::OpenRoundPlan {
    use porch_gate::rounds::{
        OpenRoundPlan, ProducerInvocation, RequirementSpec, Resolution, Role,
    };
    OpenRoundPlan {
        run_id: run_id.to_string(),
        producers: vec![ProducerInvocation {
            descriptor_json: r#"{"slot":0}"#.into(),
            descriptor_equivalence_digest: "equiv-floor".into(),
        }],
        requirements: vec![RequirementSpec {
            slot: 0,
            role: Role::Floor,
            resolution: Resolution::Resolved,
            expected_equivalence_digest: Some("equiv-floor".into()),
            reason: None,
        }],
    }
}

fn floor_only_bindings(inventory: &[u8]) -> porch_gate::rounds::RoundBindings {
    use porch_gate::rounds::{
        self, ContextApplication, ContextApplicationState, ContextSource, RoundBindings,
        capture_context_element, context_applicability_digest, sha256_hex,
    };
    let intent = capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    let intent_digest = context_applicability_digest("intent", "present", inventory);
    RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: sha256_hex(inventory),
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: rounds::PROTOCOL_SCHEMA_VERSION,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent],
        context_applications: vec![ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: ContextApplicationState::Applied,
            effective_digest: Some(intent_digest),
        }],
    }
}

fn finish_floor_only_round(
    db: &porch_gate::Db,
    round_id: &porch_gate::rounds::RoundId,
    run_id: &str,
) {
    use porch_gate::rounds::{
        self, AssuranceCompletion, CoverageState, ExecutionState, FinalizeOutcome,
        FinalizeProposal, FindingInstanceProposal, RoundCoverageProposal,
    };
    let producer = rounds::producers_for_round(db, round_id).unwrap()[0]
        .id
        .clone();
    let (rev, _) = rounds::read_history(db, run_id).unwrap();
    assert_eq!(
        rounds::finalize_round(
            db,
            round_id,
            &FinalizeProposal {
                execution: ExecutionState::Finished,
                assurance_completion: AssuranceCompletion::Complete,
                completion_reason: None,
                coverage: vec![RoundCoverageProposal {
                    producer_invocation_id: producer.clone(),
                    path: "a.rs".into(),
                    state: CoverageState::Completed,
                    reason: None,
                    authority: None,
                    completion_evidence: Some("reviewed".into()),
                }],
                producer_durations: Vec::new(),
                review_duration_ms: None,
                instances: vec![FindingInstanceProposal {
                    producer_invocation_id: producer,
                    fingerprint: "fp".into(),
                    fingerprint_version: 1,
                    candidate_key: "ck".into(),
                    criterion_id: "rust/unwrap-in-lib".into(),
                    evidence: "e".into(),
                    consequence: "c".into(),
                    action: "must-fix".into(),
                    severity: "error".into(),
                    provenance_json: "{}".into(),
                    confidence_value: None,
                    confidence_kind: None,
                    path: "a.rs".into(),
                    anchor_kind: "symbol".into(),
                    anchor_value: "foo".into(),
                }],
            },
            rev,
        )
        .unwrap(),
        FinalizeOutcome::Finalized
    );
}

fn seed_floor_only_round(s: &Setup) -> String {
    use porch_gate::rounds;
    use porch_gate::{Db, db_path};

    let db = Db::open(&db_path(&s.home)).unwrap();
    let run = db
        .insert_run(&repo_id(s), "feat-voice", "aaa", Some("inv"), Some("flag"))
        .unwrap();
    let round_id =
        rounds::open_round(&db, &floor_only_plan(&run.id), &floor_only_bindings(b"inv")).unwrap();
    finish_floor_only_round(&db, &round_id, &run.id);
    let (rec, _) = porch_gate::resolve_run_assurance(&db, &run).unwrap();
    assert_eq!(rec.assurance_shape(), Some("floor-only"));
    run.id
}

#[test]
fn status_names_voice_phrase_when_assurance_record_exists() {
    let s = setup();
    assert!(daemon_pid(&s.home).is_none());
    let _run_id = seed_floor_only_round(&s);
    assert!(daemon_pid(&s.home).is_none());

    let phrase = porch_review::voice_phrase(true);
    let out = porch(&s).arg("status").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(phrase),
        "status must name voice phrase: {stdout}"
    );
    assert!(
        daemon_pid(&s.home).is_none(),
        "status must not start a daemon"
    );

    let runs = porch(&s).arg("runs").output().unwrap();
    assert!(runs.status.success(), "{runs:?}");
    let runs_out = String::from_utf8_lossy(&runs.stdout);
    assert!(
        runs_out.contains(phrase),
        "runs must name voice phrase: {runs_out}"
    );
    assert!(
        daemon_pid(&s.home).is_none(),
        "runs must not start a daemon"
    );
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
