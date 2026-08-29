use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use ulid::Ulid;

use crate::Result;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub id: String,
    pub worktree_path: PathBuf,
    pub bare_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: String,
    pub repo_id: String,
    pub branch: String,
    pub sha: String,
    pub status: String,
}

impl Db {
    /// Open (or create) the `SQLite` database and apply M1 schema.
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
            ",
        )?;
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
    pub fn upsert_repo(&self, id: &str, worktree: &Path, bare: &Path) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO repos (id, worktree_path, bare_path, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
               worktree_path = excluded.worktree_path,
               bare_path = excluded.bare_path",
            rusqlite::params![id, path_str(worktree), path_str(bare), now_secs()],
        )?;
        Ok(())
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
        let mut stmt =
            conn.prepare("SELECT id, worktree_path, bare_path FROM repos WHERE bare_path = ?1")?;
        let mut rows = stmt.query(rusqlite::params![path_str(&want)])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(RepoRow {
                id: row.get(0)?,
                worktree_path: PathBuf::from(row.get::<_, String>(1)?),
                bare_path: PathBuf::from(row.get::<_, String>(2)?),
            }));
        }
        // Defense in depth: match even if a stored path was not canonicalized.
        let mut stmt = conn.prepare("SELECT id, worktree_path, bare_path FROM repos")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let bare_path = PathBuf::from(row.get::<_, String>(2)?);
            if canonicalize_path(&bare_path) == want {
                return Ok(Some(RepoRow {
                    id: row.get(0)?,
                    worktree_path: PathBuf::from(row.get::<_, String>(1)?),
                    bare_path,
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
    pub fn insert_run(&self, repo_id: &str, branch: &str, sha: &str) -> Result<RunRow> {
        let id = Ulid::new().to_string();
        let conn = self.conn.lock().expect("db mutex");
        conn.execute(
            "INSERT INTO runs (id, repo_id, branch, sha, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            rusqlite::params![id, repo_id, branch, sha, now_secs()],
        )?;
        Ok(RunRow {
            id,
            repo_id: repo_id.to_string(),
            branch: branch.to_string(),
            sha: sha.to_string(),
            status: "pending".into(),
        })
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
        let mut stmt = conn.prepare(
            "SELECT id, repo_id, branch, sha, status FROM runs WHERE repo_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo_id], |row| {
            Ok(RunRow {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                branch: row.get(2)?,
                sha: row.get(3)?,
                status: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
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
