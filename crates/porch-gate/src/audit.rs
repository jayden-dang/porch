//! Derived audit document over durable rounds, instances, and authority events.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::Db;
use crate::rounds::phase::{
    PhaseAttemptRow, PhaseEventKind, PhaseEventRow, attempts_for_run_conn, events_for_run_conn,
};
use crate::rounds::{
    AuthorityEventRecord, AuthorityMemberRecord, applicable_round_id_tx,
    events_for_run_conn as authority_events_for_run_conn,
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

/// Phase slice rebuilt from `phase_events` (or explicitly unavailable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPhase {
    pub kind: String,
    #[serde(default)]
    pub attempts: Vec<AuditAttempt>,
    pub steps: Vec<AuditStep>,
}

/// One phase attempt in the audit tree, with nested operations as children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAttempt {
    pub id: String,
    pub phase: String,
    pub ordinal: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_id: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default)]
    pub children: Vec<AuditAttempt>,
}

/// One step projected from a terminal or evidence phase event.
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

/// String or `{ unavailable }` text field on an audit producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuditText {
    Text(String),
    Unavailable { unavailable: String },
}

/// Reported-version object; always unavailable, never a substitute string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReportedVersion {
    pub unavailable: String,
}

/// Observed producer identity: artifact SHA-256 or unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuditObservedIdentity {
    ArtifactSha256 { artifact_sha256: String },
    Unavailable { unavailable: String },
}

/// Producer invocation on the audit document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditProducer {
    pub round_id: String,
    pub id: String,
    pub slot: i64,
    pub descriptor_equivalence_digest: String,
    pub adapter_kind: AuditText,
    pub declared_engine_kind: AuditText,
    pub reported_version: AuditReportedVersion,
    pub observed_version_identity: AuditObservedIdentity,
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
    #[serde(default)]
    pub producers: Vec<AuditProducer>,
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
    let phase = load_phase(&tx, run_id)?;
    let producers = load_producers(&tx, run_id)?;
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
        unreadable_producer_anomaly(&producers)
    };

    tx.commit()?;

    Ok(AuditDocument {
        schema_version: 3,
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
        phase,
        anomaly,
        producers,
    })
}

#[derive(Deserialize)]
struct ProducerDescriptorView {
    adapter_kind: AuditText,
    declared_engine_kind: AuditText,
    reported_version: AuditReportedVersion,
    observed_version_identity: AuditObservedIdentity,
}

