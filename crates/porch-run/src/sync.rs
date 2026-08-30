//! Custody / sync: author branch vs pipeline HEAD (JSON for `porch agent sync`).

use std::path::Path;

use porch_gate::{Db, RunRow, db_path, repo_id_for};
use porch_git::GitDir;
use serde::Serialize;

use crate::AgentCliResult;

#[derive(Debug)]
enum SyncErr {
    Usage(String),
    Fail(String),
}

/// Recovery ref under the bare: `refs/porch/recover/<run_id>`.
#[must_use]
pub fn recovery_ref_name(run_id: &str) -> String {
    format!("refs/porch/recover/{run_id}")
}

/// JSON document for `porch agent sync`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub state: String,
    pub branch: String,
    pub local_head: Option<String>,
    pub pipeline_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub relation: String,
    /// Operator instructions; never rewrites `origin`.
    pub fetch_hint: String,
    pub recoverable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_ref: Option<String>,
    pub recovered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Inspect (or recover) custody for the cwd branch.
#[must_use]
pub fn agent_sync(
    home: &Path,
    work_tree: &Path,
    run_id: Option<&str>,
    recover: bool,
) -> AgentCliResult {
    match agent_sync_inner(home, work_tree, run_id, recover) {
        Ok(status) => AgentCliResult {
            exit_code: i32::from(status.error.is_some()),
            json: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            already_emitted: false,
        },
        Err(SyncErr::Usage(msg)) => AgentCliResult {
            exit_code: 2,
            json: serde_json::json!({"error": msg, "code": "usage"}).to_string(),
            already_emitted: false,
        },
        Err(SyncErr::Fail(msg)) => AgentCliResult {
            exit_code: 1,
            json: serde_json::json!({"error": msg}).to_string(),
            already_emitted: false,
        },
    }
}

fn agent_sync_inner(
    home: &Path,
    work_tree: &Path,
    run_id: Option<&str>,
    recover: bool,
) -> Result<SyncStatus, SyncErr> {
    let work = work_tree
        .canonicalize()
        .map_err(|e| SyncErr::Fail(e.to_string()))?;
    let db = Db::open(&db_path(home)).map_err(|e| SyncErr::Fail(e.to_string()))?;
    let repo_id = repo_id_for(&work);
    let branch = current_branch(&work)?;
    if branch == "HEAD" {
        return Err(SyncErr::Usage("detached HEAD — checkout a branch".into()));
    }

    let run = resolve_sync_run(&db, &repo_id, &branch, run_id)?;
    let repo = db
        .repo_by_id(&repo_id)
        .map_err(|e| SyncErr::Fail(e.to_string()))?
        .ok_or_else(|| SyncErr::Fail(format!("unknown repo {repo_id}")))?;
    let bare = GitDir::new(&repo.bare_path).map_err(|e| SyncErr::Fail(e.to_string()))?;

    let local_head =
        porch_git::rev_parse_c(&work, "HEAD").map_err(|e| SyncErr::Fail(e.to_string()))?;

    let recovery_name = run.as_ref().map(|r| recovery_ref_name(&r.id));
    let recovery_sha = recovery_name
        .as_ref()
        .and_then(|r| porch_git::rev_parse(&bare, r).ok());

    let pipeline_head = pipeline_head_for(run.as_ref(), &bare, &branch, recovery_sha.as_deref());

    let relation = classify_relation(&work, &local_head, pipeline_head.as_deref());
    let recoverable = recovery_sha
        .as_ref()
        .is_some_and(|rec| rec != &local_head && relation != "equal");

    let rec_label = recovery_name
        .as_deref()
        .unwrap_or("refs/porch/recover/<run-id>");
    // When a recovery tip exists, porch/{branch} may still be the submit SHA —
    // prefer the recovery ref / `porch agent sync --recover` as the primary hint.
    let fetch_hint = if recoverable {
        format!(
            "porch agent sync --recover  \
# or: git fetch porch {rec_label} && git merge --ff-only FETCH_HEAD  \
(never rewrites origin; porch/{branch} may still be submit SHA)"
        )
    } else {
        format!(
            "git fetch porch && git merge --ff-only porch/{branch}  \
(never rewrites origin)"
        )
    };

    let mut status = SyncStatus {
        state: state_for(&relation, recoverable),
        branch: branch.clone(),
        local_head: Some(local_head.clone()),
        pipeline_head,
        run_id: run.as_ref().map(|r| r.id.clone()),
        relation: relation.clone(),
        fetch_hint,
        recoverable,
        recovery_ref: if recovery_sha.is_some() {
            recovery_name
        } else {
            None
        },
        recovered: false,
        error: None,
    };

    if recover {
        apply_recover(
            &work,
            &bare,
            &mut status,
            &local_head,
            recovery_sha.as_deref(),
        )?;
    }

    Ok(status)
}

