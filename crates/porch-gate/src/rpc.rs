//! JSON-RPC client helpers over the daemon Unix socket.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::{Db, RunRow, StepResultRow};
use crate::events::{Event, EventHub};
use crate::home::socket_path;

/// Soft cap for on-demand finding hunk / diff payloads (bytes).
pub const FINDING_HUNK_MAX_BYTES: usize = 16_384;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub result: serde_json::Value,
    pub id: u64,
}

/// Full run snapshot returned by `get_run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub run_id: String,
    pub repo_id: String,
    pub branch: String,
    pub status: String,
    pub sha: String,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub review_approved_head_sha: Option<String>,
    pub error: Option<String>,
    pub pr_url: Option<String>,
    pub worktree_dir: Option<String>,
    pub findings: serde_json::Value,
    pub steps: Vec<StepSnapshot>,
    pub state_rev: u64,
}

/// One `step_results` row in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSnapshot {
    pub step: String,
    pub status: String,
    pub error: Option<String>,
}

/// Compact run row for `list_runs`.
#[must_use]
pub fn compact_run_row(run: &RunRow) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "repo_id": run.repo_id,
        "branch": run.branch,
        "status": run.status,
        "sha": run.sha,
        "head_sha": run.head_sha,
        "error": run.error,
        "pr_url": run.pr_url,
    })
}

/// Build a [`RunSnapshot`] from DB rows + hub revision.
#[must_use]
pub fn build_run_snapshot(run: &RunRow, steps: &[StepResultRow], state_rev: u64) -> RunSnapshot {
    let findings = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!([]));
    RunSnapshot {
        run_id: run.id.clone(),
        repo_id: run.repo_id.clone(),
        branch: run.branch.clone(),
        status: run.status.clone(),
        sha: run.sha.clone(),
        head_sha: run.head_sha.clone(),
        base_sha: run.base_sha.clone(),
        review_approved_head_sha: run.review_approved_head_sha.clone(),
        error: run.error.clone(),
        pr_url: run.pr_url.clone(),
        worktree_dir: run
            .worktree_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        findings,
        steps: steps
            .iter()
            .map(|s| StepSnapshot {
                step: s.step.clone(),
                status: s.status.clone(),
                error: s.error.clone(),
            })
            .collect(),
        state_rev,
    }
}

fn rpc_call(home: &Path, method: &str, params: Option<serde_json::Value>) -> Result<Response> {
    let mut stream = UnixStream::connect(socket_path(home))?;
    let req = Request {
        jsonrpc: "2.0".into(),
        method: method.into(),
        id: 1,
        params,
    };
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&req).map_err(|e| crate::Error::Other(e.to_string()))?
    )?;
    let mut buf = String::new();
    BufReader::new(&mut stream).read_line(&mut buf)?;
    serde_json::from_str(buf.trim()).map_err(|e| crate::Error::Other(e.to_string()))
}

