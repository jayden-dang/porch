use std::fmt;

use crate::Result;
use crate::db::Db;

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
