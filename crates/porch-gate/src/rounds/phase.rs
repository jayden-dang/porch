use std::fmt;

use rusqlite::{Transaction, TransactionBehavior};
use ulid::Ulid;

use super::{RunEffects, StepEffect};
use crate::Result;
use crate::db::{self, Db};

use db::now_secs;

/// Stable id for one phase attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttemptId(String);

impl AttemptId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AttemptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for AttemptId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Canonical top-level phase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhaseName {
    Intent,
    Rebase,
    Review,
    Certify,
    Deliver,
}

impl PhaseName {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Rebase => "rebase",
            Self::Review => "review",
            Self::Certify => "certify",
            Self::Deliver => "deliver",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "intent" => Ok(Self::Intent),
            "rebase" => Ok(Self::Rebase),
            "review" => Ok(Self::Review),
            "certify" => Ok(Self::Certify),
            "deliver" => Ok(Self::Deliver),
            other => Err(crate::Error::Other(format!("unknown phase name: {other}"))),
        }
    }
}

/// Nested operation recorded under a parent phase attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    Compose,
    Fixer,
    DeliverRepair,
}

impl OperationKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compose => "compose",
            Self::Fixer => "fixer",
            Self::DeliverRepair => "deliver_repair",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "compose" => Ok(Self::Compose),
            "fixer" => Ok(Self::Fixer),
            "deliver_repair" => Ok(Self::DeliverRepair),
            other => Err(crate::Error::Other(format!(
                "unknown operation kind: {other}"
            ))),
        }
    }
}

/// Kind of a phase-event row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhaseEventKind {
    Started,
    Terminal,
    Evidence,
}

impl PhaseEventKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Terminal => "terminal",
            Self::Evidence => "evidence",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "started" => Ok(Self::Started),
            "terminal" => Ok(Self::Terminal),
            "evidence" => Ok(Self::Evidence),
            other => Err(crate::Error::Other(format!(
                "unknown phase event kind: {other}"
            ))),
        }
    }
}

/// Persisted phase-attempt row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseAttemptRow {
    pub id: AttemptId,
    pub run_id: String,
    pub phase: PhaseName,
    pub ordinal: i64,
    pub parent_attempt_id: Option<AttemptId>,
    pub caused_by_attempt_id: Option<AttemptId>,
    pub operation_kind: Option<OperationKind>,
    pub created_at: String,
}

/// Persisted phase-event row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseEventRow {
    pub id: String,
    pub run_id: String,
    pub attempt_id: AttemptId,
    pub seq: i64,
    pub kind: PhaseEventKind,
    pub outcome: Option<String>,
    pub cause: Option<String>,
    pub created_at: String,
}

/// One phase-lifecycle write, co-committed with [`RunEffects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseTransition {
    Start {
        run_id: String,
        phase: PhaseName,
    },
    Terminal {
        attempt: AttemptId,
        outcome: String,
        cause: Option<String>,
    },
    Evidence {
        attempt: AttemptId,
        cause: String,
    },
}

