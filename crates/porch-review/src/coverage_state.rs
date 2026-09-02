//! Per-path coverage state derivation from producer output.
//!
//! Stateless. Never infers `completed` from mere path presence — an explicit
//! completion signal is required.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Error, ReviewJson};

/// Coverage state recorded for one changed path under one producer invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Selected,
    Completed,
    Failed,
    Waived,
}

impl CoverageState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Waived => "waived",
        }
    }
}

/// One derived coverage row for a changed path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub path: String,
    pub state: CoverageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_evidence: Option<String>,
}

impl CoverageEntry {
    #[must_use]
    pub fn selected(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            state: CoverageState::Selected,
            reason: None,
            authority: None,
            completion_evidence: None,
        }
    }

    #[must_use]
    pub fn completed(path: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some(evidence.into()),
        }
    }

    #[must_use]
    pub fn failed(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            state: CoverageState::Failed,
            reason: Some(reason.into()),
            authority: None,
            completion_evidence: None,
        }
    }

    #[must_use]
    pub fn waived(
        path: impl Into<String>,
        reason: impl Into<String>,
        authority: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            state: CoverageState::Waived,
            reason: Some(reason.into()),
            authority: Some(authority.into()),
            completion_evidence: None,
        }
    }
}

/// Explicit producer signal for one path (manifest row or status entry).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathSignal {
    pub path: String,
    pub reason: Option<String>,
    pub authority: Option<String>,
    pub evidence: Option<String>,
}

impl PathSignal {
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }
}

/// Producer coverage claims used to derive states for the changed inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProducerOutput {
    /// Paths merely present in output (`files[]`, comments) — never completion alone.
    pub present_paths: Vec<String>,
    pub selected: Vec<PathSignal>,
    pub completed: Vec<PathSignal>,
    pub failed: Vec<PathSignal>,
    pub waived: Vec<PathSignal>,
}

impl ProducerOutput {
    /// Build claims from porch-review CLI / OCR JSON.
    #[must_use]
    pub fn from_review_json(parsed: &ReviewJson) -> Self {
        let mut present = BTreeSet::new();
        for p in &parsed.files {
            if !p.is_empty() {
                present.insert(p.clone());
            }
        }
        for c in &parsed.comments {
            if !c.path.is_empty() {
                present.insert(c.path.clone());
            }
        }
        for g in &parsed.groups {
            for f in &g.files {
                if !f.is_empty() {
                    present.insert(f.clone());
                }
            }
        }

        let mut out = Self {
            present_paths: present.into_iter().collect(),
            ..Self::default()
        };

        if let Some(m) = &parsed.manifest {
            out.selected = items_to_signals(&m.coverage.selected);
            out.completed = items_to_signals_completed(&m.coverage.completed, "manifest:completed");
            // OCR "reused" is an explicit completion claim with reuse evidence.
            out.completed.extend(items_to_signals_completed(
                &m.coverage.reused,
                "manifest:reused",
            ));
            out.failed = items_to_signals(&m.coverage.failed);
            out.waived = items_to_signals(&m.coverage.waived);
        }

        // First-party flat status rows (`pass` / `skip`) merge with OCR manifest claims.
        if !parsed.coverage.is_empty() {
            let from_status = Self::from_status_rows(&parsed.coverage);
            out.completed.extend(from_status.completed);
            out.failed.extend(from_status.failed);
            out.waived.extend(from_status.waived);
            out.selected.extend(from_status.selected);
            out.present_paths.extend(from_status.present_paths);
        }

        out
    }

    /// True when every derived entry is `completed` or `waived` (required for Complete).
    #[must_use]
    pub fn meets_required(entries: &[CoverageEntry]) -> bool {
        !entries
            .iter()
            .any(|e| matches!(e.state, CoverageState::Selected | CoverageState::Failed))
    }

    /// Build claims from flat status rows (`pass` / `skip` / …), e.g. quality or agent coverage.
    #[must_use]
    pub fn from_status_rows(rows: &[StatusRow]) -> Self {
        let mut out = Self::default();
        for row in rows {
            if row.path.is_empty() {
                continue;
            }
            let st = row.status.to_ascii_lowercase();
            match st.as_str() {
                "pass" | "completed" => {
                    out.completed.push(PathSignal {
                        path: row.path.clone(),
                        evidence: Some(
                            row.evidence
                                .clone()
                                .unwrap_or_else(|| format!("status:{st}")),
                        ),
                        reason: row.reason.clone(),
                        authority: row.authority.clone(),
                    });
                }
                "skip" | "skipped" | "waived" => {
                    out.waived.push(PathSignal {
                        path: row.path.clone(),
                        reason: row.reason.clone(),
                        authority: Some(row.authority.clone().unwrap_or_else(|| "producer".into())),
                        evidence: row.evidence.clone(),
                    });
                }
                "failed" | "fail" => {
                    out.failed.push(PathSignal {
                        path: row.path.clone(),
                        reason: row.reason.clone(),
                        authority: row.authority.clone(),
                        evidence: row.evidence.clone(),
                    });
                }
                "selected" => {
                    out.selected.push(PathSignal::path(row.path.clone()));
                }
                _ => {
                    out.present_paths.push(row.path.clone());
                }
            }
        }
        out
    }
}

