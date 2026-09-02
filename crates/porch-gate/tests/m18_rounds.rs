//! M18: review round store — migration and durable open.

use std::path::{Path, PathBuf};
use std::process::Command;

use porch_gate::db_path;
use porch_gate::rounds::retention::{self, config_ref_name};
use porch_gate::rounds::{
    self, Applicability, AssuranceCompletion, ContextApplication, ContextElement, ContextSource,
    CoverageState, EquivalenceInput, ExecutionState, FinalizeOutcome, FinalizeProposal,
    FindingInstanceProposal, ObservedVersionForEquivalence, OpenRoundPlan, ProducerInvocation,
    RequirementRow, RequirementSpec, Resolution, Role, RoundBindings, RoundCoverageProposal,
    SNAPSHOT_CEILING_BYTES, STALE_REVISION_RETRIES, SnapshotState, SourceState, applicable_round,
    applicable_round_for_run, capture_context_element, context_applicability_digest,
    descriptor_equivalence_digest, required_set_digest, run_required_set_digest, sha256_hex,
};
use porch_gate::{Db, Error};
use porch_git::GitDir;
use rusqlite::Connection;
use tempfile::TempDir;

fn seed_legacy_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE repos (
            id TEXT PRIMARY KEY,
            worktree_path TEXT NOT NULL,
            bare_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            default_branch TEXT NOT NULL DEFAULT 'main'
        );
        CREATE TABLE runs (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            sha TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(repo_id) REFERENCES repos(id)
        );
        INSERT INTO repos (id, worktree_path, bare_path, created_at, default_branch)
        VALUES ('repo-legacy', '/tmp/wt', '/tmp/bare.git', '1', 'main');
        INSERT INTO runs (id, repo_id, branch, sha, status, created_at)
        VALUES ('run-legacy', 'repo-legacy', 'feat', 'abc', 'parked', '2');
        ",
    )
    .unwrap();
}

fn fixture_db(home: &Path) -> Db {
    let path = db_path(home);
    Db::open(&path).unwrap()
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
        ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest,
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: 2,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent.clone()],
        context_applications: vec![ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(context_applicability_digest("intent", "present", inventory)),
        }],
    }
}

#[test]
fn opening_legacy_database_adds_round_tables_and_keeps_existing_rows() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let path = db_path(home);
    std::fs::create_dir_all(home).unwrap();
    seed_legacy_db(&path);

    let db = Db::open(&path).unwrap();

    let run = db.run_by_id("run-legacy").unwrap().expect("legacy run");
    assert_eq!(run.status, "parked");
    assert_eq!(run.branch, "feat");

    db.set_run_status("run-legacy", "pending", None).unwrap();
    let active = db.active_runs(Some("repo-legacy"), None).unwrap();
    assert!(active.iter().any(|r| r.id == "run-legacy"));

    let parked = db.latest_parked_for_repo("repo-legacy").unwrap();
    assert!(parked.is_none());

    let conn = Connection::open(&path).unwrap();
    for table in [
        "review_rounds",
        "round_producers",
        "round_context_elements",
        "round_context_applications",
        "round_coverage",
        "finding_instances",
        "content_blobs",
        "round_required_producers",
        "round_producer_durations",
    ] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing table {table}");
    }
    let revision: i64 = conn
        .query_row(
            "SELECT review_history_revision FROM runs WHERE id='run-legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 0);
    let required: i64 = conn
        .query_row("SELECT COUNT(*) FROM round_required_producers", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        required, 0,
        "opening an existing database must not invent requirement rows"
    );
}

fn sqlite_paths_under(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.ends_with(".sqlite") || n.ends_with(".sqlite-wal") || n.ends_with(".sqlite-shm")
            }) {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn open_round_commits_before_returning_id_and_allocates_ordinals() {
    let root = TempDir::new().unwrap();
    let root = root.path();
    let home = root.join("porch-home");
    let sibling = root.join("unused-sibling");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();

    let db = fixture_db(&home);
    let run_id = seed_run(&db, &home);
    let inventory = b"a.rs\nb.rs\n";
    let digest = sha256_hex(inventory);

    let first = rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory))
        .expect("first open");
    let loaded = rounds::get_round(&db, &first).unwrap().expect("committed");
    assert_eq!(loaded.ordinal, 1);
    assert_eq!(loaded.execution, ExecutionState::Running);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Pending);
    assert_eq!(loaded.from_sha, "from");
    assert_eq!(loaded.to_sha, "to");
    assert_eq!(loaded.inventory_digest, digest);
    assert_eq!(loaded.trusted_config_sha, "config");
    assert_eq!(loaded.protocol_schema_version, 2);
    assert_eq!(loaded.fingerprint_version, 1);

    let producers = rounds::producers_for_round(&db, &first).unwrap();
    assert_eq!(producers.len(), 1);
    assert!(producers[0].descriptor_json.contains("not_on_path"));
    assert_eq!(producers[0].descriptor_equivalence_digest, "equiv-digest-1");

    let state = db_path(&home);
    let conn = Connection::open(&state).unwrap();
    let (element_name, source_state, snapshot_state, snapshot_digest): (
        String,
        String,
        String,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT element_name, source_state, snapshot_state, snapshot_digest
             FROM round_context_elements WHERE round_id = ?1",
            [first.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(element_name, "intent");
    assert_eq!(source_state, "present");
    assert_eq!(snapshot_state, "stored");
    assert_eq!(snapshot_digest.as_deref(), Some(digest.as_str()));

    let (app_element, app_state, effective): (String, String, Option<String>) = conn
        .query_row(
            "SELECT element_name, application, effective_digest
             FROM round_context_applications WHERE round_id = ?1",
            [first.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(app_element, "intent");
    assert_eq!(app_state, "applied");
    assert_eq!(
        effective.as_deref(),
        Some(context_applicability_digest("intent", "present", inventory).as_str())
    );
    let app_producer: String = conn
        .query_row(
            "SELECT producer_invocation_id FROM round_context_applications WHERE round_id = ?1",
            [first.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(app_producer, producers[0].id);

    let second = rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory))
        .expect("second open");
    assert_ne!(first.as_str(), second.as_str());
    let loaded2 = rounds::get_round(&db, &second).unwrap().unwrap();
    assert_eq!(loaded2.ordinal, 2);

    let round_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM review_rounds", [], |row| row.get(0))
        .unwrap();
    assert_eq!(round_count, 2);

    let sqlite_files = sqlite_paths_under(root);
    assert!(
        sqlite_files.iter().all(|p| p.starts_with(&home)),
        "round storage escaped porch home: {sqlite_files:?}"
    );
    assert!(
        sqlite_files.iter().any(|p| p == &state),
        "expected rounds in {}",
        state.display()
    );
    assert!(
        sibling.read_dir().unwrap().next().is_none(),
        "unused sibling path must stay empty"
    );
}

#[test]
fn content_blob_digest_mismatch_is_refused() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inventory-one";
    let digest = sha256_hex(inventory);

    rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();

    let mut bad = sample_bindings(b"inventory-two");
    bad.inventory_digest = digest;
    bad.inventory_bytes = b"inventory-two".to_vec();

    let err = rounds::open_round(&db, &sample_plan(&run_id), &bad).unwrap_err();
    match err {
        Error::Other(msg) => assert!(
            msg.contains("digest") || msg.contains("blob"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected refuse, got {other:?}"),
    }

    let rounds_for_run = rounds::rounds_for_run(&db, &run_id).unwrap();
    assert_eq!(rounds_for_run.len(), 1);
}

#[test]
fn content_blob_rejects_digest_that_does_not_hash_bytes() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let mut bad = sample_bindings(b"fresh-bytes");
    bad.inventory_digest = sha256_hex(b"different-bytes");
    bad.context_elements.clear();
    bad.context_applications.clear();

    let err = rounds::open_round(&db, &sample_plan(&run_id), &bad).unwrap_err();
    match err {
        Error::Other(msg) => assert!(
            msg.contains("digest") || msg.contains("blob"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected refuse, got {other:?}"),
    }

    assert!(rounds::rounds_for_run(&db, &run_id).unwrap().is_empty());
    let conn = Connection::open(db_path(home)).unwrap();
    let blobs: i64 = conn
        .query_row("SELECT COUNT(*) FROM content_blobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(blobs, 0);
}

#[test]
fn absent_and_present_empty_context_elements_differ() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv\n";

    let mut bindings = sample_bindings(inventory);
    let absent =
        capture_context_element("path_instructions", ContextSource::Absent { reason: None });
    let empty = capture_context_element("intent", ContextSource::Present { bytes: Vec::new() });
    assert_eq!(absent.source_state, SourceState::Absent);
    assert_eq!(empty.source_state, SourceState::Present);
    assert_eq!(empty.snapshot_state, SnapshotState::Stored);
    assert_eq!(empty.snapshot_bytes.as_deref(), Some(&[][..]));

    bindings.context_elements = vec![absent, empty];
    bindings.context_applications = vec![
        ContextApplication {
            element_name: "path_instructions".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::NotApplied,
            effective_digest: None,
        },
        ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(context_applicability_digest("intent", "present", &[])),
        },
    ];

    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    let elements = rounds::context_elements_for_round(&db, &round_id).unwrap();
    let by_name: std::collections::BTreeMap<_, _> = elements
        .into_iter()
        .map(|e| (e.element_name.clone(), e))
        .collect();

    assert_eq!(
        by_name["path_instructions"].source_state,
        SourceState::Absent
    );
    assert_eq!(by_name["path_instructions"].snapshot_digest, None);
    assert_eq!(by_name["intent"].source_state, SourceState::Present);
    assert_eq!(by_name["intent"].snapshot_state, SnapshotState::Stored);
    assert_eq!(by_name["intent"].snapshot_bytes.as_deref(), Some(&[][..]));

    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.intent_source.as_deref(), Some("flag"));
    assert!(
        !by_name.contains_key("intent_source"),
        "intent_source must stay outside review-context elements"
    );
}

#[test]
fn oversized_context_element_omits_snapshot_keeps_digest_and_source() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-oversize\n";
    let oversized = vec![b'x'; SNAPSHOT_CEILING_BYTES + 1];
    let digest = sha256_hex(&oversized);

    let element = capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: oversized.clone(),
        },
    );
    assert_eq!(element.source_state, SourceState::Present);
    assert_eq!(element.snapshot_state, SnapshotState::Omitted);
    assert_eq!(element.snapshot_reason.as_deref(), Some("too_large"));
    assert_eq!(element.snapshot_digest.as_deref(), Some(digest.as_str()));
    assert!(element.snapshot_bytes.is_none());

    let mut bindings = sample_bindings(inventory);
    bindings.context_elements = vec![element];
    bindings.context_applications = vec![ContextApplication {
        element_name: "intent".into(),
        producer_slot: 0,
        application: rounds::ContextApplicationState::Applied,
        effective_digest: Some(context_applicability_digest(
            "intent", "present", &oversized,
        )),
    }];

    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    let elements = rounds::context_elements_for_round(&db, &round_id).unwrap();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].source_state, SourceState::Present);
    assert_eq!(elements[0].snapshot_state, SnapshotState::Omitted);
    assert_eq!(elements[0].snapshot_reason.as_deref(), Some("too_large"));
    assert_eq!(
        elements[0].snapshot_digest.as_deref(),
        Some(digest.as_str())
    );
    assert!(elements[0].snapshot_bytes.is_none());

    let conn = Connection::open(db_path(home)).unwrap();
    let blob_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM content_blobs WHERE digest = ?1",
            [digest],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blob_hits, 0, "oversized snapshot must not store blob bytes");
}

