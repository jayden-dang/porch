//! Per-finding operator notes under `$PORCH_HOME/runs/<id>/finding_notes.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::home::run_artifact_dir;

/// Path of the per-run finding notes map.
#[must_use]
pub fn finding_notes_path(home: &Path, run_id: &str) -> PathBuf {
    run_artifact_dir(home, run_id).join("finding_notes.json")
}

/// Load `finding_id → note` for a run. Missing file → empty map.
///
/// # Errors
///
/// Returns I/O or JSON errors when the file exists but cannot be read/parsed.
pub fn load_finding_notes(home: &Path, run_id: &str) -> Result<BTreeMap<String, String>> {
    let path = finding_notes_path(home, run_id);
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(&path)?;
    let map: BTreeMap<String, String> =
        serde_json::from_str(&raw).map_err(|e| crate::Error::Other(e.to_string()))?;
    Ok(map)
}

/// Set or clear one finding note. Empty `note` removes the key.
///
/// # Errors
///
/// Returns I/O or JSON errors when the notes file cannot be updated.
pub fn set_finding_note(home: &Path, run_id: &str, finding_id: &str, note: &str) -> Result<()> {
    let mut map = load_finding_notes(home, run_id)?;
    if note.is_empty() {
        map.remove(finding_id);
    } else {
        map.insert(finding_id.to_string(), note.to_string());
    }
    let dir = run_artifact_dir(home, run_id);
    fs::create_dir_all(&dir)?;
    let path = finding_notes_path(home, run_id);
    let json =
        serde_json::to_string_pretty(&map).map_err(|e| crate::Error::Other(e.to_string()))?;
    fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn set_load_and_clear_finding_note() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        set_finding_note(home, "run1", "f0", "prefer early return").unwrap();
        let map = load_finding_notes(home, "run1").unwrap();
        assert_eq!(
            map.get("f0").map(String::as_str),
            Some("prefer early return")
        );
        set_finding_note(home, "run1", "f0", "").unwrap();
        let map = load_finding_notes(home, "run1").unwrap();
        assert!(!map.contains_key("f0"));
    }
}
