//! Whether a recorded round may authorize the current change.

use std::collections::BTreeMap;

use ulid::Ulid;

use super::{
    AssuranceCompletion, ContextApplication, ContextApplicationRecord, ContextElement,
    CoverageState, ExecutionState, RoundBindings, RoundId, capture_context_element,
    context_applications_for_round, context_elements_for_round, coverage_for_round, get_round,
    producers_for_round, rounds_for_run, sha256_hex,
};
use super::{ContextSource, SnapshotState};
use crate::Result;
use crate::db::{Db, RunRow};

const EQUIVALENCE_DOMAIN: &[u8] = b"porch-producer-equivalence/v1";

/// Observed artifact identity used in the equivalence preimage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedVersionForEquivalence {
    ArtifactSha256(String),
    /// Reason is audit-only; digest uses `unavailable` plus a fresh nonce.
    Unavailable {
        reason: String,
    },
}

/// Equivalence-bearing producer fields; audit-only fields omitted.
#[derive(Debug, Clone)]
pub struct EquivalenceInput<'a> {
    pub adapter_kind: &'a str,
    pub argv_prefix: &'a [String],
    pub observed_version: ObservedVersionForEquivalence,
    pub consumed_context: &'a [String],
}

/// Outcome of asking whether any round may authorize the current change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applicability {
    Applicable(RoundId),
    RequiresNew { reason: String },
}

/// SHA-256 hex of the producer equivalence preimage.
///
/// `selection_source`, `declared_engine_kind`, and `reported_version` are absent.
/// An unavailable observed identity includes a per-invocation nonce so two calls
/// never compare equal.
#[must_use]
pub fn descriptor_equivalence_digest(input: &EquivalenceInput<'_>) -> String {
    let argv_joined = input.argv_prefix.join("\u{1f}");
    let mut consumed: Vec<&str> = input.consumed_context.iter().map(String::as_str).collect();
    consumed.sort_unstable();
    let consumed_joined = consumed.join("\u{1f}");

    let observed = match &input.observed_version {
        ObservedVersionForEquivalence::ArtifactSha256(hex) => hex.clone(),
        ObservedVersionForEquivalence::Unavailable { reason: _ } => {
            format!("unavailable{}", Ulid::new())
        }
    };

    let preimage = length_delimited_join(&[
        EQUIVALENCE_DOMAIN,
        input.adapter_kind.as_bytes(),
        argv_joined.as_bytes(),
        observed.as_bytes(),
        consumed_joined.as_bytes(),
    ]);
    sha256_hex(&preimage)
}

/// Find a recorded round that may authorize the current change, or require a new one.
///
/// # Errors
///
/// Returns a storage error when round rows cannot be read.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn applicable_round(
    db: &Db,
    run_id: &str,
    bindings: &RoundBindings,
    required_producer_digests: &[String],
) -> Result<Applicability> {
    let rounds = rounds_for_run(db, run_id)?;
    // Newest ordinal first (store returns ascending).
    for round in rounds.into_iter().rev() {
        if round_is_applicable(db, &round.id, bindings, required_producer_digests)? {
            return Ok(Applicability::Applicable(round.id));
        }
    }
    Ok(Applicability::RequiresNew {
        reason: "no applicable round may authorize the current change".into(),
    })
}

/// Serve/authorize helper: reconstruct decision bindings from the run tip and ask
/// [`applicable_round`].
///
/// Inventory, producer digests, and context applications come from the newest
/// tip-matching finished/complete round (seed). Live run fields that can drift
/// (`head_sha`, `trusted_config_sha`, intent presence) are taken from `run` so a
/// mismatched parked decision fails closed.
///
/// # Errors
///
/// Returns a storage error when round or blob rows cannot be read.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn applicable_round_for_run(db: &Db, run: &RunRow) -> Result<Applicability> {
    let Some((bindings, required)) = decision_bindings_for_run(db, run)? else {
        return Ok(Applicability::RequiresNew {
            reason: "no tip-complete round available to reconstruct decision bindings".into(),
        });
    };
    applicable_round(db, &run.id, &bindings, &required)
}

