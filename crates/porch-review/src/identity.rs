//! Porch-owned finding contract and producer-independent candidate keys.
//!
//! Stateless — mints no fingerprints and reads no history. Reconciliation assigns
//! canonical fingerprints later from these keys.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Finding, ReviewComment, map_comment};

/// Fingerprint / candidate-key algorithm version in force.
pub const FINGERPRINT_VERSION: u32 = 1;

const CANDIDATE_DOMAIN: &[u8] = b"porch-candidate-key/v1";

/// Registered producer `rule_id` → porch-normalized `criterion_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CriterionMapping {
    by_rule: BTreeMap<String, String>,
}

impl CriterionMapping {
    /// Empty mapping (category / unclassified fallback only).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Built-in first-party quality rule mappings (`pack/rule` → canonical criterion).
    #[must_use]
    pub fn builtin() -> Self {
        let mut m = Self::empty();
        for id in [
            "rust/unwrap-in-lib",
            "rust/expect-in-lib",
            "rust/dbg-macro",
            "rust/todo-macro",
            "js/loose-equality",
            "js/console-log",
            "js/var-keyword",
            "generic/leftover-conflict-marker",
            "generic/todo-fixme-blocker",
        ] {
            m.insert(id, id);
        }
        m
    }

    /// Register or replace one mapping.
    pub fn insert(&mut self, rule_id: impl Into<String>, criterion_id: impl Into<String>) {
        self.by_rule.insert(rule_id.into(), criterion_id.into());
    }

    /// Resolve criterion: mapped `rule_id`, else normalized category, else `unclassified`.
    #[must_use]
    pub fn resolve(&self, rule_id: Option<&str>, category: Option<&str>) -> String {
        if let Some(rid) = rule_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(c) = self.by_rule.get(rid) {
                return c.clone();
            }
        }
        if let Some(cat) = category.map(str::trim).filter(|s| !s.is_empty()) {
            return cat.to_ascii_lowercase();
        }
        "unclassified".into()
    }
}

/// Producer-local key retained as provenance only (never identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Provenance {
    /// Producer-supplied finding / rule key (`rule_id`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_key: Option<String>,
}

/// Optional confidence typed by producer epistemology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confidence {
    pub value: String,
    pub kind: ConfidenceKind,
}

/// How a confidence value was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceKind {
    /// Stochastic / model-reported score.
    Model,
    /// Deterministic producer signal (not manufactured for quality engine findings).
    Deterministic,
}

/// Structural anchor used in the candidate key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub value: String,
}

/// Anchor kinds in resolution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Symbol,
    Hunk,
    Snippet,
    None,
}

impl AnchorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::Hunk => "hunk",
            Self::Snippet => "snippet",
            Self::None => "none",
        }
    }
}

/// Producer-independent candidate key (not a fingerprint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateKey {
    pub digest: String,
    pub fingerprint_version: u32,
    pub path_key: String,
    pub criterion_id: String,
    pub anchor_kind: String,
    pub anchor_value: String,
}

/// Optional inputs for anchor resolution beyond the finding itself.
#[derive(Debug, Clone, Default)]
pub struct AnchorContext<'a> {
    pub file_content: Option<&'a str>,
    pub hunk_header: Option<&'a str>,
}

/// Map a raw comment into an enriched finding (severity/action + contract fields).
///
/// Deterministic producers pass `deterministic = true` so model-style confidence is dropped.
#[must_use]
pub fn enrich_from_comment(
    comment: &ReviewComment,
    mapping: &CriterionMapping,
    anchor_ctx: &AnchorContext<'_>,
    deterministic: bool,
) -> Option<Finding> {
    let mut finding = map_comment(comment)?;
    apply_contract(&mut finding, comment, mapping, anchor_ctx, deterministic);
    Some(finding)
}

