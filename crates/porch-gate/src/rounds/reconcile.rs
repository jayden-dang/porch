//! What a restart concludes about a forward that may already have happened.
//!
//! ROAD-7 made the forward boundary leave a durable [`ForwardRecordRow`] trail.
//! This module reads it. The classification is pure over that trail plus one
//! optional local discriminator, so the states it distinguishes are testable
//! without a daemon, a repository, or a push (ARCH-13).
//!
//! It concludes about `origin`, never about a pull request: the durable evidence
//! for a crash before the pull request call and for a crash after it and before
//! `set_pr_url` is identical, so any claim that a pull request is absent would
//! be false half the time, in the direction that makes an operator open a
//! duplicate by hand.

use std::fmt;

use rusqlite::Transaction;
use ulid::Ulid;

use super::forward::{ForwardKind, ForwardRecordRow};
use super::phase::AttemptId;
use crate::Result;
use crate::db::{self, Db};

use db::now_secs;

/// What a restart concluded about one forward attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Porch never reached the forward boundary for this attempt.
    NotAttempted,
    /// Porch's own record shows `origin` carries the authorized SHA.
    ReachedOrigin,
    /// Porch invoked the push and cannot say locally whether it landed.
    Indeterminate,
}

impl Verdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::ReachedOrigin => "reached_origin",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// # Errors
    ///
    /// Returns a storage error for a verdict this binary does not know.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "not_attempted" => Ok(Self::NotAttempted),
            "reached_origin" => Ok(Self::ReachedOrigin),
            "indeterminate" => Ok(Self::Indeterminate),
            other => Err(crate::Error::Other(format!(
                "unknown forward verdict {other}"
            ))),
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The durable shape the verdict rests on.
///
/// Named rather than implied so that ROAD-9's fault injection has an
/// enumeration to assert against: a real kill must produce one of these and no
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// S0 — no forward record at all.
    NoRecord,
    /// S1 — an intent with no outcome.
    IntentOnly,
    /// S2 — an intent and a `pushed` outcome.
    PushReportedSuccess,
    /// S3 — an intent and an `already_current` outcome.
    RefAlreadyCurrent,
    /// S4 — an intent and a `push_failed` outcome.
    PushReportedFailure,
    /// S1 or S4, upgraded by the gate repository's remote-tracking ref.
    TrackingRefMatched,
}

impl Evidence {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoRecord => "no_record",
            Self::IntentOnly => "intent_only",
            Self::PushReportedSuccess => "push_reported_success",
            Self::RefAlreadyCurrent => "ref_already_current",
            Self::PushReportedFailure => "push_reported_failure",
            Self::TrackingRefMatched => "tracking_ref_matched",
        }
    }
}

/// Classify one `deliver` attempt's forward records.
///
/// `tracking_sha` is the gate repository's `refs/remotes/origin/<branch>` for
/// the run's branch, where it could be read. Git writes that ref only after the
/// receiving end acknowledged the push, so a match is proof the push landed —
/// but its absence proves nothing, because the ref is missing on a fresh gate
/// repository and `porch init` can drop it. It may therefore only upgrade
/// [`Verdict::Indeterminate`] to [`Verdict::ReachedOrigin`], never downgrade
/// anything.
#[must_use]
pub fn classify(
    records: &[ForwardRecordRow],
    authorized_sha: &str,
    tracking_sha: Option<&str>,
) -> (Verdict, Evidence) {
    let mut evidence = Evidence::NoRecord;
    let mut has_intent = false;
    for row in records {
        match row.kind {
            ForwardKind::Intent => has_intent = true,
            ForwardKind::Pushed => evidence = Evidence::PushReportedSuccess,
            ForwardKind::AlreadyCurrent => evidence = Evidence::RefAlreadyCurrent,
            // Not evidence that `origin` is unchanged: the pushing command
            // reported on itself, not on the remote.
            ForwardKind::PushFailed => evidence = Evidence::PushReportedFailure,
        }
    }
    if !has_intent && matches!(evidence, Evidence::NoRecord) {
        return (Verdict::NotAttempted, Evidence::NoRecord);
    }
    if matches!(evidence, Evidence::NoRecord) {
        evidence = Evidence::IntentOnly;
    }

    let verdict = match evidence {
        Evidence::PushReportedSuccess | Evidence::RefAlreadyCurrent => Verdict::ReachedOrigin,
        _ => Verdict::Indeterminate,
    };
    if verdict == Verdict::Indeterminate && tracking_sha == Some(authorized_sha) {
        return (Verdict::ReachedOrigin, Evidence::TrackingRefMatched);
    }
    (verdict, evidence)
}

