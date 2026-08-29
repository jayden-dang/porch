use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use ulid::Ulid;

use crate::Result;

const RUN_SELECT_FROM: &str =
    "SELECT id, repo_id, branch, sha, status, worktree_dir, head_sha, base_sha,
                    intent, intent_source, error, review_approved_head_sha, findings_json
             FROM runs";
const RUN_SELECT: &str =
    "SELECT id, repo_id, branch, sha, status, worktree_dir, head_sha, base_sha,
                    intent, intent_source, error, review_approved_head_sha, findings_json
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
        Ok(Self {
            conn: Mutex::new(conn),
        })
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

    /// Mark all `running` runs as failed; return them (for worktree cleanup).
    ///
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
        conn.execute(
            "UPDATE runs SET status = 'failed', error = ?1 WHERE status = 'running'",
            rusqlite::params![error],
        )?;
        for run in &mut stale {
            run.status = "failed".into();
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
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
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
