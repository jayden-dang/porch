//! M18: review round store — migration and durable open.

use std::path::Path;

use porch_gate::db_path;
use porch_gate::rounds::{
    self, AssuranceCompletion, ContextApplication, ContextElement, ExecutionState, OpenRoundPlan,
    ProducerInvocation, RoundBindings, SnapshotState, SourceState, sha256_hex,
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
    RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest.clone(),
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: 1,
        fingerprint_version: 1,
        context_elements: vec![ContextElement {
            element_name: "intent".into(),
            source_state: SourceState::Present,
            source_reason: None,
            snapshot_state: SnapshotState::Stored,
            snapshot_reason: None,
            snapshot_digest: Some(digest.clone()),
            snapshot_bytes: Some(inventory.to_vec()),
        }],
        context_applications: vec![ContextApplication {
            element_name: "intent".into(),
            producer_slot: 0,
            application: rounds::ContextApplicationState::Applied,
            effective_digest: Some(digest),
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
    assert_eq!(effective.as_deref(), Some(digest.as_str()));
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
