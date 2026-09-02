use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use ulid::Ulid;

use crate::Result;

const RUN_SELECT_FROM: &str =
    "SELECT id, repo_id, branch, sha, status, worktree_dir, head_sha, base_sha,
                    intent, intent_source, error, review_approved_head_sha, findings_json,
                    fixer_session_id, pr_url, deliver_repair_attempts, trusted_config_sha,
                    pr_title_written
             FROM runs";
const RUN_SELECT: &str =
    "SELECT id, repo_id, branch, sha, status, worktree_dir, head_sha, base_sha,
                    intent, intent_source, error, review_approved_head_sha, findings_json,
                    fixer_session_id, pr_url, deliver_repair_attempts, trusted_config_sha,
                    pr_title_written
             FROM runs WHERE id = ?1";

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub id: String,
    pub worktree_path: PathBuf,
    pub bare_path: PathBuf,
    pub default_branch: String,
}

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: String,
    pub repo_id: String,
    pub branch: String,
    pub sha: String,
    pub status: String,
    pub worktree_dir: Option<PathBuf>,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub intent: Option<String>,
    pub intent_source: Option<String>,
    pub error: Option<String>,
    pub review_approved_head_sha: Option<String>,
    pub findings_json: Option<String>,
    pub fixer_session_id: Option<String>,
    pub pr_url: Option<String>,
    /// Deliver mechanical repair attempts started (budget default 3).
    pub deliver_repair_attempts: u32,
    /// Pinned default-branch tip SHA used for trusted `.porch.yaml` (E10).
    pub trusted_config_sha: Option<String>,
    /// Last PR title porch wrote (managed-title heuristic).
    pub pr_title_written: Option<String>,
}

/// Persisted fixer commit span awaiting a completed review (E23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncertifiedPipelineRange {
    pub repo_id: String,
    pub branch: String,
    pub from_sha: String,
    pub to_sha: String,
    pub source_run_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct StepResultRow {
    pub id: String,
    pub run_id: String,
    pub step: String,
    pub status: String,
    pub error: Option<String>,
}

