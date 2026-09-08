//! Audit document projection of producer invocations and per-path coverage.

use porch_gate::rounds::{
    self, AssuranceCompletion, ContextApplication, ContextApplicationState, ContextSource,
    CoverageState, ExecutionState, FinalizeOutcome, FinalizeProposal, FindingInstanceProposal,
    OpenRoundPlan, ProducerInvocation, RoundBindings, RoundCoverageProposal,
    capture_context_element, context_applicability_digest, sha256_hex,
};
use porch_gate::{AuditDocument, Db, build_audit};
use serde_json::json;
use tempfile::TempDir;

const ARTIFACT_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn seed_pending_run(home: &std::path::Path) -> (Db, String) {
    std::fs::create_dir_all(home).unwrap();
    let db = Db::open(&home.join("state.sqlite")).unwrap();
    db.upsert_repo("repo1", home, &home.join("bare.git"), "main")
        .unwrap();
    let run = db
        .insert_run("repo1", "feat-audit-trace", "deadbeef", None, None)
        .unwrap();
    (db, run.id)
}

fn projectable_descriptor() -> String {
    json!({
        "adapter_kind": "porch_json_cli",
        "declared_engine_kind": "quality",
        "reported_version": { "unavailable": "not_reported" },
        "observed_version_identity": { "artifact_sha256": ARTIFACT_SHA256 },
    })
    .to_string()
}

fn open_round_with_producers(
    db: &Db,
    run_id: &str,
    producers: Vec<ProducerInvocation>,
) -> rounds::RoundId {
    let inventory = b"stale-inv\n";
    let digest = sha256_hex(inventory);
    let intent = capture_context_element(
        "intent",
        ContextSource::Present {
            bytes: inventory.to_vec(),
        },
    );
    let intent_digest = context_applicability_digest("intent", "present", inventory);
    let context_applications = (0..producers.len())
        .map(|slot| ContextApplication {
            element_name: "intent".into(),
            producer_slot: slot,
            application: ContextApplicationState::Applied,
            effective_digest: Some(intent_digest.clone()),
        })
        .collect();
    let plan = OpenRoundPlan {
        run_id: run_id.to_string(),
        producers,
        requirements: vec![],
    };
    let bindings = RoundBindings {
        from_sha: "from".into(),
        to_sha: "to".into(),
        inventory_digest: digest,
        inventory_bytes: inventory.to_vec(),
        trusted_config_sha: "config".into(),
        protocol_schema_version: 1,
        fingerprint_version: 1,
        intent_source: Some("flag".into()),
        context_elements: vec![intent],
        context_applications,
    };
    rounds::open_round(db, &plan, &bindings).unwrap()
}

fn producer_id(db: &Db, round_id: &rounds::RoundId) -> String {
    rounds::producers_for_round(db, round_id).unwrap()[0]
        .id
        .clone()
}

fn sample_complete_proposal(producer_invocation_id: &str) -> FinalizeProposal {
    FinalizeProposal {
        execution: ExecutionState::Finished,
        assurance_completion: AssuranceCompletion::Complete,
        completion_reason: None,
        coverage: vec![RoundCoverageProposal {
            producer_invocation_id: producer_invocation_id.into(),
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        }],
        producer_durations: Vec::new(),
        review_duration_ms: None,
        instances: vec![FindingInstanceProposal {
            producer_invocation_id: producer_invocation_id.into(),
            fingerprint: "fp-one".into(),
            fingerprint_version: 1,
            candidate_key: "ck-one".into(),
            criterion_id: "rust/unwrap-in-lib".into(),
            evidence: "unwrap here".into(),
            consequence: "panic risk".into(),
            action: "must-fix".into(),
            severity: "error".into(),
            provenance_json: r#"{"producer_key":"rust/unwrap-in-lib"}"#.into(),
            confidence_value: None,
            confidence_kind: None,
            path: "a.rs".into(),
            anchor_kind: "symbol".into(),
            anchor_value: "foo".into(),
        }],
    }
}

