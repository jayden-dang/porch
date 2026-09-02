use rusqlite::Transaction;

use super::RoundId;
use crate::Result;
use crate::db::Db;

/// Producer role in a round's required set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Floor,
    Judgment,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Floor => "floor",
            Self::Judgment => "judgment",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "floor" => Ok(Self::Floor),
            "judgment" => Ok(Self::Judgment),
            other => Err(crate::Error::Other(format!(
                "unknown requirement role: {other}"
            ))),
        }
    }
}

/// Whether a required producer was resolved to an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Resolved,
    Unresolved,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "resolved" => Ok(Self::Resolved),
            "unresolved" => Ok(Self::Unresolved),
            other => Err(crate::Error::Other(format!(
                "unknown requirement resolution: {other}"
            ))),
        }
    }
}

/// Input describing one required producer when opening a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementSpec {
    pub slot: i64,
    pub role: Role,
    pub resolution: Resolution,
    pub expected_equivalence_digest: Option<String>,
    pub reason: Option<String>,
}

/// Recorded required-producer row for a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementRow {
    pub slot: i64,
    pub role: Role,
    pub resolution: Resolution,
    pub expected_equivalence_digest: Option<String>,
    pub producer_invocation_id: Option<String>,
    pub reason: Option<String>,
}

const REQUIRED_SET_DOMAIN: &[u8] = b"porch.required_set.v1";
const UNRESOLVED_MARKER: &str = "unresolved";

/// Presentation label for an assurance shape. Authorization uses the digest, not this name.
#[must_use]
pub fn assurance_shape(roles: impl IntoIterator<Item = Role>) -> &'static str {
    if roles.into_iter().any(|role| role == Role::Judgment) {
        "floor+judgment"
    } else {
        "floor-only"
    }
}

/// Presentation label for recorded requirement rows. An empty set has no shape.
#[must_use]
pub fn assurance_shape_for_rows(rows: &[RequirementRow]) -> Option<&'static str> {
    if rows.is_empty() {
        None
    } else {
        Some(assurance_shape(rows.iter().map(|row| row.role)))
    }
}

/// Canonical digest of a required producer set.
#[must_use]
pub fn required_set_digest(protocol_version: i64, rows: &[RequirementRow]) -> String {
    let mut ordered: Vec<&RequirementRow> = rows.iter().collect();
    ordered.sort_by_key(|row| row.slot);

    let version = protocol_version.to_string();
    let mut owned: Vec<Vec<u8>> = vec![REQUIRED_SET_DOMAIN.to_vec(), version.into_bytes()];
    for row in ordered {
        owned.push(row.role.as_str().as_bytes().to_vec());
        owned.push(row.resolution.as_str().as_bytes().to_vec());
        let third = match row.resolution {
            Resolution::Resolved => row.expected_equivalence_digest.as_deref().unwrap_or(""),
            Resolution::Unresolved => UNRESOLVED_MARKER,
        };
        owned.push(third.as_bytes().to_vec());
    }
    let parts: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    super::sha256_hex(&super::length_delimited_join(&parts))
}

/// Canonical digest of a required producer set described by open-round specs.
///
/// Same preimage as [`required_set_digest`]. Invocation ids and reasons are not hashed.
#[must_use]
pub fn digest_for_specs(protocol_version: i64, specs: &[RequirementSpec]) -> String {
    let rows: Vec<RequirementRow> = specs
        .iter()
        .map(|spec| RequirementRow {
            slot: spec.slot,
            role: spec.role,
            resolution: spec.resolution,
            expected_equivalence_digest: spec.expected_equivalence_digest.clone(),
            producer_invocation_id: None,
            reason: spec.reason.clone(),
        })
        .collect();
    required_set_digest(protocol_version, &rows)
}

/// The run's pinned required-set digest, if the first round has opened.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn run_required_set_digest(db: &Db, run_id: &str) -> Result<Option<String>> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT required_set_digest FROM runs WHERE id = ?1")?;
    let mut rows = stmt.query([run_id])?;
    match rows.next()? {
        Some(row) => Ok(row.get(0)?),
        None => Ok(None),
    }
}

