//! M18: review round store — migration and durable open.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
use porch_gate::{Db, Error, RunExecutor, db_path, run_daemon, wait_for_health};
use porch_git::GitDir;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use tempfile::TempDir;

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

fn set_run_status_raw(home: &Path, run_id: &str, status: &str, error: Option<&str>) {
    let conn = Connection::open(db_path(home)).unwrap();
    register_current_writer_protocol(&conn);
    conn.execute(
        "UPDATE runs SET status = ?1, error = ?2 WHERE id = ?3",
        rusqlite::params![status, error, run_id],
    )
    .unwrap();
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
        protocol_schema_version: rounds::PROTOCOL_SCHEMA_VERSION,
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
    assert_eq!(run.status, "failed");
    assert!(
        run.error.as_deref().is_some_and(|e| e.contains("upgraded")),
        "legacy active runs must be terminalized, got {:?}",
        run.error
    );
    assert_eq!(run.branch, "feat");

    set_run_status_raw(home, "run-legacy", "pending", None);
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
    assert_eq!(
        loaded.protocol_schema_version,
        rounds::PROTOCOL_SCHEMA_VERSION
    );
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
        protocol_schema_version: rounds::PROTOCOL_SCHEMA_VERSION,
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
    git(&seed, &["init", "-b", "main"]);
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
fn this_feature_records_current_protocol_and_leaves_legacy_rounds_untouched() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let inventory = b"inv-protocol-current\n";
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
        current.protocol_schema_version,
        rounds::PROTOCOL_SCHEMA_VERSION,
        "rounds opened by this binary must record the current protocol version"
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

    let future = rounds::PROTOCOL_SCHEMA_VERSION + 1;
    let conn = Connection::open(db_path(home)).unwrap();
    conn.execute(
        "UPDATE review_rounds SET protocol_schema_version = ?1 WHERE id = ?2",
        rusqlite::params![future, round_id.as_str()],
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
            msg.contains("protocol")
                && (msg.contains(&future.to_string()) || msg.contains("understood")),
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
    assert_eq!(run.status, "failed");
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

#[test]
fn a_connection_without_the_writer_function_cannot_create_or_approve_a_run() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    db.set_review_approved_head_sha(&run_id, Some("approved-sha"))
        .expect("a registered connection can write an approval");
    let created = db
        .insert_run("repo1", "feat-2", "cafebabe", None, None)
        .expect("a registered connection can create a run");
    assert_eq!(created.status, "pending");

    let conn = Connection::open(db_path(home)).unwrap();
    let insert_err = conn
        .execute(
            "INSERT INTO runs (id, repo_id, branch, sha, status, created_at)
             VALUES ('old-writer', 'repo1', 'feat-old', 'deadbeef', 'pending', '9')",
            [],
        )
        .expect_err("an unregistered connection must not create a run");
    let insert_msg = insert_err.to_string();
    assert!(
        insert_msg.contains("porch_writer_protocol"),
        "absence must fail closed, got {insert_msg}"
    );

    let approve_err = conn
        .execute(
            "UPDATE runs SET review_approved_head_sha = 'sneak' WHERE id = ?1",
            [&run_id],
        )
        .expect_err("an unregistered connection must not write an approval");
    let approve_msg = approve_err.to_string();
    assert!(
        approve_msg.contains("porch_writer_protocol"),
        "absence must fail closed, got {approve_msg}"
    );

    let sha: Option<String> = conn
        .query_row(
            "SELECT review_approved_head_sha FROM runs WHERE id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sha.as_deref(), Some("approved-sha"));
}

#[test]
fn a_connection_without_the_writer_function_cannot_update_run_status() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    drop(db);

    let conn = Connection::open(db_path(home)).unwrap();
    let status_before: String = conn
        .query_row("SELECT status FROM runs WHERE id = ?1", [&run_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status_before, "pending");

    let status_err = conn
        .execute(
            "UPDATE runs SET status = 'failed', error = 'sneak' WHERE id = ?1",
            [&run_id],
        )
        .expect_err("an unregistered connection must not update run status");
    let status_msg = status_err.to_string();
    assert!(
        status_msg.contains("porch_writer_protocol"),
        "absence must fail closed on status, got {status_msg}"
    );

    let status_after: String = conn
        .query_row("SELECT status FROM runs WHERE id = ?1", [&run_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status_after, "pending");
}

fn seed_active_legacy_runs(path: &Path) {
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
            review_approved_head_sha TEXT,
            FOREIGN KEY(repo_id) REFERENCES repos(id)
        );
        INSERT INTO repos (id, worktree_path, bare_path, created_at, default_branch)
        VALUES ('repo-legacy', '/tmp/wt', '/tmp/bare.git', '1', 'main');
        INSERT INTO runs (id, repo_id, branch, sha, status, created_at, review_approved_head_sha)
        VALUES
          ('run-parked', 'repo-legacy', 'feat-parked', 'aaa', 'parked', '2', 'approved-before'),
          ('run-pending', 'repo-legacy', 'feat-pending', 'bbb', 'pending', '3', NULL),
          ('run-running', 'repo-legacy', 'feat-running', 'ccc', 'running', '4', NULL),
          ('run-done', 'repo-legacy', 'feat-done', 'ddd', 'completed', '5', 'keep-me');
        ",
    )
    .unwrap();
}

fn trigger_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1",
            [name],
            |row| row.get(0),
        )
        .unwrap();
    count == 1
}

fn writer_fence_installed(conn: &Connection) -> bool {
    table_exists(conn, "porch_state_meta")
        && trigger_exists(conn, "porch_runs_writer_insert")
        && trigger_exists(conn, "porch_runs_writer_approve")
}

fn run_status_and_approval(conn: &Connection, id: &str) -> (String, Option<String>) {
    conn.query_row(
        "SELECT status, review_approved_head_sha FROM runs WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

fn min_writer_protocol(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT min_writer_protocol FROM porch_state_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn set_upgrade_poison(path: &Path, install: bool) {
    let conn = Connection::open(path).unwrap();
    if install {
        conn.execute_batch(
            "
            CREATE TRIGGER poison_upgrade BEFORE UPDATE ON runs
            BEGIN
                SELECT RAISE(ABORT, 'forced mid-upgrade failure');
            END;
            ",
        )
        .unwrap();
    } else {
        conn.execute_batch("DROP TRIGGER IF EXISTS poison_upgrade;")
            .unwrap();
    }
}

fn assert_legacy_runs_untouched(conn: &Connection) {
    assert!(
        !writer_fence_installed(conn),
        "marker and triggers must be absent after a rolled-back upgrade"
    );
    assert_eq!(
        run_status_and_approval(conn, "run-parked"),
        ("parked".into(), Some("approved-before".into()))
    );
    assert_eq!(
        run_status_and_approval(conn, "run-pending"),
        ("pending".into(), None)
    );
    assert_eq!(
        run_status_and_approval(conn, "run-running"),
        ("running".into(), None)
    );
    assert_eq!(
        run_status_and_approval(conn, "run-done"),
        ("completed".into(), Some("keep-me".into()))
    );
}

fn assert_legacy_runs_terminalized(db: &Db) {
    let parked = db.run_by_id("run-parked").unwrap().unwrap();
    assert_eq!(parked.status, "failed");
    assert!(
        parked
            .error
            .as_deref()
            .is_some_and(|e| { e.contains("upgraded") && e.contains("writer protocol") }),
        "legacy active runs must name the protocol upgrade, got {:?}",
        parked.error
    );
    assert!(parked.review_approved_head_sha.is_none());
    let pending = db.run_by_id("run-pending").unwrap().unwrap();
    assert_eq!(pending.status, "failed");
    assert!(pending.review_approved_head_sha.is_none());
    assert_eq!(
        db.run_by_id("run-running").unwrap().unwrap().status,
        "failed"
    );
    let done = db.run_by_id("run-done").unwrap().unwrap();
    assert_eq!(done.status, "completed");
    assert_eq!(done.review_approved_head_sha.as_deref(), Some("keep-me"));
}

#[test]
fn upgrading_the_state_root_is_atomic_and_idempotent() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    std::fs::create_dir_all(home).unwrap();
    let path = db_path(home);
    seed_active_legacy_runs(&path);
    set_upgrade_poison(&path, true);

    assert!(
        Db::open(&path).is_err(),
        "a forced mid-upgrade failure must abort the open"
    );
    assert_legacy_runs_untouched(&Connection::open(&path).unwrap());
    set_upgrade_poison(&path, false);

    let db = Db::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    assert!(
        writer_fence_installed(&conn),
        "upgrade must install the marker and both run triggers"
    );
    assert!(
        trigger_exists(&conn, "porch_runs_writer_status"),
        "upgrade must also install the status writer trigger"
    );
    assert_eq!(min_writer_protocol(&conn), rounds::PROTOCOL_SCHEMA_VERSION);
    assert_legacy_runs_terminalized(&db);

    let live = db
        .insert_run("repo-legacy", "feat-live", "eeee", None, None)
        .unwrap();
    rounds::open_round(&db, &sample_plan(&live.id), &sample_bindings(b"live.rs\n")).unwrap();
    set_run_status_raw(home, &live.id, "parked", None);
    db.set_review_approved_head_sha(&live.id, Some("live-approved"))
        .unwrap();
    let admitted = db
        .insert_run("repo-legacy", "feat-admitted", "ffff", None, None)
        .unwrap();
    let snapshot = (
        min_writer_protocol(&conn),
        db.run_by_id("run-parked").unwrap().unwrap().status,
        db.run_by_id(&live.id)
            .unwrap()
            .unwrap()
            .review_approved_head_sha,
    );
    drop(conn);
    drop(db);

    let db_again = Db::open(&path).unwrap();
    let conn_again = Connection::open(&path).unwrap();
    assert!(writer_fence_installed(&conn_again));
    assert_eq!(min_writer_protocol(&conn_again), snapshot.0);
    assert_eq!(
        db_again.run_by_id("run-parked").unwrap().unwrap().status,
        snapshot.1
    );
    let live_again = db_again.run_by_id(&live.id).unwrap().unwrap();
    assert_eq!(live_again.status, "parked");
    assert_eq!(live_again.review_approved_head_sha, snapshot.2);
    assert_eq!(
        db_again.run_by_id(&admitted.id).unwrap().unwrap().status,
        "pending"
    );
}

#[test]
fn opening_a_state_root_above_this_binary_fails_with_a_readable_message() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let path = db_path(home);
    let _db = fixture_db(home);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE porch_state_meta SET min_writer_protocol = 99 WHERE id = 1",
        [],
    )
    .unwrap();
    drop(conn);

    let Err(err) = Db::open(&path) else {
        panic!("a too-old binary must not open this state root");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("99") && msg.contains("understands"),
        "open must name the incompatibility, got {msg}"
    );
}

