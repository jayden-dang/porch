//! Deliver phase: lease-push exact SHA, scaffold PR, park compose (watch later).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use porch_deliver::{
    Attestation, CheckRow, MANAGED_BEGIN, MANAGED_END, MergeableState, PrOpts, ScaffoldFacts,
    StepSnapshot, TemplateSource, WatchChecksOpts, WatchOutcome, build_scaffold_body,
    check_poll_interval, check_timeout, compose_managed_interior, create_pr,
    default_scaffold_interior, deterministic_pr_title, edit_pr_body, edit_pr_title,
    ensure_gh_runnable, find_open_pr, gh_bin, gh_timeout, is_porch_managed_title, load_pr_template,
    merge_porch_managed, pr_mergeable, theater_reject_rules, validate_compose_body, view_pr,
    watch_allowlisted_checks,
};

#[cfg(test)]
use std::sync::Mutex;

/// Test-only `gh` override (edition 2024 forbids `env::set_var` in unit tests).
#[cfg(test)]
static TEST_GH_BIN: Mutex<Option<String>> = Mutex::new(None);

fn resolve_gh_bin() -> String {
    #[cfg(test)]
    {
        if let Ok(guard) = TEST_GH_BIN.lock() {
            if let Some(bin) = guard.as_ref() {
                return bin.clone();
            }
        }
    }
    gh_bin()
}
use porch_gate::{Db, event_hub, resolve_run_assurance, run_artifact_dir};
use porch_git::{
    GitDir, PushDecision, RemoteTip, ls_remote_sha, push_exact_sha, remote_commits_incorporated,
    resolve_push_decision,
};
use serde_json::json;

use crate::config::{PorchConfig, effective_base_branch, load_trusted_at_sha};

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeliverError {
    #[error(transparent)]
    Gate(#[from] porch_gate::Error),
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Deliver(#[from] porch_deliver::Error),
    /// Genuine allowlisted red — eligible for mechanical repair.
    #[error("allowlisted check failed: {}", failed_names(.checks))]
    AllowlistFailed { checks: Vec<CheckRow> },
    /// Terminal non-mechanical allowlisted state (cancelled / `timed_out` / …).
    #[error("allowlisted check non-repairable: {}", .names.join(", "))]
    AllowlistNonRepairable { names: Vec<String> },
    /// Forge reports the PR base is CONFLICTING.
    #[error("pr merge conflicting")]
    MergeConflicting,
    #[error("allowlisted checks not green before poll timeout")]
    WatchTimeout,
    /// Agent compose body failed validation; run stays parked.
    #[error("{0}")]
    ComposeRejected(String),
    #[error("{0}")]
    Msg(String),
}

/// Outcome of [`run_deliver_phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliverOutcome {
    /// Scaffold PR written; compose step parked — do not mark deliver completed.
    ParkedCompose,
    /// Deliver finished (already-composed redeliver + optional watch).
    Completed,
}

/// Continuity must already hold. Order: ensure `gh` → lease-push → scaffold → park compose
/// (or refresh + watch when compose already resolved on this run).
///
/// # Errors
///
/// Fail closed on missing continuity facts, unverifiable remote, incorporate
/// refuse, `gh` missing, PR listing undecodable, or allowlisted check red/timeout.
pub(crate) fn run_deliver_phase(
    db: &Db,
    home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: Option<&AtomicBool>,
) -> Result<DeliverOutcome, DeliverError> {
    if cancelled(cancel) {
        return Err(DeliverError::Msg("cancelled".into()));
    }

    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;

    let head_sha = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head_sha), None)?;

    let bin = resolve_gh_bin();
    // Prefer fail before push when gh cannot run (no branch without PR adapter).
    ensure_gh_runnable(&bin)?;

    let trusted = load_trusted_deliver(db, run_id, bare, default_branch)?;
    let deliver_cfg = &trusted.deliver_github;
    // M6: never raise rerun budget; refuse if trusted yaml somehow requested it
    // while we do not implement rerun.
    if deliver_cfg.rerun_transient > 0 {
        tracing::warn!(
            rerun_transient = deliver_cfg.rerun_transient,
            "trusted rerun_transient > 0 ignored in M6 (no gh run rerun)"
        );
    }
    let pr_base = effective_base_branch(&trusted.pr_base_branch, default_branch).to_string();

    let refname = format!("refs/heads/{}", run.branch);
    lease_push_exact(bare, &refname, &head_sha, run.base_sha.as_deref())?;

    if cancelled(cancel) {
        return Err(DeliverError::Msg("cancelled".into()));
    }

    let already_composed = compose_already_resolved(db, run_id)?;
    let scaffold = assemble_scaffold(db, bare, &run, &head_sha, wt)?;
    let timeout = gh_timeout();

    let written = create_or_update_scaffold_pr(&bin, timeout, wt, &run, &pr_base, &scaffold)?;
    db.set_pr_url(run_id, Some(&written.url))?;
    if let Some(ref title) = written.title_written {
        db.set_pr_title_written(run_id, Some(title))?;
    }
    probe_mergeable(&bin, timeout, wt, written.number)?;

    if already_composed {
        maybe_watch(
            &bin,
            timeout,
            wt,
            written.number,
            &deliver_cfg.watch_checks,
            cancel,
        )?;
        return Ok(DeliverOutcome::Completed);
    }

    write_compose_packet(
        home,
        &run,
        &head_sha,
        &written.url,
        written.number,
        &scaffold,
    )?;
    park_compose(db, run_id)?;
    Ok(DeliverOutcome::ParkedCompose)
}

