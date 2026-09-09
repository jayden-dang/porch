//! Execute a porch run: disposable worktree, intent, rebase, review, certify, deliver.

mod agent_run;
mod certify;
mod config;
mod deliver;
mod sync;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Serialize fetch + tip resolve across concurrent rebases in this process.
static FETCH_RESOLVE_LOCK: Mutex<()> = Mutex::new(());

use porch_agent::{
    RunFixerOpts, fixer_bin, fixer_timeout, run_fixer, write_deliver_repair_inputs,
    write_fixer_inputs, write_rebase_fix_inputs,
};
use porch_gate::rounds::phase::{
    self as phase, AttemptId, OperationKind, PhaseName, PhaseTransition,
};
use porch_gate::rounds::{
    self, ActorKind, AssuranceCompletion, AuthorityError, AuthorityKind, ContextApplication,
    ContextApplicationState, ContextSource, EquivalenceInput, ExecutionState, FinalizeOutcome,
    FinalizeProposal, FindingInstanceProposal, MemberRole, ObservedVersionForEquivalence,
    OpenRoundPlan, PersistAuthorityPlan, ProducerDuration, ProducerInvocation, RequirementSpec,
    Resolution, Role, RoundCoverageProposal, RoundId, RunEffects, STALE_REVISION_RETRIES,
    StepEffect, capture_context_element, descriptor_equivalence_digest, digest_for_specs,
    persist_authority, persist_authority_with_run_effects, run_required_set_digest, sha256_hex,
};
use porch_gate::{
    Db, RunExecutor, RunRow, StatusFindingDto, db_path, event_hub, load_finding_notes, repo_id_for,
    resolve_run_assurance, round_for_decision, rpc_start_run, run_artifact_dir,
    run_deliver_repair_dir, run_fixer_dir, run_worktree_dir,
};
use porch_git::GitDir;
use porch_review::{
    Action, AdapterKind, CriterionMapping, CurrentFinding, EngineKind, Finding, History,
    ObservedVersionIdentity, PrepareOpts, PreparedInvocation, PriorInstance, ProducerDescriptor,
    ProducerOutput, RunReviewOpts, Severity, SourceRange, check_artifacts_stable, derive,
    load_home_config, prepare, producer_artifact_dir, reconcile, review_timeout, run_review,
};
use serde::Serialize;
use ulid::Ulid;

pub use agent_run::{AgentRunOpts, agent_run};
pub use sync::{SyncStatus, agent_sync, recovery_ref_name, sync_hint_for};

use crate::config::{
    effective_base_branch, load_trusted_at_sha, persist_path_instructions,
    resolve_default_branch_tip,
};

/// Phases in locked order (D5).
const PHASES: &[&str] = &["intent", "rebase", "review", "certify", "deliver"];

/// Mechanical deliver auto-fix budget (architecture; not overloading `rerun_transient`).
const DELIVER_REPAIR_BUDGET: u32 = 3;

const DELIVER_REPAIR_SUBJECT: &str = "porch: repair allowlisted checks";

/// Production executor injected into the daemon from the `porch` binary.
#[derive(Debug, Default, Clone, Copy)]
pub struct PipelineExecutor;

impl RunExecutor for PipelineExecutor {
    fn execute(&self, home: &Path, run_id: &str, cancel: &AtomicBool) {
        if let Err(e) = execute_run(home, run_id, cancel) {
            tracing::warn!(run_id, error = %e, "run failed");
        }
    }