/// A `deliver` attempt whose forward has no recorded verdict yet.
#[derive(Debug, Clone)]
pub struct AwaitingVerdict {
    pub run_id: String,
    pub deliver_attempt_id: AttemptId,
    pub ref_name: String,
    pub authorized_sha: String,
    /// The run's branch, for the gate repository's remote-tracking ref.
    pub branch: String,
    /// The gate repository, which `recover_stale` does not remove — unlike the
    /// worktree, which this classification never reads.
    pub bare_path: std::path::PathBuf,
}

/// One recorded conclusion.
#[derive(Debug, Clone)]
pub struct VerdictRow {
    pub id: String,
    pub run_id: String,
    pub deliver_attempt_id: AttemptId,
    pub seq: i64,
    pub verdict: Verdict,
    pub ref_name: String,
    pub authorized_sha: String,
    pub evidence: String,
    pub observed_tracking_sha: Option<String>,
    pub created_at: String,
}

/// The `deliver` attempts that were interrupted mid-forward and have no verdict.
///
/// Selected from the record's own shape rather than from `runs.status`. A
/// writer-protocol upgrade terminalizes every active run inside `Db::open`,
/// before recovery runs, so status-based selection would be silently disarmed by
/// the next bump — and it writes no phase event, so an attempt interrupted
/// mid-forward and then caught by an upgrade still presents as open here.
///
/// "Interrupted" is the absence of a `terminal` phase event on the attempt. Every
/// path that concludes on its own writes one, `fail_run_with_phase` included, so
/// an attempt that reached a terminal event reported its own error and must not be
/// classified: a post-push verification failure holds an intent and a `pushed`
/// record while porch has *proven* `origin` does not carry the SHA, and
/// classifying it would tell the operator the opposite.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn attempts_awaiting_verdict(db: &Db) -> Result<Vec<AwaitingVerdict>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT f.run_id, f.deliver_attempt_id, f.ref_name, f.authorized_sha,
                runs.branch, repos.bare_path
         FROM forward_records f
         JOIN runs ON runs.id = f.run_id
         JOIN repos ON repos.id = runs.repo_id
         WHERE f.kind = 'intent'
           AND NOT EXISTS (
               SELECT 1 FROM forward_reconciliations r
               WHERE r.deliver_attempt_id = f.deliver_attempt_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM phase_events e
               WHERE e.attempt_id = f.deliver_attempt_id AND e.kind = 'terminal'
           )
         ORDER BY f.seq, f.id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(AwaitingVerdict {
            run_id: row.get(0)?,
            deliver_attempt_id: AttemptId::from_raw(row.get::<_, String>(1)?),
            ref_name: row.get(2)?,
            authorized_sha: row.get(3)?,
            branch: row.get(4)?,
            bare_path: std::path::PathBuf::from(row.get::<_, String>(5)?),
        });
    }
    Ok(out)
}

/// The gate repository's remote-tracking ref for `branch`, if it can be read.
///
/// A purely local `rev-parse` against `--git-dir`: no network, so it cannot hang
/// on an unreachable remote, which matters because this runs on the daemon
/// startup path before the socket binds and a recovery error refuses to serve.
///
/// Every failure — missing repository, missing ref, unreadable git dir — maps to
/// `None`. That is not a silent swallow: the absence of this ref is genuinely
/// not evidence, so the only honest thing to do with a failed read is to decline
/// to upgrade the verdict.
#[must_use]
pub fn observed_tracking_sha(bare_path: &std::path::Path, branch: &str) -> Option<String> {
    let git_dir = porch_git::GitDir::new(bare_path).ok()?;
    porch_git::rev_parse(&git_dir, &format!("refs/remotes/origin/{branch}")).ok()
}