#[test]
fn audit_lists_producer_invocations_ordered_by_ordinal_slot_then_id() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_pending_run(tmp.path());

    let first = open_round_with_producers(
        &db,
        &run_id,
        vec![
            ProducerInvocation {
                descriptor_json: projectable_descriptor(),
                descriptor_equivalence_digest: "equiv-slot-0".into(),
            },
            ProducerInvocation {
                descriptor_json: projectable_descriptor(),
                descriptor_equivalence_digest: "equiv-slot-1".into(),
            },
        ],
    );
    let second = open_round_with_producers(
        &db,
        &run_id,
        vec![ProducerInvocation {
            descriptor_json: projectable_descriptor(),
            descriptor_equivalence_digest: "equiv-round-2".into(),
        }],
    );

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.schema_version, 3);
    assert_eq!(doc.run_status, "pending");
    assert!(doc.anomaly.is_none());
    assert_eq!(doc.producers.len(), 3);
    assert_eq!(doc.producers[0].round_id, first.as_str());
    assert_eq!(doc.producers[0].slot, 0);
    assert_eq!(
        doc.producers[0].descriptor_equivalence_digest,
        "equiv-slot-0"
    );
    assert_eq!(doc.producers[1].round_id, first.as_str());
    assert_eq!(doc.producers[1].slot, 1);
    assert_eq!(
        doc.producers[1].descriptor_equivalence_digest,
        "equiv-slot-1"
    );
    assert_eq!(doc.producers[2].round_id, second.as_str());
    assert_eq!(doc.producers[2].slot, 0);
    assert_eq!(
        doc.producers[2].descriptor_equivalence_digest,
        "equiv-round-2"
    );
    let order_keys: Vec<(i64, i64, String)> = doc
        .producers
        .iter()
        .map(|p| {
            let ordinal = doc
                .rounds
                .iter()
                .find(|r| r.id == p.round_id)
                .expect("producer round")
                .ordinal;
            (ordinal, p.slot, p.id.clone())
        })
        .collect();
    let mut sorted = order_keys.clone();
    sorted.sort();
    assert_eq!(order_keys, sorted);

    let wire = serde_json::to_value(&doc.producers[0]).unwrap();
    assert_eq!(wire["adapter_kind"], "porch_json_cli");
    assert_eq!(wire["declared_engine_kind"], "quality");
    assert_eq!(wire["reported_version"]["unavailable"], "not_reported");
    assert_eq!(
        wire["observed_version_identity"]["artifact_sha256"],
        ARTIFACT_SHA256
    );
    assert!(wire.get("verdict").is_none());
    assert!(wire.get("approval").is_none());
    assert_eq!(wire["round_id"], first.as_str());
    assert!(!doc.producers[0].id.is_empty());
}

#[test]
fn unreadable_descriptor_still_returns_row_columns_and_marks_fields_unavailable() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_pending_run(tmp.path());
    let round = open_round_with_producers(
        &db,
        &run_id,
        vec![ProducerInvocation {
            descriptor_json: r#"{"adapter_kind":"porch_json_cli"}"#.into(),
            descriptor_equivalence_digest: "equiv-stub".into(),
        }],
    );

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "pending");

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.schema_version, 3);
    assert_eq!(doc.run_status, "pending");
    assert_eq!(doc.producers.len(), 1);
    let producer = &doc.producers[0];
    assert_eq!(producer.round_id, round.as_str());
    assert!(!producer.id.is_empty());
    assert_eq!(producer.slot, 0);
    assert_eq!(producer.descriptor_equivalence_digest, "equiv-stub");

    let wire = serde_json::to_value(producer).unwrap();
    let adapter_reason = wire["adapter_kind"]["unavailable"]
        .as_str()
        .expect("adapter_kind unavailable with parse reason");
    assert!(
        !adapter_reason.is_empty(),
        "parse reason must not be empty: {adapter_reason}"
    );
    assert_eq!(wire["declared_engine_kind"]["unavailable"], adapter_reason);
    assert_eq!(wire["reported_version"]["unavailable"], adapter_reason);
    assert_eq!(
        wire["observed_version_identity"]["unavailable"],
        adapter_reason
    );

    let anomaly = doc.anomaly.expect("descriptor parse sets anomaly");
    assert_eq!(anomaly.code, "unreadable_producer_descriptor");
    assert!(
        anomaly.detail.contains(adapter_reason) || anomaly.detail == adapter_reason,
        "anomaly detail should carry the parse reason, got {}",
        anomaly.detail
    );

    let run = db.run_by_id(&run_id).unwrap().unwrap();
    assert_eq!(run.status, "pending", "audit must not park the run");
}