    fn recover_stale(&self, home: &Path) -> std::result::Result<(), String> {
        if std::env::var_os("PORCH_TEST_FAIL_RECOVER_STALE").is_some() {
            return Err("test recover_stale failure".into());
        }
        let db = Db::open(&db_path(home)).map_err(|e| e.to_string())?;
        rounds::reconcile_stale(&db).map_err(|e| e.to_string())?;
        recover_stale_running(home).map_err(|e| e.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(transparent)]
    Gate(#[from] porch_gate::Error),
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Review(#[from] porch_review::Error),
    #[error(transparent)]
    Agent(#[from] porch_agent::Error),
    #[error(transparent)]
    Certify(#[from] certify::CertifyError),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
}

type Result<T> = std::result::Result<T, RunError>;

/// Publish state (+ optional activity) when a daemon `EventHub` is installed.
fn publish_run(run_id: &str, activity: &str) {
    if let Some(hub) = event_hub() {
        hub.publish_state(run_id);
        if !activity.is_empty() {
            hub.publish_activity(run_id, activity);
        }
    }
}

fn phase_fail(err: &phase::PhaseError) -> RunError {
    RunError::Msg(err.to_string())
}

fn gate_fail(err: &porch_gate::Error) -> RunError {
    RunError::Msg(err.to_string())
}

fn apply_phase(db: &Db, transition: PhaseTransition, effects: RunEffects) -> Result<AttemptId> {
    phase::persist_phase_transition(db, transition, effects).map_err(|e| phase_fail(&e))
}

fn persist_effects(
    db: &Db,
    run_id: &str,
    transition: PhaseTransition,
    effects: RunEffects,
) -> Result<AttemptId> {
    let status = effects.status.clone();
    let steps = effects.steps.clone();
    let id = apply_phase(db, transition, effects)?;
    if let Some(status) = status.as_deref() {
        publish_run(run_id, &format!("status={status}"));
    }
    for step in &steps {
        publish_run(
            run_id,
            &format!("step={} status={}", step.step, step.status),
        );
    }
    Ok(id)
}

fn open_attempt(db: &Db, run_id: &str, name: PhaseName) -> Result<AttemptId> {
    phase::nonterminal_attempt(db, run_id, name)
        .map_err(|e| gate_fail(&e))?
        .map(|row| row.id)
        .ok_or_else(|| {
            RunError::Msg(format!(
                "no open {} attempt for run {run_id}",
                name.as_str()
            ))
        })
}

fn start_phase(db: &Db, run_id: &str, name: PhaseName, effects: RunEffects) -> Result<AttemptId> {
    persist_effects(
        db,
        run_id,
        PhaseTransition::Start {
            run_id: run_id.to_string(),
            phase: name,
        },
        effects,
    )
}

fn terminal_open(
    db: &Db,
    run_id: &str,
    name: PhaseName,
    outcome: &str,
    cause: Option<&str>,
    effects: RunEffects,
) -> Result<AttemptId> {
    let attempt = open_attempt(db, run_id, name)?;
    persist_effects(
        db,
        run_id,
        PhaseTransition::Terminal {
            attempt,
            outcome: outcome.to_string(),
            cause: cause.map(str::to_string),
        },
        effects,
    )
}

fn evidence_open(
    db: &Db,
    run_id: &str,
    name: PhaseName,
    cause: &str,
    effects: RunEffects,
) -> Result<AttemptId> {
    let attempt = open_attempt(db, run_id, name)?;
    persist_effects(
        db,
        run_id,
        PhaseTransition::Evidence {
            attempt,
            cause: cause.to_string(),
        },
        effects,
    )
}

fn parse_phase_name(step: &str) -> Option<PhaseName> {
    match step {
        "intent" => Some(PhaseName::Intent),
        "rebase" => Some(PhaseName::Rebase),
        "review" => Some(PhaseName::Review),
        "certify" => Some(PhaseName::Certify),
        "deliver" => Some(PhaseName::Deliver),
        _ => None,
    }
}

fn skip_phase_step(db: &Db, run_id: &str, step: &str, detail: &str) -> Result<()> {
    let Some(name) = parse_phase_name(step) else {
        return Err(RunError::Msg(format!(
            "cannot skip non-canonical step {step}"
        )));
    };
    let _ = start_phase(db, run_id, name, RunEffects::none())?;
    let _ = terminal_open(
        db,
        run_id,
        name,
        "skipped",
        Some(detail),
        RunEffects {
            status: None,
            error: None,
            approved_head: None,
            steps: vec![StepEffect {
                step: step.to_string(),
                status: "skipped".to_string(),
                error: Some(detail.to_string()),
            }],
        },
    )?;
    Ok(())
}

fn complete_phase_step(
    db: &Db,
    run_id: &str,
    step: &str,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    let Some(name) = parse_phase_name(step) else {
        return Err(RunError::Msg(format!(
            "cannot complete non-canonical step {step}"
        )));
    };
    if phase::nonterminal_attempt(db, run_id, name)
        .map_err(|e| gate_fail(&e))?
        .is_none()
    {
        let _ = start_phase(db, run_id, name, RunEffects::none())?;
    }
    let _ = terminal_open(
        db,
        run_id,
        name,
        status,
        error,
        RunEffects {
            status: None,
            error: None,
            approved_head: None,
            steps: vec![StepEffect {
                step: step.to_string(),
                status: status.to_string(),
                error: error.map(str::to_string),
            }],
        },
    )?;
    Ok(())
}

fn park_phase_step(
    db: &Db,
    run_id: &str,
    step: &str,
    detail: Option<&str>,
    run_status_error: Option<&str>,
) -> Result<()> {
    let Some(name) = parse_phase_name(step) else {
        return Err(RunError::Msg(format!(
            "cannot park non-canonical step {step}"
        )));
    };
    if phase::nonterminal_attempt(db, run_id, name)
        .map_err(|e| gate_fail(&e))?
        .is_none()
    {
        let _ = start_phase(db, run_id, name, RunEffects::none())?;
    }
    let cause = detail.unwrap_or("parked");
    let _ = evidence_open(
        db,
        run_id,
        name,
        cause,
        RunEffects {
            status: Some("parked".into()),
            error: run_status_error.map(str::to_string),
            approved_head: None,
            steps: vec![StepEffect {
                step: step.to_string(),
                status: "parked".to_string(),
                error: detail.map(str::to_string),
            }],
        },
    )?;
    Ok(())
}

fn cancel_run_with_phase(db: &Db, run_id: &str, cause: &str) -> Result<()> {
    phase::cancel_run(db, run_id, cause).map_err(|e| phase_fail(&e))?;
    publish_run(run_id, "status=cancelled");
    Ok(())
}

fn fail_run_with_phase(db: &Db, run_id: &str, msg: &str) -> Result<()> {
    let effects = RunEffects {
        status: Some("failed".into()),
        error: Some(msg.to_string()),
        approved_head: None,
        steps: vec![],
    };
    for name in [
        PhaseName::Deliver,
        PhaseName::Certify,
        PhaseName::Review,
        PhaseName::Rebase,
        PhaseName::Intent,
    ] {
        if phase::nonterminal_attempt(db, run_id, name)
            .map_err(|e| gate_fail(&e))?
            .is_some()
        {
            let _ = terminal_open(db, run_id, name, "failed", Some(msg), effects)?;
            return Ok(());
        }
    }
    invent_terminal_effects(db, run_id, PhaseName::Intent, "failed", Some(msg), effects)
}

fn complete_run_with_phase(db: &Db, run_id: &str) -> Result<()> {
    let effects = RunEffects {
        status: Some("completed".into()),
        error: None,
        approved_head: None,
        steps: vec![],
    };
    for name in [
        PhaseName::Deliver,
        PhaseName::Certify,
        PhaseName::Review,
        PhaseName::Rebase,
        PhaseName::Intent,
    ] {
        if phase::nonterminal_attempt(db, run_id, name)
            .map_err(|e| gate_fail(&e))?
            .is_some()
        {
            let _ = terminal_open(db, run_id, name, "completed", None, effects)?;
            return Ok(());
        }
    }
    // All phases already terminal — still need a justifying event for status.
    if let Some(deliver) = phase::attempts_for_run(db, run_id)
        .map_err(|e| gate_fail(&e))?
        .into_iter()
        .rev()
        .find(|a| a.phase == PhaseName::Deliver && a.parent_attempt_id.is_none())
    {
        let _ = persist_effects(
            db,
            run_id,
            PhaseTransition::Evidence {
                attempt: deliver.id,
                cause: "run_completed".into(),
            },
            effects,
        )?;
        return Ok(());
    }
    invent_terminal_effects(db, run_id, PhaseName::Deliver, "completed", None, effects)
}

fn invent_terminal_effects(
    db: &Db,
    run_id: &str,
    phase: PhaseName,
    outcome: &str,
    cause: Option<&str>,
    effects: RunEffects,
) -> Result<()> {
    let status = effects.status.clone();
    let steps = effects.steps.clone();
    phase::invent_and_terminal(db, run_id, phase, outcome, cause, effects)
        .map_err(|e| phase_fail(&e))?;
    if let Some(status) = status.as_deref() {
        publish_run(run_id, &format!("status={status}"));
    }
    for step in &steps {
        publish_run(
            run_id,
            &format!("step={} status={}", step.step, step.status),
        );
    }
    Ok(())
}

#[derive(Debug)]
enum PhaseLoop {
    Continue,
    /// Review parked; leave worktree and stop the pipeline.
    Parked,
}

#[allow(clippy::too_many_lines)]
fn execute_run(home: &Path, run_id: &str, cancel: &AtomicBool) -> Result<()> {
    let db = Db::open(&db_path(home))?;
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    if run.status == "cancelled" {
        return Ok(());
    }

    let repo = db
        .repo_by_id(&run.repo_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown repo {}", run.repo_id)))?;
    let bare = GitDir::new(&repo.bare_path)?;
    let wt_path = run_worktree_dir(home, &run.repo_id, run_id);

    let _ = start_phase(
        &db,
        run_id,
        PhaseName::Intent,
        RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )?;
    db.set_worktree_dir(run_id, &wt_path)?;

    if let Err(e) = porch_git::worktree_add_detach(&bare, &wt_path, &run.sha) {
        let msg = format!("worktree add: {e}");
        let _ = fail_run_with_phase(&db, run_id, &msg);
        remove_run_worktree(&bare, &wt_path);
        return Err(RunError::Msg(msg));
    }
    db.set_run_shas(run_id, Some(&run.sha), None)?;

    let mut skip_remaining = false;
    let outcome = (|| -> Result<PhaseLoop> {
        for phase in PHASES {
            if cancel.load(Ordering::SeqCst) {
                return Err(RunError::Msg("cancelled".into()));
            }
            if skip_remaining {
                skip_phase_step(&db, run_id, phase, "skip remaining")?;
                continue;
            }
            match *phase {
                "intent" => {
                    if run.intent.as_ref().is_some_and(|s| !s.trim().is_empty()) {
                        complete_phase_step(&db, run_id, phase, "completed", None)?;
                    } else {
                        complete_phase_step(&db, run_id, phase, "skipped", Some("no intent"))?;
                    }
                }
                "rebase" => {
                    let _ = start_phase(&db, run_id, PhaseName::Rebase, RunEffects::none())?;
                    match run_rebase(&db, home, run_id, &bare, &wt_path, &repo.default_branch)? {
                        RebaseOutcome::Completed { empty } => {
                            complete_phase_step(&db, run_id, phase, "completed", None)?;
                            if empty {
                                skip_remaining = true;
                            }
                        }
                        RebaseOutcome::Parked { detail } => {
                            park_phase_step(&db, run_id, phase, Some(&detail), Some(&detail))?;
                            return Ok(PhaseLoop::Parked);
                        }
                    }
                }
                "review" => {
                    let _ = start_phase(&db, run_id, PhaseName::Review, RunEffects::none())?;
                    match run_review_phase(&db, home, run_id, &bare, &wt_path, false)? {
                        ReviewPhase::Approved => {
                            complete_phase_step(&db, run_id, phase, "completed", None)?;
                        }
                        ReviewPhase::Parked => {
                            park_phase_step(&db, run_id, phase, None, None)?;
                            return Ok(PhaseLoop::Parked);
                        }
                    }
                }
                "certify" => {
                    let _ = start_phase(&db, run_id, PhaseName::Certify, RunEffects::none())?;
                    execute_certify_step(
                        &db,
                        home,
                        run_id,
                        &bare,
                        &wt_path,
                        &repo.default_branch,
                        cancel,
                    )?;
                }
                "deliver" => {
                    let _ = start_phase(&db, run_id, PhaseName::Deliver, RunEffects::none())?;
                    match execute_deliver_step(
                        &db,
                        home,
                        run_id,
                        &bare,
                        &wt_path,
                        &repo.default_branch,
                        cancel,
                    )? {
                        PhaseLoop::Parked => return Ok(PhaseLoop::Parked),
                        PhaseLoop::Continue => {}
                    }
                }
                _ => {}
            }
        }
        Ok(PhaseLoop::Continue)
    })();

    let cancelled = cancel.load(Ordering::SeqCst);
    match &outcome {
        Ok(PhaseLoop::Parked) => {
            // Worktree kept for agent respond.
            return Ok(());
        }
        // Supersede wins over success and over deliver/certify failure (e.g.
        // watch poll timeout after cancel while babysitting checks).
        _ if cancelled => {
            let _ = cancel_run_with_phase(&db, run_id, "superseded by new push");
        }
        Ok(PhaseLoop::Continue) => {
            let _ = complete_run_with_phase(&db, run_id);
        }
        Err(RunError::Msg(m)) if m == "cancelled" => {
            let _ = cancel_run_with_phase(&db, run_id, "superseded by new push");
        }
        Err(e) => {
            let _ = fail_run_with_phase(&db, run_id, &e.to_string());
        }
    }
    if let Ok(Some(final_run)) = db.run_by_id(run_id) {
        finish_remove_worktree(&bare, &final_run, &wt_path);
    } else {
        remove_run_worktree(&bare, &wt_path);
    }
    match outcome {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

enum ReviewPhase {
    Approved,
    Parked,
}

const PROTOCOL_SCHEMA_VERSION: i64 = rounds::PROTOCOL_SCHEMA_VERSION;

fn run_review_phase(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    after_fix: bool,
) -> Result<ReviewPhase> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    let base_sha = run
        .base_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("review requires base_sha".into()))?;
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;

    let from_sha = resolve_review_from(db, wt, &run, base_sha, &head, after_fix)?;
    let range = format!("{from_sha}..{head}");
    let changed = porch_git::diff_name_only(wt, &range)?;

    let opened = open_review_round(db, home, bare, &run, &from_sha, &head, &changed)?;
    if let Some(UnsatisfiedRequired::Floor { reason }) = &opened.unsatisfied {
        finalize_incomplete(db, &opened.round_id, "floor_unresolved")?;
        return Err(RunError::Review(porch_review::Error::FloorUnresolved {
            reason: reason.clone(),
        }));
    }
    let spawned = spawn_review_for_round(
        db,
        &SpawnReviewCtx {
            home,
            wt,
            run: &run,
            from_sha: &from_sha,
            head: &head,
            changed: &changed,
            opened: &opened,
        },
    )?;
    if let Some(UnsatisfiedRequired::Judgment { reason }) = &opened.unsatisfied {
        finalize_incomplete(db, &opened.round_id, "judgment_unresolved")?;
        return Err(RunError::Msg(reason.clone()));
    }
    finalize_complete_round(db, run_id, &opened, &spawned)?;
    publish_run(run_id, "findings updated");

    if spawned.has_blocking() {
        return Ok(ReviewPhase::Parked);
    }

    db.set_review_approved_head_sha(run_id, Some(&head))?;
    clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)?;
    Ok(ReviewPhase::Approved)
}

struct OpenedSlot {
    producer_invocation_id: String,
    artifact_dir: PathBuf,
    plan: porch_review::InvocationPlan,
    role: Role,
}

enum UnsatisfiedRequired {
    Floor { reason: String },
    Judgment { reason: String },
}

struct OpenedRound {
    round_id: RoundId,
    slots: Vec<OpenedSlot>,
    unsatisfied: Option<UnsatisfiedRequired>,
}

struct SpawnedReview {
    findings: Vec<(String, Finding)>,
    coverage: Vec<RoundCoverageProposal>,
    producer_durations: Vec<ProducerDuration>,
    review_duration_ms: i64,
}

impl SpawnedReview {
    fn has_blocking(&self) -> bool {
        self.findings
            .iter()
            .any(|(_, finding)| finding.is_blocking())
    }
}

fn engine_is_quality(home: &Path) -> bool {
    load_home_config(home)
        .ok()
        .flatten()
        .and_then(|cfg| cfg.review.engine_kind())
        == Some(EngineKind::Quality)
}

struct ComposedProducer {
    role: Role,
    invocation: PreparedInvocation,
}

enum ComposedRound {
    Prepared(Vec<ComposedProducer>),
    FloorUnresolved {
        reason: String,
    },
    JudgmentUnresolved {
        floor: Box<PreparedInvocation>,
        reason: String,
    },
}

fn compose_prepared_invocations(
    home: &Path,
    intent: Option<&[u8]>,
    path_instructions: Option<&[u8]>,
) -> ComposedRound {
    match porch_review::floor::resolve() {
        Ok(floor) => {
            if engine_is_quality(home) {
                return ComposedRound::Prepared(vec![ComposedProducer {
                    role: Role::Floor,
                    invocation: floor,
                }]);
            }
            match prepare(&PrepareOpts {
                porch_home: Some(home),
                review_bin: None,
                agent_bin: None,
                prefer_agent: None,
                intent,
                path_instructions,
            }) {
                Ok(judgment) => ComposedRound::Prepared(vec![
                    ComposedProducer {
                        role: Role::Floor,
                        invocation: floor,
                    },
                    ComposedProducer {
                        role: Role::Judgment,
                        invocation: judgment,
                    },
                ]),
                Err(e) => ComposedRound::JudgmentUnresolved {
                    floor: Box::new(floor),
                    reason: e.to_string(),
                },
            }
        }
        Err(porch_review::Error::FloorUnresolved { reason }) => {
            ComposedRound::FloorUnresolved { reason }
        }
        Err(e) => ComposedRound::FloorUnresolved {
            reason: e.to_string(),
        },
    }
}

fn unresolved_floor_requirement(reason: String) -> RequirementSpec {
    RequirementSpec {
        slot: 0,
        role: Role::Floor,
        resolution: Resolution::Unresolved,
        expected_equivalence_digest: None,
        reason: Some(reason),
    }
}

fn unresolved_judgment_requirement(reason: String) -> RequirementSpec {
    RequirementSpec {
        slot: 1,
        role: Role::Judgment,
        resolution: Resolution::Unresolved,
        expected_equivalence_digest: None,
        reason: Some(reason),
    }
}

fn producer_invocation(desc: &ProducerDescriptor) -> Result<ProducerInvocation> {
    let descriptor_json = serde_json::to_string(desc)
        .map_err(|e| RunError::Msg(format!("serialize producer descriptor: {e}")))?;
    Ok(ProducerInvocation {
        descriptor_equivalence_digest: producer_equivalence_digest(desc),
        descriptor_json,
    })
}

fn pinned_assurance_shape(db: &Db, run_id: &str) -> Result<&'static str> {
    let rounds = rounds::rounds_for_run(db, run_id)?;
    let Some(first) = rounds.first() else {
        return Err(RunError::Msg(
            "run pin exists without a recorded round to name the pinned assurance shape".into(),
        ));
    };
    let rows = rounds::requirements_for_round(db, &first.id)?;
    rounds::assurance_shape_for_rows(&rows).ok_or_else(|| {
        RunError::Msg(
            "run pin exists without recorded requirements to name the pinned assurance shape"
                .into(),
        )
    })
}

fn recorded_role(requirements: &[RequirementSpec], producer_slot: i64) -> Result<Role> {
    requirements
        .iter()
        .find(|spec| spec.slot == producer_slot && spec.resolution == Resolution::Resolved)
        .map(|spec| spec.role)
        .ok_or_else(|| {
            RunError::Msg(format!(
                "opened producer slot {producer_slot} has no recorded requirement role"
            ))
        })
}

fn refuse_shape_mismatch(
    db: &Db,
    run_id: &str,
    pinned_digest: &str,
    pinned_shape: &str,
    attempted_digest: &str,
    attempted_shape: &str,
) -> Result<OpenedRound> {
    let payload = serde_json::json!({
        "kind": "assurance_shape_mismatch",
        "pinned_digest": pinned_digest,
        "pinned_shape": pinned_shape,
        "attempted_digest": attempted_digest,
        "attempted_shape": attempted_shape,
    })
    .to_string();
    complete_phase_step(db, run_id, "review", "failed", Some(&payload))?;
    Err(RunError::Msg(payload))
}

fn resolved_requirement(slot: i64, role: Role, desc: &ProducerDescriptor) -> RequirementSpec {
    RequirementSpec {
        slot,
        role,
        resolution: Resolution::Resolved,
        expected_equivalence_digest: Some(producer_equivalence_digest(desc)),
        reason: None,
    }
}

fn applications_for_prepared(
    slot: usize,
    prepared: &PreparedInvocation,
) -> Vec<ContextApplication> {
    prepared
        .context_elements
        .iter()
        .map(|el| ContextApplication {
            element_name: el.element_name.clone(),
            producer_slot: slot,
            application: if el.applied {
                ContextApplicationState::Applied
            } else {
                ContextApplicationState::NotApplied
            },
            effective_digest: el.effective_digest.clone(),
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // compose, pin, and record one round before spawn
fn open_review_round(
    db: &Db,
    home: &Path,
    bare: &GitDir,
    run: &RunRow,
    from_sha: &str,
    head: &str,
    changed: &[String],
) -> Result<OpenedRound> {
    let run_id = run.id.as_str();
    let intent_bytes = run.intent.as_ref().map(|s| s.as_bytes().to_vec());
    let path_instructions_path = run_artifact_dir(home, run_id).join("path_instructions.json");
    let path_instructions_bytes = if path_instructions_path.is_file() {
        Some(std::fs::read(&path_instructions_path)?)
    } else {
        None
    };

    let composed = compose_prepared_invocations(
        home,
        intent_bytes.as_deref(),
        path_instructions_bytes.as_deref(),
    );

    let trusted_config_sha = run
        .trusted_config_sha
        .clone()
        .ok_or_else(|| RunError::Msg("review requires trusted_config_sha".into()))?;
    // Pin before open so a failed open leaves a sweepable ref leak, not an unpinned round.
    rounds::retention::pin_trusted_config(bare, &trusted_config_sha).map_err(|e| {
        RunError::Msg(format!(
            "failed to pin trusted config before round open: {e}"
        ))
    })?;
    let inv_bytes = inventory_bytes(changed);
    let inventory_digest = sha256_hex(&inv_bytes);

    let context_elements = vec![
        capture_context_element(
            "intent",
            match &intent_bytes {
                Some(b) => ContextSource::Present { bytes: b.clone() },
                None => ContextSource::Absent { reason: None },
            },
        ),
        capture_context_element(
            "path_instructions",
            match &path_instructions_bytes {
                Some(b) => ContextSource::Present { bytes: b.clone() },
                None => ContextSource::Absent { reason: None },
            },
        ),
    ];
    let mut context_applications = Vec::new();
    match &composed {
        ComposedRound::Prepared(prepared) => {
            for (slot, producer) in prepared.iter().enumerate() {
                context_applications.extend(applications_for_prepared(slot, &producer.invocation));
            }
        }
        ComposedRound::JudgmentUnresolved { floor, .. } => {
            context_applications.extend(applications_for_prepared(0, floor));
        }
        ComposedRound::FloorUnresolved { .. } => {}
    }

    let mut bindings = rounds::RoundBindings {
        from_sha: from_sha.to_string(),
        to_sha: head.to_string(),
        inventory_digest,
        inventory_bytes: inv_bytes,
        trusted_config_sha,
        protocol_schema_version: PROTOCOL_SCHEMA_VERSION,
        fingerprint_version: i64::from(porch_review::FINGERPRINT_VERSION),
        intent_source: run.intent_source.clone(),
        context_elements,
        context_applications,
    };
    // Test hook: force open_round to refuse before any spawn.
    if std::env::var_os("PORCH_TEST_FAIL_ROUND_OPEN").is_some() {
        bindings.inventory_digest = sha256_hex(b"porch-test-fail-round-open");
    }

    let (open_plan, prepared, unsatisfied) = match composed {
        ComposedRound::FloorUnresolved { reason } => (
            OpenRoundPlan {
                run_id: run_id.to_string(),
                producers: Vec::new(),
                requirements: vec![unresolved_floor_requirement(reason.clone())],
            },
            Vec::new(),
            Some(UnsatisfiedRequired::Floor { reason }),
        ),
        ComposedRound::JudgmentUnresolved { floor, reason } => (
            OpenRoundPlan {
                run_id: run_id.to_string(),
                producers: vec![producer_invocation(&floor.plan.descriptor)?],
                requirements: vec![
                    resolved_requirement(0, Role::Floor, &floor.plan.descriptor),
                    unresolved_judgment_requirement(reason.clone()),
                ],
            },
            vec![ComposedProducer {
                role: Role::Floor,
                invocation: *floor,
            }],
            Some(UnsatisfiedRequired::Judgment { reason }),
        ),
        ComposedRound::Prepared(prepared) => {
            let mut producers = Vec::with_capacity(prepared.len());
            let mut requirements = Vec::with_capacity(prepared.len());
            for (i, producer) in prepared.iter().enumerate() {
                let slot = i64::try_from(i)
                    .map_err(|_| RunError::Msg("producer slot exceeds i64".into()))?;
                producers.push(producer_invocation(&producer.invocation.plan.descriptor)?);
                requirements.push(resolved_requirement(
                    slot,
                    producer.role,
                    &producer.invocation.plan.descriptor,
                ));
            }
            (
                OpenRoundPlan {
                    run_id: run_id.to_string(),
                    producers,
                    requirements,
                },
                prepared,
                None,
            )
        }
    };

    let attempted_digest =
        digest_for_specs(bindings.protocol_schema_version, &open_plan.requirements);
    if let Some(pinned) = run_required_set_digest(db, run_id)? {
        if pinned != attempted_digest {
            return refuse_shape_mismatch(
                db,
                run_id,
                &pinned,
                pinned_assurance_shape(db, run_id)?,
                &attempted_digest,
                rounds::assurance_shape(open_plan.requirements.iter().map(|spec| spec.role)),
            );
        }
    }

    let round_id = rounds::open_round(db, &open_plan, &bindings).map_err(|e| {
        RunError::Msg(format!(
            "failed to open review round before producer spawn: {e}"
        ))
    })?;
    if matches!(unsatisfied, Some(UnsatisfiedRequired::Floor { .. })) {
        return Ok(OpenedRound {
            round_id,
            slots: Vec::new(),
            unsatisfied,
        });
    }
    if std::env::var_os("PORCH_TEST_ABORT_AFTER_ROUND_OPEN").is_some() {
        return Err(RunError::Msg("test abort after round open".into()));
    }
    let recorded = rounds::producers_for_round(db, &round_id)?;
    if recorded.len() != prepared.len() {
        return Err(RunError::Msg(
            "opened round producer count does not match the required set".into(),
        ));
    }
    let mut slots = Vec::with_capacity(prepared.len());
    for (producer, rec) in prepared.iter().zip(recorded) {
        let artifact_dir = producer_artifact_dir(home, run_id, round_id.as_str(), &rec.id);
        std::fs::create_dir_all(&artifact_dir)?;
        slots.push(OpenedSlot {
            producer_invocation_id: rec.id,
            artifact_dir,
            plan: producer.invocation.plan.clone(),
            role: recorded_role(&open_plan.requirements, rec.slot)?,
        });
    }
    Ok(OpenedRound {
        round_id,
        slots,
        unsatisfied,
    })
}

struct SpawnReviewCtx<'a> {
    home: &'a Path,
    wt: &'a Path,
    run: &'a RunRow,
    from_sha: &'a str,
    head: &'a str,
    changed: &'a [String],
    opened: &'a OpenedRound,
}

fn map_spawn_err(e: porch_review::Error, role: Role) -> RunError {
    match role {
        Role::Floor => RunError::Msg(match &e {
            porch_review::Error::Timeout(d) => format!("floor timed out after {d:?}"),
            porch_review::Error::Exit { status, stderr } => {
                format!("floor exited {status}: {stderr}")
            }
            other => format!("floor: {other}"),
        }),
        Role::Judgment => match e {
            porch_review::Error::Timeout(d) => {
                RunError::Msg(format!("review timed out after {d:?}"))
            }
            other => RunError::Review(other),
        },
    }
}

fn abort_slot(
    db: &Db,
    round_id: &RoundId,
    err: porch_review::Error,
    role: Role,
) -> Result<SpawnedReview> {
    finalize_incomplete(db, round_id, incomplete_reason(&err, role))?;
    Err(map_spawn_err(err, role))
}

fn coverage_proposals(
    producer_id: &str,
    entries: &[porch_review::CoverageEntry],
) -> Vec<RoundCoverageProposal> {
    entries
        .iter()
        .map(|e| RoundCoverageProposal {
            producer_invocation_id: producer_id.to_string(),
            path: e.path.clone(),
            state: map_coverage_state(e.state),
            reason: e.reason.clone(),
            authority: e.authority.clone(),
            completion_evidence: e.completion_evidence.clone(),
        })
        .collect()
}

fn slot_coverage(
    changed: &[String],
    producer_id: &str,
    outcome: &porch_review::ReviewOutcome,
) -> std::result::Result<Vec<RoundCoverageProposal>, porch_review::Error> {
    let entries = porch_review::derive_states(changed, &outcome.coverage)?;
    if !ProducerOutput::meets_required(&entries) {
        let short = entries
            .iter()
            .find(|e| {
                matches!(
                    e.state,
                    porch_review::CoverageState::Selected | porch_review::CoverageState::Failed
                )
            })
            .map_or_else(
                || changed.first().cloned().unwrap_or_default(),
                |e| e.path.clone(),
            );
        return Err(porch_review::Error::Coverage(short));
    }
    Ok(coverage_proposals(producer_id, &entries))
}

fn spawn_review_for_round(db: &Db, ctx: &SpawnReviewCtx<'_>) -> Result<SpawnedReview> {
    let opened = ctx.opened;
    let mut findings = Vec::new();
    let mut coverage = Vec::new();
    let mut producer_durations = Vec::new();
    let review_started = Instant::now();
    for slot in &opened.slots {
        let role = slot.role;
        if let Err(e) = check_artifacts_stable(&slot.plan) {
            return abort_slot(db, &opened.round_id, e, role);
        }
        let spawn_started = Instant::now();
        let spawn_result = run_review(&RunReviewOpts {
            work_tree: ctx.wt,
            from_sha: ctx.from_sha,
            to_sha: ctx.head,
            changed_files: ctx.changed,
            bin: slot.plan.spawned_target_absolute.to_str().unwrap_or(""),
            timeout: review_timeout(),
            porch_home: Some(ctx.home),
            run_id: Some(&ctx.run.id),
            intent: ctx.run.intent.as_deref(),
            plan: Some(&slot.plan),
            artifact_dir: Some(&slot.artifact_dir),
        });
        let duration_ms = millis_i64(spawn_started.elapsed());
        match spawn_result {
            Ok(outcome) => match slot_coverage(ctx.changed, &slot.producer_invocation_id, &outcome)
            {
                Ok(rows) => {
                    producer_durations.push(ProducerDuration {
                        producer_invocation_id: slot.producer_invocation_id.clone(),
                        duration_ms,
                    });
                    coverage.extend(rows);
                    for finding in outcome.findings {
                        findings.push((slot.producer_invocation_id.clone(), finding));
                    }
                }
                Err(e) => return abort_slot(db, &opened.round_id, e, role),
            },
            Err(e) => return abort_slot(db, &opened.round_id, e, role),
        }
    }
    if std::env::var_os("PORCH_TEST_ABORT_BEFORE_ROUND_FINALIZE").is_some() {
        return Err(RunError::Msg("test abort before round finalize".into()));
    }
    Ok(SpawnedReview {
        findings,
        coverage,
        producer_durations,
        review_duration_ms: millis_i64(review_started.elapsed()),
    })
}

fn millis_i64(elapsed: std::time::Duration) -> i64 {
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

fn incomplete_reason(err: &porch_review::Error, role: Role) -> &'static str {
    match (role, err) {
        (Role::Floor, porch_review::Error::Timeout(_)) => "floor_timeout",
        (Role::Floor, porch_review::Error::Exit { .. }) => "floor_exit",
        (Role::Floor, porch_review::Error::Coverage(_)) => "floor_coverage_shortfall",
        (Role::Floor, porch_review::Error::ProducerArtifactChanged) => "floor_artifact_changed",
        (Role::Floor, porch_review::Error::FloorUnresolved { .. }) => "floor_unresolved",
        (Role::Floor, _) => "floor_malformed_output",
        (_, porch_review::Error::Timeout(_)) => "producer_timeout",
        (_, porch_review::Error::Exit { .. }) => "producer_exit",
        (_, porch_review::Error::Coverage(_)) => "coverage_shortfall",
        (_, porch_review::Error::ProducerArtifactChanged) => "producer_artifact_changed",
        _ => "malformed_output",
    }
}

fn finalize_complete_round(
    db: &Db,
    run_id: &str,
    opened: &OpenedRound,
    spawned: &SpawnedReview,
) -> Result<()> {
    let mapping = CriterionMapping::builtin();
    let mut current = Vec::with_capacity(spawned.findings.len());
    let mut provisional = Vec::with_capacity(spawned.findings.len());
    for (producer_id, finding) in &spawned.findings {
        let key = derive(finding, &mapping);
        let instance_id = Ulid::new().to_string();
        let range = match (finding.start_line, finding.end_line) {
            (Some(s), Some(e)) => Some(SourceRange::new(s, e)),
            (Some(s), None) | (None, Some(s)) => Some(SourceRange::new(s, s)),
            (None, None) => None,
        };
        current.push(CurrentFinding {
            instance_id: instance_id.clone(),
            key: key.clone(),
            producer_invocation_id: producer_id.clone(),
            range,
        });
        provisional.push((instance_id, finding, key, producer_id.clone()));
    }

    finalize_with_reconcile(
        db,
        run_id,
        &opened.round_id,
        &current,
        &provisional,
        spawned,
    )?;
    Ok(())
}

fn inventory_bytes(changed: &[String]) -> Vec<u8> {
    let mut lines = changed.to_vec();
    lines.sort();
    let mut out = lines.join("\n").into_bytes();
    if !out.is_empty() {
        out.push(b'\n');
    }
    out
}

fn producer_equivalence_digest(desc: &ProducerDescriptor) -> String {
    let adapter_kind = match desc.adapter_kind {
        AdapterKind::NativeAgent => "native_agent",
        AdapterKind::PorchJsonCli => "porch_json_cli",
    };
    let observed_version = match &desc.observed_version_identity {
        ObservedVersionIdentity::ArtifactSha256(hex) => {
            ObservedVersionForEquivalence::ArtifactSha256(hex.clone())
        }
        ObservedVersionIdentity::Unavailable(reason) => {
            ObservedVersionForEquivalence::Unavailable {
                reason: reason.clone(),
            }
        }
    };
    descriptor_equivalence_digest(&EquivalenceInput {
        adapter_kind,
        argv_prefix: &desc.invocation.argv_prefix,
        observed_version,
        consumed_context: &desc.consumed_context,
    })
}

fn map_coverage_state(state: porch_review::CoverageState) -> rounds::CoverageState {
    match state {
        porch_review::CoverageState::Selected => rounds::CoverageState::Selected,
        porch_review::CoverageState::Completed => rounds::CoverageState::Completed,
        porch_review::CoverageState::Failed => rounds::CoverageState::Failed,
        porch_review::CoverageState::Waived => rounds::CoverageState::Waived,
    }
}

fn action_str(action: Action) -> &'static str {
    match action {
        Action::AutoFix => "auto-fix",
        Action::AskUser => "ask-user",
        Action::NoOp => "no-op",
    }
}

fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn finding_instance_proposal(
    producer_invocation_id: &str,
    finding: &Finding,
    key: &porch_review::CandidateKey,
    fingerprint: &str,
) -> Result<FindingInstanceProposal> {
    let provenance_json = match &finding.provenance {
        Some(p) => serde_json::to_string(p)?,
        None => "{}".into(),
    };
    let (confidence_value, confidence_kind) = match &finding.confidence {
        Some(c) => (
            Some(c.value.clone()),
            Some(match c.kind {
                porch_review::ConfidenceKind::Model => "model".into(),
                porch_review::ConfidenceKind::Deterministic => "deterministic".into(),
            }),
        ),
        None => (None, None),
    };
    Ok(FindingInstanceProposal {
        producer_invocation_id: producer_invocation_id.into(),
        fingerprint: fingerprint.into(),
        fingerprint_version: i64::from(key.fingerprint_version),
        candidate_key: key.digest.clone(),
        criterion_id: key.criterion_id.clone(),
        evidence: finding
            .evidence
            .clone()
            .unwrap_or_else(|| finding.message.clone()),
        consequence: finding
            .consequence
            .clone()
            .unwrap_or_else(|| finding.message.clone()),
        action: action_str(finding.action).into(),
        severity: severity_str(finding.severity).into(),
        provenance_json,
        confidence_value,
        confidence_kind,
        path: finding.path.clone(),
        anchor_kind: key.anchor_kind.clone(),
        anchor_value: key.anchor_value.clone(),
    })
}

fn stored_to_prior(stored: rounds::StoredPriorInstance) -> PriorInstance {
    PriorInstance {
        instance_id: stored.finding_instance_id,
        fingerprint: stored.fingerprint,
        fingerprint_version: u32::try_from(stored.fingerprint_version).unwrap_or(0),
        key: porch_review::CandidateKey {
            digest: stored.candidate_key.clone(),
            fingerprint_version: u32::try_from(stored.fingerprint_version).unwrap_or(0),
            path_key: stored.path,
            criterion_id: String::new(),
            anchor_kind: stored.anchor_kind,
            anchor_value: stored.anchor_value,
        },
    }
}

fn finalize_with_reconcile(
    db: &Db,
    run_id: &str,
    round_id: &RoundId,
    current: &[CurrentFinding],
    provisional: &[(String, &Finding, porch_review::CandidateKey, String)],
    spawned: &SpawnedReview,
) -> Result<()> {
    let mut attempts = 0_u32;
    loop {
        let (rev, stored) = rounds::read_history(db, run_id)?;
        let history = History {
            priors: stored.into_iter().map(stored_to_prior).collect(),
            renames: Vec::new(),
        };
        let proposal = reconcile(current, &history);
        let mut by_id = std::collections::BTreeMap::new();
        for a in &proposal.assignments {
            by_id.insert(a.instance_id.clone(), a.fingerprint.clone());
        }
        let mut instances = Vec::with_capacity(provisional.len());
        for (id, finding, key, producer_id) in provisional {
            let fp = by_id
                .get(id)
                .cloned()
                .ok_or_else(|| RunError::Msg(format!("reconcile omitted assignment for {id}")))?;
            instances.push(finding_instance_proposal(producer_id, finding, key, &fp)?);
        }
        let fin = FinalizeProposal {
            execution: ExecutionState::Finished,
            assurance_completion: AssuranceCompletion::Complete,
            completion_reason: None,
            coverage: spawned.coverage.clone(),
            instances,
            producer_durations: spawned.producer_durations.clone(),
            review_duration_ms: Some(spawned.review_duration_ms),
        };
        match rounds::finalize_round(db, round_id, &fin, rev)? {
            FinalizeOutcome::Finalized => return Ok(()),
            FinalizeOutcome::Stale => {
                attempts += 1;
                if attempts >= STALE_REVISION_RETRIES {
                    rounds::abandon_for_history_contention(db, round_id)?;
                    return Err(RunError::Msg(
                        "review round finalization abandoned after history contention".into(),
                    ));
                }
            }
        }
    }
}

fn finalize_incomplete(db: &Db, round_id: &RoundId, reason: &str) -> Result<()> {
    let run_id = rounds::get_round(db, round_id)?
        .ok_or_else(|| RunError::Msg(format!("missing round {}", round_id.as_str())))?
        .run_id;
    let mut attempts = 0_u32;
    loop {
        let (rev, _) = rounds::read_history(db, &run_id)?;
        let fin = FinalizeProposal {
            execution: ExecutionState::Finished,
            assurance_completion: AssuranceCompletion::Incomplete,
            completion_reason: Some(reason.into()),
            coverage: Vec::new(),
            instances: Vec::new(),
            producer_durations: Vec::new(),
            review_duration_ms: None,
        };
        match rounds::finalize_round(db, round_id, &fin, rev)? {
            FinalizeOutcome::Finalized => return Ok(()),
            FinalizeOutcome::Stale => {
                attempts += 1;
                if attempts >= STALE_REVISION_RETRIES {
                    rounds::abandon_for_history_contention(db, round_id)?;
                    return Ok(());
                }
            }
        }
    }
}

fn resolve_review_from(
    db: &Db,
    wt: &Path,
    run: &RunRow,
    base: &str,
    head: &str,
    after_fix: bool,
) -> Result<String> {
    let Some(rng) = db.get_uncertified_pipeline_range(&run.repo_id, &run.branch)? else {
        return Ok(base.to_string());
    };

    if after_fix {
        if porch_git::is_ancestor(wt, &rng.from_sha, head)? {
            return Ok(rng.from_sha);
        }
        return Ok(base.to_string());
    }

    // Initial review: bind when range tip is HEAD or an ancestor of HEAD,
    // and range from is an ancestor of HEAD.
    let tip_ok = rng.to_sha == head || porch_git::is_ancestor(wt, &rng.to_sha, head)?;
    if tip_ok && porch_git::is_ancestor(wt, &rng.from_sha, head)? {
        return Ok(rng.from_sha);
    }
    Ok(base.to_string())
}

fn clear_uncertified_if_certified(
    db: &Db,
    wt: &Path,
    repo_id: &str,
    branch: &str,
    approved_head: &str,
) -> Result<()> {
    let Some(rng) = db.get_uncertified_pipeline_range(repo_id, branch)? else {
        return Ok(());
    };
    let certified =
        rng.to_sha == approved_head || porch_git::is_ancestor(wt, &rng.to_sha, approved_head)?;
    if certified {
        db.delete_uncertified_pipeline_range(repo_id, branch)?;
    }
    Ok(())
}

fn execute_certify_step(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    assert_head_continuity(db, run_id, wt)?;
    match certify::run_certify_phase(db, home, run_id, bare, wt, default_branch, Some(cancel)) {
        Ok(()) => {
            complete_phase_step(db, run_id, "certify", "completed", None)?;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            complete_phase_step(db, run_id, "certify", "failed", Some(&msg))?;
            if msg == "cancelled" {
                return Err(RunError::Msg("cancelled".into()));
            }
            Err(RunError::Certify(e))
        }
    }
}

fn execute_deliver_step(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: &AtomicBool,
) -> Result<PhaseLoop> {
    assert_head_continuity(db, run_id, wt)?;
    deliver_with_repair(db, home, run_id, bare, wt, default_branch, Some(cancel))
}

/// Push/PR/watch; on mechanical allowlisted red or CONFLICTING PR, repair and
/// restart at review → certify → deliver (same `run_id`, no intent/rebase).
#[allow(clippy::too_many_lines)] // repair loop co-writes nested ops and handoff
fn deliver_with_repair(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: Option<&AtomicBool>,
) -> Result<PhaseLoop> {
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err(RunError::Msg("cancelled".into()));
        }
        match deliver::run_deliver_phase(db, home, run_id, bare, wt, default_branch, cancel) {
            Ok(deliver::DeliverOutcome::ParkedCompose) => {
                // compose+parked already recorded inside deliver; do not complete deliver.
                return Ok(PhaseLoop::Parked);
            }
            Ok(deliver::DeliverOutcome::Completed) => {
                complete_phase_step(db, run_id, "deliver", "completed", None)?;
                return Ok(PhaseLoop::Continue);
            }
            Err(e) => {
                let msg = e.to_string();
                // Repairable failures stay nonterminal evidence on deliver (PHASE-2.8).
                let repairable = matches!(
                    &e,
                    deliver::DeliverError::AllowlistFailed { .. }
                        | deliver::DeliverError::MergeConflicting
                );
                if repairable {
                    let cause = match &e {
                        deliver::DeliverError::AllowlistFailed { .. } => "AllowlistFailed",
                        deliver::DeliverError::MergeConflicting => "MergeConflicting",
                        _ => "deliver_failed",
                    };
                    let _ = evidence_open(
                        db,
                        run_id,
                        PhaseName::Deliver,
                        cause,
                        RunEffects {
                            status: None,
                            error: None,
                            approved_head: None,
                            steps: vec![StepEffect {
                                step: "deliver".into(),
                                status: "failed".into(),
                                error: Some(msg.clone()),
                            }],
                        },
                    )?;
                } else {
                    complete_phase_step(db, run_id, "deliver", "failed", Some(&msg))?;
                }
                if msg == "cancelled" {
                    return Err(RunError::Msg("cancelled".into()));
                }
                if !repairable {
                    return Err(RunError::Deliver(e));
                }
                let started =
                    phase::repair_attempts_started(db, run_id).map_err(|e| gate_fail(&e))?;
                if started >= DELIVER_REPAIR_BUDGET {
                    return Err(RunError::Msg(format!(
                        "deliver repair budget exhausted ({DELIVER_REPAIR_BUDGET})"
                    )));
                }
                let attempt = started + 1;
                let deliver_attempt = open_attempt(db, run_id, PhaseName::Deliver)?;
                let repair_attempt = persist_effects(
                    db,
                    run_id,
                    PhaseTransition::NestedStart {
                        parent: deliver_attempt.clone(),
                        kind: OperationKind::DeliverRepair,
                    },
                    RunEffects::none(),
                )?;
                let pre_repair_head = porch_git::rev_parse_c(wt, "HEAD")?;
                match &e {
                    deliver::DeliverError::AllowlistFailed { checks } => {
                        attempt_allowlist_repair(db, home, run_id, wt, checks)?;
                    }
                    deliver::DeliverError::MergeConflicting => {
                        attempt_merge_conflict_rebase(db, home, bare, wt, run_id, default_branch)?;
                    }
                    _ => unreachable!("filtered by repairable"),
                }
                let new_head = porch_git::rev_parse_c(wt, "HEAD")?;
                if new_head == pre_repair_head {
                    // Attempt counted; loop will re-deliver / re-watch or exhaust.
                    tracing::warn!(run_id, attempt, "deliver repair attempt did not move HEAD");
                    let _ = persist_effects(
                        db,
                        run_id,
                        PhaseTransition::NestedTerminal {
                            attempt: repair_attempt,
                            outcome: "completed".into(),
                            cause: Some(format!("attempt {attempt} unchanged_head")),
                        },
                        RunEffects {
                            status: None,
                            error: None,
                            approved_head: None,
                            steps: vec![StepEffect {
                                step: "deliver_repair".into(),
                                status: "completed".into(),
                                error: Some(format!("attempt {attempt}")),
                            }],
                        },
                    )?;
                    // Unchanged-HEAD success hands off to the next `deliver`
                    // attempt and never reuses the same ordinal.
                    let _ = persist_effects(
                        db,
                        run_id,
                        PhaseTransition::Handoff {
                            from: deliver_attempt,
                            to_phase: PhaseName::Deliver,
                            outcome: "deliver_repair".into(),
                            cause: Some(format!("attempt {attempt} unchanged_head")),
                        },
                        RunEffects::none(),
                    )?;
                    continue;
                }
                // Revoke review binding; do not upsert uncertified_pipeline_ranges.
                db.set_review_approved_head_sha(run_id, None)?;
                db.set_run_shas(run_id, Some(&new_head), None)?;
                let _ = persist_effects(
                    db,
                    run_id,
                    PhaseTransition::NestedTerminal {
                        attempt: repair_attempt,
                        outcome: "completed".into(),
                        cause: Some(format!("attempt {attempt}")),
                    },
                    RunEffects {
                        status: None,
                        error: None,
                        approved_head: None,
                        steps: vec![StepEffect {
                            step: "deliver_repair".into(),
                            status: "completed".into(),
                            error: Some(format!("attempt {attempt}")),
                        }],
                    },
                )?;
                // Head moved: hand deliver → review before rereview.
                let _ = persist_effects(
                    db,
                    run_id,
                    PhaseTransition::Handoff {
                        from: deliver_attempt,
                        to_phase: PhaseName::Review,
                        outcome: "deliver_repair".into(),
                        cause: Some(format!("attempt {attempt}")),
                    },
                    RunEffects::none(),
                )?;

                // Session-free rereview (after_fix never passes fixer session).
                match run_review_phase(db, home, run_id, bare, wt, true)? {
                    ReviewPhase::Approved => {
                        complete_phase_step(
                            db,
                            run_id,
                            "review",
                            "completed",
                            Some("deliver_repair"),
                        )?;
                        let local_cancel = AtomicBool::new(false);
                        let cancel_flag = cancel.unwrap_or(&local_cancel);
                        let _ = start_phase(db, run_id, PhaseName::Certify, RunEffects::none())?;
                        execute_certify_step(
                            db,
                            home,
                            run_id,
                            bare,
                            wt,
                            default_branch,
                            cancel_flag,
                        )?;
                        assert_head_continuity(db, run_id, wt)?;
                        // Loop: lease-push + PR update + re-watch.
                        let _ = start_phase(db, run_id, PhaseName::Deliver, RunEffects::none())?;
                    }
                    ReviewPhase::Parked => {
                        park_phase_step(db, run_id, "review", Some("deliver_repair"), None)?;
                        return Ok(PhaseLoop::Parked);
                    }
                }
            }
        }
    }
}

fn attempt_allowlist_repair(
    db: &Db,
    home: &Path,
    run_id: &str,
    wt: &Path,
    checks: &[porch_deliver::CheckRow],
) -> Result<()> {
    let findings = serde_json::json!(
        checks
            .iter()
            .map(|c| {
                let mut row = serde_json::json!({
                    "name": c.name,
                    "state": c.state,
                });
                if let Some(link) = c.link.as_deref() {
                    row["link"] = serde_json::json!(link);
                }
                row
            })
            .collect::<Vec<_>>()
    );
    let findings_json = findings.to_string();
    let repair_dir = run_deliver_repair_dir(home, run_id);
    let (prompt_file, findings_file) = write_deliver_repair_inputs(&repair_dir, &findings_json)?;

    let bin = fixer_bin().map_err(|e| RunError::Msg(e.to_string()))?;
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;

    run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    })?;

    // If the fixer left a dirty tree, commit with porch identity (same as certify).
    maybe_deliver_repair_commit(wt)?;
    Ok(())
}

fn attempt_merge_conflict_rebase(
    db: &Db,
    home: &Path,
    bare: &GitDir,
    wt: &Path,
    run_id: &str,
    default_branch: &str,
) -> Result<()> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    // Keep the initial-rebase pin; do not refresh trusted_config_sha on repair.
    let trusted_sha = run
        .trusted_config_sha
        .as_deref()
        .ok_or_else(|| RunError::Msg("merge conflict repair requires trusted_config_sha".into()))?;
    let cfg = load_trusted_at_sha(bare, trusted_sha).map_err(RunError::Msg)?;
    let onto = {
        let _guard = FETCH_RESOLVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        resolve_onto_tip(bare, default_branch, &cfg.pr_base_branch)?
    };
    if let Err(e) = porch_git::rebase(wt, &onto) {
        let _ = porch_git::rebase_abort(wt);
        return Err(RunError::Msg(format!("rebase conflict: {e}")));
    }
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), None)?;
    maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &cfg.path_instructions)?;
    Ok(())
}

