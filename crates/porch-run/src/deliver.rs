//! Deliver phase: lease-push exact SHA, `gh` PR, allowlisted check watch.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use porch_deliver::{
    Attestation, PrOpts, StepSnapshot, WatchChecksOpts, build_pr_body, check_poll_interval,
    check_timeout, create_pr, edit_pr_body, ensure_gh_runnable, find_open_pr, gh_bin, gh_timeout,
    pr_title, watch_allowlisted_checks,
};
use porch_gate::Db;
use porch_git::{
    GitDir, PushDecision, RemoteTip, ls_remote_sha, push_exact_sha, remote_commits_incorporated,
    resolve_push_decision,
};

use crate::config::{DeliverGithub, parse_porch_config};

const TRUSTED_CONFIG_PATH: &str = ".porch.yaml";

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeliverError {
    #[error(transparent)]
    Gate(#[from] porch_gate::Error),
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Deliver(#[from] porch_deliver::Error),
    #[error("{0}")]
    Msg(String),
}

/// Continuity must already hold. Order: ensure `gh` → lease-push → PR → watch.
///
/// # Errors
///
/// Fail closed on missing continuity facts, unverifiable remote, incorporate
/// refuse, `gh` missing, PR listing undecodable, or allowlisted check red/timeout.
pub(crate) fn run_deliver_phase(
    db: &Db,
    run_id: &str,
    bare: &GitDir,
    wt: &Path,
    default_branch: &str,
    cancel: Option<&AtomicBool>,
) -> Result<(), DeliverError> {
    if cancelled(cancel) {
        return Err(DeliverError::Msg("cancelled".into()));
    }

    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;

    let head_sha = porch_git::rev_parse_c(wt, "HEAD")?;
    db.set_run_shas(run_id, Some(&head_sha), None)?;

    let bin = gh_bin();
    // Prefer fail before push when gh cannot run (no branch without PR adapter).
    ensure_gh_runnable(&bin)?;

    let deliver_cfg = load_trusted_deliver(db, run_id, bare)?;
    // M6: never raise rerun budget; refuse if trusted yaml somehow requested it
    // while we do not implement rerun.
    if deliver_cfg.rerun_transient > 0 {
        tracing::warn!(
            rerun_transient = deliver_cfg.rerun_transient,
            "trusted rerun_transient > 0 ignored in M6 (no gh run rerun)"
        );
    }

    let refname = format!("refs/heads/{}", run.branch);
    lease_push_exact(bare, &refname, &head_sha, run.base_sha.as_deref())?;

    if cancelled(cancel) {
        return Err(DeliverError::Msg("cancelled".into()));
    }

    let timeout = gh_timeout();
    let title = pr_title(&run.branch);
    let body = assemble_body(db, &run, &head_sha, wt)?;

    let (pr_url, pr_number) = if let Some(existing) = find_open_pr(&bin, timeout, wt, &run.branch)?
    {
        edit_pr_body(&bin, timeout, wt, existing.number, &body)?;
        if existing.url.is_empty() {
            return Err(DeliverError::Msg("existing PR has empty url".into()));
        }
        (existing.url, existing.number)
    } else {
        let url = create_pr(&PrOpts {
            bin: &bin,
            timeout,
            work_tree: wt,
            head_branch: &run.branch,
            base_branch: default_branch,
            title: &title,
            body: &body,
        })?;
        let number = if deliver_cfg.watch_checks.is_empty() {
            0
        } else {
            find_open_pr(&bin, timeout, wt, &run.branch)?
                .map(|p| p.number)
                .ok_or_else(|| {
                    DeliverError::Msg("pr create succeeded but open PR listing is empty".into())
                })?
        };
        (url, number)
    };
    db.set_pr_url(run_id, Some(&pr_url))?;
    maybe_watch(
        &bin,
        timeout,
        wt,
        pr_number,
        &deliver_cfg.watch_checks,
        cancel,
    )?;
    Ok(())
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
    watch_allowlisted_checks(&WatchChecksOpts {
        bin,
        gh_timeout,
        work_tree: wt,
        pr_number,
        allowlist,
        poll_deadline: check_timeout(),
        poll_interval: check_poll_interval(),
        cancel,
    })?;
    Ok(())
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
) -> Result<DeliverGithub, DeliverError> {
    let run = db
        .run_by_id(run_id)?
        .ok_or_else(|| DeliverError::Msg(format!("unknown run {run_id}")))?;
    let trusted_sha = run
        .base_sha
        .as_deref()
        .ok_or_else(|| DeliverError::Msg("deliver requires base_sha".into()))?;

    match porch_git::show_path_at(bare, trusted_sha, TRUSTED_CONFIG_PATH)? {
        None => Ok(DeliverGithub::default()),
        Some(bytes) => Ok(parse_porch_config(&bytes)
            .map_err(DeliverError::Msg)?
            .deliver_github),
    }
}

fn assemble_body(
    db: &Db,
    run: &porch_gate::RunRow,
    head_sha: &str,
    wt: &Path,
) -> Result<String, DeliverError> {
    let steps = db.step_results_for_run(&run.id)?;
    let snapshots: Vec<StepSnapshot> = steps
        .iter()
        .map(|s| StepSnapshot {
            step: s.step.clone(),
            status: s.status.clone(),
        })
        .collect();

    let what = match porch_git::diff_name_only(
        wt,
        &format!(
            "{}..{head_sha}",
            run.base_sha.as_deref().unwrap_or(head_sha)
        ),
    ) {
        Ok(files) if !files.is_empty() => files.join("\n"),
        _ => "_see commits_".into(),
    };

    let review = if run.review_approved_head_sha.is_some() {
        format!(
            "approved at `{}`",
            run.review_approved_head_sha.as_deref().unwrap_or("")
        )
    } else {
        "n/a".into()
    };

    Ok(build_pr_body(
        run.intent.as_deref(),
        &what,
        "_not assessed_",
        &review,
        "completed",
        "intent → rebase → review → certify → deliver",
        &Attestation {
            head_sha: head_sha.to_string(),
            steps: snapshots,
        },
    ))
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
}