/// Fill criterion, evidence, consequence, provenance, confidence, and anchor on `finding`.
pub fn apply_contract(
    finding: &mut Finding,
    comment: &ReviewComment,
    mapping: &CriterionMapping,
    anchor_ctx: &AnchorContext<'_>,
    deterministic: bool,
) {
    let producer_key = comment
        .rule_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    finding.provenance = Some(Provenance {
        producer_key: producer_key.clone(),
    });

    finding.criterion_id =
        Some(mapping.resolve(producer_key.as_deref(), finding.category.as_deref()));

    let evidence = comment
        .existing_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(comment.content.as_str())
        .to_string();
    finding.evidence = Some(evidence);
    finding.consequence = Some(comment.content.clone());

    if deterministic {
        finding.confidence = None;
    } else {
        finding.confidence.clone_from(&comment.confidence);
    }

    let anchor = resolve_anchor(
        &finding.path,
        finding.start_line,
        comment.existing_code.as_deref(),
        anchor_ctx,
    );
    finding.anchor_kind = Some(anchor.kind.as_str().to_string());
    finding.anchor_value = Some(anchor.value);
}

/// Derive the producer-independent candidate key for an enriched finding.
#[must_use]
pub fn derive(finding: &Finding, mapping: &CriterionMapping) -> CandidateKey {
    let path_key = path_key(&finding.path);
    let criterion_id = finding.criterion_id.clone().unwrap_or_else(|| {
        mapping.resolve(
            finding
                .provenance
                .as_ref()
                .and_then(|p| p.producer_key.as_deref()),
            finding.category.as_deref(),
        )
    });
    let anchor_kind = finding
        .anchor_kind
        .clone()
        .unwrap_or_else(|| AnchorKind::None.as_str().to_string());
    let anchor_value = finding.anchor_value.clone().unwrap_or_default();

    let digest = candidate_digest(
        FINGERPRINT_VERSION,
        &path_key,
        &criterion_id,
        &anchor_kind,
        &anchor_value,
    );

    CandidateKey {
        digest,
        fingerprint_version: FINGERPRINT_VERSION,
        path_key,
        criterion_id,
        anchor_kind,
        anchor_value,
    }
}

/// Repository-relative path as git reports it (no `./` prefix; no Unicode normalization).
#[must_use]
pub fn path_key(path: &str) -> String {
    let p = path.trim();
    let stripped = p.strip_prefix("./").unwrap_or(p);
    stripped.to_string()
}

pub(crate) fn candidate_digest(
    fingerprint_version: u32,
    path_key: &str,
    criterion_id: &str,
    anchor_kind: &str,
    anchor_value: &str,
) -> String {
    let ver = fingerprint_version.to_string();
    let preimage = length_delimited_join(&[
        CANDIDATE_DOMAIN,
        ver.as_bytes(),
        path_key.as_bytes(),
        criterion_id.as_bytes(),
        anchor_kind.as_bytes(),
        anchor_value.as_bytes(),
    ]);
    hex::encode(Sha256::digest(&preimage))
}

fn resolve_anchor(
    path: &str,
    start_line: Option<u32>,
    existing_code: Option<&str>,
    ctx: &AnchorContext<'_>,
) -> Anchor {
    if let Some(content) = ctx.file_content {
        if let Some(sym) = symbol_anchor(path, content, start_line) {
            return Anchor {
                kind: AnchorKind::Symbol,
                value: sym,
            };
        }
    }
    if let Some(hunk) = ctx.hunk_header.map(str::trim).filter(|s| !s.is_empty()) {
        return Anchor {
            kind: AnchorKind::Hunk,
            value: hunk.to_string(),
        };
    }
    if let Some(snippet) = snippet_anchor(existing_code) {
        return Anchor {
            kind: AnchorKind::Snippet,
            value: snippet,
        };
    }
    Anchor {
        kind: AnchorKind::None,
        value: String::new(),
    }
}

fn symbol_anchor(path: &str, file_content: &str, start_line: Option<u32>) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext != "rs" {
        return None;
    }
    let line_no = start_line? as usize;
    if line_no == 0 {
        return None;
    }
    let lines: Vec<&str> = file_content.lines().collect();
    let idx = line_no - 1;
    if idx >= lines.len() {
        return None;
    }
    for line in lines[..=idx].iter().rev() {
        if let Some(decl) = rust_declaration(line) {
            return Some(decl);
        }
    }
    None
}

