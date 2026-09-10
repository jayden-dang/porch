//! Daemon-free inspect join (`LOOK`).
//!
//! `porch status` and `porch runs` print this. None of it starts a daemon, and
//! the database arm uses [`Db::open_read`] so a look cannot migrate or
//! terminalize.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::condition::daemon_condition;
use crate::db::Db;
use crate::eject::resolve_repo_id;
use crate::home::{db_path, repos_dir, worktrees_dir};
use crate::rounds::forward::{self, ForwardRecordRow};
use crate::rounds::reconcile::{self, Verdict};
use crate::rpc::compact_run_row;

/// What inspect could conclude about the state database.
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseArm {
    pub readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One `refs/porch/recover/<run_id>` tip.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryTip {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// A leftover disposable worktree HEAD (declined pin, `ESCAPE-1.3`).
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeTip {
    pub path: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// One deliver-attempt forward projection.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardView {
    pub run_id: String,
    pub deliver_attempt_id: String,
    pub source: String,
    pub verdict: String,
    pub evidence: String,
    pub ref_name: String,
    pub authorized_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<String>,
}

/// The inspect document `porch status` prints.
#[derive(Debug, Clone, Serialize)]
pub struct LookReport {
    pub daemon_healthy: bool,
    pub condition: String,
    pub porch_home: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_reason: Option<String>,
    pub database: DatabaseArm,
    pub recovery_tips: Vec<RecoveryTip>,
    pub worktree_tips: Vec<WorktreeTip>,
    pub forwards: Vec<ForwardView>,
    pub runs: Vec<serde_json::Value>,
}

/// Join daemon condition, git custody tips, and forward facts.
///
/// `work` is the operator checkout when inspect is repo-scoped. `run_limit`
/// caps the `runs` list (newest first).
#[must_use]
pub fn look(home: &Path, work: Option<&Path>, run_limit: usize) -> LookReport {
    let cond = daemon_condition(home);
    let mut report = LookReport {
        daemon_healthy: cond.is_ready(),
        condition: cond.label().to_string(),
        porch_home: home.to_path_buf(),
        repo_id: None,
        repo_reason: None,
        database: DatabaseArm {
            readable: false,
            reason: Some("not opened".into()),
        },
        recovery_tips: Vec::new(),
        worktree_tips: Vec::new(),
        forwards: Vec::new(),
        runs: Vec::new(),
    };

    let Some(work) = work else {
        report.database.reason = Some("no checkout in scope".into());
        return report;
    };

    match resolve_repo_id(work) {
        Ok(id) => report.repo_id = Some(id),
        Err(e) => {
            report.repo_reason = Some(e.to_string());
            fill_database_arm(home, &mut report);
            return report;
        }
    }

    let Some(repo_id) = report.repo_id.clone() else {
        return report;
    };
    let bare_path = repos_dir(home).join(format!("{repo_id}.git"));
    report.recovery_tips = recovery_tips(&bare_path);
    report.worktree_tips = worktree_tips(home, &repo_id);

    fill_database_arm(home, &mut report);
    if !report.database.readable {
        return report;
    }

    let db = match Db::open_read(&db_path(home)) {
        Ok(db) => db,
        Err(e) => {
            report.database = DatabaseArm {
                readable: false,
                reason: Some(e.to_string()),
            };
            return report;
        }
    };

    match db.recent_runs(Some(&repo_id), run_limit.max(1)) {
        Ok(rows) => {
            let take = if run_limit == 0 { 0 } else { run_limit };
            for run in rows.iter().take(take) {
                let mut row = compact_run_row(run);
                match project_run_forwards(&db, &bare_path, &run.id) {
                    Ok(fwds) => {
                        if let Some(first) = fwds.first() {
                            row["forward"] =
                                serde_json::to_value(first).unwrap_or(serde_json::Value::Null);
                        }
                        report.forwards.extend(fwds);
                    }
                    Err(e) => {
                        row["forward_error"] = serde_json::Value::String(e);
                    }
                }
                report.runs.push(row);
            }
        }
        Err(e) => {
            report.database = DatabaseArm {
                readable: false,
                reason: Some(e.to_string()),
            };
        }
    }

    report
}

fn fill_database_arm(home: &Path, report: &mut LookReport) {
    match Db::open_read(&db_path(home)) {
        Ok(_) => {
            report.database = DatabaseArm {
                readable: true,
                reason: None,
            };
        }
        Err(e) => {
            report.database = DatabaseArm {
                readable: false,
                reason: Some(e.to_string()),
            };
        }
    }
}

fn recovery_tips(bare_path: &Path) -> Vec<RecoveryTip> {
    let Ok(git_dir) = porch_git::GitDir::new(bare_path) else {
        return Vec::new();
    };
    let Ok(rows) = porch_git::for_each_ref(&git_dir, "refs/porch/recover") else {
        return Vec::new();
    };
    rows.into_iter()
        .map(|(sha, ref_name)| {
            let run_id = ref_name
                .strip_prefix("refs/porch/recover/")
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            RecoveryTip {
                ref_name,
                sha,
                run_id,
            }
        })
        .collect()
}

fn worktree_tips(home: &Path, repo_id: &str) -> Vec<WorktreeTip> {
    let root = worktrees_dir(home).join(repo_id);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut tips = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(sha) = porch_git::rev_parse_c(&path, "HEAD") else {
            continue;
        };
        let run_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(ToString::to_string);
        tips.push(WorktreeTip {
            path: path.display().to_string(),
            sha,
            run_id,
        });
    }
    tips.sort_by(|a, b| a.path.cmp(&b.path));
    tips
}

fn project_run_forwards(
    db: &Db,
    bare_path: &Path,
    run_id: &str,
) -> std::result::Result<Vec<ForwardView>, String> {
    let records = forward::records_for_run(db, run_id).map_err(|e| e.to_string())?;
    let stored = stored_verdicts(db, run_id)?;
    let terminal = terminal_attempts(db, run_id);
    let tracking = db
        .run_by_id(run_id)
        .ok()
        .flatten()
        .and_then(|run| reconcile::observed_tracking_sha(bare_path, &run.branch));

    let mut by_attempt: BTreeMap<String, Vec<ForwardRecordRow>> = BTreeMap::new();
    for rec in records {
        by_attempt
            .entry(rec.deliver_attempt_id.as_str().to_string())
            .or_default()
            .push(rec);
    }

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (attempt, recs) in &by_attempt {
        seen.insert(attempt.clone());
        if let Some(view) = stored.get(attempt) {
            out.push(view.clone());
            continue;
        }
        if terminal.contains(attempt) {
            continue;
        }
        let Some(intent) = recs
            .iter()
            .find(|r| matches!(r.kind, crate::rounds::forward::ForwardKind::Intent))
        else {
            continue;
        };
        let (verdict, evidence) =
            reconcile::classify(recs, &intent.authorized_sha, tracking.as_deref());
        out.push(forward_view(
            run_id,
            attempt,
            "derived",
            verdict.as_str(),
            evidence.as_str(),
            &intent.ref_name,
            &intent.authorized_sha,
        ));
    }

    for (attempt, view) in stored {
        if !seen.contains(&attempt) {
            out.push(view);
        }
    }
    Ok(out)
}

fn stored_verdicts(
    db: &Db,
    run_id: &str,
) -> std::result::Result<BTreeMap<String, ForwardView>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT deliver_attempt_id, seq, verdict, ref_name, authorized_sha, evidence
             FROM forward_reconciliations
             WHERE run_id = ?1
             ORDER BY seq, id",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([run_id]).map_err(|e| e.to_string())?;
    let mut latest: BTreeMap<String, ForwardView> = BTreeMap::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let attempt: String = row.get(0).map_err(|e| e.to_string())?;
        let raw: String = row.get(2).map_err(|e| e.to_string())?;
        let ref_name: String = row.get(3).map_err(|e| e.to_string())?;
        let authorized: String = row.get(4).map_err(|e| e.to_string())?;
        let evidence: String = row.get(5).map_err(|e| e.to_string())?;
        let (verdict, source) = match Verdict::parse(&raw) {
            Ok(v) => (v.as_str().to_string(), "stored".to_string()),
            Err(_) => (raw, "unknown".to_string()),
        };
        latest.insert(
            attempt.clone(),
            forward_view(
                run_id,
                &attempt,
                &source,
                &verdict,
                &evidence,
                &ref_name,
                &authorized,
            ),
        );
    }
    Ok(latest)
}