#[test]
fn unsupplied_context_element_is_not_applied_without_effective_digest() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-not-applied\n";
    let intent_bytes = b"keep me";

    let mut bindings = sample_bindings(inventory);
    bindings.context_elements = vec![
        capture_context_element(
            "intent",
            ContextSource::Present {
                bytes: intent_bytes.to_vec(),
            },
        ),
        capture_context_element(
            "path_instructions",
            ContextSource::Present {
                bytes: b"paths".to_vec(),
            },
        ),
    ];
    bindings.context_applications = vec![
        ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(context_applicability_digest(
                "intent",
                "present",
                intent_bytes,
            )),
        },
        ContextApplication {
            element_name: "path_instructions".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::NotApplied,
            effective_digest: None,
        },
    ];

    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    let apps = rounds::context_applications_for_round(&db, &round_id).unwrap();
    let by_element: std::collections::BTreeMap<_, _> = apps
        .into_iter()
        .map(|a| (a.element_name.clone(), a))
        .collect();

    assert_eq!(
        by_element["intent"].application,
        rounds::ContextApplicationState::Applied
    );
    assert!(by_element["intent"].effective_digest.is_some());
    assert_eq!(
        by_element["path_instructions"].application,
        rounds::ContextApplicationState::NotApplied
    );
    assert_eq!(by_element["path_instructions"].effective_digest, None);
}

fn seed_task1_round_schema_with_snapshot_blob_fk(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        CREATE TABLE repos (
            id TEXT PRIMARY KEY,
            worktree_path TEXT NOT NULL,
            bare_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            default_branch TEXT NOT NULL DEFAULT 'main'
        );
        CREATE TABLE runs (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            sha TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(repo_id) REFERENCES repos(id)
        );
        CREATE TABLE content_blobs (
            digest TEXT PRIMARY KEY,
            byte_length INTEGER NOT NULL,
            bytes BLOB NOT NULL,
            CHECK (byte_length = length(bytes))
        );
        CREATE TABLE review_rounds (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            from_sha TEXT NOT NULL,
            to_sha TEXT NOT NULL,
            inventory_digest TEXT NOT NULL REFERENCES content_blobs(digest),
            execution TEXT NOT NULL CHECK (execution IN ('running','finished','interrupted')),
            assurance_completion TEXT NOT NULL
                CHECK (assurance_completion IN ('pending','complete','incomplete')),
            completion_reason TEXT,
            trusted_config_sha TEXT NOT NULL,
            protocol_schema_version INTEGER NOT NULL,
            fingerprint_version INTEGER NOT NULL,
            opened_at TEXT NOT NULL,
            finalized_at TEXT,
            UNIQUE (run_id, ordinal)
        );
        CREATE TABLE round_producers (
            id TEXT PRIMARY KEY,
            round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
            slot INTEGER NOT NULL,
            descriptor_json TEXT NOT NULL,
            descriptor_equivalence_digest TEXT NOT NULL,
            UNIQUE (round_id, slot),
            UNIQUE (round_id, id)
        );
        CREATE TABLE round_context_elements (
            round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
            element_name TEXT NOT NULL,
            source_state TEXT NOT NULL
                CHECK (source_state IN ('absent','present','unreadable')),
            source_reason TEXT,
            snapshot_state TEXT NOT NULL CHECK (snapshot_state IN ('stored','omitted')),
            snapshot_reason TEXT,
            snapshot_digest TEXT REFERENCES content_blobs(digest),
            PRIMARY KEY (round_id, element_name)
        );
        CREATE TABLE round_context_applications (
            round_id TEXT NOT NULL,
            element_name TEXT NOT NULL,
            producer_invocation_id TEXT NOT NULL,
            application TEXT NOT NULL CHECK (application IN ('applied','not_applied')),
            effective_digest TEXT,
            PRIMARY KEY (round_id, element_name, producer_invocation_id),
            FOREIGN KEY (round_id, element_name)
                REFERENCES round_context_elements(round_id, element_name) ON DELETE CASCADE,
            FOREIGN KEY (round_id, producer_invocation_id)
                REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
            CHECK ((application = 'applied') = (effective_digest IS NOT NULL))
        );
        ",
    )
    .unwrap();

    let fk_present = {
        let mut stmt = conn
            .prepare("PRAGMA foreign_key_list('round_context_elements')")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut found = false;
        while let Some(row) = rows.next().unwrap() {
            let table: String = row.get(2).unwrap();
            let from: String = row.get(3).unwrap();
            if from == "snapshot_digest" && table == "content_blobs" {
                found = true;
                break;
            }
        }
        found
    };
    assert!(
        fk_present,
        "precondition: Task-1 schema must reference content_blobs from snapshot_digest"
    );
}

#[test]
fn migrate_clears_task1_snapshot_digest_fk_so_oversized_omit_commits() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    std::fs::create_dir_all(home).unwrap();
    let path = db_path(home);
    seed_task1_round_schema_with_snapshot_blob_fk(&path);

    let db = Db::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let fk_remaining = {
        let mut stmt = conn
            .prepare("PRAGMA foreign_key_list('round_context_elements')")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut found = false;
        while let Some(row) = rows.next().unwrap() {
            let table: String = row.get(2).unwrap();
            let from: String = row.get(3).unwrap();
            if from == "snapshot_digest" && table == "content_blobs" {
                found = true;
                break;
            }
        }
        found
    };
    assert!(
        !fk_remaining,
        "migrate must drop snapshot_digest → content_blobs FK"
    );

    let run_id = seed_run(&db, home);
    let inventory = b"inv-task1-migrate\n";
    let oversized = vec![b'y'; SNAPSHOT_CEILING_BYTES + 1];
    let digest = sha256_hex(&oversized);

    let mut bindings = sample_bindings(inventory);
    bindings.context_elements = vec![capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: oversized.clone(),
        },
    )];
    bindings.context_applications = vec![ContextApplication {
        element_name: "intent".into(),
        producer_slot: 0,
        application: rounds::ContextApplicationState::Applied,
        effective_digest: Some(context_applicability_digest(
            "intent", "present", &oversized,
        )),
    }];

    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings)
        .expect("oversized omit must commit after FK rebuild");
    let elements = rounds::context_elements_for_round(&db, &round_id).unwrap();
    assert_eq!(elements[0].snapshot_state, SnapshotState::Omitted);
    assert_eq!(
        elements[0].snapshot_digest.as_deref(),
        Some(digest.as_str())
    );
    assert!(elements[0].snapshot_bytes.is_none());
}