struct WrittenPr {
    url: String,
    number: u64,
    title_written: Option<String>,
}

/// Soft-fail `view_pr`: warn and return a view with empty body (and caller title).
fn view_pr_or_empty(
    bin: &str,
    timeout: std::time::Duration,
    wt: &Path,
    number: u64,
    url: &str,
    title: &str,
    during: &str,
) -> porch_deliver::PrView {
    match view_pr(bin, timeout, wt, number) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                pr = number,
                "view_pr failed during {during}; merging onto empty body"
            );
            porch_deliver::PrView {
                number,
                url: url.to_string(),
                title: title.to_string(),
                body: String::new(),
            }
        }
    }
}

fn create_or_update_scaffold_pr(
    bin: &str,
    timeout: std::time::Duration,
    wt: &Path,
    run: &porch_gate::RunRow,
    pr_base: &str,
    scaffold: &ScaffoldAssembly,
) -> Result<WrittenPr, DeliverError> {
    if let Some(existing) = find_open_pr(bin, timeout, wt, &run.branch)? {
        let viewed = view_pr_or_empty(
            bin,
            timeout,
            wt,
            existing.number,
            &existing.url,
            &existing.title,
            "scaffold update",
        );
        let body = merge_porch_managed(
            &viewed.body,
            &scaffold.managed_interior,
            &scaffold.attestation,
        );
        edit_pr_body(bin, timeout, wt, existing.number, &body)?;

        let mut title_written = run.pr_title_written.clone();
        if is_porch_managed_title(
            &viewed.title,
            run.pr_title_written.as_deref(),
            &scaffold.title,
        ) {
            edit_pr_title(bin, timeout, wt, existing.number, &scaffold.title)?;
            title_written = Some(scaffold.title.clone());
        }

        if existing.url.is_empty() {
            return Err(DeliverError::Msg("existing PR has empty url".into()));
        }
        return Ok(WrittenPr {
            url: existing.url,
            number: existing.number,
            title_written,
        });
    }

    let url = create_pr(&PrOpts {
        bin,
        timeout,
        work_tree: wt,
        head_branch: &run.branch,
        base_branch: pr_base,
        title: &scaffold.title,
        body: &scaffold.body,
    })?;
    let number = find_open_pr(bin, timeout, wt, &run.branch)?
        .map(|p| p.number)
        .ok_or_else(|| {
            DeliverError::Msg("pr create succeeded but open PR listing is empty".into())
        })?;
    Ok(WrittenPr {
        url,
        number,
        title_written: Some(scaffold.title.clone()),
    })
}