fn terminal_attempts(db: &Db, run_id: &str) -> HashSet<String> {
    let conn = db.conn();
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT attempt_id FROM phase_events
         WHERE run_id = ?1 AND kind = 'terminal'",
    ) else {
        return HashSet::new();
    };
    let Ok(mut rows) = stmt.query([run_id]) else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    while let Ok(Some(row)) = rows.next() {
        if let Ok(id) = row.get::<_, String>(0) {
            out.insert(id);
        }
    }
    out
}

fn forward_view(
    run_id: &str,
    attempt: &str,
    source: &str,
    verdict: &str,
    evidence: &str,
    ref_name: &str,
    authorized_sha: &str,
) -> ForwardView {
    let parsed = Verdict::parse(verdict).ok();
    let note = parsed.and_then(|v| reconcile::operator_note(v, ref_name));
    let pr_state = (parsed == Some(Verdict::ReachedOrigin)).then(|| "unrecorded".to_string());
    ForwardView {
        run_id: run_id.to_string(),
        deliver_attempt_id: attempt.to_string(),
        source: source.to_string(),
        verdict: verdict.to_string(),
        evidence: evidence.to_string(),
        ref_name: ref_name.to_string(),
        authorized_sha: authorized_sha.to_string(),
        note,
        pr_state,
    }
}

