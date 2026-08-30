//! post-receive: record runs and ask the daemon to start them.

use std::io::Read;
use std::path::Path;

use crate::Result;
use crate::db::Db;
use crate::home::db_path;
use crate::rpc;

/// Parse post-receive lines and insert a pending run per updated branch.
///
/// Intent (E17): non-empty `intent_cli` wins (source `cli`); else non-empty
/// `PORCH_INTENT` (source `env`); empty skips the intent phase and does not fail.
///
/// After insert, requests the daemon to `start_run` (best-effort if down).
///
/// # Errors
///
/// Returns an error if stdin cannot be read, the database cannot be opened, the
/// bare path is unknown, or a run row cannot be inserted.
pub fn notify_push(
    home: &Path,
    git_dir: &Path,
    stdin: impl Read,
    intent_cli: Option<&str>,
) -> Result<Vec<String>> {
    notify_push_inner(home, git_dir, stdin, intent_cli)
}

fn notify_push_inner(
    home: &Path,
    git_dir: &Path,
    mut stdin: impl Read,
    intent_cli: Option<&str>,
) -> Result<Vec<String>> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    let db = Db::open(&db_path(home))?;
    let repo = db
        .repo_by_bare(git_dir)?
        .ok_or_else(|| crate::Error::Other(format!("no repo for bare {}", git_dir.display())))?;
    let (intent, intent_source) = resolve_intent(intent_cli, std::env::var("PORCH_INTENT").ok());
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
        // Tags and other non-branch refs still update the bare; do not enqueue runs.
        let Some(branch) = rref.strip_prefix("refs/heads/") else {
            continue;
        };
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

/// CLI `--intent` preferred when the flag is present; otherwise env value.
/// Empty string skips (does not fail).
fn resolve_intent(
    intent_cli: Option<&str>,
    intent_env: Option<String>,
) -> (Option<String>, Option<&'static str>) {
    if let Some(raw) = intent_cli {
        if raw.trim().is_empty() {
            return (None, None);
        }
        return (Some(raw.to_string()), Some("cli"));
    }
    match intent_env {
        Some(s) if !s.trim().is_empty() => (Some(s), Some("env")),
        _ => (None, None),
    }
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

#[cfg(test)]
mod tests {
    use super::resolve_intent;

    #[test]
    fn cli_intent_preferred_over_env() {
        let (intent, src) = resolve_intent(Some("from-cli"), Some("from-env".into()));
        assert_eq!(intent.as_deref(), Some("from-cli"));
        assert_eq!(src, Some("cli"));
    }

    #[test]
    fn empty_cli_intent_skips_without_env_fallback() {
        let (intent, src) = resolve_intent(Some("  "), Some("from-env".into()));
        assert!(intent.is_none());
        assert!(src.is_none());
    }

    #[test]
    fn env_intent_when_cli_absent() {
        let (intent, src) = resolve_intent(None, Some("from-env".into()));
        assert_eq!(intent.as_deref(), Some("from-env"));
        assert_eq!(src, Some("env"));
    }

    #[test]
    fn empty_env_skips() {
        let (intent, src) = resolve_intent(None, Some("  ".into()));
        assert!(intent.is_none());
        assert!(src.is_none());
    }
}