/// Fetch and resolve the effective rebase-onto tip without changing the trusted pin.
fn resolve_onto_tip(bare: &GitDir, default_branch: &str, pr_base_branch: &str) -> Result<String> {
    let base_branch = effective_base_branch(pr_base_branch, default_branch);
    let refspec = format!("refs/heads/{base_branch}:refs/remotes/origin/{base_branch}");
    porch_git::fetch(bare, "origin", &refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{base_branch}: {e}")))?;
    let origin_ref = format!("refs/remotes/origin/{base_branch}");
    porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| RunError::Msg(format!("resolve origin/{base_branch}: {e}")))
}

/// Fetch trusted default tip, pin SHA, honor `pr.base_branch`.
/// Returns `(onto_sha, config, trusted_config_sha)`.
///
/// Unparseable trusted yaml is treated as empty for rebase onto selection;
/// certify/deliver still fail closed on the same pinned bytes.
fn resolve_rebase_onto(
    bare: &GitDir,
    default_branch: &str,
) -> Result<(String, crate::config::PorchConfig, String)> {
    let default_refspec =
        format!("refs/heads/{default_branch}:refs/remotes/origin/{default_branch}");
    porch_git::fetch(bare, "origin", &default_refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{default_branch}: {e}")))?;
    let trusted_sha = resolve_default_branch_tip(bare, default_branch).map_err(RunError::Msg)?;
    let cfg = match load_trusted_at_sha(bare, &trusted_sha) {
        Ok(c) => c,
        Err(e) if e.contains("parse error") || e.contains("not utf-8") => {
            tracing::warn!(error = %e, "trusted yaml unparseable at rebase; using default_branch");
            crate::config::PorchConfig::default()
        }
        Err(e) => return Err(RunError::Msg(e)),
    };
    let base_branch = effective_base_branch(&cfg.pr_base_branch, default_branch).to_string();
    if base_branch == default_branch {
        return Ok((trusted_sha.clone(), cfg, trusted_sha));
    }
    let refspec = format!("refs/heads/{base_branch}:refs/remotes/origin/{base_branch}");
    porch_git::fetch(bare, "origin", &refspec)
        .map_err(|e| RunError::Msg(format!("fetch origin/{base_branch}: {e}")))?;
    let origin_ref = format!("refs/remotes/origin/{base_branch}");
    let onto = porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| RunError::Msg(format!("resolve origin/{base_branch}: {e}")))?;
    Ok((onto, cfg, trusted_sha))
}

