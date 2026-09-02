//! Conservative reconciliation of current findings against prior-round history.
//!
//! Pure: no IO, no clock, no database. Callers supply history and already-minted
//! instance ids; this module only assigns fingerprints (reuse or mint).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{CandidateKey, FINGERPRINT_VERSION};

const FINGERPRINT_DOMAIN: &[u8] = b"porch-fingerprint/v1";
const CANDIDATE_DOMAIN: &[u8] = b"porch-candidate-key/v1";

/// Inclusive source line range used only for within-round collapse evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub start_line: u32,
    pub end_line: u32,
}

impl SourceRange {
    #[must_use]
    pub fn new(start_line: u32, end_line: u32) -> Self {
        let (start_line, end_line) = if start_line <= end_line {
            (start_line, end_line)
        } else {
            (end_line, start_line)
        };
        Self {
            start_line,
            end_line,
        }
    }

    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start_line.max(other.start_line);
        let end = self.end_line.min(other.end_line);
        if start <= end {
            Some(Self {
                start_line: start,
                end_line: end,
            })
        } else {
            None
        }
    }
}

/// One current-round finding occurrence ready for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentFinding {
    /// Caller-minted finding instance id (ULID in production).
    pub instance_id: String,
    pub key: CandidateKey,
    /// Producer invocation identity for within-round collapse rules.
    pub producer_invocation_id: String,
    pub range: Option<SourceRange>,
}

/// Prior finding instance from an earlier round of the same run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorInstance {
    pub instance_id: String,
    pub fingerprint: String,
    pub fingerprint_version: u32,
    pub key: CandidateKey,
}

/// Rename pair from `git diff -M` (path bytes as git reports them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameEvidence {
    pub from: String,
    pub to: String,
}

/// Caller-supplied history for one reconcile call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub priors: Vec<PriorInstance>,
    pub renames: Vec<RenameEvidence>,
}

/// Fingerprint assignment for one current finding instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub instance_id: String,
    pub fingerprint: String,
    /// Set when this assignment reuses a prior instance's fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_prior_instance_id: Option<String>,
}

/// Result of reconciliation: one assignment per current finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub assignments: Vec<Assignment>,
}

/// Reconcile current findings against history.
///
/// Matching is conservative: ties and other ambiguity mint. Only priors whose
/// `fingerprint_version` equals [`FINGERPRINT_VERSION`] participate.
#[must_use]
pub fn reconcile(current: &[CurrentFinding], history: &History) -> Proposal {
    let groups = within_round_groups(current);
    let priors: Vec<&PriorInstance> = history
        .priors
        .iter()
        .filter(|p| p.fingerprint_version == FINGERPRINT_VERSION)
        .collect();

    // group_idx → matching prior instance ids (after rename rewrite)
    let mut matches_for_group: Vec<Vec<usize>> = Vec::with_capacity(groups.len());
    for group in &groups {
        let key = &current[group.members[0]].key;
        let mut hits = Vec::new();
        for (pi, prior) in priors.iter().enumerate() {
            if keys_match(key, &prior.key, &history.renames) {
                hits.push(pi);
            }
        }
        matches_for_group.push(hits);
    }

    // prior_idx → group indices that claim it
    let mut claimants: HashMap<usize, Vec<usize>> = HashMap::new();
    for (gi, hits) in matches_for_group.iter().enumerate() {
        for &pi in hits {
            claimants.entry(pi).or_default().push(gi);
        }
    }

    // group_idx → reused prior index, if a strict one-to-one claim holds
    let mut reuse_prior: Vec<Option<usize>> = vec![None; groups.len()];
    for (gi, hits) in matches_for_group.iter().enumerate() {
        if hits.len() != 1 {
            continue;
        }
        let pi = hits[0];
        if claimants.get(&pi).is_some_and(|gs| gs.len() == 1) {
            reuse_prior[gi] = Some(pi);
        }
    }

    let mut assignments: Vec<Assignment> = Vec::with_capacity(current.len());
    // Stable output order: input order
    let mut by_instance: BTreeMap<String, Assignment> = BTreeMap::new();

    for (gi, group) in groups.iter().enumerate() {
        let fingerprint = if let Some(pi) = reuse_prior[gi] {
            priors[pi].fingerprint.clone()
        } else {
            let mint_id = group
                .members
                .iter()
                .map(|&i| current[i].instance_id.as_str())
                .min()
                .unwrap_or("");
            let key = &current[group.members[0]].key;
            mint_fingerprint(key, mint_id)
        };
        let reused = reuse_prior[gi].map(|pi| priors[pi].instance_id.clone());
        for &idx in &group.members {
            let id = current[idx].instance_id.clone();
            by_instance.insert(
                id.clone(),
                Assignment {
                    instance_id: id,
                    fingerprint: fingerprint.clone(),
                    reused_prior_instance_id: reused.clone(),
                },
            );
        }
    }

    for c in current {
        if let Some(a) = by_instance.remove(&c.instance_id) {
            assignments.push(a);
        }
    }

    Proposal { assignments }
}

