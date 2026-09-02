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

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "absent" => Ok(Self::Absent),
            "present" => Ok(Self::Present),
            "unreadable" => Ok(Self::Unreadable),
            other => Err(Error::Other(format!("unknown source state: {other}"))),
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

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "stored" => Ok(Self::Stored),
            "omitted" => Ok(Self::Omitted),
            other => Err(Error::Other(format!("unknown snapshot state: {other}"))),
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

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "applied" => Ok(Self::Applied),
            "not_applied" => Ok(Self::NotApplied),
            other => Err(Error::Other(format!(
                "unknown context application: {other}"
            ))),
        }
    }
}

/// Maximum retained snapshot bytes per context element (256 KiB).
pub const SNAPSHOT_CEILING_BYTES: usize = 256 * 1024;

/// Observed source of a review-context element before snapshotting.
#[derive(Debug, Clone)]
pub enum ContextSource {
    Absent { reason: Option<String> },
    Present { bytes: Vec<u8> },
    Unreadable { reason: String },
}

/// Build a context element, omitting snapshot bytes above the ceiling.
#[must_use]
pub fn capture_context_element(name: impl Into<String>, source: ContextSource) -> ContextElement {
    match source {
        ContextSource::Absent { reason } => ContextElement {
            element_name: name.into(),
            source_state: SourceState::Absent,
            source_reason: reason,
            snapshot_state: SnapshotState::Omitted,
            snapshot_reason: None,
            snapshot_digest: None,
            snapshot_bytes: None,
        },
        ContextSource::Unreadable { reason } => ContextElement {
            element_name: name.into(),
            source_state: SourceState::Unreadable,
            source_reason: Some(reason),
            snapshot_state: SnapshotState::Omitted,
            snapshot_reason: None,
            snapshot_digest: None,
            snapshot_bytes: None,
        },
        ContextSource::Present { bytes } => {
            let digest = sha256_hex(&bytes);
            if bytes.len() > SNAPSHOT_CEILING_BYTES {
                ContextElement {
                    element_name: name.into(),
                    source_state: SourceState::Present,
                    source_reason: None,
                    snapshot_state: SnapshotState::Omitted,
                    snapshot_reason: Some("too_large".into()),
                    snapshot_digest: Some(digest),
                    snapshot_bytes: None,
                }
            } else {
                ContextElement {
                    element_name: name.into(),
                    source_state: SourceState::Present,
                    source_reason: None,
                    snapshot_state: SnapshotState::Stored,
                    snapshot_reason: None,
                    snapshot_digest: Some(digest),
                    snapshot_bytes: Some(bytes),
                }
            }
        }
    }
}

/// Applicability digest over the exact effective bytes a layer received.
#[must_use]
pub fn context_applicability_digest(
    element_name: &str,
    effective_state_tag: &str,
    effective_bytes: &[u8],
) -> String {
    let len = effective_bytes.len().to_string();
    let preimage = length_delimited_join(&[
        b"porch-review-context/v1",
        element_name.as_bytes(),
        effective_state_tag.as_bytes(),
        len.as_bytes(),
        effective_bytes,
    ]);
    sha256_hex(&preimage)
}

fn length_delimited_join(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(0x1F);
        }
        out.extend_from_slice(part.len().to_string().as_bytes());
        out.extend_from_slice(part);
    }
    out
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
    /// Audit-only; outside the review-context applicability binding.
    pub intent_source: Option<String>,
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
    pub intent_source: Option<String>,
    pub protocol_schema_version: i64,
    pub fingerprint_version: i64,
    pub opened_at: String,
    pub finalized_at: Option<String>,
}

/// Persisted review-context element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextElementRecord {
    pub element_name: String,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub snapshot_state: SnapshotState,
    pub snapshot_reason: Option<String>,
    pub snapshot_digest: Option<String>,
    pub snapshot_bytes: Option<Vec<u8>>,
}

/// Persisted per-recipient context application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextApplicationRecord {
    pub element_name: String,
    pub producer_invocation_id: String,
    pub application: ContextApplicationState,
    pub effective_digest: Option<String>,
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

    let elements: Vec<ContextElement> = bindings
        .context_elements
        .iter()
        .map(normalize_context_element)
        .collect();

    ensure_blob(&tx, &bindings.inventory_digest, &bindings.inventory_bytes)?;
    ensure_stored_snapshots(&tx, &elements)?;

    let ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM review_rounds WHERE run_id = ?1",
        [&plan.run_id],
        |row| row.get(0),
    )?;

    let round_id = Ulid::new().to_string();
    insert_review_round(&tx, &round_id, ordinal, plan, bindings)?;
    let producer_ids = insert_producers(&tx, &round_id, plan)?;
    insert_context_rows(&tx, &round_id, &elements, &producer_ids, bindings)?;

    tx.commit()?;
    Ok(RoundId(round_id))
}

