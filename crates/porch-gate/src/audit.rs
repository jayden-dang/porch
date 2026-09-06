//! Derived audit document over durable rounds, instances, and authority events.

use std::collections::BTreeMap;

use rusqlite::{Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::Db;
use crate::rounds::{
    AuthorityEventRecord, AuthorityMemberRecord, applicable_round_id_tx, events_for_run_conn,
};

/// Watermark pair binding one assembled audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditWatermark {
    pub audit_rev: i64,
    pub review_history_revision: i64,
}

/// Structured consistency anomaly on a still-successful audit response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAnomaly {
    pub code: String,
    pub detail: String,
}

/// Phase projection inferred from `step_results`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPhase {
    pub kind: String,
    pub steps: Vec<AuditStep>,
}

/// One `step_results` row copied into the inferred phase list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditStep {
    pub step: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Round summary in the audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRound {
    pub id: String,
    pub ordinal: i64,
    pub from_sha: String,
    pub to_sha: String,
    pub execution: String,
    pub assurance_completion: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<String>,
}

/// Finding instance row in the audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditInstance {
    pub id: String,
    pub round_id: String,
    pub fingerprint: String,
    pub fingerprint_version: i64,
    pub path: String,
    pub criterion_id: String,
    pub evidence: String,
    pub severity: String,
    pub action: String,
}

/// Authority event member in the audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventMember {
    pub finding_instance_id: String,
    pub role: String,
}

/// Authority event in the audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_round_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_head: Option<String>,
    pub actor_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_changed: Option<bool>,
    pub identity_unavailable: bool,
    pub created_at: String,
    pub members: Vec<AuditEventMember>,
}

/// Fingerprint equivalence group (len ≥ 2), ordered by round ordinal then instance id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedOccurrenceGroup {
    pub fingerprint: String,
    pub fingerprint_version: i64,
    pub instance_ids: Vec<String>,
}

/// Derived audit document as-of a durable watermark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditDocument {
    pub schema_version: u32,
    pub run_id: String,
    pub run_status: String,
    pub completeness: String,
    pub watermark: AuditWatermark,
    pub rounds: Vec<AuditRound>,
    pub instances: Vec<AuditInstance>,
    pub events: Vec<AuditEvent>,
    pub related_occurrences: Vec<RelatedOccurrenceGroup>,
    pub phase: AuditPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<AuditAnomaly>,
}

/// Build a derived audit document for `run_id` from one Deferred `SQLite` snapshot.
///
/// # Errors
///
/// Returns a storage error if the run is missing or a query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn build_audit(db: &Db, run_id: &str) -> Result<AuditDocument> {
    let conn = db.conn();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;

    let (status, findings_json, audit_rev, review_history_revision): (
        String,
        Option<String>,
        i64,
        i64,
    ) = tx.query_row(
        "SELECT status, findings_json, audit_rev, review_history_revision
         FROM runs WHERE id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    let rounds = load_rounds(&tx, run_id)?;
    let instances = load_instances(&tx, run_id)?;
    let events = load_events(&tx, run_id)?;
    let related_occurrences = related_occurrences_from(&instances);
    let steps = load_steps(&tx, run_id)?;
    let has_applicable = applicable_round_id_tx(&tx, run_id)?.is_some();
    let has_identity_unavailable = events.iter().any(|e| e.identity_unavailable);
    let has_legacy_findings = findings_json
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());

    let completeness = if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
        "terminal"
    } else {
        "as_of"
    };

    let anomaly = if matches!(status.as_str(), "parked" | "running")
        && !has_applicable
        && !has_identity_unavailable
        && !has_legacy_findings
    {
        Some(AuditAnomaly {
            code: "inconsistent_review_state".into(),
            detail: "parked or running review has neither an applicable round, \
                     legacy findings_json, nor an identity_unavailable authority event"
                .into(),
        })
    } else {
        None
    };

    tx.commit()?;

    Ok(AuditDocument {
        schema_version: 1,
        run_id: run_id.to_string(),
        run_status: status,
        completeness: completeness.into(),
        watermark: AuditWatermark {
            audit_rev,
            review_history_revision,
        },
        rounds,
        instances,
        events,
        related_occurrences,
        phase: AuditPhase {
            kind: "step_results_inferred".into(),
            steps,
        },
        anomaly,
    })
}