#[test]
fn contending_runs_still_count_only_pending_running_and_parked() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();

    let pending = db.insert_run("repo1", "feat", "aaa", None, None).unwrap();
    let running = db.insert_run("repo1", "feat", "bbb", None, None).unwrap();
    set_run_status_raw(home, &running.id, "running", None);
    let parked = db.insert_run("repo1", "feat", "ccc", None, None).unwrap();
    set_run_status_raw(home, &parked.id, "parked", None);
    let failed = db.insert_run("repo1", "feat", "ddd", None, None).unwrap();
    set_run_status_raw(home, &failed.id, "failed", None);
    let completed = db.insert_run("repo1", "feat", "eee", None, None).unwrap();
    set_run_status_raw(home, &completed.id, "completed", None);
    let interrupted = db.insert_run("repo1", "feat", "fff", None, None).unwrap();
    set_run_status_raw(home, &interrupted.id, "ci_monitor_interrupted", None);

    let active: Vec<String> = db
        .active_runs(Some("repo1"), Some("feat"))
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(active.contains(&pending.id));
    assert!(active.contains(&running.id));
    assert!(active.contains(&parked.id));
    assert!(!active.contains(&failed.id));
    assert!(!active.contains(&completed.id));
    assert!(!active.contains(&interrupted.id));

    let inflight: Vec<String> = db
        .in_flight_same_branch("repo1", "feat", &pending.id)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(inflight.contains(&running.id));
    assert!(inflight.contains(&parked.id));
    assert!(!inflight.contains(&pending.id));
    assert!(!inflight.contains(&failed.id));
    assert!(!inflight.contains(&completed.id));
    assert!(!inflight.contains(&interrupted.id));
}

struct RecoveringExecutor;

impl RunExecutor for RecoveringExecutor {
    fn execute(&self, _home: &Path, _run_id: &str, _cancel: &AtomicBool) {}

    fn recover_stale(&self, home: &Path) -> std::result::Result<(), String> {
        let db = Db::open(&db_path(home)).map_err(|e| e.to_string())?;
        db.fail_stale_running("daemon restarted while run was in progress")
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

struct FailingRecoverExecutor;

impl RunExecutor for FailingRecoverExecutor {
    fn execute(&self, _home: &Path, _run_id: &str, _cancel: &AtomicBool) {}

    fn recover_stale(&self, _home: &Path) -> std::result::Result<(), String> {
        Err("test recover_stale failure".into())
    }
}

#[test]
fn daemon_startup_still_recovers_stale_runs_and_refuses_when_recovery_fails() {
    {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        std::fs::create_dir_all(&home).unwrap();
        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
            .unwrap();
        let run = db.insert_run("repo1", "feat", "abc", None, None).unwrap();
        set_run_status_raw(&home, &run.id, "running", None);
        drop(db);

        let home_t = home.clone();
        let _handle = std::thread::spawn(move || {
            let exec: Arc<dyn RunExecutor> = Arc::new(RecoveringExecutor);
            let _ = run_daemon(&home_t, &exec);
        });
        wait_for_health(&home, Duration::from_secs(5)).unwrap();
        let recovered = Db::open(&db_path(&home))
            .unwrap()
            .run_by_id(&run.id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status, "failed");
        assert!(
            recovered
                .error
                .as_deref()
                .is_some_and(|e| e.contains("daemon restarted")),
            "error={:?}",
            recovered.error
        );
    }

    {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        std::fs::create_dir_all(&home).unwrap();
        let _ = Db::open(&db_path(&home)).unwrap();
        let home_t = home.clone();
        let handle = std::thread::spawn(move || {
            let exec: Arc<dyn RunExecutor> = Arc::new(FailingRecoverExecutor);
            let _ = run_daemon(&home_t, &exec);
        });
        let health = wait_for_health(&home, Duration::from_secs(2));
        assert!(
            health.is_err(),
            "daemon must refuse to serve when recovery fails"
        );
        let _ = handle.join();
    }
}

fn instance_row_bytes(db_file: &Path, instance_id: &str) -> Vec<u8> {
    let conn = Connection::open(db_file).unwrap();
    conn.query_row(
        "SELECT id || round_id || producer_invocation_id || fingerprint ||
                CAST(fingerprint_version AS TEXT) || candidate_key || criterion_id ||
                evidence || consequence || action || severity || provenance_json ||
                IFNULL(confidence_value, '') || IFNULL(confidence_kind, '') ||
                path || anchor_kind || IFNULL(anchor_value, '')
         FROM finding_instances WHERE id = ?1",
        [instance_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap()
    .into_bytes()
}

fn park_finished_round(db: &Db, home: &Path) -> (String, rounds::RoundId, String) {
    let run_id = seed_run(db, home);
    let inventory = b"inv-authority\n";
    let round_id =
        rounds::open_round(db, &sample_plan(&run_id), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(db, &round_id);
    let (rev, _) = rounds::read_history(db, &run_id).unwrap();
    assert_eq!(
        rounds::finalize_round(db, &round_id, &sample_complete_proposal(&producer), rev).unwrap(),
        FinalizeOutcome::Finalized
    );
    db.set_run_shas(&run_id, Some("to"), None).unwrap();
    let instance_id = rounds::instances_for_round(db, &round_id).unwrap()[0]
        .id
        .clone();
    (run_id, round_id, instance_id)
}

#[test]
fn review_approved_persists_context_members_without_mutating_instances() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let (run_id, round_id, instance_id) = park_finished_round(&db, home);
    let before = instance_row_bytes(&db_path(home), &instance_id);
    let audit_before: i64 = {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.query_row(
            "SELECT audit_rev FROM runs WHERE id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap()
    };

    let event_id = rounds::persist_authority(
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
            members: vec![(instance_id.clone(), rounds::MemberRole::Context)],
        },
    )
    .expect("persist approved");

    assert!(!event_id.is_empty());
    let after = instance_row_bytes(&db_path(home), &instance_id);
    assert_eq!(before, after, "finding instance row must stay immutable");

    let events = rounds::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);
    assert_eq!(events[0].kind, rounds::AuthorityKind::ReviewApproved);
    assert_eq!(
        events[0].review_round_id.as_deref(),
        Some(round_id.as_str())
    );
    assert_eq!(events[0].reviewed_head.as_deref(), Some("to"));
    assert!(!events[0].identity_unavailable);
    assert_eq!(events[0].members.len(), 1);
    assert_eq!(events[0].members[0].finding_instance_id, instance_id);
    assert_eq!(events[0].members[0].role, rounds::MemberRole::Context);

    let audit_after: i64 = {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.query_row(
            "SELECT audit_rev FROM runs WHERE id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(audit_after > audit_before, "persist must bump audit_rev");
}

#[test]
fn drifted_round_or_head_fails_closed_without_writing_an_event() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let (run_id, round_id, instance_id) = park_finished_round(&db, home);

    let other_home_run = {
        db.upsert_repo(
            "repo-other-auth",
            home,
            &home.join("bare-other-auth.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-other-auth",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let other_round = rounds::open_round(
        &db,
        &sample_plan(&other_home_run),
        &sample_bindings(b"inv-other-auth\n"),
    )
    .unwrap();
    let other_producer = producer_id(&db, &other_round);
    let (other_rev, _) = rounds::read_history(&db, &other_home_run).unwrap();
    assert_eq!(
        rounds::finalize_round(
            &db,
            &other_round,
            &sample_complete_proposal(&other_producer),
            other_rev
        )
        .unwrap(),
        FinalizeOutcome::Finalized
    );

    let wrong_round = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::ReviewApproved,
            expected_round_id: Some(other_round),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(instance_id.clone(), rounds::MemberRole::Context)],
        },
    );
    assert!(matches!(wrong_round, Err(rounds::AuthorityError::Stale)));
    assert!(rounds::events_for_run(&db, &run_id).unwrap().is_empty());

    let wrong_live = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::ReviewApproved,
            expected_round_id: Some(round_id.clone()),
            expected_head: Some("to".into()),
            live_head: Some("moved".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(instance_id.clone(), rounds::MemberRole::Context)],
        },
    );
    assert!(matches!(wrong_live, Err(rounds::AuthorityError::Stale)));
    assert!(rounds::events_for_run(&db, &run_id).unwrap().is_empty());

    // DISPO-2.6: skip fail-closes on live_head drift the same as approve.
    let skip_wrong_live = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::ReviewSkipped,
            expected_round_id: Some(round_id),
            expected_head: Some("to".into()),
            live_head: Some("moved".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(instance_id, rounds::MemberRole::Context)],
        },
    );
    assert!(matches!(
        skip_wrong_live,
        Err(rounds::AuthorityError::Stale)
    ));
    assert!(rounds::events_for_run(&db, &run_id).unwrap().is_empty());
}