fn ensure_stored_snapshots(tx: &Transaction<'_>, elements: &[ContextElement]) -> Result<()> {
    for element in elements {
        if element.snapshot_state == SnapshotState::Stored {
            if let (Some(digest), Some(bytes)) = (&element.snapshot_digest, &element.snapshot_bytes)
            {
                ensure_blob(tx, digest, bytes)?;
            }
        }
    }
    Ok(())
}

fn insert_review_round(
    tx: &Transaction<'_>,
    round_id: &str,
    ordinal: i64,
    plan: &OpenRoundPlan,
    bindings: &RoundBindings,
) -> Result<()> {
    tx.execute(
        "INSERT INTO review_rounds (
            id, run_id, ordinal, from_sha, to_sha, inventory_digest,
            execution, assurance_completion, completion_reason,
            trusted_config_sha, intent_source, protocol_schema_version, fingerprint_version,
            opened_at, finalized_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13, NULL)",
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
            bindings.intent_source,
            bindings.protocol_schema_version,
            bindings.fingerprint_version,
            now_secs(),
        ],
    )?;
    Ok(())
}

fn insert_producers(
    tx: &Transaction<'_>,
    round_id: &str,
    plan: &OpenRoundPlan,
) -> Result<Vec<String>> {
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
    Ok(producer_ids)
}

fn insert_context_rows(
    tx: &Transaction<'_>,
    round_id: &str,
    elements: &[ContextElement],
    producer_ids: &[String],
    bindings: &RoundBindings,
) -> Result<()> {
    for element in elements {
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
    Ok(())
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
                trusted_config_sha, intent_source, protocol_schema_version, fingerprint_version,
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
                trusted_config_sha, intent_source, protocol_schema_version, fingerprint_version,
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

/// Context elements recorded for a round.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn context_elements_for_round(
    db: &Db,
    round_id: &RoundId,
) -> Result<Vec<ContextElementRecord>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT e.element_name, e.source_state, e.source_reason,
                e.snapshot_state, e.snapshot_reason, e.snapshot_digest,
                b.bytes
         FROM round_context_elements e
         LEFT JOIN content_blobs b
           ON e.snapshot_state = 'stored' AND e.snapshot_digest = b.digest
         WHERE e.round_id = ?1
         ORDER BY e.element_name",
    )?;
    let mut rows = stmt.query([round_id.as_str()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let source_state: String = row.get(1)?;
        let snapshot_state: String = row.get(3)?;
        let snapshot_state = SnapshotState::parse(&snapshot_state)?;
        let snapshot_bytes: Option<Vec<u8>> = row.get(6)?;
        out.push(ContextElementRecord {
            element_name: row.get(0)?,
            source_state: SourceState::parse(&source_state)?,
            source_reason: row.get(2)?,
            snapshot_state,
            snapshot_reason: row.get(4)?,
            snapshot_digest: row.get(5)?,
            snapshot_bytes: if snapshot_state == SnapshotState::Stored {
                snapshot_bytes
            } else {
                None
            },
        });
    }
    Ok(out)
}

/// Context applications recorded for a round.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn context_applications_for_round(
    db: &Db,
    round_id: &RoundId,
) -> Result<Vec<ContextApplicationRecord>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT element_name, producer_invocation_id, application, effective_digest
         FROM round_context_applications
         WHERE round_id = ?1
         ORDER BY element_name, producer_invocation_id",
    )?;
    let mut rows = stmt.query([round_id.as_str()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let application: String = row.get(2)?;
        out.push(ContextApplicationRecord {
            element_name: row.get(0)?,
            producer_invocation_id: row.get(1)?,
            application: ContextApplicationState::parse(&application)?,
            effective_digest: row.get(3)?,
        });
    }
    Ok(out)
}

fn normalize_context_element(element: &ContextElement) -> ContextElement {
    match (
        element.source_state,
        element.snapshot_state,
        &element.snapshot_bytes,
    ) {
        (SourceState::Present, SnapshotState::Stored, Some(bytes))
            if bytes.len() > SNAPSHOT_CEILING_BYTES =>
        {
            let digest = element
                .snapshot_digest
                .clone()
                .unwrap_or_else(|| sha256_hex(bytes));
            ContextElement {
                element_name: element.element_name.clone(),
                source_state: SourceState::Present,
                source_reason: element.source_reason.clone(),
                snapshot_state: SnapshotState::Omitted,
                snapshot_reason: Some("too_large".into()),
                snapshot_digest: Some(digest),
                snapshot_bytes: None,
            }
        }
        _ => element.clone(),
    }
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
        intent_source: row.get(10)?,
        protocol_schema_version: row.get(11)?,
        fingerprint_version: row.get(12)?,
        opened_at: row.get(13)?,
        finalized_at: row.get(14)?,
    })
}

fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |d| d.as_secs().to_string())
}
