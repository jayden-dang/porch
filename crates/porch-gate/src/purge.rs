//! Read-only `--purge` predicate (`PURGE` / ROAD-24).
//!
//! Evaluated before any **Eject** mutation. LOOK's tip listers stay fail-soft
//! for inspect; this module wraps them fail-closed.

use std::path::Path;

use serde_json::{Value, json};
use ulid::Ulid;

use crate::db::Db;
use crate::home::{abandoned_dir, db_path};
use crate::look::{self, RecoveryTip, WorktreeTip};
use crate::rounds::forward;
use crate::rounds::reconcile::{self, Verdict};
use crate::{Error, Result};

/// One custody tip the predicate considers.
#[derive(Debug, Clone)]
pub(crate) struct CustodyTip {
    pub kind: &'static str,
    pub label: String,
    pub sha: String,
    pub run_id: Option<String>,
}

/// Why `--purge` without `--abandon` must not proceed.
#[derive(Debug, Clone)]
pub(crate) struct PurgeBlock {
    pub unproven: Vec<UnprovenTip>,
    pub active_run_ids: Vec<String>,
    pub inventory_error: Option<String>,
    pub database_error: Option<String>,
}

/// A tip that is neither in the checkout nor proven to have reached `origin`.
#[derive(Debug, Clone)]
pub(crate) struct UnprovenTip {
    pub tip: CustodyTip,
    pub why: String,
}

impl PurgeBlock {
    pub(crate) fn is_blocked(&self) -> bool {
        self.inventory_error.is_some()
            || self.database_error.is_some()
            || !self.unproven.is_empty()
            || !self.active_run_ids.is_empty()
    }

    pub(crate) fn manifest(&self) -> String {
        let mut lines = vec!["refuse --purge: loss is not proven empty or chosen".to_string()];
        if let Some(err) = &self.inventory_error {
            lines.push(format!("cannot inventory custody tips: {err}"));
        }
        if let Some(err) = &self.database_error {
            lines.push(format!(
                "state database unreadable: {err} (cannot prove origin via records or that no runs are active)"
            ));
        }
        if !self.unproven.is_empty() {
            lines.push("unforwarded custody tips:".into());
            for item in &self.unproven {
                let run = item.tip.run_id.as_deref().unwrap_or("-");
                lines.push(format!(
                    "  {}  sha={}  run={}  ({})",
                    item.tip.label, item.tip.sha, run, item.why
                ));
            }
        }
        if !self.active_run_ids.is_empty() {
            lines.push(format!(
                "active runs (pending/running/parked): {}",
                self.active_run_ids.join(", ")
            ));
        }
        lines.push(String::new());
        lines.push("inspect:  porch status".into());
        lines.push("recover:  porch agent sync --recover".into());
        lines.push("override: porch eject --purge --abandon".into());
        lines.join("\n")
    }
}