fn resolve_sync_run(
    db: &Db,
    repo_id: &str,
    branch: &str,
    run_id: Option<&str>,
) -> Result<Option<RunRow>, SyncErr> {
    if let Some(id) = run_id {
        return Ok(Some(
            db.run_by_id(id)
                .map_err(|e| SyncErr::Fail(e.to_string()))?
                .ok_or_else(|| SyncErr::Usage(format!("unknown run {id}")))?,
        ));
    }
    db.latest_run_for_branch(repo_id, branch)
        .map_err(|e| SyncErr::Fail(e.to_string()))
}

fn pipeline_head_for(
    run: Option<&RunRow>,
    bare: &GitDir,
    branch: &str,
    recovery_sha: Option<&str>,
) -> Option<String> {
    if let Some(rec) = recovery_sha {
        return Some(rec.to_string());
    }
    if let Some(run) = run {
        if let Some(h) = run.head_sha.as_deref() {
            return Some(h.to_string());
        }
    }
    let branch_ref = format!("refs/heads/{branch}");
    porch_git::rev_parse(bare, &branch_ref).ok()
}

fn classify_relation(work: &Path, local: &str, pipeline: Option<&str>) -> String {
    let Some(pipe) = pipeline else {
        return "unknown".into();
    };
    if local == pipe {
        return "equal".into();
    }
    let local_has_pipe = porch_git::is_ancestor(work, pipe, local).unwrap_or(false);
    let pipe_has_local = porch_git::is_ancestor(work, local, pipe).unwrap_or(false);
    match (local_has_pipe, pipe_has_local) {
        (true, false) => "ahead".into(),
        (false, true) => "behind".into(),
        (false, false) => "diverged".into(),
        _ => "equal".into(),
    }
}

fn state_for(relation: &str, recoverable: bool) -> String {
    match relation {
        "behind" if recoverable => "behind_recoverable".into(),
        "behind" => "behind".into(),
        "ahead" => "local_ahead".into(),
        "diverged" => "diverged".into(),
        "equal" => "synchronized".into(),
        other => other.into(),
    }
}

fn apply_recover(
    work: &Path,
    bare: &GitDir,
    status: &mut SyncStatus,
    local_head: &str,
    recovery_sha: Option<&str>,
) -> Result<(), SyncErr> {
    let Some(rec) = recovery_sha else {
        status.error = Some("no recorded recovery ref for this run".into());
        return Ok(());
    };
    if rec == local_head {
        status.recovered = true;
        status.state = "synchronized".into();
        status.relation = "equal".into();
        return Ok(());
    }

    let rec_ref = status
        .recovery_ref
        .clone()
        .ok_or_else(|| SyncErr::Fail("missing recovery_ref".into()))?;
    let bare_path = bare.as_path().to_string_lossy().into_owned();
    porch_git::run_c(
        work,
        &[
            "fetch",
            &bare_path,
            &format!("{rec_ref}:refs/porch/sync-recover"),
        ],
    )
    .map_err(|e| SyncErr::Fail(format!("fetch recovery: {e}")))?;

    match porch_git::is_ancestor(work, local_head, rec) {
        Ok(true) => {
            porch_git::run_c(work, &["merge", "--ff-only", rec])
                .map_err(|e| SyncErr::Fail(format!("ff-only recover: {e}")))?;
            status.recovered = true;
            status.local_head = Some(rec.to_string());
            status.relation = "equal".into();
            status.state = "custody_returned".into();
            status.recoverable = false;
            Ok(())
        }
        Ok(false) => {
            status.error = Some(
                "recovery refused: local HEAD is not an ancestor of the recorded pipeline tip \
(fail closed; recovery ref kept)"
                    .into(),
            );
            Ok(())
        }
        Err(e) => Err(SyncErr::Fail(e.to_string())),
    }
}