#[test]
fn empty_modern_context_and_legacy_abort_both_persist() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);

    let run_empty = seed_run(&db, home);
    let inventory = b"inv-empty-auth\n";
    let round_empty =
        rounds::open_round(&db, &sample_plan(&run_empty), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &round_empty);
    let (rev, _) = rounds::read_history(&db, &run_empty).unwrap();
    let mut proposal = sample_complete_proposal(&producer);
    proposal.instances.clear();
    assert_eq!(
        rounds::finalize_round(&db, &round_empty, &proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );
    db.set_run_shas(&run_empty, Some("to"), None).unwrap();
    assert!(
        rounds::instances_for_round(&db, &round_empty)
            .unwrap()
            .is_empty()
    );

    let empty_event = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_empty.clone(),
            kind: rounds::AuthorityKind::ReviewApproved,
            expected_round_id: Some(round_empty),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![],
        },
    )
    .expect("empty context still inserts");
    let empty_events = rounds::events_for_run(&db, &run_empty).unwrap();
    assert_eq!(empty_events.len(), 1);
    assert_eq!(empty_events[0].id, empty_event);
    assert!(empty_events[0].members.is_empty());
    assert!(!empty_events[0].identity_unavailable);

    let run_legacy = {
        db.upsert_repo(
            "repo-legacy-auth",
            home,
            &home.join("bare-legacy-auth.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-legacy-auth",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let legacy_event = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_legacy.clone(),
            kind: rounds::AuthorityKind::ReviewAborted,
            expected_round_id: None,
            expected_head: None,
            live_head: None,
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: true,
            members: vec![],
        },
    )
    .expect("legacy abort");
    let legacy_events = rounds::events_for_run(&db, &run_legacy).unwrap();
    assert_eq!(legacy_events.len(), 1);
    assert_eq!(legacy_events[0].id, legacy_event);
    assert!(legacy_events[0].identity_unavailable);
    assert!(legacy_events[0].review_round_id.is_none());
    assert!(legacy_events[0].members.is_empty());
}

#[test]
fn modern_review_aborted_and_skipped_persist_with_context_freeze() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);

    let (abort_run, abort_round, abort_instance) = park_finished_round(&db, home);
    let abort_event = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: abort_run.clone(),
            kind: rounds::AuthorityKind::ReviewAborted,
            expected_round_id: Some(abort_round.clone()),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(abort_instance.clone(), rounds::MemberRole::Context)],
        },
    )
    .expect("modern abort");
    let abort_events = rounds::events_for_run(&db, &abort_run).unwrap();
    assert_eq!(abort_events.len(), 1);
    assert_eq!(abort_events[0].id, abort_event);
    assert_eq!(abort_events[0].kind, rounds::AuthorityKind::ReviewAborted);
    assert!(!abort_events[0].identity_unavailable);
    assert_eq!(
        abort_events[0].review_round_id.as_deref(),
        Some(abort_round.as_str())
    );
    assert_eq!(abort_events[0].members.len(), 1);
    assert_eq!(
        abort_events[0].members[0].finding_instance_id,
        abort_instance
    );
    assert_eq!(abort_events[0].members[0].role, rounds::MemberRole::Context);

    let skip_run = {
        db.upsert_repo(
            "repo-skip-auth",
            home,
            &home.join("bare-skip-auth.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-skip-auth",
            "feat",
            "cafebabe",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };
    let inventory = b"inv-skip-auth\n";
    let skip_round =
        rounds::open_round(&db, &sample_plan(&skip_run), &sample_bindings(inventory)).unwrap();
    let producer = producer_id(&db, &skip_round);
    let (rev, _) = rounds::read_history(&db, &skip_run).unwrap();
    assert_eq!(
        rounds::finalize_round(&db, &skip_round, &sample_complete_proposal(&producer), rev)
            .unwrap(),
        FinalizeOutcome::Finalized
    );
    db.set_run_shas(&skip_run, Some("to"), None).unwrap();
    let skip_instance = rounds::instances_for_round(&db, &skip_round).unwrap()[0]
        .id
        .clone();

    let skip_event = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: skip_run.clone(),
            kind: rounds::AuthorityKind::ReviewSkipped,
            expected_round_id: Some(skip_round.clone()),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Porch,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(skip_instance.clone(), rounds::MemberRole::Context)],
        },
    )
    .expect("modern skip");
    let skip_events = rounds::events_for_run(&db, &skip_run).unwrap();
    assert_eq!(skip_events.len(), 1);
    assert_eq!(skip_events[0].id, skip_event);
    assert_eq!(skip_events[0].kind, rounds::AuthorityKind::ReviewSkipped);
    assert!(!skip_events[0].identity_unavailable);
    assert_eq!(
        skip_events[0].review_round_id.as_deref(),
        Some(skip_round.as_str())
    );
    assert_eq!(skip_events[0].members.len(), 1);
    assert_eq!(skip_events[0].members[0].finding_instance_id, skip_instance);
}

#[test]
fn identity_unavailable_rejected_for_non_abort_kinds_without_writing() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = {
        db.upsert_repo(
            "repo-id-unavail",
            home,
            &home.join("bare-id-unavail.git"),
            "main",
        )
        .unwrap();
        db.insert_run(
            "repo-id-unavail",
            "feat",
            "deadbeef",
            Some("intent"),
            Some("flag"),
        )
        .unwrap()
        .id
    };

    for kind in [
        rounds::AuthorityKind::ReviewApproved,
        rounds::AuthorityKind::ReviewSkipped,
        rounds::AuthorityKind::FixRequested,
    ] {
        let result = rounds::persist_authority(
            &db,
            rounds::PersistAuthorityPlan {
                run_id: run_id.clone(),
                kind,
                expected_round_id: None,
                expected_head: None,
                live_head: None,
                actor_kind: rounds::ActorKind::Operator,
                authority_event_id: None,
                head_changed: None,
                identity_unavailable: true,
                members: vec![],
            },
        );
        assert!(
            result.is_err(),
            "identity_unavailable must be rejected for {kind:?}"
        );
    }
    assert!(
        rounds::events_for_run(&db, &run_id).unwrap().is_empty(),
        "rejected identity_unavailable must not write a row"
    );

    let legacy_ok = rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::ReviewAborted,
            expected_round_id: None,
            expected_head: None,
            live_head: None,
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: true,
            members: vec![],
        },
    );
    assert!(
        legacy_ok.is_ok(),
        "legacy abort with identity_unavailable still succeeds"
    );
}