#[test]
fn stored_context_snapshot_without_bytes_is_refused() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-stored-incomplete\n";
    let digest = sha256_hex(b"ghost");

    let mut bindings = sample_bindings(inventory);
    bindings.context_elements = vec![ContextElement {
        element_name: "intent".into(),
        source_state: SourceState::Present,
        source_reason: None,
        snapshot_state: SnapshotState::Stored,
        snapshot_reason: None,
        snapshot_digest: Some(digest),
        snapshot_bytes: None,
    }];
    bindings.context_applications = vec![ContextApplication {
        element_name: "intent".into(),
        producer_slot: 0,
        application: rounds::ContextApplicationState::NotApplied,
        effective_digest: None,
    }];

    let err = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap_err();
    match err {
        Error::Other(msg) => assert!(
            msg.contains("stored") && msg.contains("bytes"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected refuse, got {other:?}"),
    }
    assert!(rounds::rounds_for_run(&db, &run_id).unwrap().is_empty());
}

fn producer_id(db: &Db, round_id: &rounds::RoundId) -> String {
    rounds::producers_for_round(db, round_id).unwrap()[0]
        .id
        .clone()
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

fn bump_history_revision(home: &Path, run_id: &str) {
    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute(
        "UPDATE runs SET review_history_revision = review_history_revision + 1 WHERE id = ?1",
        [run_id],
    )
    .unwrap();
}

#[test]
fn finalize_is_atomic_coverage_instances_and_terminal_land_together() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-atomic\n";
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();

    let mut bad = sample_complete_proposal(&producer);
    bad.coverage.push(RoundCoverageProposal {
        producer_invocation_id: producer.clone(),
        path: "b.rs".into(),
        state: CoverageState::Completed,
        reason: None,
        authority: None,
        completion_evidence: None, // CHECK: completed requires evidence
    });

    let err = rounds::finalize_round(&db, &round_id, &bad, rev).unwrap_err();
    match err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected finalize refuse, got {other:?}"),
    }

    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Running);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Pending);
    assert!(loaded.finalized_at.is_none());
    assert!(
        rounds::coverage_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        rounds::instances_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    let (rev_after, history) = rounds::read_history(&db, &run_id).unwrap();
    assert_eq!(rev_after, rev);
    assert!(history.is_empty());

    let ok = sample_complete_proposal(&producer);
    assert_eq!(
        rounds::finalize_round(&db, &round_id, &ok, rev).unwrap(),
        FinalizeOutcome::Finalized
    );
    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Finished);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Complete);
    assert!(loaded.finalized_at.is_some());
    assert_eq!(rounds::coverage_for_round(&db, &round_id).unwrap().len(), 1);
    assert_eq!(
        rounds::instances_for_round(&db, &round_id).unwrap().len(),
        1
    );
}

#[test]
fn stale_revision_between_phases_yields_stale_without_durable_finalization() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-stale\n";
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let (seen, _) = rounds::read_history(&db, &run_id).unwrap();

    bump_history_revision(home, &run_id);

    let outcome =
        rounds::finalize_round(&db, &round_id, &sample_complete_proposal(&producer), seen).unwrap();
    assert_eq!(outcome, FinalizeOutcome::Stale);

    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Running);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Pending);
    assert!(
        rounds::coverage_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        rounds::instances_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn finding_instances_get_distinct_ids_even_when_fingerprints_match() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-ids\n";
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();

    let mut proposal = sample_complete_proposal(&producer);
    proposal.instances.push(FindingInstanceProposal {
        producer_invocation_id: producer.clone(),
        fingerprint: "fp-one".into(),
        fingerprint_version: 1,
        candidate_key: "ck-two".into(),
        criterion_id: "rust/unwrap-in-lib".into(),
        evidence: "another unwrap".into(),
        consequence: "panic risk".into(),
        action: "must-fix".into(),
        severity: "error".into(),
        provenance_json: "{}".into(),
        confidence_value: None,
        confidence_kind: None,
        path: "a.rs".into(),
        anchor_kind: "symbol".into(),
        anchor_value: "bar".into(),
    });

    assert_eq!(
        rounds::finalize_round(&db, &round_id, &proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );

    let instances = rounds::instances_for_round(&db, &round_id).unwrap();
    assert_eq!(instances.len(), 2);
    assert_ne!(instances[0].id, instances[1].id);
    assert_eq!(instances[0].fingerprint, "fp-one");
    assert_eq!(instances[1].fingerprint, "fp-one");
    assert_eq!(instances[0].fingerprint_version, 1);
    assert_eq!(instances[1].round_id, round_id.as_str());

    let (_, history) = rounds::read_history(&db, &run_id).unwrap();
    assert_eq!(history.len(), 2);
    assert_ne!(
        history[0].finding_instance_id,
        history[1].finding_instance_id
    );
}

#[test]
fn contention_free_finalization_commits_exactly_two_writes_beyond_preround() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-writes\n";

    rounds::reset_committed_write_count();
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    assert_eq!(
        rounds::finalize_round(&db, &round_id, &sample_complete_proposal(&producer), rev).unwrap(),
        FinalizeOutcome::Finalized
    );
    assert_eq!(
        rounds::take_committed_write_count(),
        2,
        "open + finalize only"
    );
}

#[test]
fn stale_retries_are_bounded_then_history_contention_closes() {
    assert_eq!(STALE_REVISION_RETRIES, 3);

    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-contend\n";
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);

    rounds::reset_committed_write_count();
    rounds::reset_finalize_attempt_count();

    for _ in 0..STALE_REVISION_RETRIES {
        let (seen, _) = rounds::read_history(&db, &run_id).unwrap();
        bump_history_revision(home, &run_id);
        let outcome =
            rounds::finalize_round(&db, &round_id, &sample_complete_proposal(&producer), seen)
                .unwrap();
        assert_eq!(outcome, FinalizeOutcome::Stale);
    }

    assert_eq!(
        rounds::take_finalize_attempt_count(),
        u64::from(STALE_REVISION_RETRIES)
    );
    assert_eq!(
        rounds::take_committed_write_count(),
        0,
        "stale attempts must not commit finalization"
    );
    assert!(
        rounds::coverage_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        rounds::instances_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );

    rounds::abandon_for_history_contention(&db, &round_id).unwrap();
    assert_eq!(rounds::take_committed_write_count(), 1);

    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Interrupted);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Incomplete);
    assert_eq!(
        loaded.completion_reason.as_deref(),
        Some("history_contention")
    );
    assert!(loaded.finalized_at.is_some());
}

fn floor_equiv_digest() -> String {
    descriptor_equivalence_digest(&EquivalenceInput {
        adapter_kind: "porch_json_cli",
        argv_prefix: &["--engine".into(), "quality".into()],
        observed_version: ObservedVersionForEquivalence::ArtifactSha256("floor-artifact".into()),
        consumed_context: &["intent".into()],
    })
}

fn judgment_equiv_digest() -> String {
    descriptor_equivalence_digest(&EquivalenceInput {
        adapter_kind: "native_agent",
        argv_prefix: &[],
        observed_version: ObservedVersionForEquivalence::ArtifactSha256("judgment-artifact".into()),
        consumed_context: &["intent".into(), "path_instructions".into()],
    })
}

fn matching_requirements(digests: &[&str]) -> Vec<RequirementSpec> {
    digests
        .iter()
        .enumerate()
        .map(|(i, digest)| RequirementSpec {
            slot: i64::try_from(i).unwrap_or(i64::MAX),
            role: if i == 0 { Role::Floor } else { Role::Judgment },
            resolution: Resolution::Resolved,
            expected_equivalence_digest: Some((*digest).to_string()),
            reason: None,
        })
        .collect()
}

fn required_rows(digests: &[&str]) -> Vec<RequirementRow> {
    matching_requirements(digests)
        .into_iter()
        .map(|spec| RequirementRow {
            slot: spec.slot,
            role: spec.role,
            resolution: spec.resolution,
            expected_equivalence_digest: spec.expected_equivalence_digest,
            producer_invocation_id: None,
            reason: spec.reason,
        })
        .collect()
}