fn current_branch(work: &Path) -> Result<String, SyncErr> {
    let out = porch_git::run_c(work, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map_err(|e| SyncErr::Fail(e.to_string()))?;
    Ok(porch_git::stdout_trim(&out))
}

/// Short TUI / CLI hint when the author branch is behind the pipeline tip.
#[must_use]
pub fn sync_hint_for(home: &Path, work_tree: &Path) -> Option<String> {
    let Ok(status) = agent_sync_inner(home, work_tree, None, false) else {
        return None;
    };
    if matches!(status.relation.as_str(), "behind") || status.recoverable {
        Some(format!("pipeline ahead of local — {}", status.fetch_hint))
    } else {
        None
    }
}

/// Pin unpublished pipeline commits under `refs/porch/recover/<run>` on the bare.
///
/// No-op when HEAD equals the submitted SHA or is not a descendant.
pub(crate) fn pin_recovery_if_needed(bare: &GitDir, run: &RunRow, wt: &Path) -> Result<(), String> {
    if !wt.exists() {
        return Ok(());
    }
    let head = porch_git::rev_parse_c(wt, "HEAD").map_err(|e| e.to_string())?;
    if head == run.sha {
        return Ok(());
    }
    match porch_git::is_ancestor(wt, &run.sha, &head) {
        Ok(true) => {
            let name = recovery_ref_name(&run.id);
            porch_git::update_ref(bare, &name, &head).map_err(|e| e.to_string())?;
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use porch_gate::repo_id_for;
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

    struct SyncFixture {
        _tmp: TempDir,
        home: std::path::PathBuf,
        work: std::path::PathBuf,
        bare: GitDir,
        submit: String,
        pipeline: String,
        run_id: String,
    }

    fn setup_behind_pipeline() -> SyncFixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bare_path = root.join("bare.git");
        init_bare(&bare_path).unwrap();
        let bare = GitDir::new(&bare_path).unwrap();

        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init"]);
        git(&seed, &["config", "user.email", "porch@example.com"]);
        git(&seed, &["config", "user.name", "Porch"]);
        git(&seed, &["checkout", "-b", "feat-sync"]);
        std::fs::write(seed.join("README"), "submit\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "submit"]);
        let submit = git_out(&seed, &["rev-parse", "HEAD"]);
        std::fs::write(seed.join("README"), "pipeline\n").unwrap();
        git(&seed, &["add", "README"]);
        git(&seed, &["commit", "-m", "pipeline tip"]);
        let pipeline = git_out(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &[
                "push",
                bare_path.to_str().unwrap(),
                "feat-sync:refs/heads/feat-sync",
            ],
        );

        let work = root.join("work");
        let st = std::process::Command::new("git")
            .args([
                "clone",
                "--branch",
                "feat-sync",
                bare_path.to_str().unwrap(),
                work.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let work = work.canonicalize().unwrap();
        git(&work, &["config", "user.email", "porch@example.com"]);
        git(&work, &["config", "user.name", "Porch"]);
        // Author still at submit SHA; pipeline tip is unpublished elsewhere.
        git(&work, &["reset", "--hard", &submit]);

        let db = Db::open(&db_path(&home)).unwrap();
        let repo_id = repo_id_for(&work);
        db.upsert_repo(&repo_id, &work, &bare_path, "main").unwrap();
        let run = db
            .insert_run(&repo_id, "feat-sync", &submit, None, None)
            .unwrap();
        let run_id = run.id.clone();
        let rec = recovery_ref_name(&run_id);
        porch_git::update_ref(&bare, &rec, &pipeline).unwrap();

        SyncFixture {
            _tmp: tmp,
            home,
            work,
            bare,
            submit,
            pipeline,
            run_id,
        }
    }

    #[test]
    fn fetch_hint_prefers_recover_when_recoverable() {
        let fx = setup_behind_pipeline();
        let status = agent_sync_inner(&fx.home, &fx.work, Some(&fx.run_id), false).unwrap();
        assert!(status.recoverable, "{status:?}");
        assert!(
            status.fetch_hint.contains("porch agent sync --recover"),
            "primary hint should prefer --recover: {}",
            status.fetch_hint
        );
        assert!(
            status.fetch_hint.contains(&recovery_ref_name(&fx.run_id))
                || status.fetch_hint.contains("refs/porch/recover"),
            "hint should mention recovery ref: {}",
            status.fetch_hint
        );
        // porch/{branch} FF is not the primary guidance when recoverable.
        assert!(
            !status
                .fetch_hint
                .starts_with("git fetch porch && git merge --ff-only porch/"),
            "must not lead with porch/branch tip: {}",
            status.fetch_hint
        );
    }

    #[test]
    fn recover_fast_forwards_when_local_is_ancestor() {
        let fx = setup_behind_pipeline();
        assert_eq!(git_out(&fx.work, &["rev-parse", "HEAD"]), fx.submit);

        let status = agent_sync_inner(&fx.home, &fx.work, Some(&fx.run_id), true).unwrap();
        assert!(status.error.is_none(), "{status:?}");
        assert!(status.recovered, "{status:?}");
        assert_eq!(status.local_head.as_deref(), Some(fx.pipeline.as_str()));
        assert_eq!(git_out(&fx.work, &["rev-parse", "HEAD"]), fx.pipeline);
        assert_eq!(status.state, "custody_returned");
    }

    #[test]
    fn recover_refuses_when_diverged() {
        let fx = setup_behind_pipeline();
        std::fs::write(fx.work.join("README"), "author-sibling\n").unwrap();
        git(&fx.work, &["add", "README"]);
        git(&fx.work, &["commit", "-m", "sibling"]);

        let status = agent_sync_inner(&fx.home, &fx.work, Some(&fx.run_id), true).unwrap();
        assert!(!status.recovered, "{status:?}");
        assert!(
            status
                .error
                .as_deref()
                .is_some_and(|e| e.contains("recovery refused")),
            "{status:?}"
        );
        // Recovery ref kept on bare.
        let rec = recovery_ref_name(&fx.run_id);
        assert_eq!(porch_git::rev_parse(&fx.bare, &rec).unwrap(), fx.pipeline);
    }
}
