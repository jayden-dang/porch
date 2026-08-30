//! Coverage manifest: every changed path must be pass or skip+reason.

use serde::{Deserialize, Serialize};

use crate::Error;

/// One coverage row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub path: String,
    /// `pass` or `skip`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CoverageEntry {
    /// Reviewed path.
    #[must_use]
    pub fn pass(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            status: "pass".into(),
            reason: None,
        }
    }

    /// Explicit skip with reason.
    #[must_use]
    pub fn skip(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            status: "skip".into(),
            reason: Some(reason.into()),
        }
    }

    /// Whether this row counts as covered for porch assert.
    #[must_use]
    pub fn is_covered(&self) -> bool {
        matches!(self.status.as_str(), "pass" | "skip")
            && (self.status != "skip" || self.reason.as_ref().is_some_and(|r| !r.is_empty()))
    }
}

/// Fail closed if any changed path is missing a pass/skip row.
///
/// # Errors
///
/// Returns [`Error::Coverage`] for the first missing path.
pub fn assert_complete(changed: &[String], entries: &[CoverageEntry]) -> Result<(), Error> {
    for path in changed {
        let Some(row) = entries.iter().find(|e| e.path == *path) else {
            return Err(Error::Coverage(path.clone()));
        };
        if !row.is_covered() {
            return Err(Error::Coverage(path.clone()));
        }
    }
    Ok(())
}

/// Paths porch `files[]` should list (every covered row).
#[must_use]
pub fn files_list(entries: &[CoverageEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.is_covered())
        .map(|e| e.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miss_fails() {
        let entries = vec![CoverageEntry::pass("a.rs")];
        let err = assert_complete(&["a.rs".into(), "b.rs".into()], &entries).unwrap_err();
        assert!(matches!(err, Error::Coverage(ref p) if p == "b.rs"));
    }

    #[test]
    fn skip_requires_reason() {
        let mut e = CoverageEntry::skip("a.rs", "lockfile");
        assert!(e.is_covered());
        e.reason = Some(String::new());
        assert!(!e.is_covered());
    }
}