#[test]
fn authority_members_store_instance_ids_never_display_handles() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let (run_id, round_id, instance_id) = park_finished_round(&db, home);
    assert!(
        !(instance_id.starts_with('f')
            && instance_id.len() > 1
            && instance_id[1..].chars().all(|c| c.is_ascii_digit())),
        "fixture instance id must be a durable id, not fN, got {instance_id}"
    );

    rounds::persist_authority(
        &db,
        rounds::PersistAuthorityPlan {
            run_id: run_id.clone(),
            kind: rounds::AuthorityKind::FixRequested,
            expected_round_id: Some(round_id),
            expected_head: Some("to".into()),
            live_head: Some("to".into()),
            actor_kind: rounds::ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members: vec![(instance_id.clone(), rounds::MemberRole::Target)],
        },
    )
    .expect("fix requested");

    let events = rounds::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].members.len(), 1);
    let stored = &events[0].members[0].finding_instance_id;
    assert_eq!(stored, &instance_id);
    assert!(
        !(stored.starts_with('f')
            && stored.len() > 1
            && stored[1..].chars().all(|c| c.is_ascii_digit())),
        "display handle fN must not be stored, got {stored}"
    );
    let conn = Connection::open(db_path(home)).unwrap();
    let bad: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM authority_event_members m
             WHERE m.finding_instance_id GLOB 'f[0-9]*'
               AND NOT EXISTS (
                   SELECT 1 FROM finding_instances fi WHERE fi.id = m.finding_instance_id
               )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0);
}

#[test]
fn abort_with_cancelled_commits_together_and_rolls_back_on_write_failure() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let (run_id, round_id, instance_id) = park_finished_round(&db, home);
    set_run_status_raw(home, &run_id, "parked", None);

    let abort_plan = rounds::PersistAuthorityPlan {
        run_id: run_id.clone(),
        kind: rounds::AuthorityKind::ReviewAborted,
        expected_round_id: Some(round_id.clone()),
        expected_head: Some("to".into()),
        live_head: Some("to".into()),
        actor_kind: rounds::ActorKind::Operator,
        authority_event_id: None,
        head_changed: None,
        identity_unavailable: false,
        members: vec![(instance_id.clone(), rounds::MemberRole::Context)],
    };
    let cancel_effects = rounds::RunEffects {
        status: Some("cancelled".into()),
        error: Some("agent abort".into()),
        approved_head: None,
        steps: vec![],
    };

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_authority_effects BEFORE UPDATE ON runs
            BEGIN
                SELECT RAISE(ABORT, 'forced mid-txn write failure');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = rounds::persist_authority_with_run_effects(
        &db,
        abort_plan.clone(),
        cancel_effects.clone(),
        None,
    );
    assert!(
        poisoned.is_err(),
        "injected write failure must abort the transaction"
    );
    assert!(
        rounds::events_for_run(&db, &run_id).unwrap().is_empty(),
        "rolled-back txn must leave no authority event"
    );
    let parked = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(parked.status, "parked");
    assert!(parked.error.is_none());

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS poison_authority_effects;")
            .unwrap();
    }

    let event_id =
        rounds::persist_authority_with_run_effects(&db, abort_plan, cancel_effects, None)
            .expect("abort with cancelled");
    let events = rounds::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);
    assert_eq!(events[0].kind, rounds::AuthorityKind::ReviewAborted);
    let cancelled = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.error.as_deref(), Some("agent abort"));
}

#[test]
fn approve_with_run_effects_writes_approved_head_and_review_step() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let (run_id, round_id, instance_id) = park_finished_round(&db, home);
    set_run_status_raw(home, &run_id, "parked", None);

    let event_id = rounds::persist_authority_with_run_effects(
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
        rounds::RunEffects {
            status: None,
            error: None,
            approved_head: Some("to".into()),
            steps: vec![rounds::StepEffect {
                step: "review".into(),
                status: "completed".into(),
                error: Some("approved".into()),
            }],
        },
        None,
    )
    .expect("approve with effects");

    let events = rounds::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);
    assert_eq!(events[0].kind, rounds::AuthorityKind::ReviewApproved);

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.review_approved_head_sha.as_deref(), Some("to"));
    assert_eq!(run.status, "parked");

    let steps = db.step_results_for_run(&run_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step, "review");
    assert_eq!(steps[0].status, "completed");
    assert_eq!(steps[0].error.as_deref(), Some("approved"));
}

#[test]
fn opening_legacy_database_adds_phase_tables_and_keeps_existing_rows() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let path = db_path(home);
    std::fs::create_dir_all(home).unwrap();
    seed_legacy_db(&path);

    let db = Db::open(&path).unwrap();
    let run = db.run_by_id("run-legacy").unwrap().expect("legacy run");
    assert_eq!(run.branch, "feat");

    let conn = Connection::open(&path).unwrap();
    for table in ["phase_attempts", "phase_events"] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing table {table}");
    }
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='phase_events_run'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, 1, "missing phase_events_run index");

    conn.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES ('att-1', 'run-legacy', 'review', 1, NULL, NULL, NULL, '10')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO phase_events (
            id, run_id, attempt_id, seq, kind, outcome, cause, created_at
         ) VALUES ('evt-1', 'run-legacy', 'att-1', 1, 'started', NULL, NULL, '10')",
        [],
    )
    .unwrap();

    let attempts = rounds::phase::attempts_for_run(&db, "run-legacy").unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id.as_str(), "att-1");
    assert_eq!(attempts[0].phase, rounds::phase::PhaseName::Review);
    assert_eq!(attempts[0].ordinal, 1);
    assert!(attempts[0].parent_attempt_id.is_none());
    assert!(attempts[0].caused_by_attempt_id.is_none());
    assert!(attempts[0].operation_kind.is_none());

    let events = rounds::phase::events_for_run(&db, "run-legacy").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "evt-1");
    assert_eq!(events[0].attempt_id.as_str(), "att-1");
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].kind, rounds::phase::PhaseEventKind::Started);

    let open =
        rounds::phase::nonterminal_attempt(&db, "run-legacy", rounds::phase::PhaseName::Review)
            .unwrap()
            .expect("started attempt without terminal is nonterminal");
    assert_eq!(open.id.as_str(), "att-1");
}

#[test]
fn duplicate_top_level_phase_attempt_ordinal_is_rejected() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let path = db_path(home);
    let conn = Connection::open(&path).unwrap();

    conn.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, 'intent', 1, NULL, NULL, NULL, '1')",
        rusqlite::params!["att-a", &run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO phase_events (
            id, run_id, attempt_id, seq, kind, outcome, cause, created_at
         ) VALUES (?1, ?2, 'att-a', 1, 'started', NULL, NULL, '1')",
        rusqlite::params!["evt-a", &run_id],
    )
    .unwrap();

    let dup = conn.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, 'intent', 1, NULL, NULL, NULL, '2')",
        rusqlite::params!["att-b", &run_id],
    );
    assert!(
        dup.is_err(),
        "store must reject a second top-level attempt with the same run/phase/ordinal"
    );

    conn.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, 'intent', 2, NULL, NULL, NULL, '3')",
        rusqlite::params!["att-c", &run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO phase_events (
            id, run_id, attempt_id, seq, kind, outcome, cause, created_at
         ) VALUES (?1, ?2, 'att-c', 2, 'started', NULL, NULL, '3')",
        rusqlite::params!["evt-c", &run_id],
    )
    .unwrap();

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].ordinal, 1);
    assert_eq!(attempts[1].ordinal, 2);
}

#[test]
fn start_transition_commits_attempt_event_and_status_together() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let before = db.run_by_id(&run_id).unwrap().unwrap();
    assert_ne!(before.status, "running");

    let attempt_id = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "review".into(),
                status: "running".into(),
                error: None,
            }],
        },
    )
    .expect("start transition");

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, attempt_id);
    assert_eq!(attempts[0].phase, rounds::phase::PhaseName::Review);
    assert_eq!(attempts[0].ordinal, 1);
    assert!(attempts[0].parent_attempt_id.is_none());

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attempt_id, attempt_id);
    assert_eq!(events[0].kind, rounds::phase::PhaseEventKind::Started);
    assert_eq!(events[0].seq, 1);
    assert!(events[0].outcome.is_none());

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "running");
    assert!(run.error.is_none());

    let steps = db.step_results_for_run(&run_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step, "review");
    assert_eq!(steps[0].status, "running");

    let open = rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
        .unwrap()
        .expect("started attempt without terminal stays nonterminal");
    assert_eq!(open.id, attempt_id);
}