#[test]
fn v2_audit_json_without_producers_or_later_scalars_deserializes() {
    let v2 = json!({
        "schema_version": 2,
        "run_id": "run-v2",
        "run_status": "pending",
        "completeness": "as_of",
        "watermark": { "audit_rev": 0, "review_history_revision": 0 },
        "rounds": [{
            "id": "round-1",
            "ordinal": 1,
            "from_sha": "from",
            "to_sha": "to",
            "execution": "running",
            "assurance_completion": "pending"
        }],
        "instances": [{
            "id": "inst-1",
            "round_id": "round-1",
            "fingerprint": "fp",
            "fingerprint_version": 1,
            "path": "src/lib.rs",
            "criterion_id": "c",
            "evidence": "e",
            "severity": "warning",
            "action": "fix"
        }],
        "events": [],
        "related_occurrences": [],
        "phase": { "kind": "unavailable", "steps": [] }
    });
    assert!(v2.get("producers").is_none());
    assert!(v2.get("coverage").is_none());
    assert!(v2["rounds"][0].get("trusted_config_sha").is_none());
    assert!(v2["rounds"][0].get("protocol_schema_version").is_none());
    assert!(v2["instances"][0].get("producer_invocation_id").is_none());
    assert!(v2["instances"][0].get("consequence").is_none());

    let doc: AuditDocument = serde_json::from_value(v2).expect("v2 document must deserialize");
    assert_eq!(doc.schema_version, 2);
    assert_eq!(doc.run_id, "run-v2");
    assert!(doc.producers.is_empty());
    assert!(doc.coverage.is_empty());
    assert_eq!(doc.rounds.len(), 1);
    assert_eq!(doc.instances.len(), 1);
}