fn plan_with_digests(run_id: &str, digests: &[&str]) -> OpenRoundPlan {
    OpenRoundPlan {
        run_id: run_id.to_string(),
        producers: digests
            .iter()
            .enumerate()
            .map(|(i, digest)| ProducerInvocation {
                descriptor_json: format!(r#"{{"slot":{i}}}"#),
                descriptor_equivalence_digest: (*digest).to_string(),
            })
            .collect(),
        requirements: matching_requirements(digests),
    }
}

fn bindings_for_producers(inventory: &[u8], producer_count: usize) -> RoundBindings {
    let digest = sha256_hex(inventory);
    let intent = capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    let intent_digest = context_applicability_digest("intent", "present", inventory);
    let mut context_applications = Vec::with_capacity(producer_count);
    for slot in 0..producer_count {
        context_applications.push(ContextApplication {
            element_name: "intent".into(),
            producer_slot: slot,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(intent_digest.clone()),
        });
    }
    RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest,
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: 2,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent],
        context_applications,
    }
}

fn finalize_complete_with_coverage(
    db: &Db,
    round_id: &rounds::RoundId,
    run_id: &str,
    coverage: Vec<RoundCoverageProposal>,
) {
    let (rev, _) = rounds::read_history(db, run_id).unwrap();
    let producer = producer_id(db, round_id);
    let proposal = FinalizeProposal {
        execution: ExecutionState::Finished,
        assurance_completion: AssuranceCompletion::Complete,
        completion_reason: None,
        coverage,
        producer_durations: Vec::new(),
        review_duration_ms: None,
        instances: vec![FindingInstanceProposal {
            producer_invocation_id: producer,
            fingerprint: "fp-auth".into(),
            fingerprint_version: 1,
            candidate_key: "ck-auth".into(),
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
    };
    assert_eq!(
        rounds::finalize_round(db, round_id, &proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );
}

fn finish_round(db: &Db, round_id: &rounds::RoundId, run_id: &str) {
    let producers = rounds::producers_for_round(db, round_id).unwrap();
    let coverage = producers
        .iter()
        .map(|p| RoundCoverageProposal {
            producer_invocation_id: p.id.clone(),
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        })
        .collect();
    finalize_complete_with_coverage(db, round_id, run_id, coverage);
}

fn open_and_finish(
    db: &Db,
    plan: &OpenRoundPlan,
    producer_count: usize,
    inventory: &[u8],
) -> rounds::RoundId {
    let round_id =
        rounds::open_round(db, plan, &bindings_for_producers(inventory, producer_count)).unwrap();
    finish_round(db, &round_id, &plan.run_id);
    round_id
}

fn run_applicability(db: &Db, run_id: &str) -> Applicability {
    let run = db.run_by_id(run_id).unwrap().expect("run");
    applicable_round_for_run(db, &run).unwrap()
}

fn unresolved_floor() -> RequirementSpec {
    RequirementSpec {
        slot: 0,
        role: Role::Floor,
        resolution: Resolution::Unresolved,
        expected_equivalence_digest: None,
        reason: Some("floor binary missing".into()),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // four authorization refusal paths in one case
fn pending_incomplete_interrupted_or_under_covered_round_never_authorizes() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-never-auth\n";
    let digest = floor_equiv_digest();
    let required = required_rows(&[digest.as_str()]);

    // Pending (open, not finalized).
    let run_pending = seed_run(&db, home);
    let pending_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_pending, &[digest.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    match applicable_round(
        &db,
        &run_pending,
        &bindings_for_producers(inventory, 1),
        &required,
    )
    .unwrap()
    {
        Applicability::RequiresNew { reason } => {
            assert!(
                reason.contains("pending")
                    || reason.contains("applicable")
                    || reason.contains("authorize"),
                "unexpected reason: {reason}"
            );
        }
        Applicability::Applicable(id) => panic!("pending must not authorize, got {id}"),
    }
    let pending = rounds::get_round(&db, &pending_id).unwrap().unwrap();
    assert_eq!(pending.assurance_completion, AssuranceCompletion::Pending);

    // Incomplete.
    let run_incomplete = {
        db.upsert_repo("repo-inc", home, &home.join("bare-inc.git"), "main")
            .unwrap();
        db.insert_run("repo-inc", "feat", "deadbeef", Some("intent"), Some("flag"))
            .unwrap()
            .id
    };
    let incomplete_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_incomplete, &[digest.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let producer = producer_id(&db, &incomplete_id);
    let (rev, _) = rounds::read_history(&db, &run_incomplete).unwrap();
    assert_eq!(
        rounds::finalize_round(
            &db,
            &incomplete_id,
            &FinalizeProposal {
                execution: ExecutionState::Finished,
                assurance_completion: AssuranceCompletion::Incomplete,
                completion_reason: Some("coverage_shortfall".into()),
                coverage: vec![],
                instances: vec![],
                producer_durations: Vec::new(),
                review_duration_ms: None,
            },
            rev,
        )
        .unwrap(),
        FinalizeOutcome::Finalized
    );
    match applicable_round(
        &db,
        &run_incomplete,
        &bindings_for_producers(inventory, 1),
        &required,
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => panic!("incomplete must not authorize, got {id}"),
    }
    let _ = producer;

    // Interrupted.
    let run_interrupted = {
        db.upsert_repo("repo-int", home, &home.join("bare-int.git"), "main")
            .unwrap();
        db.insert_run("repo-int", "feat", "deadbeef", Some("intent"), Some("flag"))
            .unwrap()
            .id
    };
    let interrupted_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_interrupted, &[digest.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    rounds::abandon_for_history_contention(&db, &interrupted_id).unwrap();
    match applicable_round(
        &db,
        &run_interrupted,
        &bindings_for_producers(inventory, 1),
        &required,
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => panic!("interrupted must not authorize, got {id}"),
    }

    // Under-covered: finished/complete but a path remains `selected`.
    let run_under = {
        db.upsert_repo("repo-under", home, &home.join("bare-under.git"), "main")
            .unwrap();
        db.insert_run(
            "repo-under",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let under_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_under, &[digest.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let under_producer = producer_id(&db, &under_id);
    finalize_complete_with_coverage(
        &db,
        &under_id,
        &run_under,
        vec![RoundCoverageProposal {
            producer_invocation_id: under_producer,
            path: "a.rs".into(),
            state: CoverageState::Selected,
            reason: None,
            authority: None,
            completion_evidence: None,
        }],
    );
    match applicable_round(
        &db,
        &run_under,
        &bindings_for_producers(inventory, 1),
        &required,
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => panic!("under-covered must not authorize, got {id}"),
    }
}

#[test]
fn differing_only_in_selection_source_or_declared_engine_kind_stays_applicable() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-selection\n";

    let input = EquivalenceInput {
        adapter_kind: "porch_json_cli",
        argv_prefix: &["review".into()],
        observed_version: ObservedVersionForEquivalence::ArtifactSha256("same-artifact".into()),
        consumed_context: &["intent".into()],
    };
    let digest_a = descriptor_equivalence_digest(&input);
    let digest_b = descriptor_equivalence_digest(&input);
    assert_eq!(
        digest_a, digest_b,
        "equivalence digest must ignore selection_source / declared_engine_kind (absent from preimage)"
    );

    let round_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_id, &[digest_a.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let producer = producer_id(&db, &round_id);
    finalize_complete_with_coverage(
        &db,
        &round_id,
        &run_id,
        vec![RoundCoverageProposal {
            producer_invocation_id: producer,
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        }],
    );

    match applicable_round(
        &db,
        &run_id,
        &bindings_for_producers(inventory, 1),
        &required_rows(&[digest_b.as_str()]),
    )
    .unwrap()
    {
        Applicability::Applicable(id) => assert_eq!(id, round_id),
        Applicability::RequiresNew { reason } => {
            panic!("same equivalence digest must stay applicable: {reason}")
        }
    }
}

#[test]
fn unavailable_producer_version_never_establishes_equivalence() {
    let a = descriptor_equivalence_digest(&EquivalenceInput {
        adapter_kind: "porch_json_cli",
        argv_prefix: &["review".into()],
        observed_version: ObservedVersionForEquivalence::Unavailable {
            reason: "not_on_path".into(),
        },
        consumed_context: &["intent".into()],
    });
    let b = descriptor_equivalence_digest(&EquivalenceInput {
        adapter_kind: "porch_json_cli",
        argv_prefix: &["review".into()],
        observed_version: ObservedVersionForEquivalence::Unavailable {
            reason: "not_on_path".into(),
        },
        consumed_context: &["intent".into()],
    });
    assert_ne!(
        a, b,
        "unavailable observed identity must mint a per-invocation nonce"
    );

    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-unavail\n";

    let round_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_id, &[a.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let producer = producer_id(&db, &round_id);
    finalize_complete_with_coverage(
        &db,
        &round_id,
        &run_id,
        vec![RoundCoverageProposal {
            producer_invocation_id: producer,
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        }],
    );

    match applicable_round(
        &db,
        &run_id,
        &bindings_for_producers(inventory, 1),
        &required_rows(&[b.as_str()]),
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("unavailable digests must not authorize via {id}")
        }
    }
}

#[test]
fn floor_plus_judgment_round_is_not_equivalent_to_judgment_only() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-floor-judgment\n";
    let floor = floor_equiv_digest();
    let judgment = judgment_equiv_digest();

    let round_id = rounds::open_round(
        &db,
        &plan_with_digests(&run_id, &[floor.as_str(), judgment.as_str()]),
        &bindings_for_producers(inventory, 2),
    )
    .unwrap();
    let producers = rounds::producers_for_round(&db, &round_id).unwrap();
    assert_eq!(producers.len(), 2);
    finalize_complete_with_coverage(
        &db,
        &round_id,
        &run_id,
        vec![
            RoundCoverageProposal {
                producer_invocation_id: producers[0].id.clone(),
                path: "a.rs".into(),
                state: CoverageState::Completed,
                reason: None,
                authority: None,
                completion_evidence: Some("floor".into()),
            },
            RoundCoverageProposal {
                producer_invocation_id: producers[1].id.clone(),
                path: "a.rs".into(),
                state: CoverageState::Completed,
                reason: None,
                authority: None,
                completion_evidence: Some("judgment".into()),
            },
        ],
    );

    match applicable_round(
        &db,
        &run_id,
        &bindings_for_producers(inventory, 1),
        &required_rows(&[judgment.as_str()]),
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("judgment-only must not match floor+judgment round {id}")
        }
    }

    // Positive control: both producers required → applicable.
    match applicable_round(
        &db,
        &run_id,
        &bindings_for_producers(inventory, 2),
        &required_rows(&[floor.as_str(), judgment.as_str()]),
    )
    .unwrap()
    {
        Applicability::Applicable(id) => assert_eq!(id, round_id),
        Applicability::RequiresNew { reason } => {
            panic!("matching producer multiset must apply: {reason}")
        }
    }
}

#[test]
fn producers_and_resolved_requirements_must_correspond_one_to_one() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-req-bijection\n";
    let floor = floor_equiv_digest();
    let judgment = judgment_equiv_digest();

    let extra_producer_run = seed_run(&db, home);
    let mut extra_producer_plan =
        plan_with_digests(&extra_producer_run, &[floor.as_str(), judgment.as_str()]);
    extra_producer_plan.requirements = vec![resolved_floor_spec(&floor)];
    let extra_producer_round = open_and_finish(&db, &extra_producer_plan, 2, inventory);
    match run_applicability(&db, &extra_producer_run) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("extra producer without a requirement must not authorize via {id}")
        }
    }
    assert_eq!(
        rounds::producers_for_round(&db, &extra_producer_round)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        rounds::requirements_for_round(&db, &extra_producer_round)
            .unwrap()
            .len(),
        1
    );

    let extra_requirement_run = {
        db.upsert_repo(
            "repo-extra-req",
            home,
            &home.join("bare-extra-req.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-extra-req",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let mut extra_requirement_plan = plan_with_digests(&extra_requirement_run, &[floor.as_str()]);
    extra_requirement_plan.requirements = vec![resolved_floor_spec(&floor)];
    let extra_requirement_round = rounds::open_round(
        &db,
        &extra_requirement_plan,
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let invocation = producer_id(&db, &extra_requirement_round);
    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    insert_requirement_row(
        &conn,
        extra_requirement_round.as_str(),
        &requirement_insert(
            1,
            "judgment",
            "resolved",
            Some(floor.as_str()),
            Some(invocation.as_str()),
            None,
        ),
    )
    .expect("second resolved requirement may share the same-round invocation FK");
    finish_round(&db, &extra_requirement_round, &extra_requirement_run);
    match run_applicability(&db, &extra_requirement_run) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("extra requirement without its own invocation must not authorize via {id}")
        }
    }
}

#[test]
fn expected_digest_must_match_the_referenced_invocation() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-req-digest\n";
    let recorded = floor_equiv_digest();
    let expected = judgment_equiv_digest();
    assert_ne!(recorded, expected);

    let run_id = seed_run(&db, home);
    let mut plan = plan_with_digests(&run_id, &[recorded.as_str()]);
    plan.requirements = vec![resolved_floor_spec(&expected)];
    let round_id = open_and_finish(&db, &plan, 1, inventory);
    let recorded_rows = rounds::requirements_for_round(&db, &round_id).unwrap();
    let producers = rounds::producers_for_round(&db, &round_id).unwrap();
    assert_eq!(recorded_rows.len(), 1);
    assert_eq!(producers.len(), 1);
    assert_eq!(
        recorded_rows[0].producer_invocation_id.as_deref(),
        Some(producers[0].id.as_str())
    );
    assert_eq!(
        recorded_rows[0].expected_equivalence_digest.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(producers[0].descriptor_equivalence_digest, recorded);
    match run_applicability(&db, &run_id) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("digest mismatch on a valid FK must not authorize via {id}")
        }
    }
}

#[test]
fn unresolved_or_unrecorded_required_set_never_authorizes() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-req-unresolved\n";
    let floor = floor_equiv_digest();

    let unresolved_run = seed_run(&db, home);
    let mut unresolved_plan = plan_with_digests(&unresolved_run, &[floor.as_str()]);
    unresolved_plan.requirements = vec![unresolved_floor()];
    open_and_finish(&db, &unresolved_plan, 1, inventory);
    match run_applicability(&db, &unresolved_run) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("an unresolved requirement must not authorize via {id}")
        }
    }

    let unrecorded_run = {
        db.upsert_repo(
            "repo-unrecorded",
            home,
            &home.join("bare-unrecorded.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-unrecorded",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let mut unrecorded_plan = plan_with_digests(&unrecorded_run, &[floor.as_str()]);
    unrecorded_plan.requirements = vec![];
    let unrecorded_round = open_and_finish(&db, &unrecorded_plan, 1, inventory);
    assert!(
        rounds::requirements_for_round(&db, &unrecorded_round)
            .unwrap()
            .is_empty()
    );
    match run_applicability(&db, &unrecorded_run) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("a round with zero requirement rows must not authorize via {id}")
        }
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
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

/// Bare repo with one commit reachable only after pinning (branch deleted later).
fn bare_with_config_commit(root: &Path) -> (GitDir, PathBuf, String) {
    let bare_path = root.join("bare.git");
    porch_git::init_bare(&bare_path).unwrap();
    let bare = GitDir::new(&bare_path).unwrap();

    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init"]);
    git(&seed, &["config", "user.email", "porch@example.com"]);
    git(&seed, &["config", "user.name", "Porch"]);
    git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README"), "trusted-config\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "trusted"]);
    let sha = git_out(&seed, &["rev-parse", "HEAD"]);
    git(
        &seed,
        &["push", bare_path.to_str().unwrap(), "main:refs/heads/main"],
    );
    (bare, bare_path, sha)
}

#[test]
fn opening_a_round_pins_trusted_config_and_survives_prune() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let (bare, bare_path, sha) = bare_with_config_commit(&root);
    let db = fixture_db(&home);
    db.upsert_repo("repo-ret", &root, &bare_path, "main")
        .unwrap();
    let run_id = db
        .insert_run("repo-ret", "feat", "deadbeef", Some("intent"), None)
        .unwrap()
        .id;

    // Open sequence: pin before the round row commits.
    retention::pin_trusted_config(&bare, &sha).unwrap();
    let mut bindings = sample_bindings(b"inv-retention-pin\n");
    bindings.trusted_config_sha = sha.clone();
    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.trusted_config_sha, sha);

    let refname = config_ref_name(&sha);
    assert_eq!(porch_git::rev_parse(&bare, &refname).unwrap(), sha);

    // Drop every other ref so only the porch config pin keeps the object alive.
    porch_git::delete_ref(&bare, "refs/heads/main").unwrap();
    porch_git::run(&bare, &["gc", "--prune=now"]).unwrap();

    assert_eq!(
        porch_git::rev_parse(&bare, &refname).unwrap(),
        sha,
        "config pin must survive gc prune"
    );
    porch_git::run(&bare, &["cat-file", "-e", &sha]).unwrap();
}

#[test]
fn removing_last_referencing_round_sweeps_ref_after_db_commit() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let (bare, bare_path, sha) = bare_with_config_commit(&root);
    let db = fixture_db(&home);
    db.upsert_repo("repo-sweep", &root, &bare_path, "main")
        .unwrap();
    let run_id = db
        .insert_run("repo-sweep", "feat", "deadbeef", None, None)
        .unwrap()
        .id;

    retention::pin_trusted_config(&bare, &sha).unwrap();
    let mut bindings = sample_bindings(b"inv-retention-sweep\n");
    bindings.trusted_config_sha = sha.clone();
    let round_id = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    let refname = config_ref_name(&sha);
    assert_eq!(porch_git::rev_parse(&bare, &refname).unwrap(), sha);

    // Second round shares the same trusted SHA — deleting only the first must keep the pin.
    let round_b = rounds::open_round(&db, &sample_plan(&run_id), &bindings).unwrap();
    {
        let conn = Connection::open(db_path(&home)).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "DELETE FROM review_rounds WHERE id = ?1",
            [round_id.as_str()],
        )
        .unwrap();
    }
    let removed_while_shared = retention::sweep_unreferenced(&bare, &db).unwrap();
    assert_eq!(removed_while_shared, 0);
    assert_eq!(porch_git::rev_parse(&bare, &refname).unwrap(), sha);

    // Last referencing round: DB delete commits first; ref remains until sweep.
    {
        let conn = Connection::open(db_path(&home)).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "DELETE FROM review_rounds WHERE id = ?1",
            [round_b.as_str()],
        )
        .unwrap();
    }
    assert!(
        rounds::get_round(&db, &round_b).unwrap().is_none(),
        "row deletion must commit before ref removal"
    );
    assert_eq!(
        porch_git::rev_parse(&bare, &refname).unwrap(),
        sha,
        "ref must still exist after DB commit and before sweep"
    );

    let removed = retention::sweep_unreferenced(&bare, &db).unwrap();
    assert_eq!(removed, 1);
    assert!(
        porch_git::rev_parse(&bare, &refname).is_err(),
        "last reference gone → config ref removed"
    );
}