/// Failure to persist a phase transition.
#[derive(Debug, thiserror::Error)]
pub enum PhaseError {
    #[error("phase start refused: nonterminal attempt already exists for this phase")]
    NonterminalExists,
    #[error("phase transition refused: unknown attempt")]
    UnknownAttempt,
    #[error(transparent)]
    Storage(#[from] crate::Error),
}

/// Persist a phase transition and optional run status / HEAD / step rows in one Immediate txn.
///
/// # Errors
///
/// Returns [`PhaseError::NonterminalExists`] when `Start` would open a second nonterminal
/// attempt for the same top-level phase, [`PhaseError::UnknownAttempt`] when the referenced
/// attempt is missing, or a storage error when the transaction cannot commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn persist_phase_transition(
    db: &Db,
    plan: PhaseTransition,
    effects: RunEffects,
) -> std::result::Result<AttemptId, PhaseError> {
    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
        .map_err(crate::Error::from)?;

    let (run_id, attempt_id) = match plan {
        PhaseTransition::Start { run_id, phase } => {
            if nonterminal_attempt_tx(&tx, &run_id, phase)?.is_some() {
                return Err(PhaseError::NonterminalExists);
            }
            let ordinal = next_top_level_ordinal_tx(&tx, &run_id, phase)?;
            let attempt_id = AttemptId(Ulid::new().to_string());
            let created_at = now_secs();
            tx.execute(
                "INSERT INTO phase_attempts (
                    id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
                    operation_kind, created_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, ?5)",
                rusqlite::params![
                    attempt_id.as_str(),
                    &run_id,
                    phase.as_str(),
                    ordinal,
                    created_at,
                ],
            )
            .map_err(crate::Error::from)?;
            let seq = next_event_seq_tx(&tx, &run_id)?;
            insert_event_tx(
                &tx,
                &run_id,
                &attempt_id,
                seq,
                PhaseEventKind::Started,
                None,
                None,
                &created_at,
            )?;
            (run_id, attempt_id)
        }
        PhaseTransition::Terminal {
            attempt,
            outcome,
            cause,
        } => {
            let run_id = attempt_run_id_tx(&tx, &attempt)?;
            let seq = next_event_seq_tx(&tx, &run_id)?;
            insert_event_tx(
                &tx,
                &run_id,
                &attempt,
                seq,
                PhaseEventKind::Terminal,
                Some(outcome.as_str()),
                cause.as_deref(),
                &now_secs(),
            )?;
            (run_id, attempt)
        }
        PhaseTransition::Evidence { attempt, cause } => {
            let run_id = attempt_run_id_tx(&tx, &attempt)?;
            let seq = next_event_seq_tx(&tx, &run_id)?;
            insert_event_tx(
                &tx,
                &run_id,
                &attempt,
                seq,
                PhaseEventKind::Evidence,
                None,
                Some(cause.as_str()),
                &now_secs(),
            )?;
            (run_id, attempt)
        }
    };

    apply_run_effects_tx(&tx, &run_id, effects)?;
    tx.execute(
        "UPDATE runs SET audit_rev = audit_rev + 1 WHERE id = ?1",
        [&run_id],
    )
    .map_err(crate::Error::from)?;
    tx.commit().map_err(crate::Error::from)?;
    Ok(attempt_id)
}

fn nonterminal_attempt_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    phase: PhaseName,
) -> std::result::Result<Option<PhaseAttemptRow>, PhaseError> {
    let mut stmt = tx
        .prepare(
            "SELECT a.id, a.run_id, a.phase, a.ordinal, a.parent_attempt_id, a.caused_by_attempt_id,
                    a.operation_kind, a.created_at
             FROM phase_attempts a
             WHERE a.run_id = ?1
               AND a.phase = ?2
               AND a.parent_attempt_id IS NULL
               AND NOT EXISTS (
                    SELECT 1 FROM phase_events e
                    WHERE e.attempt_id = a.id AND e.kind = 'terminal'
               )
             ORDER BY a.ordinal DESC, a.id DESC
             LIMIT 1",
        )
        .map_err(crate::Error::from)?;
    let mut rows = stmt
        .query(rusqlite::params![run_id, phase.as_str()])
        .map_err(crate::Error::from)?;
    match rows.next().map_err(crate::Error::from)? {
        Some(row) => Ok(Some(map_attempt(row)?)),
        None => Ok(None),
    }
}

