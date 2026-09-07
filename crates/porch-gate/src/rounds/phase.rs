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
    NestedStart {
        parent: AttemptId,
        kind: OperationKind,
    },
    NestedTerminal {
        attempt: AttemptId,
        outcome: String,
        cause: Option<String>,
    },
    Handoff {
        from: AttemptId,
        to_phase: PhaseName,
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
    #[error("nested start refused: parent attempt already has a terminal event")]
    ParentTerminated,
    #[error("handoff refused: attempt already has a successor")]
    SuccessorExists,
    #[error("handoff refused: attempt already has a terminal event")]
    AlreadyTerminal,
    #[error(transparent)]
    Storage(#[from] crate::Error),
}

/// Persist a phase transition and optional run status / HEAD / step rows in one Immediate txn.
///
/// # Errors
///
/// Returns [`PhaseError::NonterminalExists`] when `Start` or `Handoff` would open a second
/// nonterminal attempt for the same top-level phase, [`PhaseError::ParentTerminated`] when
/// `NestedStart` targets a parent that already has a terminal event,
/// [`PhaseError::AlreadyTerminal`] when `Handoff` targets an attempt that already has a
/// terminal event, [`PhaseError::SuccessorExists`] when `Handoff` would create a second
/// successor for the same attempt, [`PhaseError::UnknownAttempt`] when the referenced attempt
/// is missing, or a storage error when the transaction cannot commit.
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

    let (run_id, attempt_id) = apply_phase_transition_tx(&tx, plan)?;
    apply_run_effects_tx(&tx, &run_id, effects)?;
    tx.execute(
        "UPDATE runs SET audit_rev = audit_rev + 1 WHERE id = ?1",
        [&run_id],
    )
    .map_err(crate::Error::from)?;
    tx.commit().map_err(crate::Error::from)?;
    Ok(attempt_id)
}

/// Apply a phase transition on an open Immediate transaction (no commit).
pub(crate) fn apply_phase_transition_tx(
    tx: &Transaction<'_>,
    plan: PhaseTransition,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    match plan {
        PhaseTransition::Start { run_id, phase } => apply_start_tx(tx, run_id, phase),
        PhaseTransition::Terminal {
            attempt,
            outcome,
            cause,
        } => apply_terminal_tx(tx, attempt, &outcome, cause.as_deref()),
        PhaseTransition::NestedStart { parent, kind } => apply_nested_start_tx(tx, &parent, kind),
        PhaseTransition::NestedTerminal {
            attempt,
            outcome,
            cause,
        } => apply_nested_terminal_tx(tx, attempt, &outcome, cause.as_deref()),
        PhaseTransition::Handoff {
            from,
            to_phase,
            outcome,
            cause,
        } => apply_handoff_tx(tx, &from, to_phase, &outcome, cause.as_deref()),
        PhaseTransition::Evidence { attempt, cause } => apply_evidence_tx(tx, attempt, &cause),
    }
}

fn apply_start_tx(
    tx: &Transaction<'_>,
    run_id: String,
    phase: PhaseName,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    if nonterminal_attempt_tx(tx, &run_id, phase)?.is_some() {
        return Err(PhaseError::NonterminalExists);
    }
    let ordinal = next_top_level_ordinal_tx(tx, &run_id, phase)?;
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
    let seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &attempt_id,
        seq,
        PhaseEventKind::Started,
        None,
        None,
        &created_at,
    )?;
    Ok((run_id, attempt_id))
}

fn apply_terminal_tx(
    tx: &Transaction<'_>,
    attempt: AttemptId,
    outcome: &str,
    cause: Option<&str>,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    let run_id = attempt_run_id_tx(tx, &attempt)?;
    let seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &attempt,
        seq,
        PhaseEventKind::Terminal,
        Some(outcome),
        cause,
        &now_secs(),
    )?;
    Ok((run_id, attempt))
}

fn apply_nested_start_tx(
    tx: &Transaction<'_>,
    parent: &AttemptId,
    kind: OperationKind,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    let parent_row = attempt_row_tx(tx, parent)?;
    if attempt_has_terminal_tx(tx, parent)? {
        return Err(PhaseError::ParentTerminated);
    }
    let run_id = parent_row.run_id;
    let ordinal = next_nested_ordinal_tx(tx, parent)?;
    let attempt_id = AttemptId(Ulid::new().to_string());
    let created_at = now_secs();
    tx.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)",
        rusqlite::params![
            attempt_id.as_str(),
            &run_id,
            parent_row.phase.as_str(),
            ordinal,
            parent.as_str(),
            kind.as_str(),
            created_at,
        ],
    )
    .map_err(crate::Error::from)?;
    let seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &attempt_id,
        seq,
        PhaseEventKind::Started,
        None,
        None,
        &created_at,
    )?;
    Ok((run_id, attempt_id))
}