impl Db {
    /// Open (or create) the `SQLite` database and apply schema migrations.
    ///
    /// # Errors
    ///
    /// Returns I/O or `SQLite` errors if the parent directory, connection, or
    /// migrations fail.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS repos (
                id TEXT PRIMARY KEY,
                worktree_path TEXT NOT NULL,
                bare_path TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                sha TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(repo_id) REFERENCES repos(id)
            );
            CREATE TABLE IF NOT EXISTS step_results (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                step TEXT NOT NULL,
                status TEXT NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY(run_id) REFERENCES runs(id)
            );
            ",
        )?;
        ensure_column(
            &conn,
            "repos",
            "default_branch",
            "TEXT NOT NULL DEFAULT 'main'",
        )?;
        ensure_column(&conn, "runs", "worktree_dir", "TEXT")?;
        ensure_column(&conn, "runs", "head_sha", "TEXT")?;
        ensure_column(&conn, "runs", "base_sha", "TEXT")?;
        ensure_column(&conn, "runs", "intent", "TEXT")?;
        ensure_column(&conn, "runs", "intent_source", "TEXT")?;
        ensure_column(&conn, "runs", "error", "TEXT")?;
        ensure_column(&conn, "runs", "review_approved_head_sha", "TEXT")?;
        ensure_column(&conn, "runs", "findings_json", "TEXT")?;
        ensure_column(&conn, "runs", "fixer_session_id", "TEXT")?;
        ensure_column(&conn, "runs", "pr_url", "TEXT")?;
        ensure_column(
            &conn,
            "runs",
            "deliver_repair_attempts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "runs", "trusted_config_sha", "TEXT")?;
        ensure_column(&conn, "runs", "pr_title_written", "TEXT")?;
        ensure_column(&conn, "runs", "required_set_digest", "TEXT")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS uncertified_pipeline_ranges (
                repo_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                from_sha TEXT NOT NULL,
                to_sha TEXT NOT NULL,
                source_run_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (repo_id, branch)
            );
            ",
        )?;
        crate::rounds::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex")
    }

    /// Insert or update a repo row keyed by `id`.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the upsert fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn upsert_repo(
        &self,
        id: &str,
        worktree: &Path,
        bare: &Path,
        default_branch: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO repos (id, worktree_path, bare_path, created_at, default_branch)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
               worktree_path = excluded.worktree_path,
               bare_path = excluded.bare_path,
               default_branch = excluded.default_branch",
            rusqlite::params![
                id,
                path_str(worktree),
                path_str(bare),
                now_secs(),
                default_branch
            ],
        )?;
        Ok(())
    }

    /// Look up a repo by id.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn repo_by_id(&self, id: &str) -> Result<Option<RepoRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT id, worktree_path, bare_path, default_branch FROM repos WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_repo(row)?));
        }
        Ok(None)
    }

    /// Look up a repo by bare path (canonical path comparison).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn repo_by_bare(&self, bare: &Path) -> Result<Option<RepoRow>> {
        let want = canonicalize_path(bare);
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT id, worktree_path, bare_path, default_branch FROM repos WHERE bare_path = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![path_str(&want)])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_repo(row)?));
        }
        // Defense in depth: match even if a stored path was not canonicalized.
        let mut stmt =
            conn.prepare("SELECT id, worktree_path, bare_path, default_branch FROM repos")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let bare_path = PathBuf::from(row.get::<_, String>(2)?);
            if canonicalize_path(&bare_path) == want {
                return Ok(Some(RepoRow {
                    id: row.get(0)?,
                    worktree_path: PathBuf::from(row.get::<_, String>(1)?),
                    bare_path,
                    default_branch: row.get(3)?,
                }));
            }
        }
        Ok(None)
    }

    /// Insert a pending run and return the new row.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the insert fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn insert_run(
        &self,
        repo_id: &str,
        branch: &str,
        sha: &str,
        intent: Option<&str>,
        intent_source: Option<&str>,
    ) -> Result<RunRow> {
        let id = Ulid::new().to_string();
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO runs (
                id, repo_id, branch, sha, status, created_at, intent, intent_source
             ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
            rusqlite::params![id, repo_id, branch, sha, now_secs(), intent, intent_source],
        )?;
        Ok(RunRow {
            id,
            repo_id: repo_id.to_string(),
            branch: branch.to_string(),
            sha: sha.to_string(),
            status: "pending".into(),
            worktree_dir: None,
            head_sha: None,
            base_sha: None,
            intent: intent.map(str::to_string),
            intent_source: intent_source.map(str::to_string),
            error: None,
            review_approved_head_sha: None,
            findings_json: None,
            fixer_session_id: None,
            pr_url: None,
            deliver_repair_attempts: 0,
            trusted_config_sha: None,
            pr_title_written: None,
        })
    }

    /// Load a single run by id.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn run_by_id(&self, id: &str) -> Result<Option<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(RUN_SELECT)?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_run(row)?));
        }
        Ok(None)
    }

    /// List runs for a repo in creation order.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn runs_for_repo(&self, repo_id: &str) -> Result<Vec<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM} WHERE repo_id = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(rusqlite::params![repo_id], map_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Pending runs in creation order.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn pending_runs(&self) -> Result<Vec<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM} WHERE status = 'pending' ORDER BY created_at"
        ))?;
        let rows = stmt.query_map([], map_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Recent runs, newest first. Optional `repo_id` filter; `limit` capped implicitly by caller.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn recent_runs(&self, repo_id: Option<&str>, limit: usize) -> Result<Vec<RunRow>> {
        let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.conn.lock().expect("db mutex");
        if let Some(repo_id) = repo_id {
            let mut stmt = conn.prepare(&format!(
                "{RUN_SELECT_FROM} WHERE repo_id = ?1 ORDER BY created_at DESC LIMIT ?2"
            ))?;
            let rows = stmt.query_map(rusqlite::params![repo_id, limit_i], map_run)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        } else {
            let mut stmt = conn.prepare(&format!(
                "{RUN_SELECT_FROM} ORDER BY created_at DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(rusqlite::params![limit_i], map_run)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        }
    }

    /// Active (pending / running / parked) runs, optionally filtered by repo and/or branch.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn active_runs(&self, repo_id: Option<&str>, branch: Option<&str>) -> Result<Vec<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM}
             WHERE status IN ('pending', 'running', 'parked')
             ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([], map_run)?;
        let mut out = Vec::new();
        for row in rows {
            let run = row?;
            if let Some(want) = repo_id {
                if run.repo_id != want {
                    continue;
                }
            }
            if let Some(want) = branch {
                if run.branch != want {
                    continue;
                }
            }
            out.push(run);
        }
        Ok(out)
    }

    /// Latest parked run for a repo (by creation time), if any.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn latest_parked_for_repo(&self, repo_id: &str) -> Result<Option<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM} WHERE repo_id = ?1 AND status = 'parked'
             ORDER BY created_at DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![repo_id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_run(row)?));
        }
        Ok(None)
    }

    /// In-flight runs (pending, running, or parked) for the same repo + branch,
    /// excluding `except_id`.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn in_flight_same_branch(
        &self,
        repo_id: &str,
        branch: &str,
        except_id: &str,
    ) -> Result<Vec<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM}
             WHERE repo_id = ?1 AND branch = ?2 AND id != ?3
               AND status IN ('pending', 'running', 'parked')
             ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(rusqlite::params![repo_id, branch, except_id], map_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Recover stale `running` runs after a daemon restart.
    ///
    /// Runs with a non-empty `pr_url` become `ci_monitor_interrupted` (PR already
    /// open; do not claim a failed push). Other `running` rows become `failed`.
    /// Parked runs are left alone (resume is operator-driven).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update or query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn fail_stale_running(&self, error: &str) -> Result<Vec<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!("{RUN_SELECT_FROM} WHERE status = 'running'"))?;
        let rows = stmt.query_map([], map_run)?;
        let mut stale = Vec::new();
        for row in rows {
            stale.push(row?);
        }
        drop(stmt);
        // Runs that already opened a PR were likely only babysitting checks —
        // mark interrupted, not failed, so an open PR is not claimed as a failed push.
        for run in &stale {
            if run.pr_url.as_ref().is_some_and(|u| !u.trim().is_empty()) {
                conn.execute(
                    "UPDATE runs SET status = 'ci_monitor_interrupted', error = ?1 WHERE id = ?2",
                    rusqlite::params![error, run.id],
                )?;
            } else {
                conn.execute(
                    "UPDATE runs SET status = 'failed', error = ?1 WHERE id = ?2",
                    rusqlite::params![error, run.id],
                )?;
            }
        }
        for run in &mut stale {
            if run.pr_url.as_ref().is_some_and(|u| !u.trim().is_empty()) {
                run.status = "ci_monitor_interrupted".into();
            } else {
                run.status = "failed".into();
            }
            run.error = Some(error.to_string());
        }
        Ok(stale)
    }

    /// Update run status and optional error message.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_run_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET status = ?1, error = ?2 WHERE id = ?3",
            rusqlite::params![status, error, id],
        )?;
        Ok(())
    }

    /// Record the disposable worktree path before `git worktree add`.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_worktree_dir(&self, id: &str, path: &Path) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET worktree_dir = ?1 WHERE id = ?2",
            rusqlite::params![path_str(path), id],
        )?;
        Ok(())
    }

    /// Persist HEAD / base SHAs after rebase (or worktree creation).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_run_shas(
        &self,
        id: &str,
        head_sha: Option<&str>,
        base_sha: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET head_sha = COALESCE(?1, head_sha), base_sha = COALESCE(?2, base_sha)
             WHERE id = ?3",
            rusqlite::params![head_sha, base_sha, id],
        )?;
        Ok(())
    }

    /// Pin the trusted default-branch tip SHA for `.porch.yaml` (E10).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_trusted_config_sha(&self, id: &str, sha: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET trusted_config_sha = ?1 WHERE id = ?2",
            rusqlite::params![sha, id],
        )?;
        Ok(())
    }

    /// Record the HEAD SHA approved by a completed review (or human approve).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_review_approved_head_sha(&self, id: &str, sha: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET review_approved_head_sha = ?1 WHERE id = ?2",
            rusqlite::params![sha, id],
        )?;
        Ok(())
    }

    /// Persist findings JSON for a parked (or completed) review round.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_findings_json(&self, id: &str, json: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET findings_json = ?1 WHERE id = ?2",
            rusqlite::params![json, id],
        )?;
        Ok(())
    }

    /// Persist fixer session id for later fix rounds of the same run.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_fixer_session_id(&self, id: &str, session_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET fixer_session_id = ?1 WHERE id = ?2",
            rusqlite::params![session_id, id],
        )?;
        Ok(())
    }

    /// Persist the GitHub PR URL after create/update.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_pr_url(&self, id: &str, url: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET pr_url = ?1 WHERE id = ?2",
            rusqlite::params![url, id],
        )?;
        Ok(())
    }

    /// Persist the last PR title porch wrote (for managed-title heuristic).
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn set_pr_title_written(&self, id: &str, title: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET pr_title_written = ?1 WHERE id = ?2",
            rusqlite::params![title, id],
        )?;
        Ok(())
    }

    /// Increment `deliver_repair_attempts` when a fix attempt starts; returns the new count.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the update fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn increment_deliver_repair_attempts(&self, id: &str) -> Result<u32> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "UPDATE runs SET deliver_repair_attempts = deliver_repair_attempts + 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        let n: i64 = conn.query_row(
            "SELECT deliver_repair_attempts FROM runs WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        u32::try_from(n)
            .map_err(|_| crate::Error::Other(format!("deliver_repair_attempts out of range: {n}")))
    }

    /// Insert or replace the uncertified fixer range for a repo branch.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the upsert fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn upsert_uncertified_pipeline_range(
        &self,
        repo_id: &str,
        branch: &str,
        from_sha: &str,
        to_sha: &str,
        source_run_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO uncertified_pipeline_ranges (
                repo_id, branch, from_sha, to_sha, source_run_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo_id, branch) DO UPDATE SET
               from_sha = excluded.from_sha,
               to_sha = excluded.to_sha,
               source_run_id = excluded.source_run_id,
               created_at = excluded.created_at",
            rusqlite::params![repo_id, branch, from_sha, to_sha, source_run_id, now_secs()],
        )?;
        Ok(())
    }

    /// Load the uncertified fixer range for a repo branch, if any.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn get_uncertified_pipeline_range(
        &self,
        repo_id: &str,
        branch: &str,
    ) -> Result<Option<UncertifiedPipelineRange>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT repo_id, branch, from_sha, to_sha, source_run_id, created_at
             FROM uncertified_pipeline_ranges
             WHERE repo_id = ?1 AND branch = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![repo_id, branch])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(UncertifiedPipelineRange {
                repo_id: row.get(0)?,
                branch: row.get(1)?,
                from_sha: row.get(2)?,
                to_sha: row.get(3)?,
                source_run_id: row.get(4)?,
                created_at: row.get(5)?,
            }));
        }
        Ok(None)
    }

    /// Delete the uncertified fixer range for a repo branch.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the delete fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn delete_uncertified_pipeline_range(&self, repo_id: &str, branch: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "DELETE FROM uncertified_pipeline_ranges WHERE repo_id = ?1 AND branch = ?2",
            rusqlite::params![repo_id, branch],
        )?;
        Ok(())
    }

    /// Insert a step result row.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the insert fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn insert_step_result(
        &self,
        run_id: &str,
        step: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<StepResultRow> {
        let id = Ulid::new().to_string();
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO step_results (id, run_id, step, status, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, run_id, step, status, error, now_secs()],
        )?;
        Ok(StepResultRow {
            id,
            run_id: run_id.to_string(),
            step: step.to_string(),
            status: status.to_string(),
            error: error.map(str::to_string),
        })
    }

    /// Step results for a run in creation order.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn step_results_for_run(&self, run_id: &str) -> Result<Vec<StepResultRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT id, run_id, step, status, error FROM step_results
             WHERE run_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![run_id], |row| {
            Ok(StepResultRow {
                id: row.get(0)?,
                run_id: row.get(1)?,
                step: row.get(2)?,
                status: row.get(3)?,
                error: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Latest step named `step` for a run (by creation time), if any.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn latest_step_for_run(&self, run_id: &str, step: &str) -> Result<Option<StepResultRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT id, run_id, step, status, error FROM step_results
             WHERE run_id = ?1 AND step = ?2
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![run_id, step])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(StepResultRow {
                id: row.get(0)?,
                run_id: row.get(1)?,
                step: row.get(2)?,
                status: row.get(3)?,
                error: row.get(4)?,
            }));
        }
        Ok(None)
    }

    /// Delete a repo and its runs / step results / uncertified ranges.
    ///
    /// Only touches rows for `repo_id` — never other repos.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if any delete fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn delete_repo(&self, repo_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM step_results WHERE run_id IN (SELECT id FROM runs WHERE repo_id = ?1)",
            rusqlite::params![repo_id],
        )?;
        tx.execute(
            "DELETE FROM uncertified_pipeline_ranges WHERE repo_id = ?1",
            rusqlite::params![repo_id],
        )?;
        tx.execute(
            "DELETE FROM runs WHERE repo_id = ?1",
            rusqlite::params![repo_id],
        )?;
        tx.execute(
            "DELETE FROM repos WHERE id = ?1",
            rusqlite::params![repo_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Latest run for a repo+branch (any status), newest first.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error if the query fails.
    ///
    /// # Panics
    ///
    /// Panics if the connection mutex is poisoned.
    pub fn latest_run_for_branch(&self, repo_id: &str, branch: &str) -> Result<Option<RunRow>> {
        let conn = self.conn.lock().expect("db mutex");
        let mut stmt = conn.prepare(&format!(
            "{RUN_SELECT_FROM} WHERE repo_id = ?1 AND branch = ?2
             ORDER BY created_at DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![repo_id, branch])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_run(row)?));
        }
        Ok(None)
    }
}