fn next_top_level_ordinal_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    phase: PhaseName,
) -> std::result::Result<i64, PhaseError> {
    let max: Option<i64> = tx
        .query_row(
            "SELECT MAX(ordinal) FROM phase_attempts
             WHERE run_id = ?1 AND phase = ?2 AND parent_attempt_id IS NULL",
            rusqlite::params![run_id, phase.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(max.unwrap_or(0) + 1)
}

fn next_event_seq_tx(tx: &Transaction<'_>, run_id: &str) -> std::result::Result<i64, PhaseError> {
    let max: Option<i64> = tx
        .query_row(
            "SELECT MAX(seq) FROM phase_events WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(max.unwrap_or(0) + 1)
}

fn attempt_run_id_tx(
    tx: &Transaction<'_>,
    attempt: &AttemptId,
) -> std::result::Result<String, PhaseError> {
    match tx.query_row(
        "SELECT run_id FROM phase_attempts WHERE id = ?1",
        [attempt.as_str()],
        |row| row.get(0),
    ) {
        Ok(run_id) => Ok(run_id),
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(PhaseError::UnknownAttempt),
        Err(err) => Err(PhaseError::Storage(err.into())),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_event_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    attempt: &AttemptId,
    seq: i64,
    kind: PhaseEventKind,
    outcome: Option<&str>,
    cause: Option<&str>,
    created_at: &str,
) -> std::result::Result<(), PhaseError> {
    let event_id = Ulid::new().to_string();
    tx.execute(
        "INSERT INTO phase_events (
            id, run_id, attempt_id, seq, kind, outcome, cause, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            event_id,
            run_id,
            attempt.as_str(),
            seq,
            kind.as_str(),
            outcome,
            cause,
            created_at,
        ],
    )
    .map_err(crate::Error::from)?;
    Ok(())
}

fn apply_run_effects_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    effects: RunEffects,
) -> std::result::Result<(), PhaseError> {
    let RunEffects {
        status,
        error,
        approved_head,
        steps,
    } = effects;
    if let Some(status) = status.as_deref() {
        tx.execute(
            "UPDATE runs SET status = ?1, error = ?2 WHERE id = ?3",
            rusqlite::params![status, error, run_id],
        )
        .map_err(crate::Error::from)?;
    }
    if let Some(head) = approved_head.as_deref() {
        tx.execute(
            "UPDATE runs SET review_approved_head_sha = ?1 WHERE id = ?2",
            rusqlite::params![head, run_id],
        )
        .map_err(crate::Error::from)?;
    }
    for StepEffect {
        step,
        status,
        error,
    } in steps
    {
        let id = Ulid::new().to_string();
        tx.execute(
            "INSERT INTO step_results (id, run_id, step, status, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, run_id, step, status, error, now_secs()],
        )
        .map_err(crate::Error::from)?;
    }
    Ok(())
}

/// Phase attempts for a run, oldest first.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn attempts_for_run(db: &Db, run_id: &str) -> Result<Vec<PhaseAttemptRow>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
                operation_kind, created_at
         FROM phase_attempts
         WHERE run_id = ?1
         ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map_attempt(row)?);
    }
    Ok(out)
}

/// Phase events for a run in sequence order.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn events_for_run(db: &Db, run_id: &str) -> Result<Vec<PhaseEventRow>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, run_id, attempt_id, seq, kind, outcome, cause, created_at
         FROM phase_events
         WHERE run_id = ?1
         ORDER BY seq, id",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map_event(row)?);
    }
    Ok(out)
}

/// The nonterminal top-level attempt for `phase` on `run_id`, if any.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn nonterminal_attempt(
    db: &Db,
    run_id: &str,
    phase: PhaseName,
) -> Result<Option<PhaseAttemptRow>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT a.id, a.run_id, a.phase, a.ordinal, a.parent_attempt_id, a.caused_by_attempt_id,
                a.operation_kind, a.created_at
         FROM phase_attempts a
         WHERE a.run_id = ?1
           AND a.phase = ?2
           AND a.parent_attempt_id IS NULL
           AND NOT EXISTS (
                SELECT 1 FROM phase_events e
                WHERE e.attempt_id = a.id AND e.kind = 'terminal'
           )
         ORDER BY a.ordinal DESC, a.id DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![run_id, phase.as_str()])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_attempt(row)?)),
        None => Ok(None),
    }
}

fn map_attempt(row: &rusqlite::Row<'_>) -> Result<PhaseAttemptRow> {
    let parent: Option<String> = row.get(4)?;
    let caused_by: Option<String> = row.get(5)?;
    let operation_kind: Option<String> = row.get(6)?;
    Ok(PhaseAttemptRow {
        id: AttemptId(row.get(0)?),
        run_id: row.get(1)?,
        phase: PhaseName::parse(&row.get::<_, String>(2)?)?,
        ordinal: row.get(3)?,
        parent_attempt_id: parent.map(AttemptId),
        caused_by_attempt_id: caused_by.map(AttemptId),
        operation_kind: operation_kind
            .as_deref()
            .map(OperationKind::parse)
            .transpose()?,
        created_at: row.get(7)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> Result<PhaseEventRow> {
    Ok(PhaseEventRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        attempt_id: AttemptId(row.get(2)?),
        seq: row.get(3)?,
        kind: PhaseEventKind::parse(&row.get::<_, String>(4)?)?,
        outcome: row.get(5)?,
        cause: row.get(6)?,
        created_at: row.get(7)?,
    })
}
