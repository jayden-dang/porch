//! Durable evidence of one forward attempt: an intent committed before the
//! pushing command mutates `origin`, and an outcome committed after it.
//!
//! The log is porch-owned evidence, never an approval (ARCH-11), and it is
//! written from porch's own command result rather than from remote state.

use std::fmt;

use rusqlite::{Transaction, TransactionBehavior};
use ulid::Ulid;

use super::phase::AttemptId;
use crate::Result;
use crate::db::{self, Db};

use db::now_secs;

/// What a forward record says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardKind {
    /// Porch is about to invoke the pushing command for this attempt.
    Intent,
    /// Porch's pushing command reported success and moved objects.
    Pushed,
    /// The lease observation found the ref already at the authorized SHA.
    AlreadyCurrent,
    /// Porch's pushing command reported failure.
    PushFailed,
}

impl ForwardKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Pushed => "pushed",
            Self::AlreadyCurrent => "already_current",
            Self::PushFailed => "push_failed",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "intent" => Ok(Self::Intent),
            "pushed" => Ok(Self::Pushed),
            "already_current" => Ok(Self::AlreadyCurrent),
            "push_failed" => Ok(Self::PushFailed),
            other => Err(crate::Error::Other(format!(
                "unknown forward record kind: {other}"
            ))),
        }
    }

    /// Whether this kind concludes an attempt rather than opening one.
    #[must_use]
    pub fn is_outcome(self) -> bool {
        matches!(self, Self::Pushed | Self::AlreadyCurrent | Self::PushFailed)
    }

    /// Whether this kind means `origin` carries the authorized SHA.
    #[must_use]
    pub fn reached_origin(self) -> bool {
        matches!(self, Self::Pushed | Self::AlreadyCurrent)
    }
}

impl fmt::Display for ForwardKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The remote tip the lease was resolved against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedRemote {
    /// The ref does not exist on the remote.
    Absent,
    /// The ref resolves to this SHA.
    Present(String),
}

impl ObservedRemote {
    fn state(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Present(_) => "present",
        }
    }

    fn tip(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Present(sha) => Some(sha),
        }
    }

    fn parse(state: &str, tip: Option<String>) -> Result<Self> {
        match (state, tip) {
            ("absent", None) => Ok(Self::Absent),
            ("present", Some(sha)) => Ok(Self::Present(sha)),
            (other, _) => Err(crate::Error::Other(format!(
                "unreadable observed remote state: {other}"
            ))),
        }
    }
}

/// Input for the record committed before the pushing command runs.
#[derive(Debug, Clone)]
pub struct ForwardIntent<'a> {
    pub run_id: &'a str,
    pub deliver_attempt_id: &'a AttemptId,
    pub ref_name: &'a str,
    pub authorized_sha: &'a str,
    pub observed: ObservedRemote,
}

/// Input for the record committed after the pushing command returns.
#[derive(Debug, Clone)]
pub struct ForwardOutcome<'a> {
    pub run_id: &'a str,
    pub deliver_attempt_id: &'a AttemptId,
    pub ref_name: &'a str,
    pub authorized_sha: &'a str,
    pub kind: ForwardKind,
    /// Required for `Pushed` and `AlreadyCurrent`.
    pub landed_sha: Option<&'a str>,
    /// Required for `PushFailed`.
    pub detail: Option<&'a str>,
}

/// One committed forward record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRecordRow {
    pub id: String,
    pub run_id: String,
    pub deliver_attempt_id: AttemptId,
    pub seq: i64,
    pub kind: ForwardKind,
    pub ref_name: String,
    pub authorized_sha: String,
    pub observed: Option<ObservedRemote>,
    pub landed_sha: Option<String>,
    pub detail: Option<String>,
    pub created_at: String,
}

/// Why an append was refused.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error(
        "forward intent refused: this deliver attempt already forwarded; a retry takes a new attempt"
    )]
    IntentExists,
    #[error("forward outcome refused: attempt has no committed intent record")]
    IntentMissing,
    #[error("forward outcome refused: {0} is not an outcome kind")]
    NotAnOutcome(ForwardKind),
    #[error("forward outcome refused: {0} requires a landed sha")]
    LandedShaMissing(ForwardKind),
    #[error("forward outcome refused: {0} requires a detail")]
    DetailMissing(ForwardKind),
    #[error(transparent)]
    Storage(#[from] crate::Error),
}

/// Append the intent record for a forward attempt.
///
/// Commits before the caller invokes any command that mutates `origin`
/// (ARCH-13). Refuses a second intent for the same `deliver` attempt.
///
/// # Errors
///
/// Returns [`ForwardError::IntentExists`] when the attempt already has an
/// intent, or a storage error when the transaction cannot commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn append_intent(
    db: &Db,
    intent: &ForwardIntent<'_>,
) -> std::result::Result<String, ForwardError> {
    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
        .map_err(crate::Error::from)?;

    if intent_exists_tx(&tx, intent.deliver_attempt_id)? {
        return Err(ForwardError::IntentExists);
    }

    let id = Ulid::new().to_string();
    let seq = next_seq_tx(&tx, intent.run_id)?;
    tx.execute(
        "INSERT INTO forward_records (
            id, run_id, deliver_attempt_id, seq, kind, ref_name, authorized_sha,
            remote_state, observed_remote_tip, landed_sha, detail, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10)",
        rusqlite::params![
            &id,
            intent.run_id,
            intent.deliver_attempt_id.as_str(),
            seq,
            ForwardKind::Intent.as_str(),
            intent.ref_name,
            intent.authorized_sha,
            intent.observed.state(),
            intent.observed.tip(),
            &now_secs(),
        ],
    )
    .map_err(crate::Error::from)?;
    tx.commit().map_err(crate::Error::from)?;
    Ok(id)
}