#[test]
fn failed_status_write_rolls_back_attempt_and_event() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let before = db.run_by_id(&run_id).unwrap().unwrap();

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_phase_status BEFORE UPDATE ON runs
            BEGIN
                SELECT RAISE(ABORT, 'forced mid-txn write failure');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Intent,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    );
    assert!(
        poisoned.is_err(),
        "injected status write failure must abort the transition"
    );
    assert!(
        rounds::phase::attempts_for_run(&db, &run_id)
            .unwrap()
            .is_empty(),
        "rolled-back txn must leave no attempt"
    );
    assert!(
        rounds::phase::events_for_run(&db, &run_id)
            .unwrap()
            .is_empty(),
        "rolled-back txn must leave no phase event"
    );
    let after = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(after.error, before.error);
}

/// PHASE-8.3: poison between Start's attempt insert and event insert.
/// Immediate txn still rolls the whole transition back (no partial commit).
#[test]
fn start_poison_after_attempt_insert_rolls_back_and_stays_consistent_after_reopen() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let before = db.run_by_id(&run_id).unwrap().unwrap();

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_phase_event_insert BEFORE INSERT ON phase_events
            BEGIN
                SELECT RAISE(ABORT, 'forced kill between attempt insert and event append');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    );
    assert!(
        poisoned.is_err(),
        "poison between Start writes must abort the Immediate txn"
    );
    assert!(
        rounds::phase::attempts_for_run(&db, &run_id)
            .unwrap()
            .is_empty(),
        "rolled-back Start must leave no attempt"
    );
    assert!(
        rounds::phase::events_for_run(&db, &run_id)
            .unwrap()
            .is_empty(),
        "rolled-back Start must leave no event"
    );
    let after = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(after.error, before.error);

    // Drop poison and reopen: log + status remain consistent; ≤1 nonterminal.
    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS poison_phase_event_insert;")
            .unwrap();
    }
    let reopened = Db::open(&db_path(home)).unwrap();
    let _ = rounds::phase::reconcile_interrupted(&reopened).expect("reconcile after reopen");
    assert!(
        rounds::phase::nonterminal_attempt(&reopened, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none(),
        "failed Start must leave zero nonterminal review attempts after reopen"
    );
    let status = reopened.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(status.status, before.status);
}

/// PHASE-8.3: poison between Handoff's terminal-event write and successor attempt insert.
#[test]
#[allow(clippy::too_many_lines)] // setup + poison + reopen/reconcile assertions
fn handoff_poison_after_terminal_event_rolls_back_and_stays_consistent_after_reopen() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let review = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("start review");
    let before_events = rounds::phase::events_for_run(&db, &run_id).unwrap().len();
    let before_attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap().len();
    let before = db.run_by_id(&run_id).unwrap().unwrap();

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch(
            "
            CREATE TRIGGER poison_handoff_successor BEFORE INSERT ON phase_attempts
            WHEN NEW.caused_by_attempt_id IS NOT NULL
            BEGIN
                SELECT RAISE(ABORT, 'forced kill between handoff terminal and successor');
            END;
            ",
        )
        .unwrap();
    }

    let poisoned = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: review.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("fixer_ok".into()),
        },
        rounds::RunEffects::none(),
    );
    assert!(
        poisoned.is_err(),
        "poison between Handoff writes must abort the Immediate txn"
    );
    assert_eq!(
        rounds::phase::events_for_run(&db, &run_id).unwrap().len(),
        before_events,
        "rolled-back Handoff must not keep the from-attempt Terminal"
    );
    assert_eq!(
        rounds::phase::attempts_for_run(&db, &run_id).unwrap().len(),
        before_attempts,
        "rolled-back Handoff must not mint a successor"
    );
    let open = rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
        .unwrap()
        .expect("from review stays nonterminal");
    assert_eq!(open.id, review);
    let after = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(after.status, before.status);

    {
        let conn = Connection::open(db_path(home)).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS poison_handoff_successor;")
            .unwrap();
    }
    // Pre-reconcile invariant after rollback: ≤1 nonterminal, status matches log.
    assert_eq!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .map(|a| a.id),
        Some(review.clone())
    );
    assert_eq!(db.run_by_id(&run_id).unwrap().unwrap().status, "running");

    let reopened = Db::open(&db_path(home)).unwrap();
    let closed = rounds::phase::reconcile_interrupted(&reopened).expect("reconcile after reopen");
    assert!(
        closed >= 1,
        "running+open review must be interrupted on restart"
    );
    assert!(
        rounds::phase::nonterminal_attempt(&reopened, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none(),
        "post-restart must leave ≤1 (here 0) nonterminal review"
    );
    let status = reopened.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(
        status.status, "failed",
        "status must match interrupted terminals in the log"
    );
    assert!(
        rounds::phase::events_for_run(&reopened, &run_id)
            .unwrap()
            .iter()
            .any(|e| {
                e.attempt_id == review
                    && e.kind == rounds::phase::PhaseEventKind::Terminal
                    && e.outcome.as_deref() == Some("interrupted")
            }),
        "reconcile must terminal the rolled-back-open review"
    );
}

#[test]
fn terminal_transition_appends_a_row_and_leaves_started_unchanged() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let attempt_id = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Certify,
        },
        rounds::RunEffects::none(),
    )
    .expect("start certify");

    let started_before = rounds::phase::events_for_run(&db, &run_id)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == rounds::phase::PhaseEventKind::Started)
        .expect("started event");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Terminal {
            attempt: attempt_id.clone(),
            outcome: "completed".into(),
            cause: Some("certified".into()),
        },
        rounds::RunEffects {
            status: Some("completed".into()),
            error: None,
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "certify".into(),
                status: "completed".into(),
                error: None,
            }],
        },
    )
    .expect("terminal certify");

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 2);
    let started_after = events
        .iter()
        .find(|e| e.id == started_before.id)
        .expect("started row still present");
    assert_eq!(started_after, &started_before);
    assert_eq!(started_after.kind, rounds::phase::PhaseEventKind::Started);
    assert!(started_after.outcome.is_none());

    let terminal = events
        .iter()
        .find(|e| e.kind == rounds::phase::PhaseEventKind::Terminal)
        .expect("terminal event appended");
    assert_ne!(terminal.id, started_before.id);
    assert_eq!(terminal.attempt_id, attempt_id);
    assert_eq!(terminal.seq, started_before.seq + 1);
    assert_eq!(terminal.outcome.as_deref(), Some("completed"));
    assert_eq!(terminal.cause.as_deref(), Some("certified"));

    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Certify)
            .unwrap()
            .is_none()
    );
}

#[test]
fn start_refused_when_a_nonterminal_attempt_for_the_phase_exists() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let first = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("parked".into()),
            error: Some("awaiting compose".into()),
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("first deliver start");

    let open = rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Deliver)
        .unwrap()
        .expect("parked attempt stays nonterminal");
    assert_eq!(open.id, first);

    let refused = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects::none(),
    );
    assert!(
        matches!(refused, Err(rounds::phase::PhaseError::NonterminalExists)),
        "second start must refuse while a nonterminal attempt exists, got {refused:?}"
    );

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, first);
    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, rounds::phase::PhaseEventKind::Started);
}

#[test]
fn nested_start_under_a_terminated_parent_is_refused() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let parent = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Terminal {
            attempt: parent.clone(),
            outcome: "completed".into(),
            cause: Some("done".into()),
        },
        rounds::RunEffects::none(),
    )
    .expect("terminal review");

    let refused = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: parent.clone(),
            kind: rounds::phase::OperationKind::Fixer,
        },
        rounds::RunEffects::none(),
    );
    assert!(
        matches!(refused, Err(rounds::phase::PhaseError::ParentTerminated)),
        "nested start under a terminated parent must be refused, got {refused:?}"
    );

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, parent);
    assert!(attempts[0].parent_attempt_id.is_none());
    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|e| e.kind != rounds::phase::PhaseEventKind::Started || e.attempt_id == parent)
    );
}