fn apply_nested_terminal_tx(
    tx: &Transaction<'_>,
    attempt: AttemptId,
    outcome: &str,
    cause: Option<&str>,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    let nested = attempt_row_tx(tx, &attempt)?;
    let run_id = nested.run_id.clone();
    let created_at = now_secs();
    let seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &attempt,
        seq,
        PhaseEventKind::Terminal,
        Some(outcome),
        cause,
        &created_at,
    )?;
    let close_parent = match nested.operation_kind {
        Some(OperationKind::Fixer)
            if nested.phase == PhaseName::Review
                && (outcome == "failed" || outcome == "interrupted") =>
        {
            true
        }
        Some(OperationKind::Compose)
            if nested.phase == PhaseName::Deliver
                && matches!(outcome, "cancelled" | "failed" | "interrupted" | "abort") =>
        {
            true
        }
        _ => false,
    };
    if close_parent {
        if let Some(parent_id) = nested.parent_attempt_id.as_ref() {
            let parent_seq = next_event_seq_tx(tx, &run_id)?;
            insert_event_tx(
                tx,
                &run_id,
                parent_id,
                parent_seq,
                PhaseEventKind::Terminal,
                Some(outcome),
                cause,
                &created_at,
            )?;
        }
    }
    Ok((run_id, attempt))
}

fn apply_handoff_tx(
    tx: &Transaction<'_>,
    from: &AttemptId,
    to_phase: PhaseName,
    outcome: &str,
    cause: Option<&str>,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    if successor_for_tx(tx, from)?.is_some() {
        return Err(PhaseError::SuccessorExists);
    }
    if attempt_has_terminal_tx(tx, from)? {
        return Err(PhaseError::AlreadyTerminal);
    }
    let from_row = attempt_row_tx(tx, from)?;
    let run_id = from_row.run_id;
    // Same-phase handoff: `from` is the open attempt for `to_phase` and is allowed.
    // Cross-phase into an already-open destination must fail closed before mutate.
    if let Some(open) = nonterminal_attempt_tx(tx, &run_id, to_phase)? {
        if open.id != *from {
            return Err(PhaseError::NonterminalExists);
        }
    }
    let created_at = now_secs();
    let terminal_seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        from,
        terminal_seq,
        PhaseEventKind::Terminal,
        Some(outcome),
        cause,
        &created_at,
    )?;
    let ordinal = next_top_level_ordinal_tx(tx, &run_id, to_phase)?;
    let new_attempt = AttemptId(Ulid::new().to_string());
    tx.execute(
        "INSERT INTO phase_attempts (
            id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
            operation_kind, created_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, ?6)",
        rusqlite::params![
            new_attempt.as_str(),
            &run_id,
            to_phase.as_str(),
            ordinal,
            from.as_str(),
            created_at,
        ],
    )
    .map_err(crate::Error::from)?;
    let started_seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &new_attempt,
        started_seq,
        PhaseEventKind::Started,
        None,
        None,
        &created_at,
    )?;
    Ok((run_id, new_attempt))
}

fn apply_evidence_tx(
    tx: &Transaction<'_>,
    attempt: AttemptId,
    cause: &str,
) -> std::result::Result<(String, AttemptId), PhaseError> {
    let run_id = attempt_run_id_tx(tx, &attempt)?;
    let seq = next_event_seq_tx(tx, &run_id)?;
    insert_event_tx(
        tx,
        &run_id,
        &attempt,
        seq,
        PhaseEventKind::Evidence,
        None,
        Some(cause),
        &now_secs(),
    )?;
    Ok((run_id, attempt))
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
    Ok(attempt_row_tx(tx, attempt)?.run_id)
}

fn attempt_row_tx(
    tx: &Transaction<'_>,
    attempt: &AttemptId,
) -> std::result::Result<PhaseAttemptRow, PhaseError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
                    operation_kind, created_at
             FROM phase_attempts
             WHERE id = ?1",
        )
        .map_err(crate::Error::from)?;
    let mut rows = stmt.query([attempt.as_str()]).map_err(crate::Error::from)?;
    match rows.next().map_err(crate::Error::from)? {
        Some(row) => Ok(map_attempt(row).map_err(PhaseError::Storage)?),
        None => Err(PhaseError::UnknownAttempt),
    }
}