struct RequirementInsert<'a> {
    slot: i64,
    role: &'a str,
    resolution: &'a str,
    digest: Option<&'a str>,
    invocation: Option<&'a str>,
    reason: Option<&'a str>,
}

fn insert_requirement_row(
    conn: &Connection,
    round_id: &str,
    row: &RequirementInsert<'_>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO round_required_producers (
            round_id, requirement_slot, role, resolution,
            expected_equivalence_digest, producer_invocation_id, resolution_reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            round_id,
            row.slot,
            row.role,
            row.resolution,
            row.digest,
            row.invocation,
            row.reason
        ],
    )
}

fn assert_constraint_rejected(result: rusqlite::Result<usize>, detail: &str) {
    match result {
        Err(rusqlite::Error::SqliteFailure(err, msg)) => {
            assert_eq!(
                err.code,
                rusqlite::ErrorCode::ConstraintViolation,
                "{detail}: expected a table constraint, got {err:?} {msg:?}"
            );
        }
        other => panic!("{detail}: expected a table constraint, got {other:?}"),
    }
}

fn requirement_insert<'a>(
    slot: i64,
    role: &'a str,
    resolution: &'a str,
    digest: Option<&'a str>,
    invocation: Option<&'a str>,
    reason: Option<&'a str>,
) -> RequirementInsert<'a> {
    RequirementInsert {
        slot,
        role,
        resolution,
        digest,
        invocation,
        reason,
    }
}