/// Flat coverage row (`status: pass|skip|…`) used by quality/agent producers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StatusRow {
    pub path: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Derive one coverage entry per changed path from producer claims.
///
/// # Errors
///
/// Returns [`Error::Coverage`] when a changed path has no claim and no presence,
/// or when `failed` / `waived` / `completed` signals omit required fields.
pub fn derive_states(
    changed: &[String],
    output: &ProducerOutput,
) -> Result<Vec<CoverageEntry>, Error> {
    let failed = index_signals(&output.failed);
    let waived = index_signals(&output.waived);
    let completed = index_signals(&output.completed);
    let selected = index_signals(&output.selected);
    let present: BTreeSet<&str> = output
        .present_paths
        .iter()
        .map(String::as_str)
        .chain(failed.keys().copied())
        .chain(waived.keys().copied())
        .chain(completed.keys().copied())
        .chain(selected.keys().copied())
        .collect();

    let mut entries = Vec::with_capacity(changed.len());
    for path in changed {
        if let Some(sig) = failed.get(path.as_str()) {
            let reason = require_nonempty(sig.reason.as_deref(), path, "failed reason")?;
            entries.push(CoverageEntry::failed(path.clone(), reason));
            continue;
        }
        if let Some(sig) = waived.get(path.as_str()) {
            let reason = require_nonempty(sig.reason.as_deref(), path, "waived reason")?;
            let authority = require_nonempty(sig.authority.as_deref(), path, "waived authority")?;
            entries.push(CoverageEntry::waived(path.clone(), reason, authority));
            continue;
        }
        if let Some(sig) = completed.get(path.as_str()) {
            let evidence = require_nonempty(
                sig.evidence.as_deref(),
                path,
                "completed completion_evidence",
            )?;
            entries.push(CoverageEntry::completed(path.clone(), evidence));
            continue;
        }
        if selected.contains_key(path.as_str()) || present.contains(path.as_str()) {
            // Presence or explicit selected — never completed without a completion signal.
            entries.push(CoverageEntry::selected(path.clone()));
            continue;
        }
        return Err(Error::Coverage(path.clone()));
    }
    Ok(entries)
}

fn items_to_signals(items: &[crate::CoverageItem]) -> Vec<PathSignal> {
    items
        .iter()
        .filter(|i| !i.path.is_empty())
        .map(|i| PathSignal {
            path: i.path.clone(),
            reason: i.reason.clone(),
            authority: i.authority.clone(),
            evidence: i.evidence.clone(),
        })
        .collect()
}

fn items_to_signals_completed(
    items: &[crate::CoverageItem],
    default_evidence: &str,
) -> Vec<PathSignal> {
    items
        .iter()
        .filter(|i| !i.path.is_empty())
        .map(|i| PathSignal {
            path: i.path.clone(),
            reason: i.reason.clone(),
            authority: i.authority.clone(),
            evidence: Some(
                i.evidence
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| default_evidence.to_string()),
            ),
        })
        .collect()
}

fn index_signals(signals: &[PathSignal]) -> BTreeMap<&str, &PathSignal> {
    let mut map = BTreeMap::new();
    for s in signals {
        map.insert(s.path.as_str(), s);
    }
    map
}