#[test]
fn audit_lists_selected_and_completed_coverage_ordered_by_ordinal_invocation_then_path() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_pending_run(tmp.path());

    let first = open_round_with_producers(
        &db,
        &run_id,
        vec![ProducerInvocation {
            descriptor_json: projectable_descriptor(),
            descriptor_equivalence_digest: "equiv-round-1".into(),
        }],
    );
    let first_producer = producer_id(&db, &first);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    let mut first_proposal = sample_complete_proposal(&first_producer);
    first_proposal.coverage = vec![
        RoundCoverageProposal {
            producer_invocation_id: first_producer.clone(),
            path: "z.rs".into(),
            state: CoverageState::Selected,
            reason: None,
            authority: None,
            completion_evidence: None,
        },
        RoundCoverageProposal {
            producer_invocation_id: first_producer.clone(),
            path: "a.rs".into(),
            state: CoverageState::Completed,
            reason: None,
            authority: None,
            completion_evidence: Some("reviewed".into()),
        },
    ];
    assert_eq!(
        rounds::finalize_round(&db, &first, &first_proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );

    let second = open_round_with_producers(
        &db,
        &run_id,
        vec![ProducerInvocation {
            descriptor_json: projectable_descriptor(),
            descriptor_equivalence_digest: "equiv-round-2".into(),
        }],
    );
    let second_producer = producer_id(&db, &second);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    let mut second_proposal = sample_complete_proposal(&second_producer);
    second_proposal.coverage = vec![RoundCoverageProposal {
        producer_invocation_id: second_producer.clone(),
        path: "a.rs".into(),
        state: CoverageState::Completed,
        reason: None,
        authority: None,
        completion_evidence: Some("reviewed".into()),
    }];
    assert_eq!(
        rounds::finalize_round(&db, &second, &second_proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.schema_version, 3);
    assert_eq!(doc.coverage.len(), 3);

    assert_eq!(doc.coverage[0].round_id, first.as_str());
    assert_eq!(doc.coverage[0].producer_invocation_id, first_producer);
    assert_eq!(doc.coverage[0].path, "a.rs");
    assert_eq!(doc.coverage[0].state, "completed");
    assert_eq!(
        doc.coverage[0].completion_evidence.as_deref(),
        Some("reviewed")
    );

    assert_eq!(doc.coverage[1].round_id, first.as_str());
    assert_eq!(doc.coverage[1].producer_invocation_id, first_producer);
    assert_eq!(doc.coverage[1].path, "z.rs");
    assert_eq!(doc.coverage[1].state, "selected");
    assert_eq!(doc.coverage[1].completion_evidence, None);

    assert_eq!(doc.coverage[2].round_id, second.as_str());
    assert_eq!(doc.coverage[2].producer_invocation_id, second_producer);
    assert_eq!(doc.coverage[2].path, "a.rs");
    assert_eq!(doc.coverage[2].state, "completed");

    let order_keys: Vec<(i64, String, String)> = doc
        .coverage
        .iter()
        .map(|row| {
            let ordinal = doc
                .rounds
                .iter()
                .find(|r| r.id == row.round_id)
                .expect("coverage round")
                .ordinal;
            (
                ordinal,
                row.producer_invocation_id.clone(),
                row.path.clone(),
            )
        })
        .collect();
    let mut sorted = order_keys.clone();
    sorted.sort();
    assert_eq!(order_keys, sorted);
}

#[test]
fn waived_and_failed_coverage_keep_stored_reason_and_authority() {
    let tmp = TempDir::new().unwrap();
    let (db, run_id) = seed_pending_run(tmp.path());
    let round = open_round_with_producers(
        &db,
        &run_id,
        vec![ProducerInvocation {
            descriptor_json: projectable_descriptor(),
            descriptor_equivalence_digest: "equiv-waive".into(),
        }],
    );
    let producer = producer_id(&db, &round);
    let (rev, _) = rounds::read_history(&db, &run_id).unwrap();
    let mut proposal = sample_complete_proposal(&producer);
    proposal.coverage = vec![
        RoundCoverageProposal {
            producer_invocation_id: producer.clone(),
            path: "skip.rs".into(),
            state: CoverageState::Waived,
            reason: Some("generated".into()),
            authority: Some("operator".into()),
            completion_evidence: None,
        },
        RoundCoverageProposal {
            producer_invocation_id: producer.clone(),
            path: "bad.rs".into(),
            state: CoverageState::Failed,
            reason: Some("timeout".into()),
            authority: None,
            completion_evidence: None,
        },
    ];
    assert_eq!(
        rounds::finalize_round(&db, &round, &proposal, rev).unwrap(),
        FinalizeOutcome::Finalized
    );

    let doc = build_audit(&db, &run_id).unwrap();
    assert_eq!(doc.coverage.len(), 2);

    let failed = doc
        .coverage
        .iter()
        .find(|row| row.path == "bad.rs")
        .expect("failed path");
    assert_eq!(failed.round_id, round.as_str());
    assert_eq!(failed.producer_invocation_id, producer);
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.reason.as_deref(), Some("timeout"));
    assert_eq!(failed.authority, None);

    let waived = doc
        .coverage
        .iter()
        .find(|row| row.path == "skip.rs")
        .expect("waived path");
    assert_eq!(waived.round_id, round.as_str());
    assert_eq!(waived.producer_invocation_id, producer);
    assert_eq!(waived.state, "waived");
    assert_eq!(waived.reason.as_deref(), Some("generated"));
    assert_eq!(waived.authority.as_deref(), Some("operator"));
}