fn probe_mergeable(
    bin: &str,
    timeout: std::time::Duration,
    wt: &Path,
    pr_number: u64,
) -> Result<(), DeliverError> {
    if pr_number == 0 {
        return Ok(());
    }
    match pr_mergeable(bin, timeout, wt, pr_number) {
        Ok(MergeableState::Conflicting) => Err(DeliverError::MergeConflicting),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(error = %e, "pr mergeable probe failed; continuing");
            Ok(())
        }
    }
}

fn park_compose(db: &Db, run_id: &str) -> Result<(), DeliverError> {
    db.insert_step_result(run_id, "compose", "parked", None)?;
    db.set_run_status(run_id, "parked", Some("awaiting compose"))?;
    if let Some(hub) = event_hub() {
        hub.publish_state(run_id);
        hub.publish_activity(run_id, "step=compose status=parked");
    }
    Ok(())
}

struct ScaffoldAssembly {
    title: String,
    /// Full PR body written on create (managed wrap + attestation).
    body: String,
    /// Managed interior only (for [`merge_porch_managed`]).
    managed_interior: String,
    attestation: Attestation,
    template_source: TemplateSource,
    template_path: Option<String>,
    change_summary: String,
}

fn assemble_scaffold(
    db: &Db,
    bare: &GitDir,
    run: &porch_gate::RunRow,
    head_sha: &str,
    wt: &Path,
) -> Result<ScaffoldAssembly, DeliverError> {
    let steps = db.step_results_for_run(&run.id)?;
    let snapshots: Vec<StepSnapshot> = steps
        .iter()
        .map(|s| StepSnapshot {
            step: s.step.clone(),
            status: s.status.clone(),
        })
        .collect();
    let attestation = Attestation {
        head_sha: head_sha.to_string(),
        steps: snapshots,
        assurance_shape: attestation_shape(db, run)?,
    };

    let trusted_sha = run.trusted_config_sha.as_deref().ok_or_else(|| {
        DeliverError::Msg("deliver requires trusted_config_sha (pin at rebase)".into())
    })?;
    let loaded = load_pr_template(bare, trusted_sha)?;
    let template_text = loaded
        .bytes
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned());

    let subjects = commit_subjects(wt, run.base_sha.as_deref(), head_sha);
    let change_summary = change_summary_prose(run.intent.as_deref(), &subjects);
    let commit_subject = subjects.first().cloned();
    let title = deterministic_pr_title(
        run.branch.as_str(),
        run.intent.as_deref(),
        commit_subject.as_deref(),
    );

    let facts = ScaffoldFacts {
        summary: summary_for_facts(run.intent.as_deref(), &subjects),
        ..ScaffoldFacts::default()
    };

    let managed_interior = match template_text.as_deref() {
        Some(t) => {
            let mut interior = t.to_string();
            if !interior.ends_with('\n') {
                interior.push('\n');
            }
            interior
        }
        None => default_scaffold_interior(&facts),
    };

    let body = build_scaffold_body(template_text.as_deref(), &facts, &attestation);

    Ok(ScaffoldAssembly {
        title,
        body,
        managed_interior,
        attestation,
        template_source: loaded.source,
        template_path: loaded.path,
        change_summary,
    })
}

