//! Durable review-round store.

mod schema;

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::db::Db;
use crate::{Error, Result};

pub(crate) use schema::migrate;

/// Stable id for one review round.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoundId(String);

impl RoundId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoundId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RoundId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Execution lifecycle of a recorded round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionState {
    Running,
    Finished,
    Interrupted,
}

impl ExecutionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "running" => Ok(Self::Running),
            "finished" => Ok(Self::Finished),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(Error::Other(format!("unknown execution state: {other}"))),
        }
    }
}

/// Whether assurance for the round reached a terminal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssuranceCompletion {
    Pending,
    Complete,
    Incomplete,
}

impl AssuranceCompletion {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "complete" => Ok(Self::Complete),
            "incomplete" => Ok(Self::Incomplete),
            other => Err(Error::Other(format!(
                "unknown assurance completion: {other}"
            ))),
        }
    }
}

/// Whether a review-context element's source was readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Absent,
    Present,
    Unreadable,
}

impl SourceState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Present => "present",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Whether a snapshot of element bytes was retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotState {
    Stored,
    Omitted,
}

impl SnapshotState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Omitted => "omitted",
        }
    }
}

/// Whether a context element was applied to a producer or layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextApplicationState {
    Applied,
    NotApplied,
}

impl ContextApplicationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::NotApplied => "not_applied",
        }
    }
}

/// One producer invocation to record when opening a round.
#[derive(Debug, Clone)]
pub struct ProducerInvocation {
    pub descriptor_json: String,
    pub descriptor_equivalence_digest: String,
}

/// Plan describing the producers that will run for this round.
#[derive(Debug, Clone)]
pub struct OpenRoundPlan {
    pub run_id: String,
    pub producers: Vec<ProducerInvocation>,
}

/// Review-context element binding captured at open.
#[derive(Debug, Clone)]
pub struct ContextElement {
    pub element_name: String,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub snapshot_state: SnapshotState,
    pub snapshot_reason: Option<String>,
    pub snapshot_digest: Option<String>,
    pub snapshot_bytes: Option<Vec<u8>>,
}

/// Per-producer application of a context element.
#[derive(Debug, Clone)]
pub struct ContextApplication {
    pub element_name: String,
    pub producer_slot: usize,
    pub application: ContextApplicationState,
    pub effective_digest: Option<String>,
}

/// Input and review-context binding for a round open.
#[derive(Debug, Clone)]
pub struct RoundBindings {
    pub from_sha: String,
    pub to_sha: String,
    pub inventory_digest: String,
    pub inventory_bytes: Vec<u8>,
    pub trusted_config_sha: String,
    pub protocol_schema_version: i64,
    pub fingerprint_version: i64,
    pub context_elements: Vec<ContextElement>,
    pub context_applications: Vec<ContextApplication>,
}

/// Committed review round row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundRecord {
    pub id: RoundId,
    pub run_id: String,
    pub ordinal: i64,
    pub from_sha: String,
    pub to_sha: String,
    pub inventory_digest: String,
    pub execution: ExecutionState,
    pub assurance_completion: AssuranceCompletion,
    pub completion_reason: Option<String>,
    pub trusted_config_sha: String,
    pub protocol_schema_version: i64,
    pub fingerprint_version: i64,
    pub opened_at: String,
    pub finalized_at: Option<String>,
}

/// Producer row recorded for a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerRecord {
    pub id: String,
    pub slot: i64,
    pub descriptor_json: String,
    pub descriptor_equivalence_digest: String,
}

/// Hex-encoded SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Open a review round: commit the binding, then return its id.
///
/// # Errors
///
/// Returns a storage error when the transaction cannot commit, when a content
/// digest does not match the bytes (or collides with a different blob), or when
/// producer/context rows are invalid.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn open_round(db: &Db, plan: &OpenRoundPlan, bindings: &RoundBindings) -> Result<RoundId> {
    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;

    ensure_blob(&tx, &bindings.inventory_digest, &bindings.inventory_bytes)?;
    for element in &bindings.context_elements {
        if let (Some(digest), Some(bytes)) = (&element.snapshot_digest, &element.snapshot_bytes) {
            ensure_blob(&tx, digest, bytes)?;
        }
    }

    let ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM review_rounds WHERE run_id = ?1",
        [&plan.run_id],
        |row| row.get(0),
    )?;

    let round_id = Ulid::new().to_string();
    let opened_at = now_secs();
    tx.execute(
        "INSERT INTO review_rounds (
            id, run_id, ordinal, from_sha, to_sha, inventory_digest,
            execution, assurance_completion, completion_reason,
            trusted_config_sha, protocol_schema_version, fingerprint_version,
            opened_at, finalized_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, NULL)",
        rusqlite::params![
            round_id,
            plan.run_id,
            ordinal,
            bindings.from_sha,
            bindings.to_sha,
            bindings.inventory_digest,
            ExecutionState::Running.as_str(),
            AssuranceCompletion::Pending.as_str(),
            bindings.trusted_config_sha,
            bindings.protocol_schema_version,
            bindings.fingerprint_version,
            opened_at,
        ],
    )?;

    let mut producer_ids = Vec::with_capacity(plan.producers.len());
    for (slot, producer) in plan.producers.iter().enumerate() {
        let producer_id = Ulid::new().to_string();
        let slot_i =
            i64::try_from(slot).map_err(|_| Error::Other("producer slot exceeds i64".into()))?;
        tx.execute(
            "INSERT INTO round_producers (
                id, round_id, slot, descriptor_json, descriptor_equivalence_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                producer_id,
                round_id,
                slot_i,
                producer.descriptor_json,
                producer.descriptor_equivalence_digest,
            ],
        )?;
        producer_ids.push(producer_id);
    }

    for element in &bindings.context_elements {
        tx.execute(
            "INSERT INTO round_context_elements (
                round_id, element_name, source_state, source_reason,
                snapshot_state, snapshot_reason, snapshot_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                round_id,
                element.element_name,
                element.source_state.as_str(),
                element.source_reason,
                element.snapshot_state.as_str(),
                element.snapshot_reason,
                element.snapshot_digest,
            ],
        )?;
    }

    for application in &bindings.context_applications {
        let producer_id = producer_ids.get(application.producer_slot).ok_or_else(|| {
            Error::Other(format!(
                "context application references missing producer slot {}",
                application.producer_slot
            ))
        })?;
        tx.execute(
            "INSERT INTO round_context_applications (
                round_id, element_name, producer_invocation_id, application, effective_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                round_id,
                application.element_name,
                producer_id,
                application.application.as_str(),
                application.effective_digest,
            ],
        )?;
    }

    tx.commit()?;
    Ok(RoundId(round_id))
}