fn rust_declaration(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let re = regex_lite_rust_decl(trimmed)?;
    Some(re)
}

/// Match `^\s*(pub\s+)?(async\s+)?(fn|struct|enum|trait|impl|mod|macro_rules!)\b` and return a
/// short anchor value (`fn name`, `struct Name`, `impl …`).
fn regex_lite_rust_decl(trimmed: &str) -> Option<String> {
    let mut s = trimmed;
    if let Some(rest) = s.strip_prefix("pub ") {
        s = rest.trim_start();
    }
    if let Some(rest) = s.strip_prefix("async ") {
        s = rest.trim_start();
    }
    let kinds = [
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "mod ",
        "macro_rules!",
    ];
    for kind in kinds {
        if let Some(rest) = s.strip_prefix(kind) {
            if kind == "macro_rules!" {
                let name = rest
                    .trim_start()
                    .trim_start_matches('!')
                    .split(|c: char| c.is_whitespace() || c == '{' || c == '(' || c == '[')
                    .next()
                    .unwrap_or("")
                    .trim();
                return if name.is_empty() {
                    Some("macro_rules!".into())
                } else {
                    Some(format!("macro_rules! {name}"))
                };
            }
            if kind == "impl " {
                let body = rest.trim();
                let short = body
                    .split('{')
                    .next()
                    .unwrap_or(body)
                    .split_whitespace()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" ");
                return Some(format!("impl {short}"));
            }
            let name = rest
                .trim_start()
                .split(|c: char| {
                    c.is_whitespace() || c == '<' || c == '(' || c == '{' || c == ':' || c == '!'
                })
                .next()
                .unwrap_or("")
                .trim();
            let label = kind.trim();
            return if name.is_empty() {
                Some(label.to_string())
            } else {
                Some(format!("{label} {name}"))
            };
        }
    }
    // `fn` without trailing space already handled via "fn "; also bare `fn\t`.
    None
}