/// Mint a canonical fingerprint using the instance disambiguator.
#[must_use]
pub fn mint_fingerprint(key: &CandidateKey, minting_instance_id: &str) -> String {
    let ver = key.fingerprint_version.to_string();
    let preimage = length_delimited_join(&[
        FINGERPRINT_DOMAIN,
        ver.as_bytes(),
        key.digest.as_bytes(),
        minting_instance_id.as_bytes(),
    ]);
    hex::encode(Sha256::digest(preimage))
}

fn keys_match(current: &CandidateKey, prior: &CandidateKey, renames: &[RenameEvidence]) -> bool {
    if current.digest == prior.digest {
        return true;
    }
    let rewritten = rewrite_path(&prior.path_key, renames);
    if rewritten == prior.path_key {
        return false;
    }
    let digest = candidate_key_digest(
        prior.fingerprint_version,
        &rewritten,
        &prior.criterion_id,
        &prior.anchor_kind,
        &prior.anchor_value,
    );
    current.digest == digest
}

fn candidate_key_digest(
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
    hex::encode(Sha256::digest(preimage))
}

fn rewrite_path(path: &str, renames: &[RenameEvidence]) -> String {
    for r in renames {
        if r.from == path {
            return r.to.clone();
        }
    }
    path.to_string()
}

struct Group {
    /// Indices into `current`.
    members: Vec<usize>,
}

fn within_round_groups(current: &[CurrentFinding]) -> Vec<Group> {
    let mut by_digest: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, c) in current.iter().enumerate() {
        by_digest.entry(c.key.digest.as_str()).or_default().push(i);
    }

    let mut groups = Vec::new();
    for indices in by_digest.into_values() {
        if indices.len() == 1 {
            groups.push(Group { members: indices });
            continue;
        }
        if can_collapse(current, &indices) {
            groups.push(Group { members: indices });
        } else {
            for i in indices {
                groups.push(Group { members: vec![i] });
            }
        }
    }
    groups
}