/// Build current decision bindings + required producer digests for serve/authorize.
fn decision_bindings_for_run(
    db: &Db,
    run: &RunRow,
) -> Result<Option<(RoundBindings, Vec<String>)>> {
    let Some(seed) = tip_complete_seed(db, run)? else {
        return Ok(None);
    };

    let inventory_bytes = blob_bytes(db, &seed.inventory_digest)?;
    let producers = producers_for_round(db, &seed.id)?;
    if producers.is_empty() {
        return Ok(None);
    }
    let required: Vec<String> = producers
        .iter()
        .map(|p| p.descriptor_equivalence_digest.clone())
        .collect();

    let id_to_slot: BTreeMap<&str, usize> = producers
        .iter()
        .map(|p| (p.id.as_str(), usize::try_from(p.slot).unwrap_or(usize::MAX)))
        .collect();

    let recorded_apps = context_applications_for_round(db, &seed.id)?;
    let mut context_applications = Vec::with_capacity(recorded_apps.len());
    for app in &recorded_apps {
        let Some(&slot) = id_to_slot.get(app.producer_invocation_id.as_str()) else {
            return Ok(None);
        };
        if slot == usize::MAX {
            return Ok(None);
        }
        context_applications.push(ContextApplication {
            element_name: app.element_name.clone(),
            producer_slot: slot,
            application: app.application,
            effective_digest: app.effective_digest.clone(),
        });
    }

    let recorded_elements = context_elements_for_round(db, &seed.id)?;
    let mut context_elements: Vec<ContextElement> =
        recorded_elements.iter().map(element_from_record).collect();

    // Intent presence follows the live run so a cleared/changed intent fails closed.
    let intent = match run.intent.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => capture_context_element(
            "intent",
            ContextSource::Present {
                bytes: s.as_bytes().to_vec(),
            },
        ),
        _ => capture_context_element("intent", ContextSource::Absent { reason: None }),
    };
    if let Some(pos) = context_elements
        .iter()
        .position(|e| e.element_name == "intent")
    {
        context_elements[pos] = intent;
    } else {
        context_elements.push(intent);
    }

    let to_sha = match run.head_sha.as_deref() {
        Some(h) => h.to_string(),
        None => seed.to_sha.clone(),
    };
    let trusted_config_sha = match run.trusted_config_sha.as_deref() {
        Some(s) => s.to_string(),
        None => seed.trusted_config_sha.clone(),
    };

    Ok(Some((
        RoundBindings {
            from_sha: seed.from_sha.clone(),
            to_sha,
            inventory_digest: seed.inventory_digest.clone(),
            inventory_bytes,
            trusted_config_sha,
            protocol_schema_version: seed.protocol_schema_version,
            fingerprint_version: seed.fingerprint_version,
            intent_source: run.intent_source.clone(),
            context_elements,
            context_applications,
        },
        required,
    )))
}

fn tip_complete_seed(db: &Db, run: &RunRow) -> Result<Option<super::RoundRecord>> {
    let rounds = rounds_for_run(db, &run.id)?;
    for round in rounds.into_iter().rev() {
        if round.finalized_at.is_none() {
            continue;
        }
        if round.execution != ExecutionState::Finished {
            continue;
        }
        if round.assurance_completion != AssuranceCompletion::Complete {
            continue;
        }
        if let Some(head) = run.head_sha.as_deref() {
            if round.to_sha != head {
                continue;
            }
        }
        return Ok(Some(round));
    }
    Ok(None)
}

fn element_from_record(rec: &super::ContextElementRecord) -> ContextElement {
    ContextElement {
        element_name: rec.element_name.clone(),
        source_state: rec.source_state,
        source_reason: rec.source_reason.clone(),
        snapshot_state: rec.snapshot_state,
        snapshot_reason: rec.snapshot_reason.clone(),
        snapshot_digest: rec.snapshot_digest.clone(),
        snapshot_bytes: if rec.snapshot_state == SnapshotState::Stored {
            rec.snapshot_bytes.clone()
        } else {
            None
        },
    }
}