/// Probe the daemon health RPC over the Unix socket.
///
/// # Errors
///
/// Returns an error when the socket cannot be connected, written, read, or
/// the response is not valid JSON-RPC health payload.
pub fn health_check(home: &Path) -> Result<bool> {
    let resp = rpc_call(home, "health", None)?;
    Ok(resp
        .result
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

/// Ask the daemon to start (or queue) a run.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached or the response is invalid.
pub fn start_run(home: &Path, run_id: &str) -> Result<()> {
    let resp = rpc_call(
        home,
        "start_run",
        Some(serde_json::json!({"run_id": run_id})),
    )?;
    if resp.result.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        let err = resp
            .result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("start_run failed");
        Err(crate::Error::Other(err.into()))
    }
}

/// List recent runs via daemon RPC.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached or the response is invalid.
pub fn list_runs(
    home: &Path,
    repo_id: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>> {
    let mut params = serde_json::Map::new();
    if let Some(repo_id) = repo_id {
        params.insert("repo_id".into(), serde_json::Value::String(repo_id.into()));
    }
    if let Some(limit) = limit {
        params.insert("limit".into(), serde_json::json!(limit));
    }
    let resp = rpc_call(home, "list_runs", Some(serde_json::Value::Object(params)))?;
    let runs = resp
        .result
        .get("runs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(runs)
}

/// Fetch a full run snapshot via daemon RPC.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached, the run is missing, or
/// the response is invalid.
pub fn get_run(home: &Path, run_id: &str) -> Result<RunSnapshot> {
    let resp = rpc_call(home, "get_run", Some(serde_json::json!({"run_id": run_id})))?;
    // RPC failures are `{"error":…}` without `run_id`. A real snapshot may include
    // `error` for a failed/cancelled run — that is not an RPC failure.
    if resp.result.get("run_id").and_then(|v| v.as_str()).is_none() {
        if let Some(err) = resp.result.get("error").and_then(|v| v.as_str()) {
            return Err(crate::Error::Other(err.into()));
        }
        return Err(crate::Error::Other("get_run: invalid response".into()));
    }
    serde_json::from_value(resp.result).map_err(|e| crate::Error::Other(e.to_string()))
}

/// Open a subscribe stream. Calls `on_event` for each NDJSON event until the
/// callback returns `false` or the peer hangs up.
///
/// The first RPC response is the subscribe ack; subsequent lines are events.
///
/// # Errors
///
/// Returns an error if the socket or subscribe ack fails.
pub fn subscribe_events<F>(home: &Path, run_id: Option<&str>, mut on_event: F) -> Result<()>
where
    F: FnMut(Event) -> bool,
{
    let mut stream = UnixStream::connect(socket_path(home))?;
    let mut params = serde_json::Map::new();
    if let Some(run_id) = run_id {
        params.insert("run_id".into(), serde_json::Value::String(run_id.into()));
    }
    let req = Request {
        jsonrpc: "2.0".into(),
        method: "subscribe".into(),
        id: 1,
        params: Some(serde_json::Value::Object(params)),
    };
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&req).map_err(|e| crate::Error::Other(e.to_string()))?
    )?;
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    if reader.read_line(&mut buf)? == 0 {
        return Err(crate::Error::Other("subscribe: empty ack".into()));
    }
    let resp: Response =
        serde_json::from_str(buf.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;
    if resp
        .result
        .get("subscribed")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        let err = resp
            .result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("subscribe failed");
        return Err(crate::Error::Other(err.into()));
    }
    loop {
        buf.clear();
        if reader.read_line(&mut buf)? == 0 {
            break;
        }
        let line = buf.trim();
        if line.is_empty() {
            continue;
        }
        let ev: Event =
            serde_json::from_str(line).map_err(|e| crate::Error::Other(e.to_string()))?;
        if !on_event(ev) {
            break;
        }
    }
    Ok(())
}

/// Server-side: build `list_runs` result from DB.
pub(crate) fn list_runs_result(
    db: &Db,
    repo_id: Option<&str>,
    limit: usize,
) -> Result<serde_json::Value> {
    let runs = db.recent_runs(repo_id, limit)?;
    let rows: Vec<serde_json::Value> = runs.iter().map(compact_run_row).collect();
    Ok(serde_json::json!({ "runs": rows }))
}

/// Server-side: build `get_run` result from DB + hub.
pub(crate) fn get_run_result(db: &Db, hub: &EventHub, run_id: &str) -> Result<serde_json::Value> {
    let Some(run) = db.run_by_id(run_id)? else {
        return Ok(serde_json::json!({"error": format!("unknown run {run_id}")}));
    };
    let steps = db.step_results_for_run(run_id)?;
    let snap = build_run_snapshot(&run, &steps, hub.state_rev());
    serde_json::to_value(snap).map_err(|e| crate::Error::Other(e.to_string()))
}

/// Fetch a capped file snippet / diff for one finding via daemon RPC.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached or the response is invalid.
pub fn get_finding_hunk(home: &Path, run_id: &str, finding_id: &str) -> Result<serde_json::Value> {
    let resp = rpc_call(
        home,
        "get_finding_hunk",
        Some(serde_json::json!({
            "run_id": run_id,
            "finding_id": finding_id,
        })),
    )?;
    Ok(resp.result)
}

/// Server-side: build a capped hunk/diff for one finding from the run worktree.
pub(crate) fn get_finding_hunk_result(
    db: &Db,
    run_id: &str,
    finding_id: &str,
) -> Result<serde_json::Value> {
    let Some(run) = db.run_by_id(run_id)? else {
        return Ok(serde_json::json!({"error": format!("unknown run {run_id}")}));
    };
    let findings = run
        .findings_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!([]));
    let Some(arr) = findings.as_array() else {
        return Ok(serde_json::json!({"error": "findings are not an array"}));
    };
    let Some(finding) = arr
        .iter()
        .find(|v| v.get("id").and_then(|x| x.as_str()) == Some(finding_id))
    else {
        return Ok(serde_json::json!({
            "error": format!("unknown finding {finding_id}")
        }));
    };
    let path = finding
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if path.is_empty() {
        return Ok(serde_json::json!({"error": "finding has empty path"}));
    }
    let start_line = finding
        .get("start_line")
        .and_then(serde_json::Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let end_line = finding
        .get("end_line")
        .and_then(serde_json::Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));

    let Some(wt) = run.worktree_dir.as_ref() else {
        return Ok(serde_json::json!({"error": "run has no worktree_dir"}));
    };
    if !wt.is_absolute() || !wt.is_dir() {
        return Ok(serde_json::json!({"error": "worktree missing or not absolute"}));
    }

    let file_path = match safe_worktree_file(wt, &path) {
        Ok(p) => p,
        Err(msg) => return Ok(serde_json::json!({"error": msg})),
    };

    let (hunk, source, truncated) = if file_path.is_file() {
        match file_snippet(&file_path, start_line, end_line, FINDING_HUNK_MAX_BYTES) {
            Ok(v) => v,
            Err(e) => return Ok(serde_json::json!({"error": e})),
        }
    } else if let Some(base) = run.base_sha.as_deref() {
        match path_diff_snippet(wt, base, &path, FINDING_HUNK_MAX_BYTES) {
            Ok(v) => v,
            Err(e) => return Ok(serde_json::json!({"error": e})),
        }
    } else {
        return Ok(serde_json::json!({
            "error": format!("path not found in worktree: {path}")
        }));
    };

    Ok(serde_json::json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "path": path,
        "start_line": start_line,
        "end_line": end_line,
        "source": source,
        "hunk": hunk,
        "truncated": truncated,
    }))
}

