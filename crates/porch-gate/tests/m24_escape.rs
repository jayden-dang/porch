//! M24: escape without the daemon — custody of porch-authored commits, and a
//! purge that completes.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use porch_gate::rounds::{
    self, AssuranceCompletion, ContextApplication, CoverageState, ExecutionState, FinalizeOutcome,
    FinalizeProposal, FindingInstanceProposal, OpenRoundPlan, ProducerInvocation, RoundBindings,
    RoundCoverageProposal, capture_context_element, context_applicability_digest, sha256_hex,
};
use porch_gate::{Db, RunExecutor, RunRow, db_path, run_daemon, wait_for_health};
use porch_git::GitDir;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use tempfile::TempDir;

fn fixture_db(home: &Path) -> Db {
    Db::open(&db_path(home)).unwrap()
}

fn register_current_writer_protocol(conn: &Connection) {
    conn.create_scalar_function(
        "porch_writer_protocol",
        0,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(rounds::PROTOCOL_SCHEMA_VERSION),
    )
    .unwrap();
}

/// Runs nothing. The superseding sweep happens before execution is dispatched, so
/// the test needs a live daemon rather than a live pipeline.
struct InertExecutor;

impl RunExecutor for InertExecutor {
    fn execute(&self, _home: &Path, _run_id: &str, _cancel: &AtomicBool) {}

    fn recover_stale(&self, _home: &Path) -> std::result::Result<(), String> {
        Ok(())
    }
}

fn seed_run(db: &Db, home: &Path) -> String {
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    db.insert_run("repo1", "feat", "deadbeef", Some("intent"), Some("flag"))
        .unwrap()
        .id
}

fn sample_plan(run_id: &str) -> OpenRoundPlan {
    OpenRoundPlan {
        run_id: run_id.to_string(),
        producers: vec![ProducerInvocation {
            descriptor_json: serde_json::json!({
                "adapter_kind": "porch_json_cli",
                "observed_version_identity": {"unavailable": "not_on_path"},
                "reported_version": {"unavailable": "not_reported"},
            })
            .to_string(),
            descriptor_equivalence_digest: "equiv-digest-1".into(),
        }],
        requirements: vec![],
    }
}

fn sample_bindings(inventory: &[u8]) -> RoundBindings {
    let digest = sha256_hex(inventory);
    let intent = capture_context_element(
        "intent",
        rounds::ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest,
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: rounds::PROTOCOL_SCHEMA_VERSION,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent],
        context_applications: vec![ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(context_applicability_digest("intent", "present", inventory)),
        }],
    }
}

fn sample_complete_proposal(producer_invocation_id: &str) -> FinalizeProposal {
    FinalizeProposal {
        execution: ExecutionState::Finished,
        assurance_completion: AssuranceCompletion::Complete,
        completion_reason: None,
        coverage: vec![RoundCoverageProposal {
            producer_invocation_id: producer_invocation_id.into(),
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        }],
        producer_durations: Vec::new(),
        review_duration_ms: None,
        instances: vec![FindingInstanceProposal {
            producer_invocation_id: producer_invocation_id.into(),
            fingerprint: "fp-one".into(),
            fingerprint_version: 1,
            candidate_key: "ck-one".into(),
            criterion_id: "rust/unwrap-in-lib".into(),
            evidence: "unwrap here".into(),
            consequence: "panic risk".into(),
            action: "must-fix".into(),
            severity: "error".into(),
            provenance_json: r#"{"producer_key":"rust/unwrap-in-lib"}"#.into(),
            confidence_value: None,
            confidence_kind: None,
            path: "a.rs".into(),
            anchor_kind: "symbol".into(),
            anchor_value: "foo".into(),
        }],
    }
}

fn row_count(home: &Path, table: &str, run_id: &str) -> i64 {
    let column = if table == "runs" { "id" } else { "run_id" };
    let conn = Connection::open(db_path(home)).unwrap();
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
        [run_id],
        |row| row.get(0),
    )
    .unwrap()
}