fn reject_requirement(
    conn: &Connection,
    round_id: &str,
    row: &RequirementInsert<'_>,
    detail: &str,
) {
    assert_constraint_rejected(insert_requirement_row(conn, round_id, row), detail);
}

fn assert_inconsistent_requirement_rows_rejected(
    conn: &Connection,
    round_id: &str,
    invocation: &str,
) {
    let rejected = [
        (
            requirement_insert(10, "floor", "resolved", Some("equiv-digest-1"), None, None),
            "resolved without an invocation reference",
        ),
        (
            requirement_insert(11, "floor", "resolved", None, Some(invocation), None),
            "resolved without an expected digest",
        ),
        (
            requirement_insert(
                12,
                "floor",
                "unresolved",
                None,
                Some(invocation),
                Some("floor binary missing"),
            ),
            "unresolved carrying an invocation reference",
        ),
        (
            requirement_insert(
                13,
                "floor",
                "unresolved",
                Some("equiv-digest-1"),
                None,
                Some("floor binary missing"),
            ),
            "unresolved carrying an expected digest",
        ),
        (
            requirement_insert(14, "floor", "unresolved", None, None, None),
            "unresolved without a reason",
        ),
        (
            requirement_insert(15, "floor", "unresolved", None, None, Some("")),
            "unresolved with a blank reason",
        ),
        (
            requirement_insert(16, "floor", "unresolved", None, None, Some("   ")),
            "unresolved with a whitespace-only reason",
        ),
    ];
    for (row, detail) in &rejected {
        reject_requirement(conn, round_id, row, detail);
    }

    insert_requirement_row(
        conn,
        round_id,
        &requirement_insert(
            20,
            "floor",
            "resolved",
            Some("equiv-digest-1"),
            Some(invocation),
            None,
        ),
    )
    .expect("resolved row with invocation and digest must be accepted");
    insert_requirement_row(
        conn,
        round_id,
        &requirement_insert(
            21,
            "judgment",
            "unresolved",
            None,
            None,
            Some("judgment not selected"),
        ),
    )
    .expect("unresolved row with a non-empty reason must be accepted");
}

#[test]
fn table_rejects_inconsistent_requirement_rows() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let round_id = rounds::open_round(
        &db,
        &sample_plan(&run_id),
        &sample_bindings(b"inv-req-check\n"),
    )
    .unwrap();
    let producer = producer_id(&db, &round_id);

    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    assert_inconsistent_requirement_rows_rejected(&conn, round_id.as_str(), producer.as_str());
}

fn resolved_floor_spec(digest: &str) -> RequirementSpec {
    RequirementSpec {
        slot: 0,
        role: Role::Floor,
        resolution: Resolution::Resolved,
        expected_equivalence_digest: Some(digest.to_string()),
        reason: None,
    }
}

#[test]
fn open_round_records_requirements_in_the_same_transaction() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-req-open\n";

    let mut plan = sample_plan(&run_id);
    plan.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let round_id = rounds::open_round(&db, &plan, &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let recorded = rounds::requirements_for_round(&db, &round_id).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].slot, 0);
    assert_eq!(recorded[0].role, Role::Floor);
    assert_eq!(recorded[0].resolution, Resolution::Resolved);
    assert_eq!(
        recorded[0].expected_equivalence_digest.as_deref(),
        Some("equiv-digest-1")
    );
    assert_eq!(
        recorded[0].producer_invocation_id.as_deref(),
        Some(producer.as_str())
    );
    assert_eq!(recorded[0].reason, None);

    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    rounds::finalize_round(&db, &round_id, &sample_complete_proposal(&producer), rev).unwrap();
    let after_finalize = rounds::requirements_for_round(&db, &round_id).unwrap();
    assert_eq!(
        after_finalize, recorded,
        "finalization must not rewrite the required set"
    );

    let run_fail = seed_run(&db, home);
    let mut bad_plan = sample_plan(&run_fail);
    bad_plan.requirements = vec![RequirementSpec {
        slot: 0,
        role: Role::Floor,
        resolution: Resolution::Resolved,
        expected_equivalence_digest: None,
        reason: None,
    }];
    let err = rounds::open_round(&db, &bad_plan, &sample_bindings(inventory)).unwrap_err();
    match err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected open to refuse an inconsistent requirement, got {other:?}"),
    }
    assert!(
        rounds::rounds_for_run(&db, &run_fail).unwrap().is_empty(),
        "a refused open must not leave a round"
    );
    let conn = Connection::open(db_path(home)).unwrap();
    let leftover: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM round_required_producers rp
             JOIN review_rounds r ON r.id = rp.round_id
             WHERE r.run_id = ?1",
            [&run_fail],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        leftover, 0,
        "a refused open must not leave requirement rows"
    );

    let run_later = seed_run(&db, home);
    let mut later_plan = sample_plan(&run_later);
    later_plan.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let mut later_bindings = sample_bindings(inventory);
    later_bindings.context_applications[0].effective_digest = None;
    let later_err = rounds::open_round(&db, &later_plan, &later_bindings).unwrap_err();
    match later_err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected a later constraint failure, got {other:?}"),
    }
    assert!(rounds::rounds_for_run(&db, &run_later).unwrap().is_empty());
    let leftover_later: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM round_required_producers rp
             JOIN review_rounds r ON r.id = rp.round_id
             WHERE r.run_id = ?1",
            [&run_later],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover_later, 0);
}

fn sample_requirement_row(reason: Option<&str>) -> RequirementRow {
    RequirementRow {
        slot: 0,
        role: Role::Floor,
        resolution: Resolution::Resolved,
        expected_equivalence_digest: Some("equiv-a".into()),
        producer_invocation_id: Some("inv-a".into()),
        reason: reason.map(str::to_string),
    }
}