/// Append a verdict inside the caller's transaction.
///
/// Committing with the terminalization keeps a restart from leaving a run
/// reconciled without a conclusion, or the reverse.
///
/// # Errors
///
/// Returns a storage error if the insert fails.
pub(crate) fn append_verdict_tx(
    tx: &Transaction<'_>,
    pending: &AwaitingVerdict,
    verdict: Verdict,
    evidence: Evidence,
    observed_tracking_sha: Option<&str>,
) -> Result<String> {
    let id = Ulid::new().to_string();
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM forward_reconciliations WHERE run_id = ?1",
            [&pending.run_id],
            |row| row.get(0),
        )
        .unwrap_or(1);
    tx.execute(
        "INSERT INTO forward_reconciliations (
            id, run_id, deliver_attempt_id, seq, verdict, ref_name, authorized_sha,
            evidence, observed_tracking_sha, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            &id,
            &pending.run_id,
            pending.deliver_attempt_id.as_str(),
            seq,
            verdict.as_str(),
            &pending.ref_name,
            &pending.authorized_sha,
            evidence.as_str(),
            observed_tracking_sha,
            &now_secs(),
        ],
    )?;
    Ok(id)
}

/// Recorded conclusions for a run, in sequence order.
///
/// A later conclusion about a terminal run's attempt is an append, not a
/// reclassification, so more than one may exist; the latest is current and the
/// earlier ones are retained.
///
/// # Errors
///
/// Returns a storage error if the query fails, or if a row carries a verdict
/// this binary does not know.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn verdicts_for_run(db: &Db, run_id: &str) -> Result<Vec<VerdictRow>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, run_id, deliver_attempt_id, seq, verdict, ref_name, authorized_sha,
                evidence, observed_tracking_sha, created_at
         FROM forward_reconciliations
         WHERE run_id = ?1
         ORDER BY seq, id",
    )?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(VerdictRow {
            id: row.get(0)?,
            run_id: row.get(1)?,
            deliver_attempt_id: AttemptId::from_raw(row.get::<_, String>(2)?),
            seq: row.get(3)?,
            verdict: Verdict::parse(&row.get::<_, String>(4)?)?,
            ref_name: row.get(5)?,
            authorized_sha: row.get(6)?,
            evidence: row.get(7)?,
            observed_tracking_sha: row.get(8)?,
            created_at: row.get(9)?,
        });
    }
    Ok(out)
}

/// What the operator is told, given a verdict.
///
/// States the remedy, not only the diagnosis: without it the build ships a
/// better status word and the same stuck operator. Never asserts that a pull
/// request is absent — that is not knowable from local evidence.
///
/// The remedy names `porch rerun` beside the re-push because an operator whose
/// gate died mid-forward usually has nothing left to push: their branch already
/// matches the gate ref, so `git push porch` reports everything up to date and
/// the advice to re-push does nothing. ROAD-9's fault injection found that by
/// following it.
#[must_use]
pub fn operator_note(verdict: Verdict, ref_name: &str) -> Option<String> {
    match verdict {
        Verdict::NotAttempted => None,
        Verdict::ReachedOrigin => Some(format!(
            "{ref_name} carries the authorized commit on origin; pull request state is \
             unrecorded. Re-push to continue, or `porch rerun` if the branch has not moved \
             since: porch adopts an existing pull request for this branch rather than \
             opening a second one."
        )),
        Verdict::Indeterminate => Some(format!(
            "porch invoked the push for {ref_name} and did not record its result, so \
             whether it reached origin is undetermined. Re-push to continue, or \
             `porch rerun` if the branch has not moved since: the push is safe to repeat \
             and porch adopts an existing pull request for this branch rather than \
             opening a second one."
        )),
    }
}