/// A repo carrying every kind of row that references its runs purges cleanly.
///
/// `review_rounds` is the only table that declares `ON DELETE CASCADE` on
/// `runs(id)`. The five added since M18 declare a plain reference, and
/// `foreign_keys` is `ON`, so `DELETE FROM runs` alone violates an immediate
/// foreign key on any repo that has run a gate pass — which is every real repo.
/// This is the regression test for a defect shipped in 0.2.2 (`ESCAPE-3.1`,
/// `ESCAPE-3.2`).
#[test]
fn delete_repo_removes_every_row_that_references_its_runs() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    // review_rounds, round_producers, round_coverage, finding_instances, content_blobs
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(b"inv\n")).unwrap();
    let producer = rounds::producers_for_round(&db, &round_id).unwrap()[0]
        .id
        .clone();
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    assert_eq!(
        rounds::finalize_round(&db, &round_id, &sample_complete_proposal(&producer), rev).unwrap(),
        FinalizeOutcome::Finalized
    );
    db.set_run_shas(&run_id, Some("to"), None).unwrap();
    let instance_id = rounds::instances_for_round(&db, &round_id).unwrap()[0]
        .id
        .clone();

    // authority_events + authority_event_members. The member references
    // finding_instances, which `runs` reaches only by cascade through
    // review_rounds — a table the delete never names.
    rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::ReviewApproved,
            expected_round_id: Some(round_id.clone()),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(instance_id, rounds::MemberRole::Context)],
        },
    )
    .unwrap();

    // phase_attempts + phase_events
    let attempt = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects::none(),
    )
    .unwrap();

    // forward_records
    rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Absent,
        },
    )
    .unwrap();

    // forward_reconciliations
    rounds::phase::reconcile_interrupted(&db).unwrap();

    for table in [
        "phase_attempts",
        "phase_events",
        "authority_events",
        "forward_records",
        "forward_reconciliations",
    ] {
        assert!(
            row_count(home, table, &run_id) > 0,
            "{table} must be populated for this test to mean anything"
        );
    }

    db.delete_repo("repo1").expect("purge must complete");

    for table in [
        "runs",
        "step_results",
        "review_rounds",
        "authority_events",
        "phase_attempts",
        "phase_events",
        "forward_records",
        "forward_reconciliations",
    ] {
        assert_eq!(
            row_count(home, table, &run_id),
            0,
            "{table} still holds rows for the purged repo"
        );
    }
    assert!(db.repo_by_id("repo1").unwrap().is_none());
    let orphan_members: i64 = {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.query_row("SELECT COUNT(*) FROM authority_event_members", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(orphan_members, 0, "authority_event_members left orphaned");
}

/// A git invocation that cannot see the ambient user or system configuration.
///
/// Without this the fixture inherits whatever the host has set. Commit signing is
/// the expensive one — a signing helper turns each `commit` and `rebase` into a
/// multi-second stall or a hang — but `init.defaultBranch`, `rebase.autoStash`, and
/// `merge.ff` would all change what this fixture proves.
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let ok = git_cmd(dir).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = git_cmd(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare with one worktree whose HEAD does **not** descend from the pushed SHA.
///
/// That is what a rebase produces: the pushed commit is replayed onto a moved
/// base, so the original SHA is not an ancestor of anything porch writes after.
struct Rebased {
    _tmp: TempDir,
    bare: GitDir,
    wt: std::path::PathBuf,
    pushed_sha: String,
    head_sha: String,
}

fn rebased_worktree() -> Rebased {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let bare_path = root.join("bare.git");
    git(&root, &["init", "--bare", "-b", "main", "bare.git"]);

    let seed = root.join("seed");
    git(&root, &["clone", bare_path.to_str().unwrap(), "seed"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    std::fs::write(seed.join("base"), "base\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "base"]);
    git(&seed, &["push", "origin", "main"]);

    // The operator's commit, as pushed to the gate — on its own branch, so that
    // main can move without carrying it.
    git(&seed, &["checkout", "-b", "feat"]);
    std::fs::write(seed.join("feature"), "one\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "feature"]);
    git(&seed, &["push", "origin", "feat"]);
    let pushed_sha = git_out(&seed, &["rev-parse", "HEAD"]);

    // main moves underneath it.
    git(&seed, &["checkout", "main"]);
    std::fs::write(seed.join("base"), "moved\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "move main"]);
    git(&seed, &["push", "origin", "main"]);

    // The pipeline worktree: checked out at the pushed SHA, rebased onto the
    // moved base, then given a porch-authored commit on top.
    let wt = root.join("wt");
    let bare = GitDir::new(&bare_path).unwrap();
    porch_git::worktree_add_detach(&bare, &wt, &pushed_sha).unwrap();
    git(&wt, &["config", "user.email", "porch@example.com"]);
    git(&wt, &["config", "user.name", "Porch"]);
    git(&wt, &["rebase", "main"]);
    std::fs::write(wt.join("formatted"), "tidy\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "porch: apply format"]);
    let head_sha = git_out(&wt, &["rev-parse", "HEAD"]);

    assert_ne!(head_sha, pushed_sha);
    assert!(
        !git_cmd(&wt)
            .args(["merge-base", "--is-ancestor", &pushed_sha, &head_sha])
            .status()
            .unwrap()
            .success(),
        "the pushed SHA must not be an ancestor, or this fixture proves nothing"
    );

    Rebased {
        _tmp: tmp,
        bare,
        wt,
        pushed_sha,
        head_sha,
    }
}

fn run_row_for(db: &Db, home: &Path, sha: &str) -> RunRow {
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    db.insert_run("repo1", "feat", sha, None, None).unwrap()
}

/// The pin fires for a rebased worktree, which the ancestry gate declined.
///
/// `runs.sha` is written once at admission and never rewritten, so requiring it to
/// be an ancestor of the worktree HEAD disarmed the pin in exactly the case it
/// exists for: the correction commit died with the worktree (`ESCAPE-1.1`,
/// `ESCAPE-1.2`).
#[test]
fn a_rebased_worktree_head_is_pinned_before_removal() {
    let fx = rebased_worktree();
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run = run_row_for(&db, home.path(), &fx.pushed_sha);

    porch_gate::finish_remove_worktree(&fx.bare, &run, &fx.wt);

    let pinned = porch_git::rev_parse(&fx.bare, &porch_gate::recovery_ref_name(&run.id))
        .expect("recovery ref must exist for a rebased worktree");
    assert_eq!(pinned, fx.head_sha);
    assert!(!fx.wt.exists(), "worktree removed once the pin succeeded");
}

/// A HEAD that is exactly the pushed SHA needs no ref: the commit is on the bare.
#[test]
fn an_unchanged_worktree_head_is_not_pinned() {
    let fx = rebased_worktree();
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run = run_row_for(&db, home.path(), &fx.head_sha);

    porch_gate::finish_remove_worktree(&fx.bare, &run, &fx.wt);

    assert!(
        porch_git::rev_parse(&fx.bare, &porch_gate::recovery_ref_name(&run.id)).is_err(),
        "no ref for a HEAD that is already the pushed SHA"
    );
    assert!(!fx.wt.exists());
}

/// A second push to the same branch does not destroy the parked run's commits.
///
/// A parked run has no inflight handle, so the superseding path swept its worktree
/// directly with `worktree remove --force` and never attempted a pin — no eject and
/// no operator intent to discard, yet the fixer commits went with it
/// (`ESCAPE-1.4`).
#[test]
fn superseding_a_parked_run_pins_its_head_before_sweeping() {
    let fx = rebased_worktree();
    let home = TempDir::new().unwrap();
    let home = home.path().to_path_buf();
    std::fs::create_dir_all(&home).unwrap();

    let db = fixture_db(&home);
    db.upsert_repo("repo1", &home, fx.bare.as_path(), "main")
        .unwrap();
    let parked = db
        .insert_run("repo1", "feat", &fx.pushed_sha, None, None)
        .unwrap();
    db.set_worktree_dir(&parked.id, &fx.wt).unwrap();
    {
        let conn = Connection::open(db_path(&home)).unwrap();
        register_current_writer_protocol(&conn);
        conn.execute(
            "UPDATE runs SET status = 'parked' WHERE id = ?1",
            [&parked.id],
        )
        .unwrap();
    }
    let superseding = db
        .insert_run("repo1", "feat", &fx.head_sha, None, None)
        .unwrap();
    drop(db);

    let home_t = home.clone();
    let _daemon = std::thread::spawn(move || {
        let exec: std::sync::Arc<dyn RunExecutor> = std::sync::Arc::new(InertExecutor);
        let _ = run_daemon(&home_t, &exec);
    });
    wait_for_health(&home, Duration::from_secs(10)).unwrap();
    porch_gate::rpc_start_run(&home, &superseding.id).unwrap();

    let ref_name = porch_gate::recovery_ref_name(&parked.id);
    let start = Instant::now();
    let pinned = loop {
        if let Ok(sha) = porch_git::rev_parse(&fx.bare, &ref_name) {
            break sha;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "superseding a parked run must pin its worktree HEAD before sweeping"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(pinned, fx.head_sha);
    // The daemon runs in this process, so `daemon.pid` holds the test runner's own
    // pid: signalling it would kill the harness. The thread is left to die with the
    // process, as the other in-process daemon tests do.
}

/// A failed pin keeps the worktree, so the commits stay reachable from its HEAD.
#[test]
fn a_failed_pin_keeps_the_worktree() {
    let fx = rebased_worktree();
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run = run_row_for(&db, home.path(), &fx.pushed_sha);

    // Make `update-ref refs/porch/recover/<run>` impossible: `refs/porch` is a file.
    let refs_porch = fx.bare.as_path().join("refs").join("porch");
    std::fs::create_dir_all(refs_porch.parent().unwrap()).unwrap();
    std::fs::write(&refs_porch, "not a directory\n").unwrap();

    porch_gate::finish_remove_worktree(&fx.bare, &run, &fx.wt);

    assert!(
        fx.wt.exists(),
        "fail closed: an unpinnable worktree must be kept"
    );
}