fn can_collapse(current: &[CurrentFinding], indices: &[usize]) -> bool {
    if indices.len() <= 1 {
        return true;
    }
    let mut producers = BTreeSet::new();
    let mut ranges: Vec<SourceRange> = Vec::with_capacity(indices.len());
    for &i in indices {
        let c = &current[i];
        if !producers.insert(c.producer_invocation_id.as_str()) {
            return false; // more than one member per producer
        }
        let Some(r) = c.range else {
            return false;
        };
        ranges.push(r);
    }
    if producers.len() != indices.len() {
        return false;
    }
    let mut common = ranges[0];
    for &r in &ranges[1..] {
        match common.intersection(r) {
            Some(next) => common = next,
            None => return false,
        }
    }
    true
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
    use serde::Deserialize;
    use std::fs;
    use std::path::Path;

    fn key(path: &str, criterion: &str, anchor_kind: &str, anchor_value: &str) -> CandidateKey {
        let path_key = crate::path_key(path);
        let digest = candidate_key_digest(
            FINGERPRINT_VERSION,
            &path_key,
            criterion,
            anchor_kind,
            anchor_value,
        );
        CandidateKey {
            digest,
            fingerprint_version: FINGERPRINT_VERSION,
            path_key,
            criterion_id: criterion.into(),
            anchor_kind: anchor_kind.into(),
            anchor_value: anchor_value.into(),
        }
    }

    fn prior_from(id: &str, k: CandidateKey) -> PriorInstance {
        let fingerprint = mint_fingerprint(&k, id);
        PriorInstance {
            instance_id: id.into(),
            fingerprint,
            fingerprint_version: FINGERPRINT_VERSION,
            key: k,
        }
    }

    fn current(
        id: &str,
        k: CandidateKey,
        producer: &str,
        range: Option<(u32, u32)>,
    ) -> CurrentFinding {
        CurrentFinding {
            instance_id: id.into(),
            key: k,
            producer_invocation_id: producer.into(),
            range: range.map(|(a, b)| SourceRange::new(a, b)),
        }
    }

    #[test]
    fn moved_code_and_rewritten_message_reuse_prior_fingerprint() {
        let k = key("src/a.rs", "rust/unwrap-in-lib", "symbol", "fn load");
        let p = prior_from("p1", k.clone());
        let history = History {
            priors: vec![p.clone()],
            renames: vec![],
        };
        // Moved lines — key unchanged.
        let moved = reconcile(
            &[current("c1", k.clone(), "quality", Some((18, 20)))],
            &history,
        );
        assert_eq!(moved.assignments[0].fingerprint, p.fingerprint);
        assert_eq!(
            moved.assignments[0].reused_prior_instance_id.as_deref(),
            Some("p1")
        );

        // Message is not part of the key; same structural key reuses.
        let rewritten = reconcile(&[current("c1", k, "quality", Some((10, 12)))], &history);
        assert_eq!(rewritten.assignments[0].fingerprint, p.fingerprint);
    }

    #[test]
    fn distinct_issues_sharing_candidate_key_get_different_fingerprints() {
        let k = key("src/a.rs", "rust/unwrap-in-lib", "symbol", "fn load");
        let proposal = reconcile(
            &[
                current("c1", k.clone(), "quality", Some((10, 12))),
                current("c2", k, "quality", Some((40, 41))),
            ],
            &History::default(),
        );
        assert_eq!(proposal.assignments.len(), 2);
        assert_ne!(
            proposal.assignments[0].fingerprint,
            proposal.assignments[1].fingerprint
        );
        assert!(
            proposal
                .assignments
                .iter()
                .all(|a| a.reused_prior_instance_id.is_none())
        );
    }

    #[test]
    fn multi_producer_collapses_only_on_common_range_intersection() {
        let cand = key("src/a.rs", "rust/unwrap-in-lib", "symbol", "fn load");
        let prior = prior_from("p1", cand.clone());
        let history = History {
            priors: vec![prior.clone()],
            renames: vec![],
        };

        let collapsed = reconcile(
            &[
                current("c1", cand.clone(), "quality", Some((18, 20))),
                current("c2", cand.clone(), "agent", Some((18, 20))),
            ],
            &history,
        );
        assert_eq!(collapsed.assignments[0].fingerprint, prior.fingerprint);
        assert_eq!(
            collapsed.assignments[0].fingerprint,
            collapsed.assignments[1].fingerprint
        );

        // Pairwise overlap but empty common intersection across three.
        let range_low = SourceRange::new(1, 5);
        let range_mid = SourceRange::new(4, 8);
        let range_high = SourceRange::new(7, 10);
        assert!(range_low.intersection(range_mid).is_some());
        assert!(range_mid.intersection(range_high).is_some());
        assert!(
            range_low
                .intersection(range_mid)
                .unwrap()
                .intersection(range_high)
                .is_none()
        );

        let no_common = reconcile(
            &[
                current("c1", cand.clone(), "quality", Some((1, 5))),
                current("c2", cand.clone(), "agent", Some((4, 8))),
                current("c3", cand, "other", Some((7, 10))),
            ],
            &History::default(),
        );
        let fps: BTreeSet<_> = no_common
            .assignments
            .iter()
            .map(|a| a.fingerprint.as_str())
            .collect();
        assert_eq!(fps.len(), 3);
    }

    #[test]
    fn ambiguity_mints_and_unclaimed_prior_disappears() {
        let k = key("src/a.rs", "rust/unwrap-in-lib", "symbol", "fn load");
        let p = prior_from("p1", k.clone());
        let history = History {
            priors: vec![p.clone()],
            renames: vec![],
        };
        // Two distinct currents (same producer → no collapse) both match p1 → mint.
        let proposal = reconcile(
            &[
                current("c1", k.clone(), "quality", Some((10, 12))),
                current("c2", k, "quality", Some((40, 41))),
            ],
            &history,
        );
        assert!(
            proposal
                .assignments
                .iter()
                .all(|a| a.reused_prior_instance_id.is_none())
        );
        assert_ne!(
            proposal.assignments[0].fingerprint,
            proposal.assignments[1].fingerprint
        );
        let claimed: BTreeSet<_> = proposal
            .assignments
            .iter()
            .filter_map(|a| a.reused_prior_instance_id.as_deref())
            .collect();
        assert!(!claimed.contains("p1"));
        // Disappearance: nothing carries p1 forward.
        assert_ne!(proposal.assignments[0].fingerprint, p.fingerprint);
        assert_ne!(proposal.assignments[1].fingerprint, p.fingerprint);
    }

    #[test]
    fn version_boundary_ignores_other_fingerprint_versions() {
        let k = key("src/a.rs", "rust/unwrap-in-lib", "symbol", "fn load");
        let mut old = prior_from("p-old", k.clone());
        old.fingerprint_version = FINGERPRINT_VERSION + 1;
        old.fingerprint = "stale-should-not-reuse".into();
        let history = History {
            priors: vec![old],
            renames: vec![],
        };
        let proposal = reconcile(&[current("c1", k, "quality", Some((1, 1)))], &history);
        assert!(proposal.assignments[0].reused_prior_instance_id.is_none());
        assert_ne!(
            proposal.assignments[0].fingerprint,
            "stale-should-not-reuse"
        );
    }

    #[test]
    fn path_rename_rewrites_prior_key_for_match() {
        let prior_key = key("old/path.rs", "rust/dbg-macro", "symbol", "fn run");
        let current_key = key("new/path.rs", "rust/dbg-macro", "symbol", "fn run");
        let p = prior_from("p1", prior_key);
        let history = History {
            priors: vec![p.clone()],
            renames: vec![RenameEvidence {
                from: "old/path.rs".into(),
                to: "new/path.rs".into(),
            }],
        };
        let proposal = reconcile(
            &[current("c1", current_key, "quality", Some((8, 9)))],
            &history,
        );
        assert_eq!(proposal.assignments[0].fingerprint, p.fingerprint);
        assert_eq!(
            proposal.assignments[0].reused_prior_instance_id.as_deref(),
            Some("p1")
        );
    }

    // --- Normative corpus ---

    #[derive(Debug, Deserialize)]
    struct Manifest {
        cases: Vec<ManifestCase>,
    }

    #[derive(Debug, Deserialize)]
    struct ManifestCase {
        case: String,
        fingerprint_version: u32,
    }

    #[derive(Debug, Deserialize)]
    struct CaseFile {
        case: String,
        fingerprint_version: u32,
        prior_rounds: Vec<PriorRound>,
        current_round: CurrentRound,
        #[serde(default)]
        rename_evidence: Vec<RenameEvidence>,
        expect: Expect,
    }

    #[derive(Debug, Deserialize)]
    struct PriorRound {
        findings: Vec<FixtureFinding>,
    }

    #[derive(Debug, Deserialize)]
    struct CurrentRound {
        findings: Vec<FixtureFinding>,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureFinding {
        #[serde(rename = "ref")]
        ref_id: String,
        path: String,
        criterion_id: String,
        anchor: FixtureAnchor,
        lines: Option<[u32; 2]>,
        producer: String,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureAnchor {
        kind: String,
        value: String,
    }

    #[derive(Debug, Deserialize)]
    struct Expect {
        reuse: Vec<ReuseExpect>,
        equivalence_groups: Vec<Vec<String>>,
        minted: Vec<String>,
        disappeared: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct ReuseExpect {
        current: String,
        prior: String,
    }

    fn corpus_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/reconcile/1")
    }

    fn finding_key(f: &FixtureFinding) -> CandidateKey {
        key(&f.path, &f.criterion_id, &f.anchor.kind, &f.anchor.value)
    }

    fn load_case(name: &str) -> CaseFile {
        let path = corpus_root().join(name).join("case.json");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
    }

    fn run_case(case: &CaseFile) -> (Proposal, BTreeMap<String, String>) {
        assert_eq!(case.fingerprint_version, FINGERPRINT_VERSION);
        let mut prior_fps = BTreeMap::new();
        let mut priors = Vec::new();
        for round in &case.prior_rounds {
            for f in &round.findings {
                let k = finding_key(f);
                let p = prior_from(&f.ref_id, k);
                prior_fps.insert(f.ref_id.clone(), p.fingerprint.clone());
                priors.push(p);
            }
        }
        let history = History {
            priors,
            renames: case.rename_evidence.clone(),
        };
        let current: Vec<_> = case
            .current_round
            .findings
            .iter()
            .map(|f| {
                current(
                    &f.ref_id,
                    finding_key(f),
                    &f.producer,
                    f.lines.map(|l| (l[0], l[1])),
                )
            })
            .collect();
        (reconcile(&current, &history), prior_fps)
    }

    fn assert_reuse(
        case_name: &str,
        by_id: &BTreeMap<&str, &Assignment>,
        prior_fps: &BTreeMap<String, String>,
        expect: &Expect,
    ) {
        for pair in &expect.reuse {
            let assigned = by_id
                .get(pair.current.as_str())
                .unwrap_or_else(|| panic!("{case_name}: missing assignment {}", pair.current));
            let expected_fp = prior_fps
                .get(&pair.prior)
                .unwrap_or_else(|| panic!("{case_name}: missing prior {}", pair.prior));
            assert_eq!(
                &assigned.fingerprint, expected_fp,
                "{case_name}: {} should reuse {}",
                pair.current, pair.prior
            );
            assert_eq!(
                assigned.reused_prior_instance_id.as_deref(),
                Some(pair.prior.as_str()),
                "{case_name}: reuse prior id"
            );
        }

        for group in &expect.equivalence_groups {
            let mut fps = BTreeSet::new();
            for id in group {
                let assigned = by_id
                    .get(id.as_str())
                    .unwrap_or_else(|| panic!("{case_name}: missing {id}"));
                fps.insert(assigned.fingerprint.as_str());
            }
            assert_eq!(
                fps.len(),
                1,
                "{case_name}: equivalence group {group:?} must share one fingerprint"
            );
        }
    }

    fn assert_minted(case_name: &str, by_id: &BTreeMap<&str, &Assignment>, expect: &Expect) {
        let reused_currents: BTreeSet<_> =
            expect.reuse.iter().map(|r| r.current.as_str()).collect();
        let mut reuse_fps: BTreeSet<&str> = BTreeSet::new();
        for pair in &expect.reuse {
            reuse_fps.insert(
                by_id
                    .get(pair.current.as_str())
                    .map(|a| a.fingerprint.as_str())
                    .unwrap(),
            );
        }

        for id in &expect.minted {
            let assigned = by_id
                .get(id.as_str())
                .unwrap_or_else(|| panic!("{case_name}: missing minted {id}"));
            assert!(
                assigned.reused_prior_instance_id.is_none(),
                "{case_name}: {id} should be minted"
            );
            assert!(
                !reuse_fps.contains(assigned.fingerprint.as_str()),
                "{case_name}: minted {id} must not equal a reused fingerprint"
            );
            assert!(
                !reused_currents.contains(id.as_str()),
                "{case_name}: {id} listed as both reuse and minted"
            );
        }

        let mut minted_groups: Vec<BTreeSet<&str>> = expect
            .equivalence_groups
            .iter()
            .map(|g| g.iter().map(String::as_str).collect())
            .collect();
        for id in &expect.minted {
            if minted_groups.iter().any(|g| g.contains(id.as_str())) {
                continue;
            }
            minted_groups.push(BTreeSet::from([id.as_str()]));
        }
        let mut seen = BTreeSet::new();
        for group in minted_groups {
            let only_minted = group.iter().all(|id| expect.minted.iter().any(|m| m == id));
            if !only_minted {
                continue;
            }
            let fp = by_id[*group.iter().next().unwrap()].fingerprint.as_str();
            assert!(
                seen.insert(fp),
                "{case_name}: distinct minted groups must not share fingerprints"
            );
        }
    }

    fn assert_disappeared(
        case_name: &str,
        proposal: &Proposal,
        prior_fps: &BTreeMap<String, String>,
        expect: &Expect,
    ) {
        let claimed: BTreeSet<_> = proposal
            .assignments
            .iter()
            .filter_map(|a| a.reused_prior_instance_id.as_deref())
            .collect();
        for prior_ref in &expect.disappeared {
            assert!(
                !claimed.contains(prior_ref.as_str()),
                "{case_name}: prior {prior_ref} should have disappeared"
            );
            let fp = prior_fps.get(prior_ref).expect("prior fingerprint");
            assert!(
                proposal.assignments.iter().all(|a| &a.fingerprint != fp),
                "{case_name}: disappeared prior fingerprint must not be assigned"
            );
        }
    }

    fn assert_case(case: &CaseFile) {
        let (proposal, prior_fps) = run_case(case);
        let by_id: BTreeMap<_, _> = proposal
            .assignments
            .iter()
            .map(|a| (a.instance_id.as_str(), a))
            .collect();
        assert_reuse(&case.case, &by_id, &prior_fps, &case.expect);
        assert_minted(&case.case, &by_id, &case.expect);
        assert_disappeared(&case.case, &proposal, &prior_fps, &case.expect);
    }

    #[test]
    fn normative_corpus_matches_expected_mappings() {
        let manifest_path = corpus_root().join("MANIFEST.json");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("MANIFEST.json"))
                .expect("parse MANIFEST");
        assert_eq!(manifest.cases.len(), 7, "seven fixture families required");
        for entry in &manifest.cases {
            assert_eq!(entry.fingerprint_version, FINGERPRINT_VERSION);
            let case = load_case(&entry.case);
            assert_eq!(case.case, entry.case);
            assert_case(&case);
        }
    }
}