#[test]
fn handoff_writes_old_terminal_and_new_started_at_consecutive_seq() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let from = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    let successor = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: from.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("fixer_ok".into()),
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("handoff to next review");

    assert_ne!(successor, from);

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 2);
    let old = attempts.iter().find(|a| a.id == from).expect("old attempt");
    let neu = attempts
        .iter()
        .find(|a| a.id == successor)
        .expect("new attempt");
    assert_eq!(old.ordinal, 1);
    assert_eq!(neu.ordinal, 2);
    assert_eq!(neu.phase, rounds::phase::PhaseName::Review);
    assert_eq!(neu.caused_by_attempt_id.as_ref(), Some(&from));
    assert!(neu.parent_attempt_id.is_none());
    assert!(neu.operation_kind.is_none());

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, rounds::phase::PhaseEventKind::Started);
    assert_eq!(events[0].attempt_id, from);
    assert_eq!(events[0].seq, 1);

    assert_eq!(events[1].kind, rounds::phase::PhaseEventKind::Terminal);
    assert_eq!(events[1].attempt_id, from);
    assert_eq!(events[1].seq, 2);
    assert_eq!(events[1].outcome.as_deref(), Some("rereview"));
    assert_eq!(events[1].cause.as_deref(), Some("fixer_ok"));

    assert_eq!(events[2].kind, rounds::phase::PhaseEventKind::Started);
    assert_eq!(events[2].attempt_id, successor);
    assert_eq!(events[2].seq, 3);
    assert_eq!(
        events[2].seq,
        events[1].seq + 1,
        "handoff terminal and new started must be consecutive"
    );

    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_some_and(|a| a.id == successor)
    );
}

#[test]
fn canonical_phase_name_is_refused_as_nested_operation_kind() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);
    let path = db_path(home);

    let parent = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects::none(),
    )
    .expect("start deliver");

    let nested = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: parent.clone(),
            kind: rounds::phase::OperationKind::Compose,
        },
        rounds::RunEffects::none(),
    )
    .expect("compose nested under deliver");

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    let child = attempts
        .iter()
        .find(|a| a.id == nested)
        .expect("nested attempt");
    assert_eq!(child.parent_attempt_id.as_ref(), Some(&parent));
    assert_eq!(child.phase, rounds::phase::PhaseName::Deliver);
    assert_eq!(
        child.operation_kind,
        Some(rounds::phase::OperationKind::Compose)
    );
    // Typed NestedStart accepts only OperationKind::{Compose, Fixer, DeliverRepair};
    // canonical PhaseName values are not expressible as kind.
    match child.operation_kind {
        Some(
            rounds::phase::OperationKind::Compose
            | rounds::phase::OperationKind::Fixer
            | rounds::phase::OperationKind::DeliverRepair,
        ) => {}
        other => panic!("nested operation_kind must be a nested kind, got {other:?}"),
    }

    let conn = Connection::open(&path).unwrap();
    let rejected = conn.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, 'deliver', 2, ?3, NULL, 'review', '99')",
        rusqlite::params!["att-bad-kind", &run_id, parent.as_str()],
    );
    assert!(
        rejected.is_err(),
        "store must reject a canonical phase name as nested operation_kind"
    );
}

#[test]
fn review_attempt_yields_at_most_one_successor() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let review1 = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    let fixer = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: review1.clone(),
            kind: rounds::phase::OperationKind::Fixer,
        },
        rounds::RunEffects::none(),
    )
    .expect("start fixer");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedTerminal {
            attempt: fixer,
            outcome: "completed".into(),
            cause: Some("head_unchanged".into()),
        },
        rounds::RunEffects::none(),
    )
    .expect("successful fixer terminal");

    let review2 = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: review1.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("fixer_ok".into()),
        },
        rounds::RunEffects::none(),
    )
    .expect("handoff after successful fixer");

    let second = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: review1.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("again".into()),
        },
        rounds::RunEffects::none(),
    );
    assert!(
        matches!(second, Err(rounds::phase::PhaseError::SuccessorExists)),
        "a review attempt must yield at most one successor, got {second:?}"
    );

    let attempts_after_handoff = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    let successors: Vec<_> = attempts_after_handoff
        .iter()
        .filter(|a| a.caused_by_attempt_id.as_ref() == Some(&review1))
        .collect();
    assert_eq!(successors.len(), 1);
    assert_eq!(successors[0].id, review2);
}

#[test]
fn failed_nested_op_yields_no_successor() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let review = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    let fixer = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: review.clone(),
            kind: rounds::phase::OperationKind::Fixer,
        },
        rounds::RunEffects::none(),
    )
    .expect("start fixer");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedTerminal {
            attempt: fixer.clone(),
            outcome: "failed".into(),
            cause: Some("fixer_error".into()),
        },
        rounds::RunEffects {
            status: Some("failed".into()),
            error: Some("fixer_error".into()),
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("failed fixer terminals nested and parent");

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 2, "failed nested must create no successor");
    assert!(attempts.iter().all(|a| a.caused_by_attempt_id.is_none()));
    assert_eq!(
        attempts
            .iter()
            .filter(|a| a.caused_by_attempt_id.as_ref() == Some(&review))
            .count(),
        0,
        "failed nested op must yield no successor"
    );

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    let terminals: Vec<_> = events
        .iter()
        .filter(|e| e.kind == rounds::phase::PhaseEventKind::Terminal)
        .collect();
    assert_eq!(terminals.len(), 2);
    assert!(terminals.iter().any(|e| e.attempt_id == fixer));
    assert!(terminals.iter().any(|e| e.attempt_id == review));
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_none()
    );
}

#[test]
fn allowlist_and_merge_conflicts_are_nonterminal_evidence_on_deliver() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("start deliver");

    for cause in ["AllowlistFailed", "MergeConflicting"] {
        rounds::phase::persist_phase_transition(
            &db,
            rounds::phase::PhaseTransition::Evidence {
                attempt: deliver.clone(),
                cause: cause.into(),
            },
            rounds::RunEffects::none(),
        )
        .unwrap_or_else(|e| panic!("evidence {cause}: {e}"));
    }

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, rounds::phase::PhaseEventKind::Started);
    assert_eq!(events[1].kind, rounds::phase::PhaseEventKind::Evidence);
    assert_eq!(events[1].cause.as_deref(), Some("AllowlistFailed"));
    assert_eq!(events[2].kind, rounds::phase::PhaseEventKind::Evidence);
    assert_eq!(events[2].cause.as_deref(), Some("MergeConflicting"));
    assert!(
        events
            .iter()
            .all(|e| e.kind != rounds::phase::PhaseEventKind::Terminal)
    );

    let open = rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Deliver)
        .unwrap()
        .expect("deliver stays nonterminal after repairable-failure evidence");
    assert_eq!(open.id, deliver);
}

#[test]
fn handoff_into_open_destination_phase_is_refused() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects::none(),
    )
    .expect("start deliver");

    let review = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    let refused = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: deliver.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("head_changed_repair".into()),
        },
        rounds::RunEffects::none(),
    );
    assert!(
        matches!(refused, Err(rounds::phase::PhaseError::NonterminalExists)),
        "handoff into an open destination phase must be refused, got {refused:?}"
    );

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().all(|a| a.caused_by_attempt_id.is_none()));
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Deliver)
            .unwrap()
            .is_some_and(|a| a.id == deliver)
    );
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Review)
            .unwrap()
            .is_some_and(|a| a.id == review)
    );
    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|e| e.kind == rounds::phase::PhaseEventKind::Started)
    );
}

#[test]
fn handoff_from_already_terminal_attempt_is_refused() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let review = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Review,
        },
        rounds::RunEffects::none(),
    )
    .expect("start review");

    let fixer = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: review.clone(),
            kind: rounds::phase::OperationKind::Fixer,
        },
        rounds::RunEffects::none(),
    )
    .expect("start fixer");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedTerminal {
            attempt: fixer,
            outcome: "failed".into(),
            cause: Some("fixer_error".into()),
        },
        rounds::RunEffects::none(),
    )
    .expect("failed fixer terminals nested and parent");

    let refused = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: review.clone(),
            to_phase: rounds::phase::PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("misuse_after_fail".into()),
        },
        rounds::RunEffects::none(),
    );
    assert!(
        matches!(refused, Err(rounds::phase::PhaseError::AlreadyTerminal)),
        "handoff from an already-terminal attempt must be refused, got {refused:?}"
    );

    let attempts = rounds::phase::attempts_for_run(&db, &run_id).unwrap();
    assert_eq!(attempts.len(), 2, "refused handoff must mint no successor");
    assert!(attempts.iter().all(|a| a.caused_by_attempt_id.is_none()));
    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    let review_terminals = events
        .iter()
        .filter(|e| e.attempt_id == review && e.kind == rounds::phase::PhaseEventKind::Terminal)
        .count();
    assert_eq!(
        review_terminals, 1,
        "refused handoff must not append a second terminal on from"
    );
}