fn safe_worktree_file(work_tree: &Path, rel: &str) -> std::result::Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err("finding path must be relative".into());
    }
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("finding path escapes worktree".into());
    }
    Ok(work_tree.join(rel_path))
}

fn file_snippet(
    path: &Path,
    start_line: Option<u32>,
    end_line: Option<u32>,
    max_bytes: usize,
) -> std::result::Result<(String, &'static str, bool), String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = raw.lines().collect();
    if lines.is_empty() {
        return Ok((String::new(), "file", false));
    }
    let last = lines.len();
    let (mut from, mut to) = match (start_line, end_line) {
        (Some(s), Some(e)) => {
            let s = usize::try_from(s.max(1)).unwrap_or(1);
            let e = usize::try_from(e.max(1)).unwrap_or(1).max(s);
            (s.saturating_sub(3).max(1), e.saturating_add(3))
        }
        (Some(s), None) => {
            let s = usize::try_from(s.max(1)).unwrap_or(1);
            (s.saturating_sub(3).max(1), s.saturating_add(3))
        }
        _ => (1usize, last.min(80)),
    };
    if from > last {
        from = last.saturating_sub(19).max(1);
        to = last;
    } else {
        to = to.min(last).max(from);
    }
    let mut out = String::new();
    let mut truncated = false;
    for (idx, line) in lines.iter().enumerate().skip(from - 1).take(to - from + 1) {
        let numbered = format!("{:>4}|{}\n", idx + 1, line);
        if out.len() + numbered.len() > max_bytes {
            truncated = true;
            break;
        }
        out.push_str(&numbered);
    }
    if out.len() > max_bytes {
        out.truncate(max_bytes);
        truncated = true;
    }
    Ok((out, "file", truncated))
}

fn path_diff_snippet(
    work_tree: &Path,
    base_sha: &str,
    path: &str,
    max_bytes: usize,
) -> std::result::Result<(String, &'static str, bool), String> {
    let range = format!("{base_sha}..HEAD");
    let out =
        porch_git::run_c(work_tree, &["diff", &range, "--", path]).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut truncated = false;
    if text.len() > max_bytes {
        text.truncate(max_bytes);
        truncated = true;
    }
    Ok((text, "diff", truncated))
}