#[test]
fn required_set_digest_tracks_role_resolution_and_expected_digest_not_reason() {
    let base = sample_requirement_row(Some("daemon cannot spawn floor"));
    let protocol = 2;

    let baseline = required_set_digest(protocol, std::slice::from_ref(&base));

    let mut role_changed = base.clone();
    role_changed.role = Role::Judgment;
    assert_ne!(
        required_set_digest(protocol, &[role_changed]),
        baseline,
        "role is part of required-set identity"
    );

    let mut resolution_changed = base.clone();
    resolution_changed.resolution = Resolution::Unresolved;
    resolution_changed.expected_equivalence_digest = None;
    resolution_changed.producer_invocation_id = None;
    assert_ne!(
        required_set_digest(protocol, &[resolution_changed]),
        baseline,
        "resolution is part of required-set identity"
    );

    let mut digest_changed = base.clone();
    digest_changed.expected_equivalence_digest = Some("equiv-b".into());
    assert_ne!(
        required_set_digest(protocol, &[digest_changed]),
        baseline,
        "expected digest is part of required-set identity"
    );

    let reason_changed = sample_requirement_row(Some("different diagnostic text"));
    assert_eq!(
        required_set_digest(protocol, &[reason_changed]),
        baseline,
        "resolution reason must not perturb required-set identity"
    );
    assert_eq!(
        required_set_digest(protocol, &[sample_requirement_row(None)]),
        baseline,
        "absent reason must not perturb required-set identity"
    );

    assert_ne!(
        required_set_digest(1, std::slice::from_ref(&base)),
        baseline,
        "protocol version is part of required-set identity"
    );

    let judgment = RequirementRow {
        slot: 1,
        role: Role::Judgment,
        resolution: Resolution::Resolved,
        expected_equivalence_digest: Some("equiv-j".into()),
        producer_invocation_id: Some("inv-j".into()),
        reason: None,
    };
    let forward = required_set_digest(protocol, &[base.clone(), judgment.clone()]);
    let reversed = required_set_digest(protocol, &[judgment, base]);
    assert_eq!(
        forward, reversed,
        "slots contribute in ascending requirement_slot order"
    );
}

#[test]
fn first_round_pins_the_assurance_contract_in_the_same_transaction() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-run-pin\n";
    let bindings = sample_bindings(inventory);

    let mut plan = sample_plan(&run_id);
    plan.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let round_id = rounds::open_round(&db, &plan, &bindings).unwrap();
    let recorded = rounds::requirements_for_round(&db, &round_id).unwrap();
    let expected = required_set_digest(bindings.protocol_schema_version, &recorded);
    assert_eq!(
        run_required_set_digest(&db, &run_id).unwrap().as_deref(),
        Some(expected.as_str()),
        "the first round must pin the required-set digest"
    );

    let run_fail = seed_run(&db, home);
    let mut fail_plan = sample_plan(&run_fail);
    fail_plan.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let mut fail_bindings = sample_bindings(inventory);
    fail_bindings.context_applications[0].effective_digest = None;
    let err = rounds::open_round(&db, &fail_plan, &fail_bindings).unwrap_err();
    match err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected a later constraint failure, got {other:?}"),
    }
    assert!(
        rounds::rounds_for_run(&db, &run_fail).unwrap().is_empty(),
        "a refused open must not leave a round"
    );
    assert_eq!(
        run_required_set_digest(&db, &run_fail).unwrap(),
        None,
        "a refused open must not pin the run"
    );
}

#[test]
fn later_round_must_keep_the_pinned_required_set() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let bindings = sample_bindings(b"inv-pin-match\n");

    let mut first = sample_plan(&run_id);
    first.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let first_id = rounds::open_round(&db, &first, &bindings).unwrap();
    let pinned = run_required_set_digest(&db, &run_id)
        .unwrap()
        .expect("first round pins the run");

    let mut matching = sample_plan(&run_id);
    matching.requirements = vec![resolved_floor_spec("equiv-digest-1")];
    let second_id = rounds::open_round(&db, &matching, &bindings)
        .expect("a later round with the same required set must open");
    assert_ne!(first_id.as_str(), second_id.as_str());
    assert_eq!(
        run_required_set_digest(&db, &run_id).unwrap().as_deref(),
        Some(pinned.as_str()),
        "a matching later round must not re-pin"
    );

    let mut different = sample_plan(&run_id);
    different.requirements = vec![resolved_floor_spec("equiv-digest-other")];
    let err = rounds::open_round(&db, &different, &bindings).unwrap_err();
    match err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected a pin mismatch to refuse open, got {other:?}"),
    }
    let rounds = rounds::rounds_for_run(&db, &run_id).unwrap();
    assert_eq!(
        rounds.len(),
        2,
        "a mismatched open must not create a round, got {rounds:?}"
    );
    assert_eq!(
        run_required_set_digest(&db, &run_id).unwrap().as_deref(),
        Some(pinned.as_str()),
        "a mismatched open must not re-pin"
    );
}

fn snapshot_table(
    conn: &Connection,
    table: &str,
    key_column: &str,
    key: &str,
) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table} WHERE {key_column} = ?1"))
        .unwrap();
    let columns = stmt.column_count();
    let mut rows = stmt.query([key]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut values = Vec::with_capacity(columns);
        for i in 0..columns {
            values.push(row.get(i).unwrap());
        }
        out.push(values);
    }
    out
}

fn requirement_count(conn: &Connection, round_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM round_required_producers WHERE round_id = ?1",
        [round_id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn this_feature_records_protocol_two_and_leaves_legacy_rounds_untouched() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-protocol-two\n";
    let digest = floor_equiv_digest();

    let current_run = seed_run(&db, home);
    let current_id = rounds::open_round(
        &db,
        &plan_with_digests(&current_run, &[digest.as_str()]),
        &bindings_for_producers(inventory, 1),
    )
    .unwrap();
    let current = rounds::get_round(&db, &current_id).unwrap().unwrap();
    assert_eq!(
        current.protocol_schema_version, 2,
        "rounds opened by this feature must record protocol version 2"
    );

    let legacy_run = {
        db.upsert_repo("repo-v1", home, &home.join("bare-v1.git"), "main")
            .unwrap();
        db.insert_run("repo-v1", "feat", "deadbeef", Some("intent"), Some("flag"))
            .unwrap()
            .id
    };
    let mut v1_bindings = bindings_for_producers(inventory, 1);
    v1_bindings.protocol_schema_version = 1;
    let legacy_id = rounds::open_round(
        &db,
        &plan_with_digests(&legacy_run, &[digest.as_str()]),
        &v1_bindings,
    )
    .unwrap();
    finish_round(&db, &legacy_id, &legacy_run);

    let conn = Connection::open(db_path(home)).unwrap();
    let before_round = snapshot_table(&conn, "review_rounds", "id", legacy_id.as_str());
    let before_producers = snapshot_table(&conn, "round_producers", "round_id", legacy_id.as_str());
    let before_required = snapshot_table(
        &conn,
        "round_required_producers",
        "round_id",
        legacy_id.as_str(),
    );
    let required_before = requirement_count(&conn, legacy_id.as_str());
    assert!(
        required_before > 0,
        "precondition: the version-1 round already has a recorded required set"
    );

    match applicable_round(
        &db,
        &legacy_run,
        &v1_bindings,
        &required_rows(&[digest.as_str()]),
    )
    .unwrap()
    {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!("a version-1 round must never authorize, got {id}")
        }
    }

    assert_eq!(
        snapshot_table(&conn, "review_rounds", "id", legacy_id.as_str()),
        before_round,
        "a version-1 round must stay byte-for-byte unchanged"
    );
    assert_eq!(
        snapshot_table(&conn, "round_producers", "round_id", legacy_id.as_str()),
        before_producers
    );
    assert_eq!(
        snapshot_table(
            &conn,
            "round_required_producers",
            "round_id",
            legacy_id.as_str()
        ),
        before_required
    );
    assert_eq!(
        requirement_count(&conn, legacy_id.as_str()),
        required_before,
        "authorization must not invent requirement rows for a version-1 round"
    );
}

#[test]
fn a_round_above_the_understood_protocol_fails_closed() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-protocol-future\n";
    let digest = floor_equiv_digest();
    let run_id = seed_run(&db, home);
    let round_id = open_and_finish(
        &db,
        &plan_with_digests(&run_id, &[digest.as_str()]),
        1,
        inventory,
    );

    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute(
        "UPDATE review_rounds SET protocol_schema_version = 3 WHERE id = ?1",
        [round_id.as_str()],
    )
    .unwrap();

    let err = match applicable_round_for_run(&db, &db.run_by_id(&run_id).unwrap().unwrap()) {
        Ok(Applicability::Applicable(id)) => {
            panic!("a future protocol round must not authorize, got {id}")
        }
        Ok(Applicability::RequiresNew { reason }) => {
            panic!("a future protocol round must fail closed, not skip: {reason}")
        }
        Err(e) => e,
    };
    match err {
        Error::Other(msg) => assert!(
            msg.contains("protocol") && (msg.contains('3') || msg.contains("understood")),
            "unexpected fail-closed message: {msg}"
        ),
        other => panic!("expected a fail-closed error, got {other:?}"),
    }
}