#[test]
fn compose_abort_nested_terminal_closes_owning_deliver_and_cancels_run() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let db = fixture_db(home);
    let run_id = seed_run(&db, home);

    let deliver = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.clone(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .expect("start deliver");

    let compose = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedStart {
            parent: deliver.clone(),
            kind: rounds::phase::OperationKind::Compose,
        },
        rounds::RunEffects {
            status: Some("parked".into()),
            error: Some("awaiting compose".into()),
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "compose".into(),
                status: "parked".into(),
                error: None,
            }],
        },
    )
    .expect("start nested compose");

    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::NestedTerminal {
            attempt: compose.clone(),
            outcome: "cancelled".into(),
            cause: Some("agent abort".into()),
        },
        rounds::RunEffects {
            status: Some("cancelled".into()),
            error: Some("agent abort".into()),
            approved_head: None,
            steps: vec![rounds::StepEffect {
                step: "compose".into(),
                status: "cancelled".into(),
                error: Some("agent abort".into()),
            }],
        },
    )
    .expect("compose abort terminals nested and parent");

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.error.as_deref(), Some("agent abort"));

    let events = rounds::phase::events_for_run(&db, &run_id).unwrap();
    let terminals: Vec<_> = events
        .iter()
        .filter(|e| e.kind == rounds::phase::PhaseEventKind::Terminal)
        .collect();
    assert_eq!(
        terminals.len(),
        2,
        "compose + deliver terminals: {events:?}"
    );
    assert!(terminals.iter().any(|e| e.attempt_id == compose));
    assert!(terminals.iter().any(|e| e.attempt_id == deliver));
    assert!(
        rounds::phase::nonterminal_attempt(&db, &run_id, rounds::phase::PhaseName::Deliver)
            .unwrap()
            .is_none()
    );
    let steps = db.step_results_for_run(&run_id).unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.step == "compose" && s.status == "cancelled"),
        "steps={steps:?}"
    );
}

// --- FWDAUTH: durable forward record ---

fn seed_deliver_attempt(db: &Db, run_id: &str) -> rounds::AttemptId {
    rounds::phase::persist_phase_transition(
        db,
        rounds::phase::PhaseTransition::Start {
            run_id: run_id.to_string(),
            phase: rounds::phase::PhaseName::Deliver,
        },
        rounds::RunEffects::none(),
    )
    .unwrap()
}

/// The nullable, per-kind columns of `forward_records`, so a test can offer a
/// combination the Rust appenders would never build.
#[derive(Default)]
struct RawForwardCols<'a> {
    remote_state: Option<&'a str>,
    observed_tip: Option<&'a str>,
    landed_sha: Option<&'a str>,
    detail: Option<&'a str>,
}

fn insert_forward_raw(
    home: &Path,
    run_id: &str,
    attempt: &rounds::AttemptId,
    kind: &str,
    cols: &RawForwardCols<'_>,
) -> rusqlite::Result<usize> {
    let conn = Connection::open(db_path(home)).unwrap();
    register_current_writer_protocol(&conn);
    conn.execute(
        "INSERT INTO forward_records (
            id, run_id, deliver_attempt_id, seq, kind, ref_name, authorized_sha,
            remote_state, observed_remote_tip, landed_sha, detail, created_at
         ) VALUES (?1, ?2, ?3, 99, ?4, 'refs/heads/feat', 'aaa', ?5, ?6, ?7, ?8, '1')",
        rusqlite::params![
            format!("raw-{kind}-{}", rand_suffix()),
            run_id,
            attempt.as_str(),
            kind,
            cols.remote_state,
            cols.observed_tip,
            cols.landed_sha,
            cols.detail,
        ],
    )
}

fn rand_suffix() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |d| d.as_nanos().to_string())
}

#[test]
fn forward_record_store_applies_additively_and_starts_empty() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());

    assert!(
        rounds::forward::records_for_run(&db, &run_id)
            .unwrap()
            .is_empty(),
        "a fresh run has no forward records"
    );
    assert!(
        db.run_by_id(&run_id).unwrap().is_some(),
        "prior rows stay readable across the new table"
    );
}

#[test]
fn forward_record_checks_reject_dishonest_rows() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);

    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "intent",
            &RawForwardCols {
                remote_state: Some("absent"),
                landed_sha: Some("bbb"),
                ..RawForwardCols::default()
            },
        )
        .is_err(),
        "an intent must not carry a landed sha"
    );
    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "intent",
            &RawForwardCols {
                remote_state: Some("present"),
                ..RawForwardCols::default()
            },
        )
        .is_err(),
        "a present remote state must carry the observed tip"
    );
    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "pushed",
            &RawForwardCols::default(),
        )
        .is_err(),
        "a pushed record must name the landed sha"
    );
    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "push_failed",
            &RawForwardCols::default(),
        )
        .is_err(),
        "a failed record must carry a detail"
    );
    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "pushed",
            &RawForwardCols {
                remote_state: Some("absent"),
                landed_sha: Some("bbb"),
                ..RawForwardCols::default()
            },
        )
        .is_err(),
        "an outcome must not carry lease observation columns"
    );
    assert!(
        insert_forward_raw(
            home.path(),
            &run_id,
            &attempt,
            "landed",
            &RawForwardCols {
                landed_sha: Some("bbb"),
                ..RawForwardCols::default()
            },
        )
        .is_err(),
        "an unknown kind is rejected"
    );
}

#[test]
fn forward_intent_is_one_per_deliver_attempt() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let first = seed_deliver_attempt(&db, &run_id);

    rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &first,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Absent,
        },
    )
    .unwrap();

    let again = rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &first,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Absent,
        },
    );
    assert!(
        matches!(again, Err(rounds::ForwardError::IntentExists)),
        "second intent on one attempt is refused: {again:?}"
    );

    // Terminal the first attempt so a second deliver attempt may open.
    rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Terminal {
            attempt: first,
            outcome: "failed".into(),
            cause: None,
        },
        rounds::RunEffects::none(),
    )
    .unwrap();
    let second = seed_deliver_attempt(&db, &run_id);
    rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &second,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Present("bbb".into()),
        },
    )
    .expect("a new deliver attempt may record its own forward");
}

/// A deliver repair that leaves HEAD unmoved hands off to the next `deliver`
/// attempt rather than reusing the ordinal, so its retry forwards under a fresh
/// attempt and the one-intent-per-attempt rule holds instead of refusing a
/// legitimate second forward.
#[test]
fn same_phase_deliver_handoff_lets_the_retry_forward() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let first = seed_deliver_attempt(&db, &run_id);

    let forward = |attempt: &rounds::AttemptId| {
        rounds::forward::append_intent(
            &db,
            &rounds::ForwardIntent {
                run_id: &run_id,
                deliver_attempt_id: attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: "aaa",
                observed: rounds::ObservedRemote::Absent,
            },
        )
    };
    forward(&first).unwrap();

    let second = rounds::phase::persist_phase_transition(
        &db,
        rounds::phase::PhaseTransition::Handoff {
            from: first.clone(),
            to_phase: rounds::phase::PhaseName::Deliver,
            outcome: "deliver_repair".into(),
            cause: Some("attempt 1 unchanged_head".into()),
        },
        rounds::RunEffects::none(),
    )
    .expect("an unchanged-HEAD repair hands deliver off to the next deliver attempt");
    assert_ne!(second, first, "the handoff never reuses the same ordinal");

    forward(&second).expect("the successor attempt may record its own forward");

    let refused = forward(&first);
    assert!(
        matches!(refused, Err(rounds::ForwardError::IntentExists)),
        "one forward per deliver attempt still holds: {refused:?}"
    );
}

/// The states ROAD-9's fault injection will assert against: a real kill must
/// produce one of these and no other.
#[test]
fn forward_verdict_covers_every_durable_state() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());

    let verdict_for = |outcome: Option<(rounds::ForwardKind, Option<&str>, Option<&str>)>| {
        let attempt = seed_deliver_attempt(&db, &run_id);
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
        if let Some((kind, landed, detail)) = outcome {
            rounds::forward::append_outcome(
                &db,
                &rounds::ForwardOutcome {
                    run_id: &run_id,
                    deliver_attempt_id: &attempt,
                    ref_name: "refs/heads/feat",
                    authorized_sha: "aaa",
                    kind,
                    landed_sha: landed,
                    detail,
                },
            )
            .unwrap();
        }
        let records = rounds::forward::records_for_attempt(&db, &attempt).unwrap();
        let out = rounds::reconcile::classify(&records, "aaa", None);
        // Terminal so the next seeded deliver attempt may open.
        rounds::phase::persist_phase_transition(
            &db,
            rounds::phase::PhaseTransition::Terminal {
                attempt,
                outcome: "failed".into(),
                cause: None,
            },
            rounds::RunEffects::none(),
        )
        .unwrap();
        out
    };

    // S0 — no record at all: nothing external happened. Classified for
    // totality; never persisted, because selection requires an intent row.
    assert_eq!(
        rounds::reconcile::classify(&[], "aaa", None),
        (rounds::Verdict::NotAttempted, rounds::Evidence::NoRecord),
        "no forward record means no forward was attempted"
    );

    // S1 — intent only: porch was pushing and never recorded the result.
    assert_eq!(
        verdict_for(None),
        (rounds::Verdict::Indeterminate, rounds::Evidence::IntentOnly)
    );

    // S2 — the push command reported success.
    assert_eq!(
        verdict_for(Some((rounds::ForwardKind::Pushed, Some("aaa"), None))),
        (
            rounds::Verdict::ReachedOrigin,
            rounds::Evidence::PushReportedSuccess
        )
    );

    // S3 — the ref already held the authorized SHA.
    assert_eq!(
        verdict_for(Some((
            rounds::ForwardKind::AlreadyCurrent,
            Some("aaa"),
            None
        ))),
        (
            rounds::Verdict::ReachedOrigin,
            rounds::Evidence::RefAlreadyCurrent
        )
    );

    // S4 — the push command reported failure. That is a statement about porch's
    // command, not about the remote, so it is undetermined and never "unchanged".
    assert_eq!(
        verdict_for(Some((
            rounds::ForwardKind::PushFailed,
            None,
            Some("rejected")
        ))),
        (
            rounds::Verdict::Indeterminate,
            rounds::Evidence::PushReportedFailure
        )
    );
}