/// Human lines for `porch status` (no JSON).
#[must_use]
pub fn format_status_human(report: &LookReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("condition={}", report.condition));
    lines.push(format!("PORCH_HOME={}", report.porch_home.display()));
    match &report.database {
        DatabaseArm {
            readable: false,
            reason: Some(r),
        } => lines.push(format!("database: unreadable ({r})")),
        DatabaseArm { readable: true, .. } => {}
        DatabaseArm {
            readable: false,
            reason: None,
        } => lines.push("database: unreadable".into()),
    }
    if let Some(reason) = &report.repo_reason {
        lines.push(reason.clone());
    }
    if report.recovery_tips.is_empty() {
        if report.repo_id.is_some() {
            lines.push("recovery: (none)".into());
        }
    } else {
        lines.push(format!("recovery: {} tip(s)", report.recovery_tips.len()));
        for tip in &report.recovery_tips {
            lines.push(format!("  {} {}", tip.ref_name, tip.sha));
        }
    }
    for tip in &report.worktree_tips {
        lines.push(format!("  worktree {} {}", tip.path, tip.sha));
    }
    if report.forwards.is_empty() {
        if report.database.readable {
            lines.push("forward: (none)".into());
        }
    } else {
        for fwd in &report.forwards {
            lines.push(format!(
                "forward: {} ({}) {} {}",
                fwd.verdict, fwd.source, fwd.evidence, fwd.ref_name
            ));
            if let Some(note) = &fwd.note {
                lines.push(note.clone());
            }
        }
    }
    match report.runs.first() {
        Some(r) => {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            lines.push(format!(
                "latest: {id} {} {}",
                r.get("branch").and_then(|v| v.as_str()).unwrap_or("?"),
                r.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
            ));
        }
        None => lines.push("latest: (none)".into()),
    }
    lines.join("\n")
}

