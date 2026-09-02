//! M18: review round store — migration and durable open.

use std::path::Path;

use porch_gate::db_path;
use porch_gate::rounds::{
    self, AssuranceCompletion, ContextApplication, ContextElement, ContextSource, ExecutionState,
    OpenRoundPlan, ProducerInvocation, RoundBindings, SNAPSHOT_CEILING_BYTES, SnapshotState,
    SourceState, capture_context_element, context_applicability_digest, sha256_hex,
};
use porch_gate::{Db, Error};
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
        protocol_schema_version: 1,
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
    assert_eq!(loaded.protocol_schema_version, 1);
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