fn write_compose_packet(
    home: &Path,
    run: &porch_gate::RunRow,
    head_sha: &str,
    pr_url: &str,
    pr_number: u64,
    scaffold: &ScaffoldAssembly,
) -> Result<(), DeliverError> {
    let dir = run_artifact_dir(home, &run.id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| DeliverError::Msg(format!("compose packet dir: {e}")))?;
    let path = dir.join("compose-packet.json");
    let packet = json!({
        "schema_version": 1,
        "run_id": run.id,
        "repo_id": run.repo_id,
        "branch": run.branch,
        "base_sha": run.base_sha,
        "head_sha": head_sha,
        "pr_url": pr_url,
        "pr_number": pr_number,
        "intent": run.intent,
        "title_scaffold": scaffold.title,
        "body_scaffold": scaffold.body,
        "template_source": scaffold.template_source.as_str(),
        "template_path": scaffold.template_path,
        "change_summary": scaffold.change_summary,
        "theater_reject_rules": theater_reject_rules(),
        "porch_managed_markers": {
            "begin": MANAGED_BEGIN,
            "end": MANAGED_END,
        },
    });
    let bytes = serde_json::to_vec_pretty(&packet)
        .map_err(|e| DeliverError::Msg(format!("compose packet json: {e}")))?;
    std::fs::write(&path, bytes)
        .map_err(|e| DeliverError::Msg(format!("write compose packet: {e}")))?;
    Ok(())
}

fn compose_already_resolved(db: &Db, run_id: &str) -> Result<bool, DeliverError> {
    Ok(db
        .latest_step_for_run(run_id, "compose")?
        .is_some_and(|s| s.status == "completed" || s.status == "skipped"))
}

/// How compose park was resolved by the Agent / Operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeResolution {
    /// Merge Agent body (+ optional title) into the scaffold PR.
    Respond { body: String, title: Option<String> },
    /// Accept scaffold prose and finish deliver.
    Skip,
    /// Fail/cancel the run; leave the GitHub PR open.
    Abort,
}

/// Apply compose respond/skip/abort and finish deliver (watch when configured).
///
/// # Errors
///
/// Validation failures stay parked (caller maps [`DeliverError::ComposeRejected`]).
/// Watch / `gh` failures fail the run after compose is marked resolved when applicable.
pub(crate) fn resume_deliver_after_compose(
    db: &Db,
    _home: &Path,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    resolution: ComposeResolution,
) -> Result<(), DeliverError> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;

    match resolution {
        ComposeResolution::Abort => {
            db.insert_step_result(run_id, "compose", "cancelled", Some("agent abort"))?;
            db.set_run_status(run_id, "cancelled", Some("agent abort"))?;
            if let Some(hub) = event_hub() {
                hub.publish_state(run_id);
                hub.publish_activity(run_id, "step=compose status=cancelled");
            }
            Ok(())
        }
        ComposeResolution::Respond { body, title } => {
            validate_compose_body(&body).map_err(DeliverError::ComposeRejected)?;
            apply_compose_respond(db, &run, bare, wt, default_branch, &body, title.as_deref())
        }
        ComposeResolution::Skip => apply_compose_skip(db, &run, bare, wt, default_branch),
    }
}

fn apply_compose_respond(
    db: &Db,
    run: &porch_gate::RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    body: &str,
    title: Option<&str>,
) -> Result<(), DeliverError> {
    let bin = resolve_gh_bin();
    let timeout = gh_timeout();
    let head_sha = porch_git::rev_parse_c(wt, "HEAD")?;
    let pr = find_open_pr(&bin, timeout, wt, &run.branch)?
        .ok_or_else(|| DeliverError::Msg("compose respond: no open PR for branch".into()))?;
    let viewed = view_pr_or_empty(&bin, timeout, wt, pr.number, &pr.url, "", "compose respond");

    let interior = compose_managed_interior(body);
    let attestation = attestation_post_compose(db, &run.id, &head_sha, "completed")?;
    let merged = merge_porch_managed(&viewed.body, &interior, &attestation);
    edit_pr_body(&bin, timeout, wt, pr.number, &merged)?;

    if let Some(new_title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        let subjects = commit_subjects(wt, run.base_sha.as_deref(), &head_sha);
        let scaffold_title = deterministic_pr_title(
            run.branch.as_str(),
            run.intent.as_deref(),
            subjects.first().map(String::as_str),
        );
        if is_porch_managed_title(
            &viewed.title,
            run.pr_title_written.as_deref(),
            &scaffold_title,
        ) {
            edit_pr_title(&bin, timeout, wt, pr.number, new_title)?;
            db.set_pr_title_written(&run.id, Some(new_title))?;
        }
    }

    let trusted = load_trusted_deliver(db, &run.id, bare, default_branch)?;
    // Compose + deliver steps resolve before allowlist watch.
    db.insert_step_result(&run.id, "compose", "completed", Some("compose=agent"))?;
    db.insert_step_result(&run.id, "deliver", "completed", Some("compose=agent"))?;
    maybe_watch(
        &bin,
        timeout,
        wt,
        pr.number,
        &trusted.deliver_github.watch_checks,
        None,
    )?;
    db.set_run_status(&run.id, "completed", None)?;
    if let Some(hub) = event_hub() {
        hub.publish_state(&run.id);
        hub.publish_activity(&run.id, "step=deliver status=completed");
    }
    Ok(())
}

