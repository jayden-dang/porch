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
