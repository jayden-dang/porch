use std::io::Read;
use std::path::Path;

use crate::Result;
use crate::db::Db;
use crate::home::db_path;
use crate::rpc;

/// Parse post-receive lines and insert a pending run per updated branch.
///
/// When `PORCH_INTENT` is set and non-empty, it is stored on the run as
/// authoritative intent (`intent_source = env`).
///
/// After insert, requests the daemon to `start_run` (best-effort if down).
///
/// # Errors
///
/// Returns an error if stdin cannot be read, the database cannot be opened, the
/// bare path is unknown, or a run row cannot be inserted.
pub fn notify_push(home: &Path, git_dir: &Path, mut stdin: impl Read) -> Result<Vec<String>> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    let db = Db::open(&db_path(home))?;
    let repo = db
        .repo_by_bare(git_dir)?
        .ok_or_else(|| crate::Error::Other(format!("no repo for bare {}", git_dir.display())))?;
    let (intent, intent_source) = match std::env::var("PORCH_INTENT") {
        Ok(s) if !s.trim().is_empty() => (Some(s), Some("env")),
        _ => (None, None),
    };
    let mut ids = Vec::new();
    for line in buf.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _old = parts.next();
        let Some(new) = parts.next() else { continue };
        let Some(rref) = parts.next() else { continue };
        if new.chars().all(|c| c == '0') {
            continue;
        }
        let branch = rref.strip_prefix("refs/heads/").unwrap_or(rref);
        let row = db.insert_run(&repo.id, branch, new, intent.as_deref(), intent_source)?;
        ids.push(row.id);
    }
    for id in &ids {
        if let Err(e) = rpc::start_run(home, id) {
            tracing::warn!(run_id = %id, "start_run rpc: {e}");
        }
    }
    Ok(ids)
}

/// Resolve `GIT_DIR` from the environment to a canonical absolute path.
///
/// # Errors
///
/// Returns [`crate::Error::Other`] when `GIT_DIR` is unset, or an I/O error
/// when the path cannot be canonicalized.
pub fn git_dir_from_env() -> Result<std::path::PathBuf> {
    let raw =
        std::env::var("GIT_DIR").map_err(|_| crate::Error::Other("GIT_DIR is not set".into()))?;
    let path = std::path::PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.canonicalize()?)
}