fn load_producers(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditProducer>> {
    let mut stmt = tx.prepare(
        "SELECT r.ordinal, p.round_id, p.id, p.slot, p.descriptor_json,
                p.descriptor_equivalence_digest
         FROM round_producers p
         INNER JOIN review_rounds r ON r.id = p.round_id
         WHERE r.run_id = ?1
         ORDER BY r.ordinal, p.slot, p.id",
    )?;
    let mapped = stmt.query_map([run_id], |row| {
        let descriptor_json: String = row.get(4)?;
        let view = project_descriptor(&descriptor_json);
        Ok(AuditProducer {
            round_id: row.get(1)?,
            id: row.get(2)?,
            slot: row.get(3)?,
            descriptor_equivalence_digest: row.get(5)?,
            adapter_kind: view.adapter_kind,
            declared_engine_kind: view.declared_engine_kind,
            reported_version: view.reported_version,
            observed_version_identity: view.observed_version_identity,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

fn unreadable_producer_anomaly(producers: &[AuditProducer]) -> Option<AuditAnomaly> {
    producers
        .iter()
        .find_map(|producer| match &producer.adapter_kind {
            AuditText::Unavailable { unavailable } => Some(AuditAnomaly {
                code: "unreadable_producer_descriptor".into(),
                detail: unavailable.clone(),
            }),
            AuditText::Text(_) => None,
        })
}

fn project_descriptor(descriptor_json: &str) -> ProducerDescriptorView {
    serde_json::from_str(descriptor_json).unwrap_or_else(|err| {
        let reason = err.to_string();
        ProducerDescriptorView {
            adapter_kind: AuditText::Unavailable {
                unavailable: reason.clone(),
            },
            declared_engine_kind: AuditText::Unavailable {
                unavailable: reason.clone(),
            },
            reported_version: AuditReportedVersion {
                unavailable: reason.clone(),
            },
            observed_version_identity: AuditObservedIdentity::Unavailable {
                unavailable: reason,
            },
        }
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
    let records = authority_events_for_run_conn(tx, run_id)?;
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

fn load_phase(tx: &Transaction<'_>, run_id: &str) -> Result<AuditPhase> {
    let attempts = attempts_for_run_conn(tx, run_id)?;
    let events = events_for_run_conn(tx, run_id)?;
    if events.is_empty() {
        return Ok(AuditPhase {
            kind: "unavailable".into(),
            attempts: Vec::new(),
            steps: Vec::new(),
        });
    }

    let mut started_at: HashMap<&str, String> = HashMap::new();
    let mut terminal: HashMap<&str, (Option<String>, Option<String>)> = HashMap::new();
    for event in &events {
        let aid = event.attempt_id.as_str();
        match event.kind {
            PhaseEventKind::Started => {
                started_at
                    .entry(aid)
                    .or_insert_with(|| event.created_at.clone());
            }
            PhaseEventKind::Terminal => {
                terminal.insert(aid, (event.outcome.clone(), event.cause.clone()));
            }
            PhaseEventKind::Evidence => {}
        }
    }

    let steps = steps_from_events(&attempts, &events);
    let tree = attempt_tree(&attempts, &started_at, &terminal);

    Ok(AuditPhase {
        kind: "phase_events".into(),
        attempts: tree,
        steps,
    })
}

fn attempt_tree(
    attempts: &[PhaseAttemptRow],
    started_at: &HashMap<&str, String>,
    terminal: &HashMap<&str, (Option<String>, Option<String>)>,
) -> Vec<AuditAttempt> {
    let mut nodes: HashMap<String, AuditAttempt> = HashMap::new();
    let mut child_ids: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();

    for attempt in attempts {
        let id = attempt.id.as_str();
        let (term, cause) = terminal.get(id).cloned().unwrap_or((None, None));
        let node = AuditAttempt {
            id: attempt.id.to_string(),
            phase: attempt.phase.as_str().to_string(),
            ordinal: attempt.ordinal,
            operation: attempt.operation_kind.map(|k| k.as_str().to_string()),
            parent_id: attempt
                .parent_attempt_id
                .as_ref()
                .map(std::string::ToString::to_string),
            caused_by_id: attempt
                .caused_by_attempt_id
                .as_ref()
                .map(std::string::ToString::to_string),
            started_at: started_at
                .get(id)
                .cloned()
                .unwrap_or_else(|| attempt.created_at.clone()),
            terminal: term,
            cause,
            children: Vec::new(),
        };
        if let Some(parent) = attempt.parent_attempt_id.as_ref() {
            child_ids
                .entry(parent.to_string())
                .or_default()
                .push(attempt.id.to_string());
        } else {
            roots.push(attempt.id.to_string());
        }
        nodes.insert(attempt.id.to_string(), node);
    }

    roots
        .iter()
        .filter_map(|id| attach_children(id, &mut nodes, &child_ids))
        .collect()
}

fn attach_children(
    id: &str,
    nodes: &mut HashMap<String, AuditAttempt>,
    child_ids: &HashMap<String, Vec<String>>,
) -> Option<AuditAttempt> {
    let mut node = nodes.remove(id)?;
    if let Some(kids) = child_ids.get(id) {
        node.children = kids
            .iter()
            .filter_map(|cid| attach_children(cid, nodes, child_ids))
            .collect();
    }
    Some(node)
}

fn steps_from_events(attempts: &[PhaseAttemptRow], events: &[PhaseEventRow]) -> Vec<AuditStep> {
    let by_id: HashMap<&str, &PhaseAttemptRow> =
        attempts.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut steps = Vec::new();
    let mut terminal_ids = std::collections::HashSet::new();

    for event in events {
        let aid = event.attempt_id.as_str();
        match event.kind {
            PhaseEventKind::Started => {}
            PhaseEventKind::Terminal => {
                terminal_ids.insert(aid.to_string());
                let Some(attempt) = by_id.get(aid) else {
                    continue;
                };
                steps.push(AuditStep {
                    step: step_name(attempt),
                    status: event.outcome.clone().unwrap_or_default(),
                    error: event.cause.clone(),
                });
            }
            PhaseEventKind::Evidence => {
                let Some(attempt) = by_id.get(aid) else {
                    continue;
                };
                steps.push(AuditStep {
                    step: step_name(attempt),
                    status: "evidence".into(),
                    error: event.cause.clone(),
                });
            }
        }
    }

    // Open (nonterminal) attempts appear as a nonterminal step, in event order.
    let mut seen_open = std::collections::HashSet::new();
    for event in events {
        if event.kind != PhaseEventKind::Started {
            continue;
        }
        let aid = event.attempt_id.as_str();
        if terminal_ids.contains(aid) || !seen_open.insert(aid.to_string()) {
            continue;
        }
        let Some(attempt) = by_id.get(aid) else {
            continue;
        };
        steps.push(AuditStep {
            step: step_name(attempt),
            status: "started".into(),
            error: event.cause.clone(),
        });
    }

    steps
}

fn step_name(attempt: &PhaseAttemptRow) -> String {
    attempt.operation_kind.map_or_else(
        || attempt.phase.as_str().to_string(),
        |k| k.as_str().to_string(),
    )
}
