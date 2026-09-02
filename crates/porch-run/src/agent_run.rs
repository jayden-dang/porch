//! `porch agent run`: wait/poll until park or terminal (D11 JSON / JSONL).

use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use porch_gate::{Db, RunSnapshot, db_path, get_run, list_runs, repo_id_for};
use serde::Serialize;

use crate::{AgentCliResult, status_exit};

/// Options for [`agent_run`].
#[derive(Debug, Clone, Copy)]
pub struct AgentRunOpts<'a> {
    pub home: &'a Path,
    pub work_tree: &'a Path,
    /// Attach to this run; otherwise active branch run or push-then-attach.
    pub run_id: Option<&'a str>,
    /// Stream JSONL until parked or terminal.
    pub wait: bool,
    /// Max seconds to wait when `wait` is set (`None` = no limit).
    pub timeout_secs: Option<u64>,
    /// Authoritative intent for a fresh push (empty skips; E17).
    pub intent: Option<&'a str>,
}

const TERMINAL_OR_PARK: &[&str] = &["parked", "completed", "failed", "cancelled"];
const ACTIVE: &[&str] = &["pending", "running", "parked"];

/// Drive or attach to a gate run.
///
/// Without `--wait`, prints one pretty JSON snapshot (via [`AgentCliResult::json`]).
/// With `--wait`, streams JSONL snapshots until park or terminal and sets
/// [`AgentCliResult::already_emitted`]. Never merges; never babysits deploy.
///
/// Callers must ensure the daemon is up.
#[must_use]
pub fn agent_run(opts: AgentRunOpts<'_>) -> AgentCliResult {
    match agent_run_inner(opts) {
        Ok(out) => out,
        Err(RunErr::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(RunErr::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        },
    }
}

enum RunErr {
    Usage(String),
    Fail(String),
}

fn agent_run_inner(opts: AgentRunOpts<'_>) -> Result<AgentCliResult, RunErr> {
    if opts.timeout_secs.is_some() && !opts.wait {
        return Err(RunErr::Usage("--timeout requires --wait".into()));
    }

    let work = opts
        .work_tree
        .canonicalize()
        .map_err(|e| RunErr::Fail(e.to_string()))?;
    let repo_id = repo_id_for(&work);
    let branch = current_branch(&work)?;

    let run_id = if let Some(id) = opts.run_id {
        if opts.intent.is_some_and(|s| !s.trim().is_empty()) {
            return Err(RunErr::Usage(
                "--intent is only valid when starting a push (omit --run-id)".into(),
            ));
        }
        id.to_string()
    } else {
        resolve_or_push(&opts, &work, &repo_id, &branch)?
    };

    let snap = get_run(opts.home, &run_id).map_err(|e| RunErr::Fail(e.to_string()))?;
    if !opts.wait {
        return Ok(snapshot_result(&snap, false));
    }

    let timeout = opts.timeout_secs.map(Duration::from_secs);
    let start = Instant::now();
    let mut last_rev = snap.state_rev;
    let mut last_status = snap.status.clone();
    emit_jsonl(&snap)?;

    if is_stop_status(&snap.status) {
        return Ok(AgentCliResult {
            exit_code: status_exit(&snap.status),
            json: String::new(),
            already_emitted: true,
        });
    }

    loop {
        if let Some(limit) = timeout {
            if start.elapsed() > limit {
                let snap = get_run(opts.home, &run_id).map_err(|e| RunErr::Fail(e.to_string()))?;
                emit_jsonl(&snap)?;
                let err = serde_json::json!({
                    "error": format!(
                        "timed out after {}s waiting for run {run_id}",
                        limit.as_secs()
                    ),
                    "run_id": run_id,
                    "status": snap.status,
                });
                println_line(&err.to_string())?;
                return Ok(AgentCliResult {
                    exit_code: 1,
                    json: String::new(),
                    already_emitted: true,
                });
            }
        }
        thread::sleep(Duration::from_millis(200));
        let snap = get_run(opts.home, &run_id).map_err(|e| RunErr::Fail(e.to_string()))?;
        if snap.state_rev != last_rev || snap.status != last_status {
            emit_jsonl(&snap)?;
            last_rev = snap.state_rev;
            last_status.clone_from(&snap.status);
        }
        if is_stop_status(&snap.status) {
            return Ok(AgentCliResult {
                exit_code: status_exit(&snap.status),
                json: String::new(),
                already_emitted: true,
            });
        }
    }
}

fn resolve_or_push(
    opts: &AgentRunOpts<'_>,
    work: &Path,
    repo_id: &str,
    branch: &str,
) -> Result<String, RunErr> {
    if let Some(id) = find_active_run(opts.home, repo_id, branch)? {
        if opts.intent.is_some() {
            let _ = writeln!(
                io::stderr(),
                "porch: warning: --intent ignored when attaching to an existing run (no push)"
            );
        }
        return Ok(id);
    }
    let before_id = latest_run_id(opts.home, repo_id, branch)?;
    push_porch(work, opts.home, branch, opts.intent)?;
    wait_for_new_run(
        opts.home,
        repo_id,
        branch,
        before_id.as_deref(),
        Duration::from_secs(30),
    )
}

fn find_active_run(home: &Path, repo_id: &str, branch: &str) -> Result<Option<String>, RunErr> {
    let runs = list_runs(home, Some(repo_id), Some(20)).map_err(|e| RunErr::Fail(e.to_string()))?;
    Ok(runs.into_iter().find_map(|r| {
        let status = r.get("status")?.as_str()?;
        let b = r.get("branch")?.as_str()?;
        if b == branch && ACTIVE.contains(&status) {
            r.get("id")?.as_str().map(str::to_string)
        } else {
            None
        }
    }))
}

fn latest_run_id(home: &Path, repo_id: &str, branch: &str) -> Result<Option<String>, RunErr> {
    let db = Db::open(&db_path(home)).map_err(|e| RunErr::Fail(e.to_string()))?;
    Ok(db
        .latest_run_for_branch(repo_id, branch)
        .map_err(|e| RunErr::Fail(e.to_string()))?
        .map(|r| r.id))
}

fn wait_for_new_run(
    home: &Path,
    repo_id: &str,
    branch: &str,
    before_id: Option<&str>,
    timeout: Duration,
) -> Result<String, RunErr> {
    let start = Instant::now();
    let db = Db::open(&db_path(home)).map_err(|e| RunErr::Fail(e.to_string()))?;
    loop {
        // Prefer a ULID newer than the pre-push snapshot (ids are time-sortable),
        // not merely "any latest on branch" — that would re-attach a completed prior run.
        if let Ok(runs) = db.runs_for_repo(repo_id) {
            if let Some(run) = runs.into_iter().rev().find(|r| {
                r.branch == branch
                    && match before_id {
                        None => true,
                        Some(old) => r.id.as_str() > old,
                    }
            }) {
                // notify-push already requested start_run; do not start again (double execute).
                return Ok(run.id);
            }
        }
        if start.elapsed() > timeout {
            return Err(RunErr::Fail(format!(
                "no run appeared for {branch} after push"
            )));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn push_porch(work: &Path, home: &Path, branch: &str, intent: Option<&str>) -> Result<(), RunErr> {
    let mut cmd = Command::new("git");
    cmd.current_dir(work).env("PORCH_HOME", home).args([
        "push",
        "porch",
        &format!("HEAD:refs/heads/{branch}"),
    ]);
    match intent.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => {
            cmd.env("PORCH_INTENT", text);
        }
        None => {
            // Explicit empty `--intent` must not inherit a stale PORCH_INTENT.
            if intent.is_some() {
                cmd.env_remove("PORCH_INTENT");
            }
        }
    }
    let out = cmd
        .output()
        .map_err(|e| RunErr::Fail(format!("git push: {e}")))?;
    if !out.status.success() {
        return Err(RunErr::Fail(format!(
            "git push porch failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn current_branch(work: &Path) -> Result<String, RunErr> {
    let out = porch_git::run_c(work, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map_err(|e| RunErr::Fail(e.to_string()))?;
    let branch = porch_git::stdout_trim(&out);
    if branch.is_empty() || branch == "HEAD" {
        return Err(RunErr::Usage(
            "detached HEAD — checkout a branch or pass --run-id".into(),
        ));
    }
    Ok(branch)
}

fn is_stop_status(status: &str) -> bool {
    TERMINAL_OR_PARK.contains(&status)
}

fn snapshot_result(snap: &RunSnapshot, already_emitted: bool) -> AgentCliResult {
    AgentCliResult {
        exit_code: status_exit(&snap.status),
        json: serde_json::to_string_pretty(&agent_status_from_snap(snap))
            .unwrap_or_else(|_| "{}".into()),
        already_emitted,
    }
}

fn agent_status_from_snap(snap: &RunSnapshot) -> AgentRunSnapshot {
    let findings = match &snap.findings {
        serde_json::Value::Array(_) => snap.findings.clone(),
        _ => serde_json::json!([]),
    };
    let phase = match snap.status.as_str() {
        "parked" => snap
            .steps
            .iter()
            .rev()
            .find(|s| {
                s.status == "parked"
                    || s.error.as_deref().is_some_and(|e| e.contains("park"))
                    || s.step == "review"
                    || s.step == "rebase"
            })
            .map(|s| s.step.clone())
            .or_else(|| snap.steps.iter().rev().map(|s| s.step.clone()).next())
            .unwrap_or_else(|| "review".into()),
        "completed" | "failed" | "cancelled" => "done".into(),
        "running" | "pending" => "pipeline".into(),
        other => other.to_string(),
    };
    AgentRunSnapshot {
        run_id: snap.run_id.clone(),
        repo_id: snap.repo_id.clone(),
        branch: snap.branch.clone(),
        status: snap.status.clone(),
        phase,
        head_sha: snap.head_sha.clone(),
        base_sha: snap.base_sha.clone(),
        review_approved_head_sha: snap.review_approved_head_sha.clone(),
        findings,
        assurance_record: snap.assurance_record.clone(),
        error: snap.error.clone(),
        pr_url: snap.pr_url.clone(),
        steps: snap
            .steps
            .iter()
            .map(|s| AgentStep {
                step: s.step.clone(),
                status: s.status.clone(),
                error: s.error.clone(),
            })
            .collect(),
        state_rev: snap.state_rev,
    }
}

#[derive(Debug, Serialize)]
struct AgentRunSnapshot {
    run_id: String,
    repo_id: String,
    branch: String,
    status: String,
    phase: String,
    head_sha: Option<String>,
    base_sha: Option<String>,
    review_approved_head_sha: Option<String>,
    findings: serde_json::Value,
    assurance_record: porch_gate::AssuranceRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_url: Option<String>,
    steps: Vec<AgentStep>,
    state_rev: u64,
}

#[derive(Debug, Serialize)]
struct AgentStep {
    step: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn emit_jsonl(snap: &RunSnapshot) -> Result<(), RunErr> {
    let line = serde_json::to_string(&agent_status_from_snap(snap))
        .map_err(|e| RunErr::Fail(e.to_string()))?;
    println_line(&line)
}

fn println_line(s: &str) -> Result<(), RunErr> {
    let mut out = io::stdout();
    writeln!(out, "{s}").map_err(|e| RunErr::Fail(e.to_string()))?;
    out.flush().map_err(|e| RunErr::Fail(e.to_string()))?;
    Ok(())
}