fn maybe_deliver_repair_commit(wt: &Path) -> Result<bool> {
    let out = porch_git::run_c(wt, &["status", "--porcelain"])?;
    if porch_git::stdout_trim(&out).is_empty() {
        return Ok(false);
    }
    porch_git::run_c(
        wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.email=porch@example.com",
            "-c",
            "user.name=Porch",
            "add",
            "-A",
        ],
    )?;
    porch_git::run_c(
        wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.email=porch@example.com",
            "-c",
            "user.name=Porch",
            "commit",
            "--no-verify",
            "-m",
            DELIVER_REPAIR_SUBJECT,
        ],
    )?;
    Ok(true)
}

/// The SHA a forward may carry, or a closed failure.
///
/// A recorded approval is mandatory. Continuity still tolerates a live HEAD
/// that descends from the approved SHA because certify's own correction commit
/// advances HEAD after review approves; binding by equality is blocked on
/// deciding whether that commit is re-reviewed first. See the Open Questions of
/// `docs/specs/2026-09-08-forward-authorization/requirements.md`.
pub(crate) fn authorized_forward_sha(db: &Db, run_id: &str, wt: &Path) -> Result<String> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| RunError::Msg(format!("unknown run {run_id}")))?;
    let approved = run
        .review_approved_head_sha
        .ok_or_else(|| RunError::Msg("HEAD continuity: review_approved_head_sha missing".into()))?;
    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    if head == approved || porch_git::is_ancestor(wt, &approved, &head)? {
        return Ok(head);
    }
    Err(RunError::Msg(format!(
        "HEAD continuity: live HEAD {head} is not a descendant of approved {approved}"
    )))
}