fn seed_pre_floor_round_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE repos (
            id TEXT PRIMARY KEY,
            worktree_path TEXT NOT NULL,
            bare_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            default_branch TEXT NOT NULL DEFAULT 'main'
        );
        CREATE TABLE runs (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            sha TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(repo_id) REFERENCES repos(id)
        );
        CREATE TABLE content_blobs (
            digest TEXT PRIMARY KEY,
            byte_length INTEGER NOT NULL,
            bytes BLOB NOT NULL,
            CHECK (byte_length = length(bytes))
        );
        CREATE TABLE review_rounds (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            from_sha TEXT NOT NULL,
            to_sha TEXT NOT NULL,
            inventory_digest TEXT NOT NULL REFERENCES content_blobs(digest),
            execution TEXT NOT NULL CHECK (execution IN ('running','finished','interrupted')),
            assurance_completion TEXT NOT NULL
                CHECK (assurance_completion IN ('pending','complete','incomplete')),
            completion_reason TEXT,
            trusted_config_sha TEXT NOT NULL,
            protocol_schema_version INTEGER NOT NULL,
            fingerprint_version INTEGER NOT NULL,
            opened_at TEXT NOT NULL,
            finalized_at TEXT,
            UNIQUE (run_id, ordinal)
        );
        CREATE TABLE round_producers (
            id TEXT PRIMARY KEY,
            round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
            slot INTEGER NOT NULL,
            descriptor_json TEXT NOT NULL,
            descriptor_equivalence_digest TEXT NOT NULL,
            UNIQUE (round_id, slot),
            UNIQUE (round_id, id)
        );
        INSERT INTO repos (id, worktree_path, bare_path, created_at, default_branch)
        VALUES ('repo-old', '/tmp/wt', '/tmp/bare.git', '1', 'main');
        INSERT INTO runs (id, repo_id, branch, sha, status, created_at)
        VALUES ('run-old', 'repo-old', 'feat', 'abc', 'parked', '2');
        INSERT INTO content_blobs (digest, byte_length, bytes)
        VALUES ('inv-old', 7, x'6f6c642e72730a');
        INSERT INTO review_rounds (
            id, run_id, ordinal, from_sha, to_sha, inventory_digest,
            execution, assurance_completion, completion_reason,
            trusted_config_sha, protocol_schema_version, fingerprint_version,
            opened_at, finalized_at
        ) VALUES (
            'round-old', 'run-old', 1, 'from', 'to', 'inv-old',
            'finished', 'complete', NULL,
            'config', 1, 1,
            '3', '4'
        );
        INSERT INTO round_producers (
            id, round_id, slot, descriptor_json, descriptor_equivalence_digest
        ) VALUES (
            'prod-old', 'round-old', 0, '{"adapter_kind":"porch_json_cli"}', 'equiv-old'
        );
        "#,
    )
    .unwrap();
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get(0),
        )
        .unwrap();
    count == 1
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let name: String = row.get(1).unwrap();
        if name == column {
            return true;
        }
    }
    false
}

#[test]
fn opening_an_older_database_adds_duration_storage_and_keeps_invocations() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    std::fs::create_dir_all(home).unwrap();
    let path = db_path(home);
    seed_pre_floor_round_db(&path);

    let db = Db::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();

    assert!(
        table_exists(&conn, "round_producer_durations"),
        "opening an older database must add round_producer_durations"
    );
    assert!(
        column_exists(&conn, "review_rounds", "review_duration_ms"),
        "opening an older database must add review_rounds.review_duration_ms"
    );

    let run = db.run_by_id("run-old").unwrap().expect("legacy run");
    assert_eq!(run.status, "parked");
    let rounds = rounds::rounds_for_run(&db, "run-old").unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].ordinal, 1);
    assert_eq!(rounds[0].protocol_schema_version, 1);
    assert_eq!(rounds[0].id.as_str(), "round-old");

    let producers = rounds::producers_for_round(&db, &rounds[0].id).unwrap();
    assert_eq!(producers.len(), 1);
    assert!(
        !producers[0].descriptor_json.is_empty(),
        "invocation descriptor must stay non-null"
    );
    assert_eq!(producers[0].descriptor_equivalence_digest, "equiv-old");

    let fresh = seed_run(&db, home);
    let first = rounds::open_round(&db, &sample_plan(&fresh), &sample_bindings(b"a.rs\n"))
        .expect("first open after migrate");
    let second = rounds::open_round(&db, &sample_plan(&fresh), &sample_bindings(b"a.rs\n"))
        .expect("second open after migrate");
    assert_eq!(rounds::get_round(&db, &first).unwrap().unwrap().ordinal, 1);
    assert_eq!(rounds::get_round(&db, &second).unwrap().unwrap().ordinal, 2);
}

#[test]
fn finalization_writes_durations_with_terminal_state_or_not_at_all() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let inventory = b"inv-durations\n";
    let round_id =
        rounds::open_round(&db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_id);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();

    let mut bad = sample_complete_proposal(&producer);
    bad.coverage.push(RoundCoverageProposal {
        producer_invocation_id: producer.clone(),
        path: "b.rs".into(),
        state: CoverageState::Completed,
        reason: None,
        authority: None,
        completion_evidence: None,
    });
    bad.producer_durations.push(rounds::ProducerDuration {
        producer_invocation_id: producer.clone(),
        duration_ms: 7,
    });
    bad.review_duration_ms = Some(11);
    let err = rounds::finalize_round(&db, &round_id, &bad, rev).unwrap_err();
    match err {
        Error::Sqlite(_) | Error::Other(_) => {}
        other => panic!("expected finalize refuse, got {other:?}"),
    }
    assert!(
        rounds::producer_durations_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Running);
    assert_eq!(loaded.review_duration_ms, None);
    assert!(
        rounds::coverage_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        rounds::instances_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );

    bump_history_revision(home, &run_id);
    let mut stale_proposal = sample_complete_proposal(&producer);
    stale_proposal
        .producer_durations
        .push(rounds::ProducerDuration {
            producer_invocation_id: producer.clone(),
            duration_ms: 7,
        });
    stale_proposal.review_duration_ms = Some(11);
    assert_eq!(
        rounds::finalize_round(&db, &round_id, &stale_proposal, rev).unwrap(),
        FinalizeOutcome::Stale
    );
    assert!(
        rounds::producer_durations_for_round(&db, &round_id)
            .unwrap()
            .is_empty()
    );
    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Running);
    assert_eq!(loaded.review_duration_ms, None);

    let (current, _) = rounds::read_history(&db, &run_id).unwrap();
    let mut ok = sample_complete_proposal(&producer);
    ok.producer_durations.push(rounds::ProducerDuration {
        producer_invocation_id: producer.clone(),
        duration_ms: 7,
    });
    ok.review_duration_ms = Some(11);
    assert_eq!(
        rounds::finalize_round(&db, &round_id, &ok, current).unwrap(),
        FinalizeOutcome::Finalized
    );
    let loaded = rounds::get_round(&db, &round_id).unwrap().unwrap();
    assert_eq!(loaded.execution, ExecutionState::Finished);
    assert_eq!(loaded.assurance_completion, AssuranceCompletion::Complete);
    assert_eq!(loaded.review_duration_ms, Some(11));
    assert_eq!(rounds::coverage_for_round(&db, &round_id).unwrap().len(), 1);
    assert_eq!(
        rounds::instances_for_round(&db, &round_id).unwrap().len(),
        1
    );
    let durations = rounds::producer_durations_for_round(&db, &round_id).unwrap();
    assert_eq!(durations.len(), 1);
    assert_eq!(durations[0].producer_invocation_id, producer);
    assert_eq!(durations[0].duration_ms, 7);
}

#[test]
fn authorization_requires_the_recorded_required_set_to_match_the_run_pin() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-pin-auth\n";
    let digest = floor_equiv_digest();
    let run_id = seed_run(&db, home);
    let round_id = open_and_finish(
        &db,
        &plan_with_digests(&run_id, &[digest.as_str()]),
        1,
        inventory,
    );
    match run_applicability(&db, &run_id) {
        Applicability::Applicable(id) => assert_eq!(id, round_id),
        Applicability::RequiresNew { reason } => {
            panic!("precondition: a matching pin must authorize, got {reason}")
        }
    }

    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute(
        "UPDATE runs SET required_set_digest = '0000000000000000000000000000000000000000000000000000000000000000' WHERE id = ?1",
        [&run_id],
    )
    .unwrap();

    match run_applicability(&db, &run_id) {
        Applicability::RequiresNew { .. } => {}
        Applicability::Applicable(id) => {
            panic!(
                "a round whose required-set digest differs from the run pin must not authorize, got {id}"
            )
        }
    }
}
