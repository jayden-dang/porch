//! Deliver phase: lease-push exact SHA, `gh` PR, allowlisted check watch.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use porch_deliver::{
    Attestation, CheckRow, MergeableState, PrOpts, StepSnapshot, WatchChecksOpts, WatchOutcome,
    build_pr_body, check_poll_interval, check_timeout, create_pr, edit_pr_body, ensure_gh_runnable,
    find_open_pr, gh_bin, gh_timeout, pr_mergeable, pr_title, watch_allowlisted_checks,
};
use porch_gate::Db;
use porch_git::{
    GitDir, PushDecision, RemoteTip, ls_remote_sha, push_exact_sha, remote_commits_incorporated,
    resolve_push_decision,
};

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
            base_branch: &pr_base,
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

    if pr_number != 0 {
        match pr_mergeable(&bin, timeout, wt, pr_number) {
            Ok(MergeableState::Conflicting) => {
                return Err(DeliverError::MergeConflicting);
            }
            Ok(_) => {}
            // Missing view support in older fakes / transient view errors: continue to watch.
            Err(e) => {
                tracing::warn!(error = %e, "pr mergeable probe failed; continuing to watch");
            }
        }
    }

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

    let what = what_changed_summary(run, head_sha, wt);
    let review = review_summary(run);
    let certify = certify_summary(&steps);
    let pipeline = pipeline_summary(&steps);

    Ok(build_pr_body(
        run.intent.as_deref(),
        &what,
        "_not assessed_",
        &review,
        &certify,
        &pipeline,
        &Attestation {
            head_sha: head_sha.to_string(),
            steps: snapshots,
        },
    ))
}

fn what_changed_summary(run: &porch_gate::RunRow, head_sha: &str, wt: &Path) -> String {
    const CAP: usize = 40;
    match porch_git::diff_name_only(
        wt,
        &format!(
            "{}..{head_sha}",
            run.base_sha.as_deref().unwrap_or(head_sha)
        ),
    ) {
        Ok(files) if !files.is_empty() => {
            let mut lines = files;
            if lines.len() > CAP {
                let extra = lines.len() - CAP;
                lines.truncate(CAP);
                lines.push(format!("…and {extra} more"));
            }
            lines.join("\n")
        }
        _ => "_see commits_".into(),
    }
}

fn review_summary(run: &porch_gate::RunRow) -> String {
    let findings: Vec<serde_json::Value> = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let approved = run.review_approved_head_sha.as_deref();
    if findings.is_empty() {
        return match approved {
            Some(sha) => format!("approved at `{sha}` (clean)"),
            None => "n/a (skipped or not approved)".into(),
        };
    }
    let mut blocking = 0usize;
    let mut other = 0usize;
    let mut samples: Vec<String> = Vec::new();
    for f in &findings {
        let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("");
        let action = f.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let is_blocking = matches!(severity, "error" | "warning") || action == "ask-user";
        if is_blocking {
            blocking += 1;
        } else {
            other += 1;
        }
        if samples.len() < 5 {
            let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = f.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let short: String = msg.chars().take(80).collect();
            samples.push(format!("- `{id}` ({severity}) {path}: {short}"));
        }
    }
    let mut out = match approved {
        Some(sha) => {
            format!("approved at `{sha}` — {blocking} blocking, {other} other finding(s) recorded")
        }
        None => format!("{blocking} blocking, {other} other finding(s) (not approved)"),
    };
    if !samples.is_empty() {
        out.push('\n');
        out.push_str(&samples.join("\n"));
        if findings.len() > samples.len() {
            use std::fmt::Write as _;
            let _ = write!(out, "\n- …and {} more", findings.len() - samples.len());
        }
    }
    out
}

fn certify_summary(steps: &[porch_gate::StepResultRow]) -> String {
    let latest = steps.iter().rev().find(|s| s.step == "certify");
    match latest {
        Some(s) if s.status == "completed" => "completed".into(),
        Some(s) if s.status == "skipped" => {
            format!(
                "skipped{}",
                s.error
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            )
        }
        Some(s) if s.status == "failed" => {
            format!(
                "failed{}",
                s.error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            )
        }
        Some(s) => format!(
            "{}{}",
            s.status,
            s.error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ),
        None => "n/a".into(),
    }
}

fn pipeline_summary(steps: &[porch_gate::StepResultRow]) -> String {
    const ORDER: &[&str] = &["intent", "rebase", "review", "certify", "deliver"];
    let mut parts = Vec::new();
    for name in ORDER {
        let latest = steps.iter().rev().find(|s| s.step == *name);
        match latest {
            Some(s) => parts.push(format!("{name}:{status}", status = s.status)),
            None => parts.push(format!("{name}:-")),
        }
    }
    parts.join(" → ")
}

#[cfg(test)]
mod body_tests {
    use super::{certify_summary, pipeline_summary, review_summary};
    use porch_gate::{RunRow, StepResultRow};

    fn run_with_findings(json: &str, approved: Option<&str>) -> RunRow {
        RunRow {
            id: "r1".into(),
            repo_id: "repo".into(),
            branch: "feat".into(),
            sha: "abc".into(),
            status: "completed".into(),
            worktree_dir: None,
            head_sha: Some("abc".into()),
            base_sha: Some("def".into()),
            intent: Some("ship it".into()),
            intent_source: Some("cli".into()),
            error: None,
            review_approved_head_sha: approved.map(str::to_string),
            findings_json: Some(json.into()),
            fixer_session_id: None,
            pr_url: None,
            deliver_repair_attempts: 0,
            trusted_config_sha: None,
        }
    }

    #[test]
    fn review_summary_lists_findings_when_approved() {
        let run = run_with_findings(
            r#"[{"id":"f0","path":"a.rs","message":"null deref","severity":"warning","action":"ask-user"}]"#,
            Some("deadbeef"),
        );
        let s = review_summary(&run);
        assert!(s.contains("approved at `deadbeef`"), "{s}");
        assert!(s.contains("1 blocking"), "{s}");
        assert!(s.contains("`f0`"), "{s}");
        assert!(s.contains("null deref"), "{s}");
    }

    #[test]
    fn review_summary_clean_when_no_findings() {
        let run = run_with_findings("[]", Some("abc"));
        let s = review_summary(&run);
        assert!(s.contains("clean"), "{s}");
    }

    #[test]
    fn certify_and_pipeline_from_steps() {
        let steps = vec![
            StepResultRow {
                id: "1".into(),
                run_id: "r".into(),
                step: "intent".into(),
                status: "skipped".into(),
                error: Some("no intent".into()),
            },
            StepResultRow {
                id: "2".into(),
                run_id: "r".into(),
                step: "rebase".into(),
                status: "completed".into(),
                error: None,
            },
            StepResultRow {
                id: "3".into(),
                run_id: "r".into(),
                step: "review".into(),
                status: "completed".into(),
                error: None,
            },
            StepResultRow {
                id: "4".into(),
                run_id: "r".into(),
                step: "certify".into(),
                status: "completed".into(),
                error: None,
            },
            StepResultRow {
                id: "5".into(),
                run_id: "r".into(),
                step: "deliver".into(),
                status: "completed".into(),
                error: None,
            },
        ];
        assert_eq!(certify_summary(&steps), "completed");
        let pipe = pipeline_summary(&steps);
        assert!(pipe.contains("intent:skipped"), "{pipe}");
        assert!(pipe.contains("certify:completed"), "{pipe}");
        assert!(pipe.contains("deliver:completed"), "{pipe}");
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
}