/// Load a committed round by id.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn get_round(db: &Db, id: &RoundId) -> Result<Option<RoundRecord>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, run_id, ordinal, from_sha, to_sha, inventory_digest,
                execution, assurance_completion, completion_reason,
                trusted_config_sha, protocol_schema_version, fingerprint_version,
                opened_at, finalized_at
         FROM review_rounds WHERE id = ?1",
    )?;
    let mut rows = stmt.query([id.as_str()])?;
    if let Some(row) = rows.next()? {
        return Ok(Some(map_round(row)?));
    }
    Ok(None)
}

/// Rounds for a run in ordinal order.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn rounds_for_run(db: &Db, run_id: &str) -> Result<Vec<RoundRecord>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, run_id, ordinal, from_sha, to_sha, inventory_digest,
                execution, assurance_completion, completion_reason,
                trusted_config_sha, protocol_schema_version, fingerprint_version,
                opened_at, finalized_at
         FROM review_rounds WHERE run_id = ?1 ORDER BY ordinal",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map_round(row)?);
    }
    Ok(out)
}

/// Producer invocations recorded for a round, ordered by slot.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn producers_for_round(db: &Db, round_id: &RoundId) -> Result<Vec<ProducerRecord>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, slot, descriptor_json, descriptor_equivalence_digest
         FROM round_producers WHERE round_id = ?1 ORDER BY slot",
    )?;
    let rows = stmt.query_map([round_id.as_str()], |row| {
        Ok(ProducerRecord {
            id: row.get(0)?,
            slot: row.get(1)?,
            descriptor_json: row.get(2)?,
            descriptor_equivalence_digest: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn ensure_blob(tx: &Transaction<'_>, digest: &str, bytes: &[u8]) -> Result<()> {
    let expected = sha256_hex(bytes);
    if digest != expected {
        return Err(Error::Other(format!(
            "content blob digest {digest} does not match sha256 of provided bytes"
        )));
    }

    let byte_length =
        i64::try_from(bytes.len()).map_err(|_| Error::Other("blob length exceeds i64".into()))?;

    match tx.query_row(
        "SELECT byte_length, bytes FROM content_blobs WHERE digest = ?1",
        [digest],
        |row| {
            let len: i64 = row.get(0)?;
            let stored: Vec<u8> = row.get(1)?;
            Ok((len, stored))
        },
    ) {
        Ok((len, stored)) => {
            if len != byte_length || stored.as_slice() != bytes {
                return Err(Error::Other(format!(
                    "content blob digest {digest} collides with different stored bytes"
                )));
            }
            Ok(())
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tx.execute(
                "INSERT INTO content_blobs (digest, byte_length, bytes) VALUES (?1, ?2, ?3)",
                rusqlite::params![digest, byte_length, bytes],
            )?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn map_round(row: &rusqlite::Row<'_>) -> Result<RoundRecord> {
    let execution: String = row.get(6)?;
    let assurance: String = row.get(7)?;
    Ok(RoundRecord {
        id: RoundId(row.get(0)?),
        run_id: row.get(1)?,
        ordinal: row.get(2)?,
        from_sha: row.get(3)?,
        to_sha: row.get(4)?,
        inventory_digest: row.get(5)?,
        execution: ExecutionState::parse(&execution)?,
        assurance_completion: AssuranceCompletion::parse(&assurance)?,
        completion_reason: row.get(8)?,
        trusted_config_sha: row.get(9)?,
        protocol_schema_version: row.get(10)?,
        fingerprint_version: row.get(11)?,
        opened_at: row.get(12)?,
        finalized_at: row.get(13)?,
    })
}

fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |d| d.as_secs().to_string())
}