fn apply_compose_skip(
    db: &Db,
    run: &porch_gate::RunRow,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
) -> Result<(), DeliverError> {
    let bin = resolve_gh_bin();
    let timeout = gh_timeout();
    let head_sha = porch_git::rev_parse_c(wt, "HEAD")?;
    let pr = find_open_pr(&bin, timeout, wt, &run.branch)?
        .ok_or_else(|| DeliverError::Msg("compose skip: no open PR for branch".into()))?;
    let viewed = view_pr_or_empty(&bin, timeout, wt, pr.number, &pr.url, "", "compose skip");

    let attestation = attestation_post_compose(db, &run.id, &head_sha, "skipped")?;
    // Keep scaffold managed interior; refresh attestation only.
    let interior = compose_managed_interior(&viewed.body);
    let interior = if interior.trim().is_empty() {
        let assembly = assemble_scaffold(db, bare, run, &head_sha, wt)?;
        assembly.managed_interior
    } else {
        interior
    };
    let merged = merge_porch_managed(&viewed.body, &interior, &attestation);
    edit_pr_body(&bin, timeout, wt, pr.number, &merged)?;

    let trusted = load_trusted_deliver(db, &run.id, bare, default_branch)?;
    // Compose + deliver steps resolve before allowlist watch.
    db.insert_step_result(&run.id, "compose", "skipped", Some("compose=scaffold"))?;
    db.insert_step_result(&run.id, "deliver", "completed", Some("compose=scaffold"))?;
    maybe_watch(
        &bin,
        timeout,
        wt,
        pr.number,
        &trusted.deliver_github.watch_checks,
        None,
    )?;
    db.set_run_status(&run.id, "completed", None)?;
    if let Some(hub) = event_hub() {
        hub.publish_state(&run.id);
        hub.publish_activity(&run.id, "step=deliver status=completed");
    }
    Ok(())
}

/// Attestation as it will look after compose resolves (exclude parked compose row).
fn attestation_post_compose(
    db: &Db,
    run_id: &str,
    head_sha: &str,
    compose_status: &str,
) -> Result<Attestation, DeliverError> {
    let steps = db.step_results_for_run(run_id)?;
    let mut snapshots: Vec<StepSnapshot> = steps
        .iter()
        .filter(|s| !(s.step == "compose" && s.status == "parked"))
        .map(|s| StepSnapshot {
            step: s.step.clone(),
            status: s.status.clone(),
        })
        .collect();
    snapshots.push(StepSnapshot {
        step: "compose".into(),
        status: compose_status.into(),
    });
    snapshots.push(StepSnapshot {
        step: "deliver".into(),
        status: "completed".into(),
    });
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;
    Ok(Attestation {
        head_sha: head_sha.to_string(),
        steps: snapshots,
        assurance_shape: attestation_shape(db, &run)?,
    })
}

fn attestation_shape(db: &Db, run: &porch_gate::RunRow) -> Result<Option<String>, DeliverError> {
    let (record, _) = resolve_run_assurance(db, run)?;
    Ok(record.assurance_shape().map(str::to_string))
}