/// Append the outcome record for a forward attempt.
///
/// Requires a committed intent for the same attempt, so the relation in the
/// spec is a property of the store rather than a convention of the caller.
///
/// # Errors
///
/// Returns [`ForwardError::NotAnOutcome`] for a non-outcome kind,
/// [`ForwardError::LandedShaMissing`] or [`ForwardError::DetailMissing`] when
/// the kind's evidence is absent, [`ForwardError::IntentMissing`] when the
/// attempt has no intent, or a storage error when the transaction cannot
/// commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn append_outcome(
    db: &Db,
    outcome: &ForwardOutcome<'_>,
) -> std::result::Result<String, ForwardError> {
    if !outcome.kind.is_outcome() {
        return Err(ForwardError::NotAnOutcome(outcome.kind));
    }
    if outcome.kind.reached_origin() && outcome.landed_sha.is_none() {
        return Err(ForwardError::LandedShaMissing(outcome.kind));
    }
    if outcome.kind == ForwardKind::PushFailed && outcome.detail.is_none() {
        return Err(ForwardError::DetailMissing(outcome.kind));
    }

    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
        .map_err(crate::Error::from)?;

    if !intent_exists_tx(&tx, outcome.deliver_attempt_id)? {
        return Err(ForwardError::IntentMissing);
    }

    let id = Ulid::new().to_string();
    let seq = next_seq_tx(&tx, outcome.run_id)?;
    tx.execute(
        "INSERT INTO forward_records (
            id, run_id, deliver_attempt_id, seq, kind, ref_name, authorized_sha,
            remote_state, observed_remote_tip, landed_sha, detail, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10)",
        rusqlite::params![
            &id,
            outcome.run_id,
            outcome.deliver_attempt_id.as_str(),
            seq,
            outcome.kind.as_str(),
            outcome.ref_name,
            outcome.authorized_sha,
            outcome.landed_sha,
            outcome.detail,
            &now_secs(),
        ],
    )
    .map_err(crate::Error::from)?;
    tx.commit().map_err(crate::Error::from)?;
    Ok(id)
}

/// Forward records for a run in sequence order.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn records_for_run(db: &Db, run_id: &str) -> Result<Vec<ForwardRecordRow>> {
    let conn = db.conn();
    records_for_run_conn(&conn, run_id)
}

/// Forward records for a run on an open connection, in sequence order.
///
/// # Errors
///
/// Returns a storage error if the query fails.
pub fn records_for_run_conn(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<Vec<ForwardRecordRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, run_id, deliver_attempt_id, seq, kind, ref_name, authorized_sha,
                remote_state, observed_remote_tip, landed_sha, detail, created_at
         FROM forward_records
         WHERE run_id = ?1
         ORDER BY seq, id",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map_record(row)?);
    }
    Ok(out)
}

/// Whether a forward attempt durably recorded that `origin` carries the
/// authorized SHA.
///
/// This is the reconciliation predicate: an attempt with an intent but no
/// reached-origin outcome is the ambiguous case that restart discovery owns.
/// Read it as positive evidence only. `false` is *not* proof that `origin` is
/// unchanged — a `push_failed` outcome, and an intent with no outcome, both
/// leave the remote's state undetermined here, because this reads porch's own
/// record and never probes `origin`.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn attempt_reached_origin(db: &Db, deliver_attempt_id: &AttemptId) -> Result<bool> {
    let conn = db.conn();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM forward_records
         WHERE deliver_attempt_id = ?1 AND kind IN ('pushed','already_current')",
        [deliver_attempt_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

fn intent_exists_tx(
    tx: &Transaction<'_>,
    attempt: &AttemptId,
) -> std::result::Result<bool, ForwardError> {
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM forward_records
             WHERE deliver_attempt_id = ?1 AND kind = 'intent'",
            [attempt.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(n > 0)
}

fn next_seq_tx(tx: &Transaction<'_>, run_id: &str) -> std::result::Result<i64, ForwardError> {
    let max: Option<i64> = tx
        .query_row(
            "SELECT MAX(seq) FROM forward_records WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(max.unwrap_or(0) + 1)
}

fn map_record(row: &rusqlite::Row<'_>) -> Result<ForwardRecordRow> {
    let kind = ForwardKind::parse(&row.get::<_, String>(4)?)?;
    let state: Option<String> = row.get(7)?;
    let tip: Option<String> = row.get(8)?;
    let observed = match state {
        Some(ref raw) => Some(ObservedRemote::parse(raw, tip)?),
        None => None,
    };
    Ok(ForwardRecordRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        deliver_attempt_id: AttemptId::from_raw(row.get::<_, String>(2)?),
        seq: row.get(3)?,
        kind,
        ref_name: row.get(5)?,
        authorized_sha: row.get(6)?,
        observed,
        landed_sha: row.get(9)?,
        detail: row.get(10)?,
        created_at: row.get(11)?,
    })
}