fn snippet_anchor(existing_code: Option<&str>) -> Option<String> {
    let code = existing_code?;
    for line in code.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            return Some(collapsed);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Severity};

    fn quality_comment() -> ReviewComment {
        ReviewComment {
            path: "src/lib.rs".into(),
            content: "[rust/unwrap-in-lib] .unwrap() in non-test Rust".into(),
            existing_code: Some("    let x = foo().unwrap();".into()),
            suggestion_code: None,
            start_line: Some(3),
            end_line: Some(3),
            category: Some("bug".into()),
            severity: Some("medium".into()),
            rule_id: Some("rust/unwrap-in-lib".into()),
            confidence: None,
        }
    }

    #[test]
    fn quality_rule_id_maps_to_canonical_criterion() {
        let mapping = CriterionMapping::builtin();
        let file = "\
pub fn other() {}\n\
fn load() {\n\
    let x = foo().unwrap();\n\
}\n";
        let finding = enrich_from_comment(
            &quality_comment(),
            &mapping,
            &AnchorContext {
                file_content: Some(file),
                hunk_header: None,
            },
            true,
        )
        .unwrap();

        assert_eq!(
            finding
                .provenance
                .as_ref()
                .and_then(|p| p.producer_key.as_deref()),
            Some("rust/unwrap-in-lib")
        );
        assert_eq!(finding.criterion_id.as_deref(), Some("rust/unwrap-in-lib"));

        let key = derive(&finding, &mapping);
        assert_eq!(key.criterion_id, "rust/unwrap-in-lib");
        assert_eq!(key.path_key, "src/lib.rs");
        assert_eq!(key.anchor_kind, "symbol");
        assert_eq!(key.anchor_value, "fn load");
        assert_eq!(key.fingerprint_version, FINGERPRINT_VERSION);
        assert_eq!(key.digest.len(), 64);
    }

    #[test]
    fn producer_key_is_provenance_not_candidate_identity() {
        let mut mapping = CriterionMapping::builtin();
        mapping.insert("rust/unwrap-in-lib", "rust/prefer-question-mark");

        let finding = enrich_from_comment(
            &quality_comment(),
            &mapping,
            &AnchorContext::default(),
            true,
        )
        .unwrap();

        let key = derive(&finding, &mapping);
        assert_eq!(
            finding
                .provenance
                .as_ref()
                .and_then(|p| p.producer_key.as_deref()),
            Some("rust/unwrap-in-lib")
        );
        assert_eq!(key.criterion_id, "rust/prefer-question-mark");
        assert_ne!(key.digest, "rust/unwrap-in-lib");
        assert!(!key.digest.contains("unwrap"));
        // Candidate key digest is not the producer key.
        assert_ne!(
            key.digest.as_str(),
            finding
                .provenance
                .as_ref()
                .unwrap()
                .producer_key
                .as_deref()
                .unwrap()
        );
    }

    #[test]
    fn deterministic_producer_has_no_model_confidence() {
        let mut comment = quality_comment();
        comment.confidence = Some(Confidence {
            value: "0.91".into(),
            kind: ConfidenceKind::Model,
        });
        let finding = enrich_from_comment(
            &comment,
            &CriterionMapping::builtin(),
            &AnchorContext::default(),
            true,
        )
        .unwrap();
        assert!(finding.confidence.is_none());
    }

    #[test]
    fn scope_extending_finding_keeps_ask_user() {
        let comment = ReviewComment {
            path: "db.rs".into(),
            content: "needs a schema migration".into(),
            existing_code: None,
            suggestion_code: None,
            start_line: None,
            end_line: None,
            category: Some("maintainability".into()),
            severity: Some("low".into()),
            rule_id: None,
            confidence: None,
        };
        let finding = enrich_from_comment(
            &comment,
            &CriterionMapping::empty(),
            &AnchorContext::default(),
            false,
        )
        .unwrap();
        assert_eq!(finding.action, Action::AskUser);
        assert_eq!(finding.severity, Severity::Warning);
        let key = derive(&finding, &CriterionMapping::empty());
        assert_eq!(key.criterion_id, "maintainability");
    }

    #[test]
    fn missing_confidence_stays_absent() {
        let finding = enrich_from_comment(
            &quality_comment(),
            &CriterionMapping::builtin(),
            &AnchorContext::default(),
            false,
        )
        .unwrap();
        assert!(finding.confidence.is_none());
    }

    #[test]
    fn unmapped_rule_falls_back_to_category_then_unclassified() {
        let mapping = CriterionMapping::empty();
        let mut comment = quality_comment();
        comment.rule_id = Some("unknown/rule".into());
        let with_cat =
            enrich_from_comment(&comment, &mapping, &AnchorContext::default(), true).unwrap();
        assert_eq!(with_cat.criterion_id.as_deref(), Some("bug"));

        comment.category = None;
        // map_comment defaults category to "other"
        let no_cat =
            enrich_from_comment(&comment, &mapping, &AnchorContext::default(), true).unwrap();
        assert_eq!(no_cat.criterion_id.as_deref(), Some("other"));
    }

    #[test]
    fn snippet_anchor_when_no_symbol_or_hunk() {
        let comment = ReviewComment {
            path: "web.js".into(),
            content: "avoid var".into(),
            existing_code: Some("  var y = 1\n".into()),
            suggestion_code: None,
            start_line: Some(2),
            end_line: Some(2),
            category: Some("style".into()),
            severity: Some("low".into()),
            rule_id: Some("js/var-keyword".into()),
            confidence: None,
        };
        let finding = enrich_from_comment(
            &comment,
            &CriterionMapping::builtin(),
            &AnchorContext::default(),
            true,
        )
        .unwrap();
        assert_eq!(finding.anchor_kind.as_deref(), Some("snippet"));
        assert_eq!(finding.anchor_value.as_deref(), Some("var y = 1"));
    }
}