pub(crate) fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut found = false;
    for name in names {
        if name? == column {
            found = true;
            break;
        }
    }
    if !found {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

fn map_repo(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepoRow> {
    Ok(RepoRow {
        id: row.get(0)?,
        worktree_path: PathBuf::from(row.get::<_, String>(1)?),
        bare_path: PathBuf::from(row.get::<_, String>(2)?),
        default_branch: row.get(3)?,
    })
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        branch: row.get(2)?,
        sha: row.get(3)?,
        status: row.get(4)?,
        worktree_dir: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        head_sha: row.get(6)?,
        base_sha: row.get(7)?,
        intent: row.get(8)?,
        intent_source: row.get(9)?,
        error: row.get(10)?,
        review_approved_head_sha: row.get(11)?,
        findings_json: row.get(12)?,
        fixer_session_id: row.get(13)?,
        pr_url: row.get(14)?,
        deliver_repair_attempts: {
            let n: i64 = row.get(15)?;
            u32::try_from(n).unwrap_or(0)
        },
        trusted_config_sha: row.get(16)?,
        pr_title_written: row.get(17)?,
    })
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn canonicalize_path(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |d| d.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fail_stale_running_splits_on_pr_url() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("state.sqlite");
        let db = Db::open(&db_path).unwrap();
        db.upsert_repo("repo1", tmp.path(), &tmp.path().join("bare.git"), "main")
            .unwrap();

        let with_pr = db
            .insert_run("repo1", "feat-pr", "aaa", None, None)
            .unwrap();
        db.set_run_status(&with_pr.id, "running", None).unwrap();
        db.set_pr_url(&with_pr.id, Some("https://example.com/pull/1"))
            .unwrap();

        let no_pr = db
            .insert_run("repo1", "feat-none", "bbb", None, None)
            .unwrap();
        db.set_run_status(&no_pr.id, "running", None).unwrap();

        let stale = db
            .fail_stale_running("daemon restarted while run was in progress")
            .unwrap();
        assert_eq!(stale.len(), 2);

        let with = db.run_by_id(&with_pr.id).unwrap().unwrap();
        assert_eq!(with.status, "ci_monitor_interrupted");
        assert!(with.pr_url.is_some());

        let without = db.run_by_id(&no_pr.id).unwrap().unwrap();
        assert_eq!(without.status, "failed");
        assert!(without.pr_url.is_none());
    }
}
