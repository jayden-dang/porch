use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use porch_git::{self, GitDir};

use crate::Result;
use crate::db::Db;
use crate::home::{db_path, repos_dir};
use crate::id::repo_id_for;

#[derive(Clone, Copy)]
pub struct InitOptions<'a> {
    pub work_tree: &'a Path,
    pub porch_home: &'a Path,
    pub porch_bin: &'a Path,
    pub start_daemon: bool,
}

pub struct InitResult {
    pub repo_id: String,
    pub bare_path: PathBuf,
}

/// Install the named remote, bare repo, and hooks. Optionally start the daemon.
///
/// # Errors
///
/// Returns an error if the work tree or porch home cannot be canonicalized,
/// git or hooks setup fails, the database cannot be updated, or the daemon
/// cannot be started when requested.
pub fn init(opts: InitOptions<'_>) -> Result<InitResult> {
    let work = opts.work_tree.canonicalize()?;
    std::fs::create_dir_all(opts.porch_home)?;
    let porch_home = opts.porch_home.canonicalize()?;
    let repo_id = existing_repo_id(&work)?.unwrap_or_else(|| repo_id_for(&work));
    let bare_path = repos_dir(&porch_home).join(format!("{repo_id}.git"));
    porch_git::init_bare(&bare_path)?;
    let bare_path = bare_path.canonicalize()?;
    write_hook(
        &bare_path.join("hooks/pre-receive"),
        &porch_home,
        opts.porch_bin,
        "admit-push",
    )?;
    write_hook(
        &bare_path.join("hooks/post-receive"),
        &porch_home,
        opts.porch_bin,
        "notify-push",
    )?;
    add_or_set_remote(&work, &bare_path)?;
    porch_git::run_c(&work, &["config", "porch.repo-id", &repo_id])?;
    let db = Db::open(&db_path(&porch_home))?;
    db.upsert_repo(&repo_id, &work, &bare_path)?;
    if opts.start_daemon {
        crate::ensure_daemon(opts.porch_bin, &porch_home)?;
    }
    Ok(InitResult { repo_id, bare_path })
}

fn existing_repo_id(work: &Path) -> Result<Option<String>> {
    let git_dir = GitDir::new(work.join(".git"))?;
    match porch_git::run(&git_dir, &["config", "--get", "porch.repo-id"]) {
        Ok(out) => {
            let id = porch_git::stdout_trim(&out);
            if id.is_empty() {
                Ok(None)
            } else {
                Ok(Some(id))
            }
        }
        Err(_) => Ok(None),
    }
}

fn add_or_set_remote(work: &Path, bare: &Path) -> Result<()> {
    let url = bare.to_string_lossy().into_owned();
    if porch_git::run_c(work, &["remote", "add", "porch", &url]).is_ok() {
        Ok(())
    } else {
        porch_git::run_c(work, &["remote", "set-url", "porch", &url])?;
        Ok(())
    }
}

fn write_hook(path: &Path, home: &Path, bin: &Path, sub: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let home_s = shell_single(&home.to_string_lossy());
    let bin_s = shell_single(&bin.to_string_lossy());
    let body = format!("#!/bin/sh\nexport PORCH_HOME={home_s}\nexec {bin_s} daemon {sub}\n");
    std::fs::write(path, body)?;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

fn shell_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