fn assert_head_continuity(db: &Db, run_id: &str, wt: &Path) -> Result<()> {
    authorized_forward_sha(db, run_id, wt).map(|_| ())
}

fn persist_uncertified_after_fix(
    db: &Db,
    wt: &Path,
    run: &RunRow,
    pre_fix_head: &str,
    new_head: &str,
) -> Result<()> {
    if pre_fix_head == new_head {
        return Ok(());
    }
    let mut from_sha = pre_fix_head.to_string();
    if let Some(existing) = db.get_uncertified_pipeline_range(&run.repo_id, &run.branch)? {
        if porch_git::is_ancestor(wt, &existing.to_sha, pre_fix_head)? {
            from_sha = existing.from_sha;
        }
    }
    db.upsert_uncertified_pipeline_range(&run.repo_id, &run.branch, &from_sha, new_head, &run.id)?;
    Ok(())
}

enum RebaseOutcome {
    Completed { empty: bool },
    Parked { detail: String },
}

fn run_rebase(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> Result<RebaseOutcome> {
    let (onto, path_instructions, trusted_sha) = {
        let _guard = FETCH_RESOLVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (onto, cfg, trusted_sha) = resolve_rebase_onto(bare, default_branch)?;
        (onto, cfg.path_instructions, trusted_sha)
    };
    db.set_trusted_config_sha(run_id, &trusted_sha)?;
    db.set_run_shas(run_id, None, Some(&onto))?;

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    if head == onto {
        db.set_run_shas(run_id, Some(&head), Some(&onto))?;
        maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &path_instructions)?;
        return Ok(RebaseOutcome::Completed { empty: true });
    }

    if porch_git::is_ancestor(wt, &head, &onto)? {
        porch_git::reset_hard(wt, &onto)?;
    } else if let Err(e) = porch_git::rebase(wt, &onto) {
        // Fail closed if abort itself fails (E15 superseded: park after clean abort).
        porch_git::rebase_abort(wt).map_err(|abort_err| {
            RunError::Msg(format!(
                "rebase conflict: {e}; rebase --abort failed: {abort_err}"
            ))
        })?;
        return Ok(RebaseOutcome::Parked {
            detail: format!("rebase conflict: {e}"),
        });
    }

    let head = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head), Some(&onto))?;
    maybe_persist_path_instructions(home, run_id, wt, &onto, &head, &path_instructions)?;
    let range = format!("{onto}..{head}");
    let empty = porch_git::diff_is_empty(wt, &range)?;
    Ok(RebaseOutcome::Completed { empty })
}

fn maybe_persist_path_instructions(
    home: &Path,
    run_id: &str,
    wt: &Path,
    onto: &str,
    head: &str,
    instructions: &[crate::config::PathInstruction],
) -> Result<()> {
    if instructions.is_empty() {
        return Ok(());
    }
    let range = format!("{onto}..{head}");
    let changed = porch_git::diff_name_only(wt, &range).unwrap_or_default();
    persist_path_instructions(home, run_id, instructions, &changed).map_err(RunError::Msg)?;
    Ok(())
}

fn remove_run_worktree(bare: &GitDir, wt: &Path) {
    let _ = porch_git::worktree_remove_force(bare, wt);
    let _ = std::fs::remove_dir_all(wt);
}

/// Pin recovery ref when required, then remove the disposable worktree.
///
/// Fail closed: if pinning unpublished pipeline commits fails, keep the worktree.
fn finish_remove_worktree(bare: &GitDir, run: &RunRow, wt: &Path) {
    porch_gate::finish_remove_worktree(bare, run, wt);
}

fn recover_stale_running(home: &Path) -> Result<()> {
    let db = Db::open(&db_path(home))?;
    let stale = db.fail_stale_running("daemon restarted while run was in progress")?;
    for run in stale {
        let Some(wt) = run.worktree_dir.as_ref() else {
            continue;
        };
        let Some(repo) = db.repo_by_id(&run.repo_id)? else {
            let _ = std::fs::remove_dir_all(wt);
            continue;
        };
        let bare_path = repo.bare_path;
        if let Ok(bare) = GitDir::new(&bare_path) {
            finish_remove_worktree(&bare, &run, wt);
        } else {
            let _ = std::fs::remove_dir_all(wt);
        }
    }
    Ok(())
}

/// Test helper: path used for a run worktree.
#[must_use]
pub fn expected_worktree_path(home: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    run_worktree_dir(home, repo_id, run_id)
}

/// Human response to a parked review or compose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentResponse {
    Approve,
    Skip,
    Abort,
    /// Spawn fixer for selected findings, then session-free rereview.
    Fix {
        /// Explicit finding ids; `None` means all blocking findings.
        finding_ids: Option<Vec<String>>,
        /// Standing consent: one fix round then approve remaining.
        yes: bool,
    },
    /// Compose park: merge Agent-authored title/body into the scaffold PR.
    Compose {
        body: String,
        title: Option<String>,
    },
}

impl AgentResponse {
    /// Parse `approve` | `skip` | `abort` | `fix` (without findings/`--yes`).
    ///
    /// # Errors
    ///
    /// Returns an error string when the token is not a supported response.
    pub fn parse_verb(s: &str) -> std::result::Result<Self, String> {
        match s {
            "approve" => Ok(Self::Approve),
            "skip" => Ok(Self::Skip),
            "abort" => Ok(Self::Abort),
            "fix" => Ok(Self::Fix {
                finding_ids: None,
                yes: false,
            }),
            other => Err(format!(
                "unknown response {other:?}; expected approve|skip|abort|fix"
            )),
        }
    }
}

/// JSON document for `porch agent status`.
#[derive(Debug, Serialize)]
pub struct AgentStatus {
    pub run_id: String,
    pub repo_id: String,
    pub branch: String,
    pub status: String,
    pub phase: String,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub review_approved_head_sha: Option<String>,
    pub findings: Vec<Finding>,
    pub assurance_record: porch_gate::AssuranceRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_packet_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_actions: Option<Vec<String>>,
}

/// Exit code for agent CLI (D11): 0 ok, 1 failed/cancelled, 2 usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliResult {
    pub exit_code: i32,
    pub json: String,
    /// When true, JSONL/JSON was already written to stdout (e.g. `agent run --wait`).
    pub already_emitted: bool,
}

/// Resolve a run id the same way status/respond do (explicit id or latest parked).
///
/// # Errors
///
/// Returns an [`AgentCliResult`] already shaped for CLI emission when the database
/// cannot be opened, the run id is unknown, or no parked run exists for the repo.
pub fn resolve_agent_run_id(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
) -> std::result::Result<String, AgentCliResult> {
    let db = match Db::open(&db_path(home)) {
        Ok(db) => db,
        Err(e) => {
            return Err(AgentCliResult {
                exit_code: 1,
                json: serde_json::json!({"error": e.to_string()}).to_string(),
                already_emitted: false,
            });
        }
    };
    match resolve_run(&db, run_id, work_tree) {
        Ok(run) => Ok(run.id),
        Err(UsageOrFail::Usage(msg)) => Err(AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        }),
        Err(UsageOrFail::Fail(msg)) => Err(AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        }),
    }
}

/// Build status JSON for a parked (or specified) run.
#[must_use]
pub fn agent_status(home: &Path, run_id: Option<&str>, work_tree: &Path) -> AgentCliResult {
    match agent_status_inner(home, run_id, work_tree) {
        Ok(status) => AgentCliResult {
            exit_code: status_exit(&status.status),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            already_emitted: false,
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        },
    }
}

