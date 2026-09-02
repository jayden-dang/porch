//! Keep trusted-config commits reachable while retained rounds need them.

use std::collections::HashSet;
use std::path::Path;

use porch_git::GitDir;

use crate::Result;
use crate::db::Db;

/// Porch-owned ref that pins a trusted-config commit on the bare.
#[must_use]
pub fn config_ref_name(sha: &str) -> String {
    format!("refs/porch/config/{sha}")
}

/// Pin `sha` under `refs/porch/config/<sha>` so gc cannot drop it.
///
/// Call before the round row commits so a failed open leaves a sweepable leak
/// rather than a committed round whose trusted commit is unpinned.
///
/// # Errors
///
/// Returns a git error when `update-ref` fails.
pub fn pin_trusted_config(bare: &GitDir, sha: &str) -> Result<()> {
    porch_git::update_ref(bare, &config_ref_name(sha), sha)?;
    Ok(())
}

/// Remove `refs/porch/config/*` refs whose SHA no retained round still references.
///
/// # Errors
///
/// Returns a storage or git error when listing rounds or deleting refs fails.
///
/// # Panics
///
/// Panics if the database mutex is poisoned.
pub fn sweep_unreferenced(bare: &GitDir, db: &Db) -> Result<usize> {
    let referenced = referenced_trusted_config_shas(db, bare.as_path())?;
    let pinned = list_config_ref_shas(bare)?;
    let mut removed = 0usize;
    for sha in pinned {
        if referenced.contains(&sha) {
            continue;
        }
        porch_git::delete_ref(bare, &config_ref_name(&sha))?;
        removed += 1;
    }
    Ok(removed)
}

fn referenced_trusted_config_shas(db: &Db, bare_path: &Path) -> Result<HashSet<String>> {
    let bare_key = bare_path
        .canonicalize()
        .unwrap_or_else(|_| bare_path.to_path_buf());
    let bare_str = bare_key.to_string_lossy().into_owned();

    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT rr.trusted_config_sha
         FROM review_rounds rr
         INNER JOIN runs r ON r.id = rr.run_id
         INNER JOIN repos repo ON repo.id = r.repo_id
         WHERE repo.bare_path = ?1",
    )?;
    let rows = stmt.query_map([&bare_str], |row| row.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

fn list_config_ref_shas(bare: &GitDir) -> Result<Vec<String>> {
    let out = porch_git::run(
        bare,
        &[
            "for-each-ref",
            "--format=%(refname:strip=3)",
            "refs/porch/config",
        ],
    )?;
    let mut shas = Vec::new();
    for line in porch_git::stdout_trim(&out).lines() {
        let sha = line.trim();
        if !sha.is_empty() {
            shas.push(sha.to_string());
        }
    }
    Ok(shas)
}
