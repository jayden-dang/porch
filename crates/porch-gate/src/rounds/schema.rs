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
    snapshot_digest TEXT REFERENCES content_blobs(digest),
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
";

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    ensure_column(
        conn,
        "runs",
        "review_history_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(ROUND_DDL)?;
    Ok(())
}