fn load_rounds(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditRound>> {
    let mut stmt = tx.prepare(
        "SELECT id, ordinal, from_sha, to_sha, execution, assurance_completion, finalized_at
         FROM review_rounds
         WHERE run_id = ?1
         ORDER BY ordinal, id",
    )?;
    let mapped = stmt.query_map([run_id], |row| {
        Ok(AuditRound {
            id: row.get(0)?,
            ordinal: row.get(1)?,
            from_sha: row.get(2)?,
            to_sha: row.get(3)?,
            execution: row.get(4)?,
            assurance_completion: row.get(5)?,
            finalized_at: row.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

fn load_instances(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditInstance>> {
    let mut stmt = tx.prepare(
        "SELECT i.id, i.round_id, i.fingerprint, i.fingerprint_version, i.path,
                i.criterion_id, i.evidence, i.severity, i.action
         FROM finding_instances i
         INNER JOIN review_rounds r ON r.id = i.round_id
         WHERE r.run_id = ?1
         ORDER BY r.ordinal, i.id",
    )?;
    let mapped = stmt.query_map([run_id], |row| {
        Ok(AuditInstance {
            id: row.get(0)?,
            round_id: row.get(1)?,
            fingerprint: row.get(2)?,
            fingerprint_version: row.get(3)?,
            path: row.get(4)?,
            criterion_id: row.get(5)?,
            evidence: row.get(6)?,
            severity: row.get(7)?,
            action: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

fn load_events(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditEvent>> {
    let records = events_for_run_conn(tx, run_id)?;
    Ok(records.into_iter().map(audit_event_from_record).collect())
}

fn audit_event_from_record(record: AuthorityEventRecord) -> AuditEvent {
    AuditEvent {
        id: record.id,
        kind: record.kind.as_str().to_string(),
        review_round_id: record.review_round_id,
        reviewed_head: record.reviewed_head,
        actor_kind: record.actor_kind.as_str().to_string(),
        authority_event_id: record.authority_event_id,
        head_changed: record.head_changed,
        identity_unavailable: record.identity_unavailable,
        created_at: record.created_at,
        members: record
            .members
            .into_iter()
            .map(audit_member_from_record)
            .collect(),
    }
}

fn audit_member_from_record(member: AuthorityMemberRecord) -> AuditEventMember {
    AuditEventMember {
        finding_instance_id: member.finding_instance_id,
        role: member.role.as_str().to_string(),
    }
}

fn related_occurrences_from(instances: &[AuditInstance]) -> Vec<RelatedOccurrenceGroup> {
    let mut groups: BTreeMap<(String, i64), Vec<String>> = BTreeMap::new();
    for instance in instances {
        groups
            .entry((instance.fingerprint.clone(), instance.fingerprint_version))
            .or_default()
            .push(instance.id.clone());
    }
    groups
        .into_iter()
        .filter(|(_, ids)| ids.len() >= 2)
        .map(
            |((fingerprint, fingerprint_version), instance_ids)| RelatedOccurrenceGroup {
                fingerprint,
                fingerprint_version,
                instance_ids,
            },
        )
        .collect()
}

fn load_steps(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditStep>> {
    let mut stmt = tx.prepare(
        "SELECT step, status, error FROM step_results
         WHERE run_id = ?1
         ORDER BY created_at, id",
    )?;
    let mapped = stmt.query_map([run_id], |row| {
        Ok(AuditStep {
            step: row.get(0)?,
            status: row.get(1)?,
            error: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}