fn commit_subjects(wt: &Path, base_sha: Option<&str>, head_sha: &str) -> Vec<String> {
    let range = match base_sha {
        Some(base) if !base.is_empty() => format!("{base}..{head_sha}"),
        _ => head_sha.to_string(),
    };
    match porch_git::run_c(wt, &["log", "--format=%s", &range]) {
        Ok(out) => porch_git::stdout_trim(&out)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn change_summary_prose(intent: Option<&str>, subjects: &[String]) -> String {
    const CAP: usize = 12;
    let mut bullets = Vec::new();
    if let Some(intent) = intent.map(str::trim).filter(|s| !s.is_empty()) {
        let line = intent.lines().map(str::trim).find(|l| !l.is_empty());
        if let Some(line) = line {
            bullets.push(format!("- Intent: {line}"));
        }
    }
    for (i, subject) in subjects.iter().enumerate() {
        if i >= CAP {
            let extra = subjects.len() - CAP;
            bullets.push(format!("- …and {extra} more commit(s)"));
            break;
        }
        bullets.push(format!("- {subject}"));
    }
    if bullets.is_empty() {
        "- (no commit subjects available)".into()
    } else {
        bullets.join("\n")
    }
}

fn summary_for_facts(intent: Option<&str>, subjects: &[String]) -> String {
    if let Some(intent) = intent.map(str::trim).filter(|s| !s.is_empty()) {
        return intent
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(intent)
            .to_string();
    }
    match subjects {
        [] => String::new(),
        [one] => one.clone(),
        many => many
            .iter()
            .take(5)
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn maybe_watch(
    bin: &str,
    gh_timeout: std::time::Duration,
    wt: &Path,
    pr_number: u64,
    allowlist: &[String],
    cancel: Option<&AtomicBool>,
) -> Result<(), DeliverError> {
    if allowlist.is_empty() {
        return Ok(());
    }
    if cancelled(cancel) {
        return Err(DeliverError::Msg("cancelled".into()));
    }
    let outcome = watch_allowlisted_checks(&WatchChecksOpts {
        bin,
        gh_timeout,
        work_tree: wt,
        pr_number,
        allowlist,
        poll_deadline: check_timeout(),
        poll_interval: check_poll_interval(),
        cancel,
    })?;
    match outcome {
        WatchOutcome::Ready => Ok(()),
        WatchOutcome::Failed { checks } => Err(DeliverError::AllowlistFailed { checks }),
        WatchOutcome::NonRepairable { names } => {
            Err(DeliverError::AllowlistNonRepairable { names })
        }
        WatchOutcome::Timeout => Err(DeliverError::WatchTimeout),
        WatchOutcome::Cancelled => Err(DeliverError::Msg("cancelled".into())),
    }
}

fn failed_names(checks: &[CheckRow]) -> String {
    checks
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn lease_push_exact(
    bare: &GitDir,
    refname: &str,
    exact_sha: &str,
    base_sha: Option<&str>,
) -> Result<(), DeliverError> {
    let tip = ls_remote_sha(bare, "origin", refname)
        .map_err(|e| DeliverError::Msg(format!("ls-remote origin {refname}: {e}")))?;

    let incorporated = match &tip {
        RemoteTip::Absent => true,
        RemoteTip::Present(remote_sha) if remote_sha == exact_sha => true,
        RemoteTip::Present(remote_sha) => {
            // Fetch remote tip so rev-list can see it.
            let fetch_ref = format!("{refname}:refs/porch-deliver/observe");
            porch_git::fetch(bare, "origin", &fetch_ref)
                .map_err(|e| DeliverError::Msg(format!("fetch observe tip: {e}")))?;
            remote_commits_incorporated(bare, exact_sha, remote_sha, base_sha)
                .map_err(|e| DeliverError::Msg(format!("incorporate check unverifiable: {e}")))?
        }
    };

    let decision = resolve_push_decision(&tip, exact_sha, incorporated);
    if matches!(decision, PushDecision::RefuseIncorporate) {
        return Err(DeliverError::Msg(
            "refuse: remote has commits not incorporated into validated history".into(),
        ));
    }

    push_exact_sha(bare, "origin", refname, exact_sha, decision)
        .map_err(|e| DeliverError::Msg(format!("push exact sha: {e}")))?;

    // Post-push ls-remote must equal pushed SHA (skip for pure up-to-date).
    let after = ls_remote_sha(bare, "origin", refname)
        .map_err(|e| DeliverError::Msg(format!("post-push ls-remote: {e}")))?;
    match after {
        RemoteTip::Present(sha) if sha == exact_sha => Ok(()),
        other => Err(DeliverError::Msg(format!(
            "post-push ls-remote mismatch: expected {exact_sha}, got {other:?}"
        ))),
    }
}

fn load_trusted_deliver(
    db: &Db,
    run_id: &str,
    bare: &GitDir,
    _default_branch: &str,
) -> Result<PorchConfig, DeliverError> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;
    if run.base_sha.is_none() {
        return Err(DeliverError::Msg("deliver requires base_sha".into()));
    }
    let trusted_sha = run.trusted_config_sha.as_deref().ok_or_else(|| {
        DeliverError::Msg("deliver requires trusted_config_sha (pin at rebase)".into())
    })?;
    load_trusted_at_sha(bare, trusted_sha).map_err(DeliverError::Msg)
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(test)]
mod already_composed_tests {
    use super::*;
    use porch_gate::db_path;
    use porch_git::{GitDir, init_bare, worktree_add_detach};
    use std::process::Command as StdCommand;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn git(work: &Path, args: &[&str]) {
        let st = StdCommand::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn git_out(work: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
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

    fn chmod_755(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn already_composed_tip_refreshes_body_without_repark() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let origin = root.join("origin.git");
        init_bare(&origin).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "main"]);
        std::fs::write(seed.join("README"), "base\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "base"]);
        let main_sha = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);

        // Feature tip already on origin (lease will be up-to-date / fast-forward).
        git(&seed, &["checkout", "-b", "feat-composed"]);
        std::fs::write(seed.join("feat.txt"), "feat\n").unwrap();
        git(&seed, &["add", "feat.txt"]);
        git(&seed, &["commit", "-m", "feat change"]);
        let head = git_out(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["push", "origin", "HEAD:refs/heads/feat-composed"]);

        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();
        // Mirror objects + origin remote for lease-push / template show.
        let st = StdCommand::new("git")
            .args([
                "--git-dir",
                bare_path.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                origin.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let st = StdCommand::new("git")
            .args([
                "--git-dir",
                bare_path.to_str().unwrap(),
                "fetch",
                "origin",
                "+refs/heads/*:refs/heads/*",
            ])
            .status()
            .unwrap();
        assert!(st.success());

        let wt = root.join("wt");
        worktree_add_detach(&bare, &wt, &head).unwrap();

        let body_file = home.join("gh-pr-body.txt");
        let log_file = home.join("gh-argv.log");
        let state_file = home.join("gh-pr-state");
        // Existing open PR with prior managed body (first park already happened).
        let prior_body = format!(
            "{MANAGED_BEGIN}\n## Summary\nold scaffold\n{MANAGED_END}\n\n<!-- porch-attestation -->\nold\n"
        );
        std::fs::write(&body_file, &prior_body).unwrap();
        std::fs::write(
            &state_file,
            r#"[{"number":7,"url":"https://example.com/pull/7","title":"porch: feat-composed"}]"#,
        )
        .unwrap();

        let fake_gh = root.join("fake-gh");
        std::fs::write(
            &fake_gh,
            format!(
                r#"#!/bin/sh
set -e
LOG="{log}"
BODY="{body}"
STATE="{state}"
{{
  printf '+'
  for a in "$@"; do printf ' %s' "$a"; done
  printf '\n'
}} >> "$LOG"
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "gh version 2.50.0 (fake)"; exit 0; fi
done
CMD=""; PREV=""
for a in "$@"; do
  if [ "$PREV" = "pr" ]; then CMD="$a"; break; fi
  PREV="$a"
done
case "$CMD" in
  list) cat "$STATE"; exit 0 ;;
  edit)
    HAS_BODY=0
    for a in "$@"; do
      if [ "$a" = "--body-file" ]; then HAS_BODY=1; fi
    done
    if [ "$HAS_BODY" -eq 1 ]; then
      cat > "$BODY"
    fi
    exit 0
    ;;
  view)
    if echo "$*" | grep -q mergeable; then
      printf '{{"mergeable":"MERGEABLE"}}\n'
    else
      BODY_ESC=$(printf '%s' "$(cat "$BODY")" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null || sed 's/"/\\"/g' "$BODY" | tr '\n' ' ')
      printf '{{"number":7,"url":"https://example.com/pull/7","title":"porch: feat-composed","body":"%s"}}\n' "$BODY_ESC"
    fi
    exit 0
    ;;
  create)
    echo "fake-gh: create must not run on already-open PR" >&2
    exit 1
    ;;
  *)
    echo "fake-gh: unhandled: $*" >&2
    exit 1
    ;;
esac
"#,
                log = log_file.display(),
                body = body_file.display(),
                state = state_file.display(),
            ),
        )
        .unwrap();
        chmod_755(&fake_gh);

        {
            let mut slot = TEST_GH_BIN.lock().unwrap();
            *slot = Some(fake_gh.to_string_lossy().into_owned());
        }

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("r-composed", &seed, &bare_path, "main")
            .unwrap();
        let run = db
            .insert_run(
                "r-composed",
                "feat-composed",
                &head,
                Some("compose already"),
                None,
            )
            .unwrap();
        db.set_run_shas(&run.id, Some(&head), Some(&main_sha))
            .unwrap();
        db.set_trusted_config_sha(&run.id, &main_sha).unwrap();
        db.set_worktree_dir(&run.id, &wt).unwrap();
        db.set_pr_url(&run.id, Some("https://example.com/pull/7"))
            .unwrap();
        db.set_pr_title_written(&run.id, Some("porch: feat-composed"))
            .unwrap();
        // Simulate Task-5 compose resolve on this tip (only completed row needed;
        // same-second parked+completed can make latest_step_for_run non-deterministic).
        db.insert_step_result(&run.id, "compose", "completed", Some("compose=scaffold"))
            .unwrap();
        db.set_run_status(&run.id, "running", None).unwrap();

        let outcome = match run_deliver_phase(&db, &home, &run.id, &bare, &wt, "main", None) {
            Ok(o) => o,
            Err(e) => {
                *TEST_GH_BIN.lock().unwrap() = None;
                panic!("already-composed deliver: {e}");
            }
        };
        *TEST_GH_BIN.lock().unwrap() = None;
        assert_eq!(outcome, DeliverOutcome::Completed);

        let steps = db.step_results_for_run(&run.id).unwrap();
        let compose_parked = steps
            .iter()
            .filter(|s| s.step == "compose" && s.status == "parked")
            .count();
        assert_eq!(
            compose_parked, 0,
            "must not re-enter compose park: {steps:?}"
        );
        assert!(
            compose_already_resolved(&db, &run.id).unwrap(),
            "compose remains resolved"
        );

        let log = std::fs::read_to_string(&log_file).unwrap_or_default();
        assert!(log.contains("pr edit"), "expected body refresh edit: {log}");
        assert!(!log.contains("pr create"), "must not create: {log}");
        let body = std::fs::read_to_string(&body_file).unwrap();
        assert!(body.contains(MANAGED_BEGIN), "{body}");
        assert!(body.contains(MANAGED_END), "{body}");
        assert!(
            body.contains("## Summary") || body.contains("feat"),
            "{body}"
        );
    }
}