/// `--json` for `porch status`: existing keys plus the join.
#[must_use]
pub fn status_json(report: &LookReport) -> serde_json::Value {
    let latest = report.runs.first().cloned();
    serde_json::json!({
        "daemon_healthy": report.daemon_healthy,
        "condition": report.condition,
        "porch_home": report.porch_home,
        "repo_id": report.repo_id,
        "database": report.database,
        "recovery_tips": report.recovery_tips,
        "worktree_tips": report.worktree_tips,
        "forwards": report.forwards,
        "latest_run": latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::home::db_path;
    use crate::rounds::RunEffects;
    use crate::rounds::forward::{ForwardIntent, ObservedRemote, append_intent};
    use crate::rounds::phase::{PhaseName, PhaseTransition, persist_phase_transition};
    use tempfile::TempDir;

    fn look_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf, String) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let work = root.join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &work, &root.join("bare.git"), "main")
            .unwrap();
        let run = db.insert_run("repo1", "feat", "aaa", None, None).unwrap();
        drop(db);
        git_config_repo_id(&work, "repo1");
        (tmp, home, work, run.id)
    }

    fn git_config_repo_id(work: &Path, id: &str) {
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(["init", "-b", "main"])
            .status()
            .unwrap();
        assert!(st.success());
        let st = std::process::Command::new("git")
            .current_dir(work)
            .args(["config", "porch.repo-id", id])
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[test]
    fn intent_only_forward_is_derived_and_does_not_claim_a_pr_is_absent() {
        let (_tmp, home, work, run_id) = look_fixture();
        let db = Db::open(&db_path(&home)).unwrap();
        let attempt = persist_phase_transition(
            &db,
            PhaseTransition::Start {
                run_id: run_id.clone(),
                phase: PhaseName::Deliver,
            },
            RunEffects::none(),
        )
        .unwrap();
        append_intent(
            &db,
            &ForwardIntent {
                run_id: &run_id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: "aaa",
                observed: ObservedRemote::Absent,
            },
        )
        .unwrap();
        drop(db);

        let report = look(&home, Some(&work), 5);
        assert!(report.database.readable);
        let fwd = report
            .forwards
            .iter()
            .find(|f| f.run_id == run_id)
            .expect("derived forward");
        assert_eq!(fwd.source, "derived");
        assert_eq!(fwd.verdict, "indeterminate");
        assert_eq!(fwd.evidence, "intent_only");
        assert!(fwd.pr_state.is_none());
        let note = fwd.note.as_deref().unwrap_or("");
        assert!(
            !note.to_lowercase().contains("no pull request")
                && !note.to_lowercase().contains("does not exist"),
            "{note}"
        );
        assert!(
            note.contains("undetermined") || note.contains("did not record"),
            "{note}"
        );
    }

    #[test]
    fn stored_verdict_wins_over_a_derived_class() {
        let (_tmp, home, work, run_id) = look_fixture();
        let db = Db::open(&db_path(&home)).unwrap();
        let attempt = persist_phase_transition(
            &db,
            PhaseTransition::Start {
                run_id: run_id.clone(),
                phase: PhaseName::Deliver,
            },
            RunEffects::none(),
        )
        .unwrap();
        append_intent(
            &db,
            &ForwardIntent {
                run_id: &run_id,
                deliver_attempt_id: &attempt,
                ref_name: "refs/heads/feat",
                authorized_sha: "aaa",
                observed: ObservedRemote::Absent,
            },
        )
        .unwrap();
        db.conn()
            .execute(
                "INSERT INTO forward_reconciliations (
                    id, run_id, deliver_attempt_id, seq, verdict, ref_name,
                    authorized_sha, evidence, observed_tracking_sha, created_at
                 ) VALUES ('v1', ?1, ?2, 1, 'reached_origin', 'refs/heads/feat',
                           'aaa', 'push_reported_success', NULL, '1')",
                rusqlite::params![run_id, attempt.as_str()],
            )
            .unwrap();
        drop(db);

        let report = look(&home, Some(&work), 5);
        let fwd = report
            .forwards
            .iter()
            .find(|f| f.run_id == run_id)
            .expect("stored forward");
        assert_eq!(fwd.source, "stored");
        assert_eq!(fwd.verdict, "reached_origin");
        assert_eq!(fwd.pr_state.as_deref(), Some("unrecorded"));
    }
}