/// Evaluate the PURGE predicate. Does not mutate. Uses `open_read` when the
/// database file exists.
pub(crate) fn evaluate(home: &Path, work: &Path, repo_id: &str, bare_path: &Path) -> PurgeBlock {
    let recover = match look::recovery_tips_strict(bare_path) {
        Ok(tips) => tips,
        Err(e) => {
            return PurgeBlock {
                unproven: Vec::new(),
                active_run_ids: Vec::new(),
                inventory_error: Some(e),
                database_error: None,
            };
        }
    };
    let leftover = match look::worktree_tips_strict(home, repo_id) {
        Ok(tips) => tips,
        Err(e) => {
            return PurgeBlock {
                unproven: Vec::new(),
                active_run_ids: Vec::new(),
                inventory_error: Some(e),
                database_error: None,
            };
        }
    };

    let mut tips: Vec<CustodyTip> = recover.into_iter().map(From::from).collect();
    tips.extend(leftover.into_iter().map(CustodyTip::from));

    let db_file = db_path(home);
    let (db, database_error) = if db_file.exists() {
        match Db::open_read(&db_file) {
            Ok(db) => (Some(db), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };

    if let Some(err) = database_error {
        return PurgeBlock {
            unproven: Vec::new(),
            active_run_ids: Vec::new(),
            inventory_error: None,
            database_error: Some(err),
        };
    }

    let mut active_run_ids = Vec::new();
    if let Some(db) = db.as_ref() {
        match db.active_runs(Some(repo_id), None) {
            Ok(runs) => {
                active_run_ids = runs.into_iter().map(|r| r.id).collect();
            }
            Err(e) => {
                return PurgeBlock {
                    unproven: Vec::new(),
                    active_run_ids: Vec::new(),
                    inventory_error: None,
                    database_error: Some(e.to_string()),
                };
            }
        }
    }

    let mut unproven = Vec::new();
    for tip in tips {
        if checkout_has(work, &tip.sha) {
            continue;
        }
        if origin_proof(bare_path, db.as_ref(), &tip) {
            continue;
        }
        unproven.push(UnprovenTip {
            tip,
            why: "not in checkout, origin unproven".into(),
        });
    }

    PurgeBlock {
        unproven,
        active_run_ids,
        inventory_error: None,
        database_error: None,
    }
}

/// Persist the chosen-loss record. Fails closed: caller must not mutate on error.
pub(crate) fn write_abandon_record(
    home: &Path,
    repo_id: &str,
    block: &PurgeBlock,
) -> Result<std::path::PathBuf> {
    let dir = abandoned_dir(home);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{repo_id}-{}.json", Ulid::new()));
    let tips: Vec<Value> = block
        .unproven
        .iter()
        .map(|item| {
            json!({
                "kind": item.tip.kind,
                "label": item.tip.label,
                "sha": item.tip.sha,
                "run_id": item.tip.run_id,
            })
        })
        .collect();
    let body = json!({
        "repo_id": repo_id,
        "abandoned_at": crate::db::now_secs(),
        "tips": tips,
        "active_run_ids": block.active_run_ids,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| Error::Other(e.to_string()))?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn checkout_has(work: &Path, sha: &str) -> bool {
    porch_git::is_ancestor(work, sha, "HEAD").unwrap_or(false)
}

fn origin_proof(bare_path: &Path, db: Option<&Db>, tip: &CustodyTip) -> bool {
    if let Some(db) = db {
        if let Some(run_id) = tip.run_id.as_deref() {
            if records_prove(db, bare_path, run_id, &tip.sha) {
                return true;
            }
            if verdicts_prove(db, bare_path, run_id, &tip.sha) {
                return true;
            }
        }
    }
    tracking_proves(bare_path, &tip.sha)
}

fn records_prove(db: &Db, bare_path: &Path, run_id: &str, tip_sha: &str) -> bool {
    let Ok(records) = forward::records_for_run(db, run_id) else {
        return false;
    };
    records.iter().any(|row| {
        row.kind.reached_origin() && tip_is_landed(bare_path, tip_sha, &row.authorized_sha)
    })
}

fn verdicts_prove(db: &Db, bare_path: &Path, run_id: &str, tip_sha: &str) -> bool {
    let Ok(rows) = reconcile::verdicts_for_run(db, run_id) else {
        return false;
    };
    rows.iter().any(|row| {
        row.verdict == Verdict::ReachedOrigin
            && tip_is_landed(bare_path, tip_sha, &row.authorized_sha)
    })
}

fn tracking_proves(bare_path: &Path, tip_sha: &str) -> bool {
    let Ok(git_dir) = porch_git::GitDir::new(bare_path) else {
        return false;
    };
    let Ok(refs) = porch_git::for_each_ref(&git_dir, "refs/remotes/origin") else {
        return false;
    };
    refs.into_iter()
        .any(|(sha, _)| tip_is_landed(bare_path, tip_sha, &sha))
}

fn tip_is_landed(bare_path: &Path, tip_sha: &str, landed: &str) -> bool {
    let Ok(git_dir) = porch_git::GitDir::new(bare_path) else {
        return false;
    };
    porch_git::is_ancestor_git_dir(&git_dir, tip_sha, landed).unwrap_or(false)
}

impl From<RecoveryTip> for CustodyTip {
    fn from(tip: RecoveryTip) -> Self {
        Self {
            kind: "recover",
            label: tip.ref_name,
            sha: tip.sha,
            run_id: tip.run_id,
        }
    }
}

impl From<WorktreeTip> for CustodyTip {
    fn from(tip: WorktreeTip) -> Self {
        Self {
            kind: "worktree",
            label: tip.path,
            sha: tip.sha,
            run_id: tip.run_id,
        }
    }
}