/// The tracking ref is one-sided evidence: a match concludes, anything else
/// concludes nothing, and it may never downgrade a verdict.
#[test]
fn tracking_ref_match_upgrades_indeterminate_only() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);
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
    let intent_only = rounds::forward::records_for_attempt(&db, &attempt).unwrap();

    assert_eq!(
        rounds::reconcile::classify(&intent_only, "aaa", Some("aaa")),
        (
            rounds::Verdict::ReachedOrigin,
            rounds::Evidence::TrackingRefMatched
        ),
        "git writes that ref only after the remote acknowledged the push"
    );
    for other in [None, Some("bbb")] {
        assert_eq!(
            rounds::reconcile::classify(&intent_only, "aaa", other).0,
            rounds::Verdict::Indeterminate,
            "absence or mismatch is not evidence the push failed: {other:?}"
        );
    }

    rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Pushed,
            landed_sha: Some("aaa"),
            detail: None,
        },
    )
    .unwrap();
    let pushed = rounds::forward::records_for_attempt(&db, &attempt).unwrap();
    assert_eq!(
        rounds::reconcile::classify(&pushed, "aaa", Some("bbb")).0,
        rounds::Verdict::ReachedOrigin,
        "a mismatched tracking ref never downgrades a recorded outcome"
    );

    // A missing gate repository is a failed read, not an error.
    assert_eq!(
        rounds::reconcile::observed_tracking_sha(&home.path().join("absent.git"), "feat"),
        None
    );
}

/// Reconciliation states what the record proves and what to do about it, without
/// changing the status rule and without claiming a pull request is absent.
#[test]
fn reconciliation_records_a_verdict_and_states_the_remedy() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);
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
    rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Pushed,
            landed_sha: Some("aaa"),
            detail: None,
        },
    )
    .unwrap();
    set_run_status_raw(home.path(), &run_id, "running", None);

    rounds::phase::reconcile_interrupted(&db).expect("reconcile");

    let verdicts = rounds::reconcile::verdicts_for_run(&db, &run_id).unwrap();
    assert_eq!(verdicts.len(), 1, "one verdict per forward: {verdicts:?}");
    assert_eq!(verdicts[0].verdict, rounds::Verdict::ReachedOrigin);
    assert_eq!(verdicts[0].evidence, "push_reported_success");

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(
        run.status, "failed",
        "the status rule is unchanged; the conclusion travels in the error"
    );
    let err = run.error.unwrap_or_default();
    assert!(
        err.starts_with("daemon restarted"),
        "the interruption phrase stays a prefix: {err}"
    );
    assert!(
        err.contains("pull request state is unrecorded"),
        "a reached-origin conclusion states the branch landed: {err}"
    );
    assert!(
        err.contains("Re-push"),
        "the operator is told the remedy, not only the diagnosis: {err}"
    );
    for forbidden in [
        "no pull request",
        "without a pull request",
        "pull request was not",
    ] {
        assert!(
            !err.contains(forbidden),
            "must never assert a pull request is absent, which is unknowable here: {err}"
        );
    }

    // A second pass appends nothing: repeated restarts are idempotent.
    rounds::phase::reconcile_interrupted(&db).expect("second reconcile");
    assert_eq!(
        rounds::reconcile::verdicts_for_run(&db, &run_id)
            .unwrap()
            .len(),
        1
    );
}

/// Selection reads the record's own shape, so a protocol upgrade that terminalizes
/// active runs inside `Db::open` — before recovery runs — cannot disarm it.
#[test]
fn verdict_selection_does_not_depend_on_run_status() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);
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
    set_run_status_raw(home.path(), &run_id, "failed", Some("already terminal"));

    let pending = rounds::reconcile::attempts_awaiting_verdict(&db).unwrap();
    assert_eq!(
        pending.len(),
        1,
        "an already-failed run's unresolved forward is still selected: {pending:?}"
    );
    assert_eq!(pending[0].branch, "feat");
    assert_eq!(pending[0].bare_path, home.path().join("bare.git"));
}

#[test]
fn forward_outcome_requires_a_committed_intent() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);

    let orphan = rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Pushed,
            landed_sha: Some("aaa"),
            detail: None,
        },
    );
    assert!(
        matches!(orphan, Err(rounds::ForwardError::IntentMissing)),
        "an outcome without an intent is refused: {orphan:?}"
    );

    let not_outcome = rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Intent,
            landed_sha: None,
            detail: None,
        },
    );
    assert!(
        matches!(not_outcome, Err(rounds::ForwardError::NotAnOutcome(_))),
        "intent is not an outcome kind: {not_outcome:?}"
    );

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

    let no_sha = rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Pushed,
            landed_sha: None,
            detail: None,
        },
    );
    assert!(
        matches!(no_sha, Err(rounds::ForwardError::LandedShaMissing(_))),
        "a pushed outcome needs the landed sha: {no_sha:?}"
    );

    let no_detail = rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::PushFailed,
            landed_sha: None,
            detail: None,
        },
    );
    assert!(
        matches!(no_detail, Err(rounds::ForwardError::DetailMissing(_))),
        "a failed outcome needs a detail: {no_detail:?}"
    );
}

#[test]
fn forward_records_order_and_reached_origin_predicate() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);

    rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Present("bbb".into()),
        },
    )
    .unwrap();

    assert!(
        !rounds::forward::attempt_reached_origin(&db, &attempt).unwrap(),
        "an intent alone is the ambiguous case, not proof origin moved"
    );

    rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::Pushed,
            landed_sha: Some("aaa"),
            detail: None,
        },
    )
    .unwrap();

    assert!(
        rounds::forward::attempt_reached_origin(&db, &attempt).unwrap(),
        "a pushed outcome means origin carries the authorized sha"
    );

    let rows = rounds::forward::records_for_run(&db, &run_id).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].kind, rounds::ForwardKind::Intent);
    assert_eq!(
        rows[0].observed,
        Some(rounds::ObservedRemote::Present("bbb".into()))
    );
    assert_eq!(rows[1].kind, rounds::ForwardKind::Pushed);
    assert_eq!(rows[1].landed_sha.as_deref(), Some("aaa"));
    assert!(rows[0].seq < rows[1].seq, "{rows:?}");
}

#[test]
fn already_current_outcome_counts_as_reaching_origin() {
    let home = TempDir::new().unwrap();
    let db = fixture_db(home.path());
    let run_id = seed_run(&db, home.path());
    let attempt = seed_deliver_attempt(&db, &run_id);

    rounds::forward::append_intent(
        &db,
        &rounds::ForwardIntent {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            observed: rounds::ObservedRemote::Present("aaa".into()),
        },
    )
    .unwrap();
    rounds::forward::append_outcome(
        &db,
        &rounds::ForwardOutcome {
            run_id: &run_id,
            deliver_attempt_id: &attempt,
            ref_name: "refs/heads/feat",
            authorized_sha: "aaa",
            kind: rounds::ForwardKind::AlreadyCurrent,
            landed_sha: Some("aaa"),
            detail: None,
        },
    )
    .unwrap();

    assert!(rounds::forward::attempt_reached_origin(&db, &attempt).unwrap());
    let rows = rounds::forward::records_for_run(&db, &run_id).unwrap();
    assert_eq!(rows[1].kind, rounds::ForwardKind::AlreadyCurrent);
}