fn require_nonempty<'a>(value: Option<&'a str>, path: &str, field: &str) -> Result<&'a str, Error> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => Ok(v),
        None => Err(Error::Msg(format!(
            "coverage for `{path}` missing required {field}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoverageItem, ReviewCoverage, ReviewManifest};

    #[test]
    fn presence_without_completion_signal_is_selected_not_completed() {
        let output = ProducerOutput {
            present_paths: vec!["a.rs".into(), "b.rs".into()],
            ..ProducerOutput::default()
        };
        let entries = derive_states(&["a.rs".into(), "b.rs".into()], &output).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.state == CoverageState::Selected));
        assert!(entries.iter().all(|e| e.completion_evidence.is_none()));
    }

    #[test]
    fn failed_waived_completed_carry_required_fields() {
        let output = ProducerOutput {
            failed: vec![PathSignal {
                path: "fail.rs".into(),
                reason: Some("parse error".into()),
                ..PathSignal::default()
            }],
            waived: vec![PathSignal {
                path: "skip.rs".into(),
                reason: Some("lockfile".into()),
                authority: Some("producer".into()),
                ..PathSignal::default()
            }],
            completed: vec![PathSignal {
                path: "ok.rs".into(),
                evidence: Some("status:pass".into()),
                ..PathSignal::default()
            }],
            selected: vec![PathSignal::path("sel.rs")],
            ..ProducerOutput::default()
        };
        let changed = vec![
            "fail.rs".into(),
            "skip.rs".into(),
            "ok.rs".into(),
            "sel.rs".into(),
        ];
        let entries = derive_states(&changed, &output).unwrap();
        let by: BTreeMap<_, _> = entries.into_iter().map(|e| (e.path.clone(), e)).collect();

        let failed = &by["fail.rs"];
        assert_eq!(failed.state, CoverageState::Failed);
        assert_eq!(failed.reason.as_deref(), Some("parse error"));

        let waived = &by["skip.rs"];
        assert_eq!(waived.state, CoverageState::Waived);
        assert_eq!(waived.reason.as_deref(), Some("lockfile"));
        assert_eq!(waived.authority.as_deref(), Some("producer"));

        let completed = &by["ok.rs"];
        assert_eq!(completed.state, CoverageState::Completed);
        assert_eq!(
            completed.completion_evidence.as_deref(),
            Some("status:pass")
        );

        assert_eq!(by["sel.rs"].state, CoverageState::Selected);
    }

    #[test]
    fn missing_changed_path_without_skip_fails_closed() {
        let output = ProducerOutput {
            present_paths: vec!["a.rs".into()],
            completed: vec![PathSignal {
                path: "a.rs".into(),
                evidence: Some("status:pass".into()),
                ..PathSignal::default()
            }],
            ..ProducerOutput::default()
        };
        let err = derive_states(&["a.rs".into(), "b.rs".into()], &output).unwrap_err();
        assert!(matches!(err, Error::Coverage(ref p) if p == "b.rs"));
    }

    #[test]
    fn manifest_completed_is_not_inferred_from_files_alone() {
        let parsed = ReviewJson {
            comments: vec![],
            files: vec!["a.rs".into(), "b.rs".into()],
            coverage: vec![],
            groups: vec![],
            manifest: Some(ReviewManifest {
                coverage: ReviewCoverage {
                    completed: vec![CoverageItem {
                        path: "a.rs".into(),
                        evidence: Some("ocr:done".into()),
                        ..CoverageItem::default()
                    }],
                    ..ReviewCoverage::default()
                },
            }),
        };
        let output = ProducerOutput::from_review_json(&parsed);
        let entries = derive_states(&["a.rs".into(), "b.rs".into()], &output).unwrap();
        let by: BTreeMap<_, _> = entries.into_iter().map(|e| (e.path.clone(), e)).collect();
        assert_eq!(by["a.rs"].state, CoverageState::Completed);
        assert_eq!(by["a.rs"].completion_evidence.as_deref(), Some("ocr:done"));
        assert_eq!(by["b.rs"].state, CoverageState::Selected);
    }

    #[test]
    fn top_level_status_coverage_marks_completed() {
        let parsed = ReviewJson {
            comments: vec![],
            files: vec!["a.rs".into()],
            coverage: vec![StatusRow {
                path: "a.rs".into(),
                status: "pass".into(),
                ..StatusRow::default()
            }],
            groups: vec![],
            manifest: None,
        };
        let output = ProducerOutput::from_review_json(&parsed);
        let entries = derive_states(&["a.rs".into()], &output).unwrap();
        assert!(ProducerOutput::meets_required(&entries));
        assert_eq!(entries[0].state, CoverageState::Completed);
        assert_eq!(
            entries[0].completion_evidence.as_deref(),
            Some("status:pass")
        );
    }

    #[test]
    fn status_skip_becomes_waived_with_producer_authority() {
        let rows = vec![
            StatusRow {
                path: "a.rs".into(),
                status: "pass".into(),
                ..StatusRow::default()
            },
            StatusRow {
                path: "Cargo.lock".into(),
                status: "skip".into(),
                reason: Some("lockfile".into()),
                ..StatusRow::default()
            },
        ];
        let entries = derive_states(
            &["a.rs".into(), "Cargo.lock".into()],
            &ProducerOutput::from_status_rows(&rows),
        )
        .unwrap();
        let by: BTreeMap<_, _> = entries.into_iter().map(|e| (e.path.clone(), e)).collect();
        assert_eq!(by["a.rs"].state, CoverageState::Completed);
        assert_eq!(by["Cargo.lock"].state, CoverageState::Waived);
        assert_eq!(by["Cargo.lock"].authority.as_deref(), Some("producer"));
        assert_eq!(by["Cargo.lock"].reason.as_deref(), Some("lockfile"));
    }

    #[test]
    fn waived_without_authority_is_rejected() {
        let output = ProducerOutput {
            waived: vec![PathSignal {
                path: "a.rs".into(),
                reason: Some("secret".into()),
                authority: None,
                ..PathSignal::default()
            }],
            ..ProducerOutput::default()
        };
        let err = derive_states(&["a.rs".into()], &output).unwrap_err();
        assert!(matches!(err, Error::Msg(ref m) if m.contains("authority")));
    }
}