fn attempt_has_terminal_tx(
    tx: &Transaction<'_>,
    attempt: &AttemptId,
) -> std::result::Result<bool, PhaseError> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM phase_events
             WHERE attempt_id = ?1 AND kind = 'terminal'",
            [attempt.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(count > 0)
}

fn next_nested_ordinal_tx(
    tx: &Transaction<'_>,
    parent: &AttemptId,
) -> std::result::Result<i64, PhaseError> {
    let max: Option<i64> = tx
        .query_row(
            "SELECT MAX(ordinal) FROM phase_attempts WHERE parent_attempt_id = ?1",
            [parent.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;
    Ok(max.unwrap_or(0) + 1)
}

fn successor_for_tx(
    tx: &Transaction<'_>,
    from: &AttemptId,
) -> std::result::Result<Option<AttemptId>, PhaseError> {
    let mut stmt = tx
        .prepare(
            "SELECT id FROM phase_attempts
             WHERE caused_by_attempt_id = ?1
             ORDER BY ordinal ASC, id ASC
             LIMIT 1",
        )
        .map_err(crate::Error::from)?;
    let mut rows = stmt.query([from.as_str()]).map_err(crate::Error::from)?;
    match rows.next().map_err(crate::Error::from)? {
        Some(row) => Ok(Some(AttemptId(row.get(0).map_err(crate::Error::from)?))),
        None => Ok(None),
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

/// Count nested `deliver_repair` started events for a run (deliver-repair budget).
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn repair_attempts_started(db: &Db, run_id: &str) -> Result<u32> {
    let conn = db.conn();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM phase_events e
         INNER JOIN phase_attempts a ON a.id = e.attempt_id
         WHERE e.run_id = ?1
           AND e.kind = 'started'
           AND a.operation_kind = 'deliver_repair'",
        [run_id],
        |row| row.get(0),
    )?;
    u32::try_from(n)
        .map_err(|_| crate::Error::Other(format!("repair started count out of range: {n}")))
}

/// Cancel a run with a justifying phase event co-written with `runs.status`.
///
/// Prefers terminating an open nested compose under deliver (which also closes
/// deliver), then any other open top-level attempt; if none exist, starts and
/// terminals an intent attempt.
///
/// # Errors
///
/// Returns a [`PhaseError`] when the transition cannot commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn cancel_run(db: &Db, run_id: &str, cause: &str) -> std::result::Result<(), PhaseError> {
    let effects = RunEffects {
        status: Some("cancelled".into()),
        error: Some(cause.to_string()),
        approved_head: None,
        steps: vec![],
    };
    if let Some(deliver) = nonterminal_attempt(db, run_id, PhaseName::Deliver)? {
        if let Some(compose) = open_nested_compose(db, run_id, &deliver.id)? {
            persist_phase_transition(
                db,
                PhaseTransition::NestedTerminal {
                    attempt: compose,
                    outcome: "cancelled".into(),
                    cause: Some(cause.to_string()),
                },
                RunEffects {
                    status: effects.status.clone(),
                    error: effects.error.clone(),
                    approved_head: None,
                    steps: vec![StepEffect {
                        step: "compose".into(),
                        status: "cancelled".into(),
                        error: Some(cause.to_string()),
                    }],
                },
            )?;
            return Ok(());
        }
        persist_phase_transition(
            db,
            PhaseTransition::Terminal {
                attempt: deliver.id,
                outcome: "cancelled".into(),
                cause: Some(cause.to_string()),
            },
            effects,
        )?;
        return Ok(());
    }
    for name in [
        PhaseName::Review,
        PhaseName::Rebase,
        PhaseName::Certify,
        PhaseName::Intent,
    ] {
        if let Some(open) = nonterminal_attempt(db, run_id, name)? {
            persist_phase_transition(
                db,
                PhaseTransition::Terminal {
                    attempt: open.id,
                    outcome: "cancelled".into(),
                    cause: Some(cause.to_string()),
                },
                effects,
            )?;
            return Ok(());
        }
    }
    persist_phase_transition(
        db,
        PhaseTransition::Start {
            run_id: run_id.to_string(),
            phase: PhaseName::Intent,
        },
        RunEffects::none(),
    )?;
    let intent =
        nonterminal_attempt(db, run_id, PhaseName::Intent)?.ok_or(PhaseError::UnknownAttempt)?;
    persist_phase_transition(
        db,
        PhaseTransition::Terminal {
            attempt: intent.id,
            outcome: "cancelled".into(),
            cause: Some(cause.to_string()),
        },
        effects,
    )?;
    Ok(())
}

fn open_nested_compose(
    db: &Db,
    run_id: &str,
    deliver: &AttemptId,
) -> std::result::Result<Option<AttemptId>, PhaseError> {
    let attempts = attempts_for_run(db, run_id).map_err(PhaseError::Storage)?;
    let events = events_for_run(db, run_id).map_err(PhaseError::Storage)?;
    Ok(attempts.into_iter().rev().find_map(|a| {
        let is_compose = a.parent_attempt_id.as_ref() == Some(deliver)
            && a.operation_kind == Some(OperationKind::Compose);
        if !is_compose {
            return None;
        }
        let terminal = events
            .iter()
            .any(|e| e.attempt_id == a.id && e.kind == PhaseEventKind::Terminal);
        (!terminal).then_some(a.id)
    }))
}

/// Append interrupted terminals for every nonterminal attempt on each `running` run,
/// co-writing that run's stale status in the same Immediate transaction.
///
/// Returns the number of attempts terminalized.
///
/// # Errors
///
/// Returns a storage error if listing or reconciling a run fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn reconcile_interrupted(db: &Db) -> Result<usize> {
    reconcile_interrupted_with_error(db, "daemon restarted while run was in progress")
}

/// Like [`reconcile_interrupted`], using `error` for `runs.error` and event cause.
pub(crate) fn reconcile_interrupted_with_error(db: &Db, error: &str) -> Result<usize> {
    let stale: Vec<(String, Option<String>)> = {
        let conn = db.conn();
        let mut stmt =
            conn.prepare("SELECT id, pr_url FROM runs WHERE status = 'running' ORDER BY id")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    let mut closed = 0usize;
    for (run_id, pr_url) in stale {
        closed += reconcile_one_running(db, &run_id, pr_url.as_deref(), error)?;
    }
    Ok(closed)
}

fn reconcile_one_running(
    db: &Db,
    run_id: &str,
    pr_url: Option<&str>,
    error: &str,
) -> Result<usize> {
    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;

    let open = nonterminal_attempts_for_run_tx(&tx, run_id).map_err(phase_err_to_storage)?;
    let n = open.len();
    for attempt in open {
        apply_phase_transition_tx(
            &tx,
            PhaseTransition::Terminal {
                attempt: attempt.id,
                outcome: "interrupted".into(),
                cause: Some(error.to_string()),
            },
        )
        .map_err(phase_err_to_storage)?;
    }

    let status = if pr_url.is_some_and(|u| !u.trim().is_empty()) {
        "ci_monitor_interrupted"
    } else {
        "failed"
    };
    apply_run_effects_tx(
        &tx,
        run_id,
        RunEffects {
            status: Some(status.into()),
            error: Some(error.to_string()),
            approved_head: None,
            steps: vec![],
        },
    )
    .map_err(phase_err_to_storage)?;
    tx.execute(
        "UPDATE runs SET audit_rev = audit_rev + 1 WHERE id = ?1",
        [run_id],
    )?;
    tx.commit()?;
    Ok(n)
}

fn nonterminal_attempts_for_run_tx(
    tx: &Transaction<'_>,
    run_id: &str,
) -> std::result::Result<Vec<PhaseAttemptRow>, PhaseError> {
    let mut stmt = tx
        .prepare(
            "SELECT a.id, a.run_id, a.phase, a.ordinal, a.parent_attempt_id, a.caused_by_attempt_id,
                    a.operation_kind, a.created_at
             FROM phase_attempts a
             WHERE a.run_id = ?1
               AND NOT EXISTS (
                    SELECT 1 FROM phase_events e
                    WHERE e.attempt_id = a.id AND e.kind = 'terminal'
               )
             ORDER BY CASE WHEN a.parent_attempt_id IS NULL THEN 1 ELSE 0 END,
                      a.ordinal ASC, a.id ASC",
        )
        .map_err(crate::Error::from)?;
    let mut rows = stmt.query([run_id]).map_err(crate::Error::from)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(crate::Error::from)? {
        out.push(map_attempt(row).map_err(PhaseError::Storage)?);
    }
    Ok(out)
}

fn phase_err_to_storage(err: PhaseError) -> crate::Error {
    match err {
        PhaseError::Storage(e) => e,
        other => crate::Error::Other(other.to_string()),
    }
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
