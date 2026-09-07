use std::collections::BTreeSet;

use rusqlite::{Transaction, TransactionBehavior};
use ulid::Ulid;

use super::{AssuranceCompletion, ExecutionState, RoundId};
use crate::Result;
use crate::db::{self, Db};

use db::now_secs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityKind {
    ReviewApproved,
    ReviewSkipped,
    FixRequested,
    ReviewAborted,
}

impl AuthorityKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewApproved => "review_approved",
            Self::ReviewSkipped => "review_skipped",
            Self::FixRequested => "fix_requested",
            Self::ReviewAborted => "review_aborted",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "review_approved" => Ok(Self::ReviewApproved),
            "review_skipped" => Ok(Self::ReviewSkipped),
            "fix_requested" => Ok(Self::FixRequested),
            "review_aborted" => Ok(Self::ReviewAborted),
            other => Err(crate::Error::Other(format!(
                "unknown authority kind: {other}"
            ))),
        }
    }

    fn binds_head(self) -> bool {
        // ReviewSkipped does not write review_approved_head_sha, but still
        // fail-closes when live HEAD drifts from the parked tip (DISPO-2.6).
        matches!(
            self,
            Self::ReviewApproved | Self::ReviewSkipped | Self::ReviewAborted | Self::FixRequested
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Context,
    Target,
}

impl MemberRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Target => "target",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "context" => Ok(Self::Context),
            "target" => Ok(Self::Target),
            other => Err(crate::Error::Other(format!(
                "unknown authority member role: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Operator,
    Porch,
}

impl ActorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Porch => "porch",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "operator" => Ok(Self::Operator),
            "porch" => Ok(Self::Porch),
            other => Err(crate::Error::Other(format!(
                "unknown authority actor kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistAuthorityPlan {
    pub run_id: String,
    pub kind: AuthorityKind,
    pub expected_round_id: Option<RoundId>,
    pub expected_head: Option<String>,
    pub live_head: Option<String>,
    pub actor_kind: ActorKind,
    pub authority_event_id: Option<String>,
    pub head_changed: Option<bool>,
    pub identity_unavailable: bool,
    pub members: Vec<(String, MemberRole)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEffect {
    pub step: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEffects {
    pub status: Option<String>,
    pub error: Option<String>,
    pub approved_head: Option<String>,
    pub steps: Vec<StepEffect>,
}

impl RunEffects {
    #[must_use]
    pub fn none() -> Self {
        Self {
            status: None,
            error: None,
            approved_head: None,
            steps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityMemberRecord {
    pub finding_instance_id: String,
    pub role: MemberRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityEventRecord {
    pub id: String,
    pub run_id: String,
    pub kind: AuthorityKind,
    pub review_round_id: Option<String>,
    pub reviewed_head: Option<String>,
    pub actor_kind: ActorKind,
    pub authority_event_id: Option<String>,
    pub head_changed: Option<bool>,
    pub identity_unavailable: bool,
    pub created_at: String,
    pub members: Vec<AuthorityMemberRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("authority persist rejected: applicable round or reviewed HEAD drifted")]
    Stale,
    #[error(transparent)]
    Storage(#[from] crate::Error),
}

/// Append one authority event under an Immediate transaction.
///
/// # Errors
///
/// Returns [`AuthorityError::Stale`] when expected round/HEAD drift is observed, or a
/// storage error when the transaction cannot commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn persist_authority(
    db: &Db,
    plan: PersistAuthorityPlan,
) -> std::result::Result<String, AuthorityError> {
    persist_authority_with_run_effects(db, plan, RunEffects::none(), None)
}

/// Append one authority event and optional run status / HEAD / step rows in one Immediate txn.
///
/// When `phase` is `Some`, the phase transition is applied in the same transaction before
/// run effects (compat until authority and phase writes share one core).
///
/// # Errors
///
/// Returns [`AuthorityError::Stale`] when expected round/HEAD drift is observed, or a
/// storage error when the transaction cannot commit.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn persist_authority_with_run_effects(
    db: &Db,
    plan: PersistAuthorityPlan,
    effects: RunEffects,
    phase: Option<super::phase::PhaseTransition>,
) -> std::result::Result<String, AuthorityError> {
    if plan.identity_unavailable {
        if plan.kind != AuthorityKind::ReviewAborted {
            return Err(AuthorityError::Storage(crate::Error::Other(
                "identity_unavailable is only valid for review_aborted".into(),
            )));
        }
        if plan.expected_round_id.is_some() || !plan.members.is_empty() {
            return Err(AuthorityError::Storage(crate::Error::Other(
                "identity_unavailable abort must omit round id and members".into(),
            )));
        }
    } else if plan.expected_round_id.is_none() {
        return Err(AuthorityError::Stale);
    }

    if plan.kind == AuthorityKind::FixRequested
        && plan
            .members
            .iter()
            .filter(|(_, role)| *role == MemberRole::Target)
            .count()
            == 0
    {
        return Err(AuthorityError::Storage(crate::Error::Other(
            "fix_requested requires a non-empty target set".into(),
        )));
    }

    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
        .map_err(crate::Error::from)?;

    let (review_round_id, reviewed_head) = if plan.identity_unavailable {
        (None, None)
    } else {
        guard_applicable(&tx, &plan)?;
        (
            plan.expected_round_id
                .as_ref()
                .map(|id| id.as_str().to_string()),
            plan.expected_head.clone(),
        )
    };

    let PersistAuthorityPlan {
        run_id,
        kind,
        expected_round_id: _,
        expected_head: _,
        live_head: _,
        actor_kind,
        authority_event_id,
        head_changed,
        identity_unavailable,
        members,
    } = plan;

    let event_id = Ulid::new().to_string();
    let head_changed = head_changed.map(i64::from);
    tx.execute(
        "INSERT INTO authority_events (
            id, run_id, kind, review_round_id, reviewed_head, actor_kind,
            authority_event_id, head_changed, identity_unavailable, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            event_id,
            run_id,
            kind.as_str(),
            review_round_id,
            reviewed_head,
            actor_kind.as_str(),
            authority_event_id,
            head_changed,
            i64::from(identity_unavailable),
            now_secs(),
        ],
    )
    .map_err(crate::Error::from)?;

    {
        let mut insert_member = tx
            .prepare(
                "INSERT INTO authority_event_members (event_id, finding_instance_id, role)
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(crate::Error::from)?;
        for (instance_id, role) in &members {
            insert_member
                .execute(rusqlite::params![event_id, instance_id, role.as_str()])
                .map_err(crate::Error::from)?;
        }
    }

    if let Some(phase) = phase {
        super::phase::apply_phase_transition_tx(&tx, phase)
            .map_err(|e| AuthorityError::Storage(crate::Error::Other(e.to_string())))?;
    }

    apply_run_effects_tx(&tx, &run_id, effects)?;

    tx.execute(
        "UPDATE runs SET audit_rev = audit_rev + 1 WHERE id = ?1",
        [&run_id],
    )
    .map_err(crate::Error::from)?;

    tx.commit().map_err(crate::Error::from)?;
    Ok(event_id)
}

/// Co-write run status / approved HEAD / `step_results` on an open Immediate txn.
pub(crate) fn apply_run_effects_tx(
    tx: &Transaction<'_>,
    run_id: &str,
    effects: RunEffects,
) -> Result<()> {
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
        )?;
    }
    if let Some(head) = approved_head.as_deref() {
        tx.execute(
            "UPDATE runs SET review_approved_head_sha = ?1 WHERE id = ?2",
            rusqlite::params![head, run_id],
        )?;
    }
    for step in steps {
        let id = Ulid::new().to_string();
        tx.execute(
            "INSERT INTO step_results (id, run_id, step, status, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, run_id, step.step, step.status, step.error, now_secs()],
        )?;
    }
    Ok(())
}

fn guard_applicable(
    tx: &Transaction<'_>,
    plan: &PersistAuthorityPlan,
) -> std::result::Result<(), AuthorityError> {
    let expected_round = plan
        .expected_round_id
        .as_ref()
        .ok_or(AuthorityError::Stale)?;
    let Some((applicable_id, to_sha)) = applicable_round_id_tx(tx, &plan.run_id)? else {
        return Err(AuthorityError::Stale);
    };
    if applicable_id != expected_round.as_str() {
        return Err(AuthorityError::Stale);
    }

    let head_sha: Option<String> = tx
        .query_row(
            "SELECT head_sha FROM runs WHERE id = ?1",
            [&plan.run_id],
            |row| row.get(0),
        )
        .map_err(crate::Error::from)?;

    if let Some(expected_head) = plan.expected_head.as_deref() {
        if to_sha != expected_head || head_sha.as_deref() != Some(expected_head) {
            return Err(AuthorityError::Stale);
        }
    } else if plan.kind != AuthorityKind::ReviewSkipped {
        return Err(AuthorityError::Stale);
    }

    if plan.kind.binds_head() {
        match (&plan.live_head, &plan.expected_head) {
            (Some(live), Some(expected)) if live == expected => {}
            _ => return Err(AuthorityError::Stale),
        }
    }

    let round_instances = instances_for_round_tx(tx, expected_round)?;
    let context: BTreeSet<&str> = plan
        .members
        .iter()
        .filter(|(_, role)| *role == MemberRole::Context)
        .map(|(id, _)| id.as_str())
        .collect();
    let targets: BTreeSet<&str> = plan
        .members
        .iter()
        .filter(|(_, role)| *role == MemberRole::Target)
        .map(|(id, _)| id.as_str())
        .collect();

    match plan.kind {
        AuthorityKind::FixRequested => {
            if !context.is_empty()
                || targets.is_empty()
                || targets.len() != plan.members.len()
                || targets.iter().any(|id| !round_instances.contains(*id))
            {
                return Err(AuthorityError::Stale);
            }
        }
        AuthorityKind::ReviewApproved
        | AuthorityKind::ReviewSkipped
        | AuthorityKind::ReviewAborted => {
            let round_refs: BTreeSet<&str> = round_instances.iter().map(String::as_str).collect();
            if !targets.is_empty() || context != round_refs {
                return Err(AuthorityError::Stale);
            }
        }
    }

    Ok(())
}

pub(crate) fn applicable_round_id_tx(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<(String, String)>> {
    let head_sha: Option<String> =
        tx.query_row("SELECT head_sha FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })?;

    let mut stmt = tx.prepare(
        "SELECT id, to_sha FROM review_rounds
         WHERE run_id = ?1
           AND execution = ?2
           AND assurance_completion = ?3
           AND finalized_at IS NOT NULL
         ORDER BY ordinal DESC",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        run_id,
        ExecutionState::Finished.as_str(),
        AssuranceCompletion::Complete.as_str(),
    ])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let to_sha: String = row.get(1)?;
        if let Some(head) = head_sha.as_deref() {
            if to_sha != head {
                continue;
            }
        }
        return Ok(Some((id, to_sha)));
    }
    Ok(None)
}

fn instances_for_round_tx(tx: &Transaction<'_>, round_id: &RoundId) -> Result<BTreeSet<String>> {
    let mut stmt =
        tx.prepare("SELECT id FROM finding_instances WHERE round_id = ?1 ORDER BY id")?;
    let mut rows = stmt.query([round_id.as_str()])?;
    let mut out = BTreeSet::new();
    while let Some(row) = rows.next()? {
        out.insert(row.get::<_, String>(0)?);
    }
    Ok(out)
}

/// Authority events recorded for a run, oldest first.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn events_for_run(db: &Db, run_id: &str) -> Result<Vec<AuthorityEventRecord>> {
    let conn = db.conn();
    events_for_run_conn(&conn, run_id)
}

/// Latest `fix_requested` event id and reviewed head for a run, if any.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn latest_fix_requested(db: &Db, run_id: &str) -> Result<Option<(String, Option<String>)>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, reviewed_head FROM authority_events
         WHERE run_id = ?1 AND kind = ?2
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        run_id,
        AuthorityKind::FixRequested.as_str()
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
        None => Ok(None),
    }
}

pub(crate) fn events_for_run_conn(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<Vec<AuthorityEventRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, run_id, kind, review_round_id, reviewed_head, actor_kind,
                authority_event_id, head_changed, identity_unavailable, created_at
         FROM authority_events
         WHERE run_id = ?1
         ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let head_changed: Option<i64> = row.get(7)?;
        events.push(AuthorityEventRecord {
            id: id.clone(),
            run_id: row.get(1)?,
            kind: AuthorityKind::parse(&row.get::<_, String>(2)?)?,
            review_round_id: row.get(3)?,
            reviewed_head: row.get(4)?,
            actor_kind: ActorKind::parse(&row.get::<_, String>(5)?)?,
            authority_event_id: row.get(6)?,
            head_changed: head_changed.map(|v| v != 0),
            identity_unavailable: row.get::<_, i64>(8)? != 0,
            created_at: row.get(9)?,
            members: members_for_event(conn, &id)?,
        });
    }
    Ok(events)
}

pub(crate) fn members_for_event(
    conn: &rusqlite::Connection,
    event_id: &str,
) -> Result<Vec<AuthorityMemberRecord>> {
    let mut stmt = conn.prepare(
        "SELECT finding_instance_id, role FROM authority_event_members
         WHERE event_id = ?1
         ORDER BY finding_instance_id, role",
    )?;
    let mut rows = stmt.query([event_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(AuthorityMemberRecord {
            finding_instance_id: row.get(0)?,
            role: MemberRole::parse(&row.get::<_, String>(1)?)?,
        });
    }
    Ok(out)
}