/// Apply `approve` | `skip` | `abort` | `fix` to a parked run.
#[must_use]
pub fn agent_respond(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
    response: AgentResponse,
) -> AgentCliResult {
    match agent_respond_inner(home, run_id, work_tree, response) {
        Ok(status) => AgentCliResult {
            exit_code: status_exit(&status.status),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            already_emitted: false,
        },
        Err(UsageOrFail::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(UsageOrFail::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        },
    }
}

enum UsageOrFail {
    Usage(String),
    Fail(String),
}

pub(crate) fn status_exit(status: &str) -> i32 {
    match status {
        "failed" | "cancelled" => 1,
        _ => 0,
    }
}

fn agent_status_inner(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
) -> std::result::Result<AgentStatus, UsageOrFail> {
    let db = Db::open(&db_path(home)).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let run = resolve_run(&db, run_id, work_tree)?;
    status_from_run(&db, &run, home).map_err(UsageOrFail::Fail)
}

fn agent_respond_inner(
    home: &Path,
    run_id: Option<&str>,
    work_tree: &Path,
    response: AgentResponse,
) -> std::result::Result<AgentStatus, UsageOrFail> {
    let db = Db::open(&db_path(home)).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let run = resolve_run(&db, run_id, work_tree)?;
    if run.status != "parked" {
        return Err(UsageOrFail::Fail(format!(
            "run {} is {}, not parked",
            run.id, run.status
        )));
    }

    let repo = db
        .repo_by_id(&run.repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail(format!("unknown repo {}", run.repo_id)))?;
    let bare = GitDir::new(&repo.bare_path).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let wt = run
        .worktree_dir
        .clone()
        .ok_or_else(|| UsageOrFail::Fail("parked run has no worktree_dir".into()))?;

    let phase = parked_phase(&db, &run).map_err(UsageOrFail::Fail)?;
    if phase == "unavailable" {
        return Err(UsageOrFail::Fail(format!(
            "parked run {} has no nonterminal phase attempt",
            run.id
        )));
    }

    // Compose park MUST be branched before review Skip (skip continues deliver).
    if phase == "compose" {
        respond_compose(home, &db, &run, &bare, &wt, &repo.default_branch, response)?;
        let run = db
            .run_by_id(&run.id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?
            .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
        return status_from_run(&db, &run, home).map_err(UsageOrFail::Fail);
    }

    match response {
        AgentResponse::Compose { .. } => {
            return Err(UsageOrFail::Usage(
                "--body-file is only valid when phase=compose".into(),
            ));
        }
        AgentResponse::Approve | AgentResponse::Skip if phase == "rebase" => {
            return Err(UsageOrFail::Usage(
                "rebase park accepts fix|abort only (not approve/skip)".into(),
            ));
        }
        AgentResponse::Approve => {
            respond_review_approve(home, &db, &run, &bare, &wt, &repo.default_branch)?;
        }
        AgentResponse::Skip => {
            respond_review_skip(&db, &run, &bare, &wt)?;
        }
        AgentResponse::Abort if phase == "rebase" => {
            cancel_run_with_phase(&db, &run.id, "agent abort")
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            finish_remove_worktree(&bare, &run, &wt);
        }
        AgentResponse::Abort => {
            respond_review_abort(&db, &run, &bare, &wt)?;
        }
        AgentResponse::Fix { finding_ids, yes } => {
            if phase == "rebase" {
                respond_rebase_fix(&db, home, &run, &bare, &wt, &repo.default_branch)?;
            } else {
                respond_fix(&db, home, &run, &bare, &wt, finding_ids.as_ref(), yes)?;
            }
        }
    }

    let run = db
        .run_by_id(&run.id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
    status_from_run(&db, &run, home).map_err(UsageOrFail::Fail)
}

fn respond_review_approve(
    home: &Path,
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> std::result::Result<(), UsageOrFail> {
    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    persist_review_bulk_or_legacy(
        db,
        run,
        AuthorityKind::ReviewApproved,
        &head,
        RunEffects {
            status: None,
            error: None,
            approved_head: Some(head.clone()),
            steps: vec![StepEffect {
                step: "review".into(),
                status: "completed".into(),
                error: Some("approved".into()),
            }],
        },
        Some(("completed", Some("approved"))),
        |db, run| {
            db.set_review_approved_head_sha(&run.id, Some(&head))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            complete_phase_step(db, &run.id, "review", "completed", Some("approved"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))
        },
    )?;
    clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let parked = finish_certify_and_deliver(home, db, bare, wt, &run.id, default_branch)?;
    if !parked {
        finish_remove_worktree(bare, run, wt);
    }
    Ok(())
}

fn respond_review_abort(
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
) -> std::result::Result<(), UsageOrFail> {
    let effects = RunEffects {
        status: Some("cancelled".into()),
        error: Some("agent abort".into()),
        approved_head: None,
        steps: vec![],
    };
    let plan = if let Some(round_id) =
        round_for_decision(db, run).map_err(|e| UsageOrFail::Fail(e.to_string()))?
    {
        let round = rounds::get_round(db, &round_id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?
            .ok_or_else(|| {
                UsageOrFail::Fail(format!("decision round {} missing", round_id.as_str()))
            })?;
        let instances = rounds::instances_for_round(db, &round_id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        let members: Vec<(String, MemberRole)> = instances
            .into_iter()
            .map(|inst| (inst.id, MemberRole::Context))
            .collect();
        let head =
            porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        PersistAuthorityPlan {
            run_id: run.id.clone(),
            kind: AuthorityKind::ReviewAborted,
            expected_round_id: Some(round_id),
            expected_head: Some(round.to_sha),
            live_head: Some(head),
            actor_kind: ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: false,
            members,
        }
    } else {
        // Legacy findings_json park: abort still records, with identity unavailable.
        tracing::warn!(
            run_id = %run.id,
            kind = "review_aborted",
            reason = "legacy",
            "abort recording with identity_unavailable (no decision round)"
        );
        PersistAuthorityPlan {
            run_id: run.id.clone(),
            kind: AuthorityKind::ReviewAborted,
            expected_round_id: None,
            expected_head: None,
            live_head: None,
            actor_kind: ActorKind::Operator,
            authority_event_id: None,
            head_changed: None,
            identity_unavailable: true,
            members: vec![],
        }
    };
    let phase = Some(review_phase_terminal(
        db,
        &run.id,
        "cancelled",
        Some("agent abort"),
    )?);
    match persist_authority_with_run_effects(db, plan, effects, phase) {
        Ok(_) => {}
        Err(AuthorityError::Stale) => {
            tracing::warn!(
                run_id = %run.id,
                kind = "review_aborted",
                reason = "stale",
                "authority persist rejected: applicable round or reviewed HEAD drifted"
            );
            return Err(UsageOrFail::Fail(
                "authority persist rejected: applicable round or reviewed HEAD drifted".into(),
            ));
        }
        Err(AuthorityError::Storage(e)) => {
            return Err(UsageOrFail::Fail(e.to_string()));
        }
    }
    finish_remove_worktree(bare, run, wt);
    Ok(())
}

fn respond_review_skip(
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
) -> std::result::Result<(), UsageOrFail> {
    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    // Authority co-writes only the review Terminal + review step; certify/deliver skips and
    // completed status land via the phase seam afterward (one transition per authority txn).
    persist_review_bulk_or_legacy(
        db,
        run,
        AuthorityKind::ReviewSkipped,
        &head,
        RunEffects {
            status: None,
            error: None,
            approved_head: None,
            steps: vec![StepEffect {
                step: "review".into(),
                status: "skipped".into(),
                error: Some("agent skip".into()),
            }],
        },
        Some(("skipped", Some("agent skip"))),
        |db, run| {
            complete_phase_step(db, &run.id, "review", "skipped", Some("agent skip"))
                .map_err(|e| UsageOrFail::Fail(e.to_string()))
        },
    )?;
    for phase in ["certify", "deliver"] {
        skip_phase_step(db, &run.id, phase, "skip remaining")
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    }
    complete_run_with_phase(db, &run.id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    finish_remove_worktree(bare, run, wt);
    Ok(())
}

/// Resolve (or start) the open review attempt and build its Terminal transition.
fn review_phase_terminal(
    db: &Db,
    run_id: &str,
    outcome: &str,
    cause: Option<&str>,
) -> std::result::Result<PhaseTransition, UsageOrFail> {
    if phase::nonterminal_attempt(db, run_id, PhaseName::Review)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .is_none()
    {
        let _ = start_phase(db, run_id, PhaseName::Review, RunEffects::none())
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    }
    let attempt = open_attempt(db, run_id, PhaseName::Review)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    Ok(PhaseTransition::Terminal {
        attempt,
        outcome: outcome.to_string(),
        cause: cause.map(str::to_string),
    })
}

/// Persist a bulk review authority event with run effects, or fall back for legacy parks.
///
/// When `phase_terminal` is `Some((outcome, cause))`, the open review attempt is terminated in
/// the same Immediate txn as the authority event and effects.
fn persist_review_bulk_or_legacy<F>(
    db: &Db,
    run: &RunRow,
    kind: AuthorityKind,
    live_head: &str,
    effects: RunEffects,
    phase_terminal: Option<(&str, Option<&str>)>,
    legacy: F,
) -> std::result::Result<(), UsageOrFail>
where
    F: FnOnce(&Db, &RunRow) -> std::result::Result<(), UsageOrFail>,
{
    let Some(round_id) =
        round_for_decision(db, run).map_err(|e| UsageOrFail::Fail(e.to_string()))?
    else {
        tracing::warn!(
            run_id = %run.id,
            kind = kind.as_str(),
            reason = "legacy",
            "authority persist falling back to legacy path (no decision round)"
        );
        return legacy(db, run);
    };
    let round = rounds::get_round(db, &round_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| {
            UsageOrFail::Fail(format!("decision round {} missing", round_id.as_str()))
        })?;
    let instances =
        rounds::instances_for_round(db, &round_id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let members: Vec<(String, MemberRole)> = instances
        .into_iter()
        .map(|inst| (inst.id, MemberRole::Context))
        .collect();
    let expected_head = round.to_sha;
    let plan = PersistAuthorityPlan {
        run_id: run.id.clone(),
        kind,
        expected_round_id: Some(round_id),
        expected_head: Some(expected_head),
        live_head: Some(live_head.to_string()),
        actor_kind: ActorKind::Operator,
        authority_event_id: None,
        head_changed: None,
        identity_unavailable: false,
        members,
    };
    let phase = match phase_terminal {
        Some((outcome, cause)) => Some(review_phase_terminal(db, &run.id, outcome, cause)?),
        None => None,
    };
    match persist_authority_with_run_effects(db, plan, effects, phase) {
        Ok(_) => Ok(()),
        Err(AuthorityError::Stale) => {
            tracing::warn!(
                run_id = %run.id,
                kind = kind.as_str(),
                reason = "stale",
                "authority persist rejected: applicable round or reviewed HEAD drifted"
            );
            Err(UsageOrFail::Fail(
                "authority persist rejected: applicable round or reviewed HEAD drifted".into(),
            ))
        }
        Err(AuthorityError::Storage(e)) => Err(UsageOrFail::Fail(e.to_string())),
    }
}

fn respond_compose(
    home: &Path,
    db: &Db,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    response: AgentResponse,
) -> std::result::Result<(), UsageOrFail> {
    let resolution = match response {
        AgentResponse::Approve | AgentResponse::Fix { .. } => {
            return Err(UsageOrFail::Usage(
                "compose park accepts respond|--body-file|skip|abort (not approve/fix)".into(),
            ));
        }
        AgentResponse::Compose { body, title } => {
            deliver::ComposeResolution::Respond { body, title }
        }
        AgentResponse::Skip => deliver::ComposeResolution::Skip,
        AgentResponse::Abort => deliver::ComposeResolution::Abort,
    };

    match deliver::resume_deliver_after_compose(
        db,
        home,
        &run.id,
        bare,
        wt,
        default_branch,
        resolution,
    ) {
        Ok(()) => {
            let run = db
                .run_by_id(&run.id)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?
                .ok_or_else(|| UsageOrFail::Fail("run disappeared".into()))?;
            if run.status != "parked" {
                finish_remove_worktree(bare, &run, wt);
            }
            Ok(())
        }
        Err(deliver::DeliverError::ComposeRejected(msg)) => {
            let _ = evidence_open(
                db,
                &run.id,
                PhaseName::Deliver,
                &msg,
                RunEffects {
                    status: Some("parked".into()),
                    error: Some(msg.clone()),
                    approved_head: None,
                    steps: vec![],
                },
            );
            Err(UsageOrFail::Fail(msg))
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = fail_run_with_phase(db, &run.id, &msg);
            if let Ok(Some(run)) = db.run_by_id(&run.id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            Err(UsageOrFail::Fail(msg))
        }
    }
}

fn parked_phase(db: &Db, run: &RunRow) -> std::result::Result<String, String> {
    if run.status != "parked" {
        return Ok(String::new());
    }
    match porch_gate::phase_view_for_run(db, &run.id) {
        Ok(Some(view)) => Ok(porch_gate::wire_phase_name(&view)),
        Ok(None) => Ok("unavailable".into()),
        Err(e) => {
            tracing::error!(
                run_id = %run.id,
                error = %e,
                "phase view read failed for parked run"
            );
            Err(format!("phase view read failed: {e}"))
        }
    }
}

/// Fixer for rebase-parked runs: edit tip, then retry rebase and continue pipeline.
#[allow(clippy::too_many_lines)] // rebase resume parks/fails/continues through the seam
fn respond_rebase_fix(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> std::result::Result<(), UsageOrFail> {
    if !wt.exists() {
        return Err(UsageOrFail::Fail("parked run worktree missing".into()));
    }
    let onto = run
        .base_sha
        .clone()
        .ok_or_else(|| UsageOrFail::Fail("rebase park missing base_sha".into()))?;
    let detail = run
        .error
        .clone()
        .unwrap_or_else(|| "rebase conflict".into());
    let findings_json = serde_json::json!([{
        "id": "rebase0",
        "path": "",
        "message": detail,
        "severity": "error",
        "action": "ask-user",
        "category": "rebase",
        "base_sha": onto,
    }])
    .to_string();

    let fixer_dir = run_fixer_dir(home, &run.id);
    let (prompt_file, findings_file) = write_rebase_fix_inputs(&fixer_dir, &findings_json)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    // Resume the parked rebase attempt.
    evidence_open(
        db,
        &run.id,
        PhaseName::Rebase,
        "fixer_resume",
        RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let bin = match fixer_bin() {
        Ok(b) => b,
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &run.sha, &e.to_string())?;
            return Ok(());
        }
    };

    if let Err(e) = run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    }) {
        fail_fix_run(db, bare, wt, run, &run.sha, &e.to_string())?;
        return Ok(());
    }

    // Retry rebase onto the recorded base (do not refresh trusted pin).
    if let Err(e) = porch_git::rebase(wt, &onto) {
        match porch_git::rebase_abort(wt) {
            Ok(()) => {
                let msg = format!("rebase conflict: {e}");
                park_phase_step(db, &run.id, "rebase", Some(&msg), Some(&msg))
                    .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
                return Ok(());
            }
            Err(abort_err) => {
                let msg = format!("rebase conflict: {e}; rebase --abort failed: {abort_err}");
                fail_run_with_phase(db, &run.id, &msg)
                    .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
                finish_remove_worktree(bare, run, wt);
                return Ok(());
            }
        }
    }

    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    db.set_run_shas(&run.id, Some(&head), Some(&onto))
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    complete_phase_step(db, &run.id, "rebase", "completed", Some("after fix"))
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let range = format!("{onto}..{head}");
    let empty = porch_git::diff_is_empty(wt, &range).unwrap_or(false);
    if empty {
        for phase in ["review", "certify", "deliver"] {
            skip_phase_step(db, &run.id, phase, "empty after rebase")
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        complete_run_with_phase(db, &run.id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        finish_remove_worktree(bare, run, wt);
        return Ok(());
    }

    start_phase(db, &run.id, PhaseName::Review, RunEffects::none())
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    match run_review_phase(db, home, &run.id, bare, wt, false) {
        Ok(ReviewPhase::Approved) => {
            complete_after_review(db, home, bare, wt, run, None)?;
        }
        Ok(ReviewPhase::Parked) => {
            park_phase_step(db, &run.id, "review", None, None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        Err(e) => {
            let msg = e.to_string();
            fail_run_with_phase(db, &run.id, &msg)
                .map_err(|err| UsageOrFail::Fail(err.to_string()))?;
            finish_remove_worktree(bare, run, wt);
        }
    }
    let _ = default_branch;
    Ok(())
}

fn respond_fix(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    finding_ids: Option<&Vec<String>>,
    yes: bool,
) -> std::result::Result<(), UsageOrFail> {
    if !wt.exists() {
        return Err(UsageOrFail::Fail("parked run worktree missing".into()));
    }

    let all_findings = findings_for_run(db, run).map_err(UsageOrFail::Fail)?;
    let selected = select_findings(&all_findings, finding_ids)?;
    if selected.is_empty() {
        return Err(UsageOrFail::Usage(
            "no findings selected; pass --findings or ensure blocking findings exist".into(),
        ));
    }

    persist_fix_requested(db, run, wt, &selected)?;

    let Some(pre_fix_head) = spawn_and_wait_fixer(db, home, run, bare, wt, &selected)? else {
        // Fixer failed closed; run already marked failed.
        return Ok(());
    };
    let new_head =
        porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    db.set_run_shas(&run.id, Some(&new_head), None)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    persist_uncertified_after_fix(db, wt, run, &pre_fix_head, &new_head)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    // Head may have moved: hand parked review → successor before session-free rereview.
    let review = open_attempt(db, &run.id, PhaseName::Review)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    persist_effects(
        db,
        &run.id,
        PhaseTransition::Handoff {
            from: review,
            to_phase: PhaseName::Review,
            outcome: "rereview".into(),
            cause: Some("fixer_ok".into()),
        },
        RunEffects::none(),
    )
    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    finish_rereview(db, home, run, bare, wt, yes)
}

/// Persist `fix_requested` with selected instance ULIDs as targets; commit before fixer spawn.
/// Legacy parks (`round_for_decision` none, `findings_json`): skip persist, no event.
fn persist_fix_requested(
    db: &Db,
    run: &RunRow,
    wt: &Path,
    selected: &[Finding],
) -> std::result::Result<(), UsageOrFail> {
    let Some(round_id) =
        round_for_decision(db, run).map_err(|e| UsageOrFail::Fail(e.to_string()))?
    else {
        tracing::warn!(
            run_id = %run.id,
            kind = "fix_requested",
            reason = "legacy",
            "skipping fix_requested authority event (no decision round)"
        );
        return Ok(());
    };
    let round = rounds::get_round(db, &round_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| {
            UsageOrFail::Fail(format!("decision round {} missing", round_id.as_str()))
        })?;
    let instances =
        rounds::instances_for_round(db, &round_id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    // Same ordinal as status_from_instance / findings_for_run: instances_for_round → f0..fN.
    let display_to_instance: std::collections::HashMap<String, String> = instances
        .into_iter()
        .enumerate()
        .map(|(i, inst)| (format!("f{i}"), inst.id))
        .collect();
    let mut members = Vec::with_capacity(selected.len());
    for finding in selected {
        let Some(instance_id) = display_to_instance.get(&finding.id) else {
            return Err(UsageOrFail::Fail(format!(
                "finding {} has no durable instance in the applicable round",
                finding.id
            )));
        };
        members.push((instance_id.clone(), MemberRole::Target));
    }
    let live_head =
        porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let plan = PersistAuthorityPlan {
        run_id: run.id.clone(),
        kind: AuthorityKind::FixRequested,
        expected_round_id: Some(round_id),
        expected_head: Some(round.to_sha),
        live_head: Some(live_head),
        actor_kind: ActorKind::Operator,
        authority_event_id: None,
        head_changed: None,
        identity_unavailable: false,
        members,
    };
    match persist_authority(db, plan) {
        Ok(_) => Ok(()),
        Err(AuthorityError::Stale) => {
            tracing::warn!(
                run_id = %run.id,
                kind = "fix_requested",
                reason = "stale",
                "authority persist rejected: applicable round or reviewed HEAD drifted"
            );
            Err(UsageOrFail::Fail(
                "authority persist rejected: applicable round or reviewed HEAD drifted".into(),
            ))
        }
        Err(AuthorityError::Storage(e)) => Err(UsageOrFail::Fail(e.to_string())),
    }
}

/// Returns `Ok(None)` when the fixer failed closed (run already marked failed).
fn spawn_and_wait_fixer(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    selected: &[Finding],
) -> std::result::Result<Option<String>, UsageOrFail> {
    let findings_json =
        findings_json_with_notes(home, &run.id, selected).map_err(UsageOrFail::Fail)?;
    let fixer_dir = run_fixer_dir(home, &run.id);
    let (prompt_file, findings_file) = write_fixer_inputs(&fixer_dir, &findings_json)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    // Nested fixer under the parked review attempt.
    let review = open_attempt(db, &run.id, PhaseName::Review)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let fixer = persist_effects(
        db,
        &run.id,
        PhaseTransition::NestedStart {
            parent: review,
            kind: OperationKind::Fixer,
        },
        RunEffects {
            status: Some("running".into()),
            error: None,
            approved_head: None,
            steps: vec![],
        },
    )
    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let pre_fix_head =
        porch_git::rev_parse_c(wt, "HEAD").map_err(|e| UsageOrFail::Fail(e.to_string()))?;

    let bin = match fixer_bin() {
        Ok(b) => b,
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &pre_fix_head, &e.to_string())?;
            return Ok(None);
        }
    };

    match run_fixer(&RunFixerOpts {
        work_tree: wt,
        prompt_file: &prompt_file,
        findings_file: &findings_file,
        porch_home: home,
        bin: &bin,
        timeout: fixer_timeout(),
        session_id: run.fixer_session_id.as_deref(),
    }) {
        Ok(outcome) => {
            if let Some(sid) = outcome.session_id.as_deref() {
                db.set_fixer_session_id(&run.id, Some(sid))
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
            // Successful nested fixer terminals before review→review handoff (PHASE-2.6).
            persist_effects(
                db,
                &run.id,
                PhaseTransition::NestedTerminal {
                    attempt: fixer,
                    outcome: "completed".into(),
                    cause: Some("fixer_ok".into()),
                },
                RunEffects::none(),
            )
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            Ok(Some(pre_fix_head))
        }
        Err(e) => {
            fail_fix_run(db, bare, wt, run, &pre_fix_head, &e.to_string())?;
            Ok(None)
        }
    }
}

#[allow(clippy::unnecessary_wraps)] // callers use `?` in Result contexts
fn fail_fix_run(
    db: &Db,
    bare: &GitDir,
    wt: &Path,
    run: &RunRow,
    pre_fix_head: &str,
    msg: &str,
) -> std::result::Result<(), UsageOrFail> {
    if let Ok(new_head) = porch_git::rev_parse_c(wt, "HEAD") {
        let _ = persist_uncertified_after_fix(db, wt, run, pre_fix_head, &new_head);
    }
    // NestedTerminal closes parent review on fixer fail.
    if let Ok(review) = open_attempt(db, &run.id, PhaseName::Review) {
        if let Ok(Some(fixer)) =
            phase::open_nested_attempt(db, &run.id, &review, Some(OperationKind::Fixer))
        {
            let _ = persist_effects(
                db,
                &run.id,
                PhaseTransition::NestedTerminal {
                    attempt: fixer.id,
                    outcome: "failed".into(),
                    cause: Some(msg.to_string()),
                },
                RunEffects {
                    status: Some("failed".into()),
                    error: Some(msg.to_string()),
                    approved_head: None,
                    steps: vec![],
                },
            );
        } else {
            let _ = fail_run_with_phase(db, &run.id, msg);
        }
    } else {
        let _ = fail_run_with_phase(db, &run.id, msg);
    }
    finish_remove_worktree(bare, run, wt);
    Ok(())
}

fn finish_rereview(
    db: &Db,
    home: &Path,
    run: &RunRow,
    bare: &GitDir,
    wt: &Path,
    yes: bool,
) -> std::result::Result<(), UsageOrFail> {
    // Session-free rereview (never pass fixer session).
    match run_review_phase(db, home, &run.id, bare, wt, true) {
        Ok(ReviewPhase::Approved) => {
            // No blocking findings: complete without synthesizing a bulk approve event.
            complete_after_review(db, home, bare, wt, run, None)?;
        }
        Ok(ReviewPhase::Parked) => {
            if yes {
                let head = porch_git::rev_parse_c(wt, "HEAD")
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                persist_standing_consent_approve(db, run, &head)?;
                clear_uncertified_if_certified(db, wt, &run.repo_id, &run.branch, &head)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
                let repo = db
                    .repo_by_id(&run.repo_id)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?
                    .ok_or_else(|| UsageOrFail::Fail(format!("unknown repo {}", run.repo_id)))?;
                let parked =
                    finish_certify_and_deliver(home, db, bare, wt, &run.id, &repo.default_branch)?;
                if !parked {
                    finish_remove_worktree(bare, run, wt);
                }
            } else {
                park_phase_step(db, &run.id, "review", Some("fix_review"), None)
                    .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            }
        }
        Err(e) => {
            let msg = e.to_string();
            fail_run_with_phase(db, &run.id, &msg).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            finish_remove_worktree(bare, run, wt);
        }
    }
    Ok(())
}

/// `--yes` standing consent: porch `review_approved` citing durable `fix_requested`, or legacy column write.
fn persist_standing_consent_approve(
    db: &Db,
    run: &RunRow,
    live_head: &str,
) -> std::result::Result<(), UsageOrFail> {
    let Some(round_id) =
        round_for_decision(db, run).map_err(|e| UsageOrFail::Fail(e.to_string()))?
    else {
        tracing::warn!(
            run_id = %run.id,
            kind = "review_approved",
            reason = "legacy",
            "standing consent falling back to legacy approve (no decision round)"
        );
        db.set_review_approved_head_sha(&run.id, Some(live_head))
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        complete_phase_step(
            db,
            &run.id,
            "review",
            "completed",
            Some("approved remaining after --yes"),
        )
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        return Ok(());
    };

    let Some((fix_id, fix_reviewed_head)) =
        rounds::latest_fix_requested(db, &run.id).map_err(|e| UsageOrFail::Fail(e.to_string()))?
    else {
        tracing::warn!(
            run_id = %run.id,
            kind = "review_approved",
            reason = "missing_cite",
            "standing consent refused: no fix_requested authority event to cite"
        );
        return Err(UsageOrFail::Fail(
            "standing consent requires a prior fix_requested authority event".into(),
        ));
    };
    let fix_reviewed_head = fix_reviewed_head.as_deref().unwrap_or("");
    let head_changed = live_head != fix_reviewed_head;

    let round = rounds::get_round(db, &round_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| {
            UsageOrFail::Fail(format!("decision round {} missing", round_id.as_str()))
        })?;
    let instances =
        rounds::instances_for_round(db, &round_id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let members: Vec<(String, MemberRole)> = instances
        .into_iter()
        .map(|inst| (inst.id, MemberRole::Context))
        .collect();
    let plan = PersistAuthorityPlan {
        run_id: run.id.clone(),
        kind: AuthorityKind::ReviewApproved,
        expected_round_id: Some(round_id),
        expected_head: Some(round.to_sha),
        live_head: Some(live_head.to_string()),
        actor_kind: ActorKind::Porch,
        authority_event_id: Some(fix_id),
        head_changed: Some(head_changed),
        identity_unavailable: false,
        members,
    };
    let effects = RunEffects {
        status: None,
        error: None,
        approved_head: Some(live_head.to_string()),
        steps: vec![StepEffect {
            step: "review".into(),
            status: "completed".into(),
            error: Some("approved remaining after --yes".into()),
        }],
    };
    let phase = Some(review_phase_terminal(
        db,
        &run.id,
        "completed",
        Some("approved remaining after --yes"),
    )?);
    match persist_authority_with_run_effects(db, plan, effects, phase) {
        Ok(_) => Ok(()),
        Err(AuthorityError::Stale) => {
            tracing::warn!(
                run_id = %run.id,
                kind = "review_approved",
                reason = "stale",
                "standing consent rejected: applicable round or reviewed HEAD drifted"
            );
            Err(UsageOrFail::Fail(
                "authority persist rejected: applicable round or reviewed HEAD drifted".into(),
            ))
        }
        Err(AuthorityError::Storage(e)) => Err(UsageOrFail::Fail(e.to_string())),
    }
}

fn complete_after_review(
    db: &Db,
    home: &Path,
    bare: &GitDir,
    wt: &Path,
    run: &RunRow,
    review_note: Option<&str>,
) -> std::result::Result<(), UsageOrFail> {
    complete_phase_step(db, &run.id, "review", "completed", review_note)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    let repo = db
        .repo_by_id(&run.repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Fail(format!("unknown repo {}", run.repo_id)))?;
    let parked = finish_certify_and_deliver(home, db, bare, wt, &run.id, &repo.default_branch)?;
    if !parked {
        finish_remove_worktree(bare, run, wt);
    }
    Ok(())
}

/// Shared certify → deliver(+repair) path for approve / post-fix complete.
///
/// Returns `true` when deliver repair rereview parked (worktree kept).
fn finish_certify_and_deliver(
    home: &Path,
    db: &Db,
    bare: &GitDir,
    wt: &Path,
    run_id: &str,
    default_branch: &str,
) -> std::result::Result<bool, UsageOrFail> {
    assert_head_continuity(db, run_id, wt).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    start_phase(db, run_id, PhaseName::Certify, RunEffects::none())
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    match certify::run_certify_phase(db, home, run_id, bare, wt, default_branch, None) {
        Ok(()) => {
            complete_phase_step(db, run_id, "certify", "completed", None)
                .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = complete_phase_step(db, run_id, "certify", "failed", Some(&msg));
            let _ = fail_run_with_phase(db, run_id, &msg);
            if let Ok(Some(run)) = db.run_by_id(run_id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            return Err(UsageOrFail::Fail(msg));
        }
    }
    assert_head_continuity(db, run_id, wt).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    start_phase(db, run_id, PhaseName::Deliver, RunEffects::none())
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?;
    match deliver_with_repair(db, home, run_id, bare, wt, default_branch, None) {
        Ok(PhaseLoop::Continue) => {
            complete_run_with_phase(db, run_id).map_err(|e| UsageOrFail::Fail(e.to_string()))?;
            Ok(false)
        }
        Ok(PhaseLoop::Parked) => Ok(true),
        Err(e) => {
            let msg = e.to_string();
            let _ = fail_run_with_phase(db, run_id, &msg);
            if let Ok(Some(run)) = db.run_by_id(run_id) {
                finish_remove_worktree(bare, &run, wt);
            } else {
                remove_run_worktree(bare, wt);
            }
            Err(UsageOrFail::Fail(msg))
        }
    }
}

fn select_findings(
    all: &[Finding],
    finding_ids: Option<&Vec<String>>,
) -> std::result::Result<Vec<Finding>, UsageOrFail> {
    match finding_ids {
        None => Ok(all.iter().filter(|f| f.is_blocking()).cloned().collect()),
        Some(ids) => {
            let mut selected = Vec::new();
            for id in ids {
                let Some(f) = all.iter().find(|f| f.id == *id) else {
                    return Err(UsageOrFail::Usage(format!("unknown finding id {id}")));
                };
                selected.push(f.clone());
            }
            Ok(selected)
        }
    }
}

/// Serialize selected findings for the fixer, merging optional operator notes.
fn findings_json_with_notes(
    home: &Path,
    run_id: &str,
    selected: &[Finding],
) -> std::result::Result<String, String> {
    let mut value = serde_json::to_value(selected).map_err(|e| e.to_string())?;
    let notes = load_finding_notes(home, run_id).unwrap_or_default();
    if let Some(arr) = value.as_array_mut() {
        for item in arr {
            let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(note) = notes.get(id) {
                if !note.is_empty() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("note".into(), serde_json::Value::String(note.clone()));
                    }
                }
            }
        }
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

fn resolve_run(
    db: &Db,
    run_id: Option<&str>,
    work_tree: &Path,
) -> std::result::Result<RunRow, UsageOrFail> {
    if let Some(id) = run_id {
        return db
            .run_by_id(id)
            .map_err(|e| UsageOrFail::Fail(e.to_string()))?
            .ok_or_else(|| UsageOrFail::Usage(format!("unknown run {id}")));
    }
    let abs = work_tree
        .canonicalize()
        .unwrap_or_else(|_| work_tree.to_path_buf());
    let repo_id = porch_gate::repo_id_for(&abs);
    db.latest_parked_for_repo(&repo_id)
        .map_err(|e| UsageOrFail::Fail(e.to_string()))?
        .ok_or_else(|| UsageOrFail::Usage("no parked run for this repo".into()))
}

pub(crate) fn status_from_run(
    db: &Db,
    run: &RunRow,
    home: &Path,
) -> std::result::Result<AgentStatus, String> {
    // Fail closed: storage/resolve errors must not look like "not reviewed".
    let (assurance_record, status_findings) =
        resolve_run_assurance(db, run).map_err(|e| e.to_string())?;
    let findings = status_findings
        .into_iter()
        .map(finding_from_status_dto)
        .collect();
    let phase = match run.status.as_str() {
        "parked" => parked_phase(db, run)?,
        "completed" | "failed" | "cancelled" => "done".into(),
        "running" | "pending" => "pipeline".into(),
        other => other.to_string(),
    };
    let (compose_packet_path, allowed_actions) = if phase == "compose" {
        let path = run_artifact_dir(home, &run.id).join("compose-packet.json");
        (
            Some(path.display().to_string()),
            Some(vec!["respond".into(), "skip".into(), "abort".into()]),
        )
    } else {
        (None, None)
    };
    Ok(AgentStatus {
        run_id: run.id.clone(),
        repo_id: run.repo_id.clone(),
        branch: run.branch.clone(),
        status: run.status.clone(),
        phase,
        head_sha: run.head_sha.clone(),
        base_sha: run.base_sha.clone(),
        review_approved_head_sha: run.review_approved_head_sha.clone(),
        findings,
        assurance_record,
        error: porch_gate::operator_failure_report(db, run)
            .map_err(|e| e.to_string())?
            .or_else(|| run.error.clone()),
        pr_url: run.pr_url.clone(),
        compose_packet_path,
        allowed_actions,
    })
}

fn findings_for_run(db: &Db, run: &RunRow) -> std::result::Result<Vec<Finding>, String> {
    let (_record, status_findings) = resolve_run_assurance(db, run).map_err(|e| e.to_string())?;
    Ok(status_findings
        .into_iter()
        .map(finding_from_status_dto)
        .collect())
}

fn finding_from_status_dto(dto: StatusFindingDto) -> Finding {
    let severity = match dto.severity.as_str() {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };
    let action = match dto.action.as_str() {
        "auto-fix" => Action::AutoFix,
        "ask-user" => Action::AskUser,
        _ => Action::NoOp,
    };
    Finding {
        id: dto.id,
        path: dto.path,
        message: dto.message,
        severity,
        action,
        category: dto.category,
        start_line: dto.start_line,
        end_line: dto.end_line,
        ..Finding::default()
    }
}

/// Enqueue a **new** run from a prior run's recorded tip (or branch tip).
///
/// Always allocates a fresh run id / worktree — never reuses a half-applied tree.
///
/// # Errors
///
/// Returns a string error on missing run, detached HEAD, or DB/RPC failure.
pub fn rerun(
    home: &Path,
    work_tree: &Path,
    run_id: Option<&str>,
) -> std::result::Result<String, String> {
    let work = work_tree.canonicalize().map_err(|e| e.to_string())?;
    let db = Db::open(&db_path(home)).map_err(|e| e.to_string())?;
    let repo_id = repo_id_for(&work);
    let prior = if let Some(id) = run_id {
        db.run_by_id(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown run {id}"))?
    } else {
        let branch = porch_git::stdout_trim(
            &porch_git::run_c(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map_err(|e| e.to_string())?,
        );
        if branch == "HEAD" {
            return Err("detached HEAD — checkout a branch or pass --run-id".into());
        }
        db.latest_run_for_branch(&repo_id, &branch)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no prior run for branch {branch}"))?
    };

    let sha = prior.sha.clone();
    let intent = prior.intent.as_deref();
    let intent_source = if intent.is_some() {
        Some("rerun")
    } else {
        None
    };
    let row = db
        .insert_run(&prior.repo_id, &prior.branch, &sha, intent, intent_source)
        .map_err(|e| e.to_string())?;
    if let Err(e) = rpc_start_run(home, &row.id) {
        tracing::warn!(run_id = %row.id, "start_run rpc: {e}");
    }
    Ok(row.id)
}

#[cfg(test)]
mod custody_tests {
    use super::*;
    use porch_git::{init_bare, worktree_add_detach};
    use tempfile::TempDir;

    fn git(work: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn git_out(work: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn pin_failure_keeps_worktree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-b", "main"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "main"]);
        std::fs::write(seed.join("README"), "submit\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "submit"]);
        let submit = git_out(&seed, &["rev-parse", "HEAD"]);
        std::fs::write(seed.join("README"), "descendant\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "pipeline"]);
        let head = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["push", bare_path.to_str().unwrap(), "main:refs/heads/main"],
        );

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("r-pin", &seed, &bare_path, "main").unwrap();
        let run = db.insert_run("r-pin", "feat", &submit, None, None).unwrap();
        let wt = root.join("wt-pin");
        worktree_add_detach(&bare, &wt, &head).unwrap();
        assert!(wt.exists());
        assert_ne!(submit, head);

        // Force update-ref refs/porch/recover/<run> to fail: refs/porch is a file.
        std::fs::create_dir_all(bare_path.join("refs")).unwrap();
        std::fs::write(bare_path.join("refs/porch"), "not-a-directory\n").unwrap();

        finish_remove_worktree(&bare, &run, &wt);
        assert!(
            wt.exists(),
            "worktree must be kept when required recovery pin fails"
        );
        assert!(
            porch_git::rev_parse(&bare, &sync::recovery_ref_name(&run.id)).is_err(),
            "recovery ref must not exist after failed pin"
        );
    }

    #[test]
    fn pin_success_then_removes_worktree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-b", "main"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "main"]);
        std::fs::write(seed.join("README"), "submit\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "submit"]);
        let submit = git_out(&seed, &["rev-parse", "HEAD"]);
        std::fs::write(seed.join("README"), "descendant\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "pipeline"]);
        let head = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["push", bare_path.to_str().unwrap(), "main:refs/heads/main"],
        );

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("r-pin-ok", &seed, &bare_path, "main")
            .unwrap();
        let run = db
            .insert_run("r-pin-ok", "feat", &submit, None, None)
            .unwrap();
        let wt = root.join("wt-pin-ok");
        worktree_add_detach(&bare, &wt, &head).unwrap();

        finish_remove_worktree(&bare, &run, &wt);
        assert!(!wt.exists(), "worktree removed after successful pin");
        assert_eq!(
            porch_git::rev_parse(&bare, &sync::recovery_ref_name(&run.id)).unwrap(),
            head
        );
    }
}

#[cfg(test)]
mod continuity_tests {
    use super::*;
    use porch_git::init_bare;
    use tempfile::TempDir;

    fn git(work: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn head_continuity_fails_if_approved_sha_missing_on_certify() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let db = Db::open(&home.join("state.sqlite")).unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "porch@example.com"]);
        git(&work, &["config", "user.name", "Porch"]);
        std::fs::write(work.join("README"), "x\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "c"]);

        db.upsert_repo("r1", &work, &work, "main").unwrap();
        let run = db.insert_run("r1", "feat", "deadbeef", None, None).unwrap();
        let err = assert_head_continuity(&db, &run.id, &work).unwrap_err();
        assert!(
            err.to_string().contains("review_approved_head_sha missing"),
            "{err}"
        );
    }

    #[test]
    fn head_continuity_refuses_a_head_off_the_approved_line() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let db = Db::open(&home.join("state.sqlite")).unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "porch@example.com"]);
        git(&work, &["config", "user.name", "Porch"]);
        std::fs::write(work.join("README"), "x\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "reviewed"]);
        let approved = porch_git::rev_parse_c(&work, "HEAD").unwrap();

        db.upsert_repo("r-desc", &work, &work, "main").unwrap();
        let run = db
            .insert_run("r-desc", "feat", &approved, None, None)
            .unwrap();
        db.set_review_approved_head_sha(&run.id, Some(&approved))
            .unwrap();

        assert_eq!(
            authorized_forward_sha(&db, &run.id, &work).unwrap(),
            approved,
            "an unmoved HEAD forwards the approved sha"
        );

        // Certify's correction commit advances HEAD after review approves, so
        // continuity tolerates a descendant and the forward carries it.
        std::fs::write(work.join("README"), "x\ny\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "correction"]);
        let descendant = porch_git::rev_parse_c(&work, "HEAD").unwrap();
        assert_eq!(
            authorized_forward_sha(&db, &run.id, &work).unwrap(),
            descendant,
            "a descendant of the approved sha stays forwardable today"
        );

        // A HEAD off the approved line is refused, naming both SHAs.
        git(&work, &["checkout", "-q", "--orphan", "unrelated"]);
        std::fs::write(work.join("README"), "elsewhere\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-m", "unrelated"]);
        let off_line = porch_git::rev_parse_c(&work, "HEAD").unwrap();

        let err = assert_head_continuity(&db, &run.id, &work).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&off_line) && msg.contains(&approved), "{msg}");
    }

    #[test]
    fn skip_review_empty_diff_does_not_require_approved_sha() {
        // Documented contract: empty-diff skip_remaining never calls assert_head_continuity.
        // Smoke: execute path with empty diff is covered by m2_run integration.
        let _ = init_bare;
    }
}

#[cfg(test)]
mod spawn_err_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn floor_timeout_and_exit_name_the_floor_layer() {
        let timeout = map_spawn_err(
            porch_review::Error::Timeout(Duration::from_secs(2)),
            Role::Floor,
        )
        .to_string();
        assert!(timeout.contains("floor"), "{timeout}");
        assert!(!timeout.contains("review CLI"), "{timeout}");
        assert!(!timeout.contains("review timed out"), "{timeout}");

        let exit = map_spawn_err(
            porch_review::Error::Exit {
                status: 1,
                stderr: "boom".into(),
            },
            Role::Floor,
        )
        .to_string();
        assert!(exit.contains("floor"), "{exit}");
        assert!(!exit.contains("review CLI"), "{exit}");
    }

    #[test]
    fn judgment_timeout_keeps_review_copy() {
        let msg = map_spawn_err(
            porch_review::Error::Timeout(Duration::from_secs(2)),
            Role::Judgment,
        )
        .to_string();
        assert_eq!(msg, "review timed out after 2s");
    }
}

#[cfg(test)]
mod notes_tests {
    use super::*;
    use porch_gate::set_finding_note;
    use porch_review::{Action, Severity};
    use tempfile::TempDir;

    #[test]
    fn findings_json_merges_operator_notes() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        set_finding_note(home, "run-n", "f0", "keep the helper public").unwrap();
        let selected = vec![Finding {
            id: "f0".into(),
            path: "src/a.rs".into(),
            message: "unused".into(),
            severity: Severity::Warning,
            action: Action::AskUser,
            category: None,
            start_line: Some(1),
            end_line: Some(2),
            ..Finding::default()
        }];
        let json = findings_json_with_notes(home, "run-n", &selected).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["note"], "keep the helper public");
        assert_eq!(v[0]["id"], "f0");
    }
}
