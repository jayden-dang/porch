use rusqlite::Connection;

use crate::Result;
use crate::db::ensure_column;

const ROUND_DDL: &str = "
CREATE TABLE IF NOT EXISTS content_blobs (
    digest TEXT PRIMARY KEY,
    byte_length INTEGER NOT NULL,
    bytes BLOB NOT NULL,
    CHECK (byte_length = length(bytes))
);

CREATE TABLE IF NOT EXISTS review_rounds (
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
    intent_source TEXT,
    protocol_schema_version INTEGER NOT NULL,
    fingerprint_version INTEGER NOT NULL,
    opened_at TEXT NOT NULL,
    finalized_at TEXT,
    UNIQUE (run_id, ordinal)
);
CREATE INDEX IF NOT EXISTS review_rounds_open
    ON review_rounds(execution, assurance_completion);

CREATE TABLE IF NOT EXISTS round_producers (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    slot INTEGER NOT NULL,
    descriptor_json TEXT NOT NULL,
    descriptor_equivalence_digest TEXT NOT NULL,
    UNIQUE (round_id, slot),
    UNIQUE (round_id, id)
);
CREATE INDEX IF NOT EXISTS round_producers_equiv
    ON round_producers(descriptor_equivalence_digest);

CREATE TABLE IF NOT EXISTS round_context_elements (
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    element_name TEXT NOT NULL,
    source_state TEXT NOT NULL
        CHECK (source_state IN ('absent','present','unreadable')),
    source_reason TEXT,
    snapshot_state TEXT NOT NULL CHECK (snapshot_state IN ('stored','omitted')),
    snapshot_reason TEXT,
    snapshot_digest TEXT,
    PRIMARY KEY (round_id, element_name)
);

CREATE TABLE IF NOT EXISTS round_context_applications (
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

CREATE TABLE IF NOT EXISTS round_coverage (
    round_id TEXT NOT NULL,
    producer_invocation_id TEXT NOT NULL,
    path TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('selected','completed','failed','waived')),
    reason TEXT,
    authority TEXT,
    completion_evidence TEXT,
    PRIMARY KEY (producer_invocation_id, path),
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
    CHECK (state <> 'waived' OR authority IS NOT NULL),
    CHECK (state NOT IN ('failed','waived') OR reason IS NOT NULL),
    CHECK (state <> 'completed' OR completion_evidence IS NOT NULL)
);

CREATE TABLE IF NOT EXISTS finding_instances (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    producer_invocation_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    fingerprint_version INTEGER NOT NULL,
    candidate_key TEXT NOT NULL,
    criterion_id TEXT NOT NULL,
    evidence TEXT NOT NULL,
    consequence TEXT NOT NULL,
    action TEXT NOT NULL,
    severity TEXT NOT NULL,
    provenance_json TEXT NOT NULL,
    confidence_value TEXT,
    confidence_kind TEXT,
    path TEXT NOT NULL,
    anchor_kind TEXT NOT NULL,
    anchor_value TEXT,
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
    CHECK ((confidence_value IS NULL) = (confidence_kind IS NULL))
);
CREATE INDEX IF NOT EXISTS finding_instances_fp
    ON finding_instances(fingerprint, fingerprint_version);
CREATE INDEX IF NOT EXISTS finding_instances_round
    ON finding_instances(round_id);

CREATE TABLE IF NOT EXISTS round_required_producers (
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    requirement_slot INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('floor','judgment')),
    resolution TEXT NOT NULL CHECK (resolution IN ('resolved','unresolved')),
    expected_equivalence_digest TEXT,
    producer_invocation_id TEXT,
    resolution_reason TEXT,
    PRIMARY KEY (round_id, requirement_slot),
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id),
    CHECK (
        (resolution = 'resolved'
            AND expected_equivalence_digest IS NOT NULL
            AND producer_invocation_id IS NOT NULL)
     OR (resolution = 'unresolved'
            AND expected_equivalence_digest IS NULL
            AND producer_invocation_id IS NULL
            AND resolution_reason IS NOT NULL
            AND length(trim(resolution_reason)) > 0)
    )
);

CREATE TABLE IF NOT EXISTS round_producer_durations (
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    producer_invocation_id TEXT NOT NULL,
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    PRIMARY KEY (round_id, producer_invocation_id),
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id)
);

CREATE TABLE IF NOT EXISTS authority_events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    kind TEXT NOT NULL CHECK (kind IN (
        'review_approved', 'review_skipped', 'fix_requested', 'review_aborted'
    )),
    review_round_id TEXT REFERENCES review_rounds(id),
    reviewed_head TEXT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('operator', 'porch')),
    authority_event_id TEXT REFERENCES authority_events(id),
    head_changed INTEGER CHECK (head_changed IN (0, 1)),
    identity_unavailable INTEGER NOT NULL DEFAULT 0 CHECK (identity_unavailable IN (0, 1)),
    created_at TEXT NOT NULL,
    CHECK (
        (identity_unavailable = 1 AND review_round_id IS NULL)
        OR (identity_unavailable = 0)
    )
);
CREATE INDEX IF NOT EXISTS authority_events_run
    ON authority_events(run_id, created_at, id);