pub(super) fn pin_run_required_set(tx: &Transaction<'_>, run_id: &str, digest: &str) -> Result<()> {
    let updated = tx.execute(
        "UPDATE runs SET required_set_digest = ?1 WHERE id = ?2 AND required_set_digest IS NULL",
        rusqlite::params![digest, run_id],
    )?;
    if updated == 1 {
        return Ok(());
    }
    let existing: Option<String> = tx.query_row(
        "SELECT required_set_digest FROM runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    match existing {
        Some(pin) if pin == digest => Ok(()),
        Some(_) => Err(crate::Error::Other(
            "required-set digest does not match the run pin".into(),
        )),
        None => Err(crate::Error::Other(
            "cannot pin a required-set digest on a missing run".into(),
        )),
    }
}

/// Load the required producer set recorded for `round_id`, ordered by slot.
///
/// # Errors
///
/// Returns a storage error if the query fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn requirements_for_round(db: &Db, round_id: &RoundId) -> Result<Vec<RequirementRow>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT requirement_slot, role, resolution, expected_equivalence_digest,
                producer_invocation_id, resolution_reason
         FROM round_required_producers
         WHERE round_id = ?1
         ORDER BY requirement_slot",
    )?;
    let mut rows = stmt.query([round_id.as_str()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let role: String = row.get(1)?;
        let resolution: String = row.get(2)?;
        out.push(RequirementRow {
            slot: row.get(0)?,
            role: Role::parse(&role)?,
            resolution: Resolution::parse(&resolution)?,
            expected_equivalence_digest: row.get(3)?,
            producer_invocation_id: row.get(4)?,
            reason: row.get(5)?,
        });
    }
    Ok(out)
}

pub(super) fn insert_requirements(
    tx: &Transaction<'_>,
    round_id: &str,
    specs: &[RequirementSpec],
    producer_ids: &[String],
) -> Result<()> {
    for spec in specs {
        let producer_invocation_id = match spec.resolution {
            Resolution::Resolved => {
                let idx = usize::try_from(spec.slot)
                    .map_err(|_| crate::Error::Other("requirement slot exceeds usize".into()))?;
                let id = producer_ids.get(idx).ok_or_else(|| {
                    crate::Error::Other(format!(
                        "resolved requirement slot {} has no producer invocation",
                        spec.slot
                    ))
                })?;
                Some(id.clone())
            }
            Resolution::Unresolved => None,
        };
        tx.execute(
            "INSERT INTO round_required_producers (
                round_id, requirement_slot, role, resolution,
                expected_equivalence_digest, producer_invocation_id, resolution_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                round_id,
                spec.slot,
                spec.role.as_str(),
                spec.resolution.as_str(),
                spec.expected_equivalence_digest,
                producer_invocation_id,
                spec.reason,
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        RequirementRow, RequirementSpec, Resolution, Role, assurance_shape, digest_for_specs,
        required_set_digest,
    };

    fn sample_spec() -> RequirementSpec {
        RequirementSpec {
            slot: 0,
            role: Role::Floor,
            resolution: Resolution::Resolved,
            expected_equivalence_digest: Some("abc".into()),
            reason: Some("ignored-in-digest".into()),
        }
    }

    #[test]
    fn digest_for_specs_matches_required_set_digest() {
        let spec = sample_spec();
        let row = RequirementRow {
            slot: spec.slot,
            role: spec.role,
            resolution: spec.resolution,
            expected_equivalence_digest: spec.expected_equivalence_digest.clone(),
            producer_invocation_id: Some("inv-not-hashed".into()),
            reason: Some("other-reason".into()),
        };
        assert_eq!(
            digest_for_specs(2, std::slice::from_ref(&spec)),
            required_set_digest(2, std::slice::from_ref(&row))
        );
    }

    #[test]
    fn assurance_shape_is_floor_only_unless_judgment_is_present() {
        assert_eq!(assurance_shape([Role::Floor]), "floor-only");
        assert_eq!(
            assurance_shape([Role::Floor, Role::Judgment]),
            "floor+judgment"
        );
        assert_eq!(
            assurance_shape(sample_spec_roles(&[sample_spec()])),
            "floor-only"
        );
        let mut with_judgment = sample_spec();
        with_judgment.slot = 1;
        with_judgment.role = Role::Judgment;
        assert_eq!(
            assurance_shape(sample_spec_roles(&[sample_spec(), with_judgment])),
            "floor+judgment"
        );
    }

    fn sample_spec_roles(specs: &[RequirementSpec]) -> impl Iterator<Item = Role> + '_ {
        specs.iter().map(|spec| spec.role)
    }
}