fn blob_bytes(db: &Db, digest: &str) -> Result<Vec<u8>> {
    let conn = db.conn();
    conn.query_row(
        "SELECT bytes FROM content_blobs WHERE digest = ?1",
        [digest],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => crate::Error::Other(format!(
            "missing content blob for inventory digest {digest}"
        )),
        other => other.into(),
    })
}

fn round_is_applicable(
    db: &Db,
    round_id: &RoundId,
    bindings: &RoundBindings,
    required_producer_digests: &[String],
) -> Result<bool> {
    let Some(round) = get_round(db, round_id)? else {
        return Ok(false);
    };

    if round.finalized_at.is_none()
        || round.execution != ExecutionState::Finished
        || round.assurance_completion != AssuranceCompletion::Complete
    {
        return Ok(false);
    }

    let coverage = coverage_for_round(db, round_id)?;
    if coverage
        .iter()
        .any(|row| matches!(row.state, CoverageState::Selected | CoverageState::Failed))
    {
        return Ok(false);
    }

    if round.from_sha != bindings.from_sha
        || round.to_sha != bindings.to_sha
        || round.inventory_digest != bindings.inventory_digest
        || round.trusted_config_sha != bindings.trusted_config_sha
        || round.protocol_schema_version != bindings.protocol_schema_version
        || round.fingerprint_version != bindings.fingerprint_version
    {
        return Ok(false);
    }

    let elements = context_elements_for_round(db, round_id)?;
    let mut recorded_sources: BTreeMap<&str, _> = BTreeMap::new();
    for element in &elements {
        recorded_sources.insert(element.element_name.as_str(), element.source_state);
    }
    if recorded_sources.len() != bindings.context_elements.len() {
        return Ok(false);
    }
    for element in &bindings.context_elements {
        match recorded_sources.get(element.element_name.as_str()) {
            Some(state) if *state == element.source_state => {}
            _ => return Ok(false),
        }
    }

    let producers = producers_for_round(db, round_id)?;
    let Some(bijection) = producer_bijection(&producers, required_producer_digests) else {
        return Ok(false);
    };

    let recorded_apps = context_applications_for_round(db, round_id)?;
    if !applications_match(&recorded_apps, &bindings.context_applications, &bijection) {
        return Ok(false);
    }

    Ok(true)
}

/// Map required producer slot → recorded producer invocation id.
fn producer_bijection(
    producers: &[super::ProducerRecord],
    required_digests: &[String],
) -> Option<BTreeMap<usize, String>> {
    if producers.len() != required_digests.len() {
        return None;
    }

    let mut by_digest: BTreeMap<&str, Vec<&super::ProducerRecord>> = BTreeMap::new();
    for producer in producers {
        by_digest
            .entry(producer.descriptor_equivalence_digest.as_str())
            .or_default()
            .push(producer);
    }

    let mut map = BTreeMap::new();
    for (slot, digest) in required_digests.iter().enumerate() {
        let bucket = by_digest.get_mut(digest.as_str())?;
        let producer = bucket.pop()?;
        map.insert(slot, producer.id.clone());
    }
    if by_digest.values().any(|v| !v.is_empty()) {
        return None;
    }
    Some(map)
}

fn applications_match(
    recorded: &[ContextApplicationRecord],
    current: &[super::ContextApplication],
    slot_to_recorded_id: &BTreeMap<usize, String>,
) -> bool {
    let mut recorded_by_key: BTreeMap<(&str, &str), &ContextApplicationRecord> = BTreeMap::new();
    for app in recorded {
        recorded_by_key.insert(
            (
                app.element_name.as_str(),
                app.producer_invocation_id.as_str(),
            ),
            app,
        );
    }

    if recorded.len() != current.len() {
        return false;
    }

    for app in current {
        let Some(recorded_id) = slot_to_recorded_id.get(&app.producer_slot) else {
            return false;
        };
        let Some(rec) = recorded_by_key.remove(&(app.element_name.as_str(), recorded_id.as_str()))
        else {
            return false;
        };
        if rec.application != app.application || rec.effective_digest != app.effective_digest {
            return false;
        }
    }

    recorded_by_key.is_empty()
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