CREATE TABLE IF NOT EXISTS authority_event_members (
    event_id TEXT NOT NULL REFERENCES authority_events(id) ON DELETE CASCADE,
    finding_instance_id TEXT NOT NULL REFERENCES finding_instances(id),
    role TEXT NOT NULL CHECK (role IN ('context', 'target')),
    PRIMARY KEY (event_id, finding_instance_id, role)
);
CREATE INDEX IF NOT EXISTS authority_event_members_instance
    ON authority_event_members(finding_instance_id);

CREATE TABLE IF NOT EXISTS phase_attempts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    phase TEXT NOT NULL CHECK (phase IN ('intent','rebase','review','certify','deliver')),
    ordinal INTEGER NOT NULL,
    parent_attempt_id TEXT REFERENCES phase_attempts(id),
    caused_by_attempt_id TEXT REFERENCES phase_attempts(id),
    operation_kind TEXT CHECK (
        operation_kind IS NULL
        OR operation_kind IN ('compose','fixer','deliver_repair')
    ),
    created_at TEXT NOT NULL,
    CHECK (
        (parent_attempt_id IS NULL AND operation_kind IS NULL)
        OR (parent_attempt_id IS NOT NULL AND operation_kind IS NOT NULL)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS phase_attempts_run_phase_ordinal
    ON phase_attempts(run_id, phase, ordinal)
    WHERE parent_attempt_id IS NULL;

CREATE TABLE IF NOT EXISTS phase_events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    attempt_id TEXT NOT NULL REFERENCES phase_attempts(id),
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('started','terminal','evidence')),
    outcome TEXT,
    cause TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS phase_events_run
    ON phase_events(run_id, seq);

CREATE TABLE IF NOT EXISTS forward_records (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    deliver_attempt_id TEXT NOT NULL REFERENCES phase_attempts(id),
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('intent','pushed','already_current','push_failed')),
    ref_name TEXT NOT NULL,
    authorized_sha TEXT NOT NULL,
    remote_state TEXT CHECK (remote_state IN ('absent','present')),
    observed_remote_tip TEXT,
    landed_sha TEXT,
    detail TEXT,
    created_at TEXT NOT NULL,
    CHECK (
        kind <> 'intent'
        OR (
            remote_state IS NOT NULL
            AND ((remote_state = 'present') = (observed_remote_tip IS NOT NULL))
            AND landed_sha IS NULL
        )
    ),
    CHECK (
        kind = 'intent'
        OR (remote_state IS NULL AND observed_remote_tip IS NULL)
    ),
    CHECK (kind NOT IN ('pushed','already_current') OR landed_sha IS NOT NULL),
    CHECK (kind <> 'push_failed' OR detail IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS forward_records_run
    ON forward_records(run_id, seq);
CREATE UNIQUE INDEX IF NOT EXISTS forward_records_attempt_intent
    ON forward_records(deliver_attempt_id)
    WHERE kind = 'intent';
";

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    ensure_column(
        conn,
        "runs",
        "review_history_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(ROUND_DDL)?;
    ensure_column(conn, "review_rounds", "intent_source", "TEXT")?;
    ensure_column(conn, "review_rounds", "review_duration_ms", "INTEGER")?;
    rebuild_context_elements_without_snapshot_blob_fk(conn)?;
    Ok(())
}

fn snapshot_digest_fk_targets_blobs(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_list('round_context_elements')")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let table: String = row.get(2)?;
        let from: String = row.get(3)?;
        if from == "snapshot_digest" && table == "content_blobs" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rebuild_context_elements_without_snapshot_blob_fk(conn: &Connection) -> Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'round_context_elements'",
        [],
        |row| row.get(0),
    )?;
    if exists == 0 || !snapshot_digest_fk_targets_blobs(conn)? {
        return Ok(());
    }

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.execute_batch(
        "
        CREATE TABLE round_context_elements__new (
            round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
            element_name TEXT NOT NULL,
            source_state TEXT NOT NULL
                CHECK (source_state IN ('absent','present','unreadable')),
            source_reason TEXT,
            snapshot_state TEXT NOT NULL CHECK (snapshot_state IN ('stored','omitted')),
            snapshot_reason TEXT,
            snapshot_digest TEXT,
            PRIMARY KEY (round_id, element_name)
        );
        INSERT INTO round_context_elements__new (
            round_id, element_name, source_state, source_reason,
            snapshot_state, snapshot_reason, snapshot_digest
        )
        SELECT
            round_id, element_name, source_state, source_reason,
            snapshot_state, snapshot_reason, snapshot_digest
        FROM round_context_elements;
        DROP TABLE round_context_elements;
        ALTER TABLE round_context_elements__new RENAME TO round_context_elements;
        ",
    )?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}
