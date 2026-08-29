//! Git operations shell out to `git`. `--git-dir` / `-C` are always absolute.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// Absolute path to a `.git` directory or bare repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDir(PathBuf);

impl GitDir {
    /// Reject relative paths. Gate operations must not depend on cwd discovery.
    ///
    /// # Errors
    ///
    /// Returns [`Error::GitDirNotAbsolute`] when `path` is not absolute.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(Error::GitDirNotAbsolute(path));
        }
        Ok(Self(path))
    }

    /// Borrow the absolute path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git dir must be absolute, got {}", .0.display())]
    GitDirNotAbsolute(PathBuf),
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git {args} failed ({status}): {stderr}")]
    Command {
        args: String,
        status: i32,
        stderr: String,
    },
}

/// Run `git --git-dir=<abs> <args>`.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if `git` cannot be started, or [`Error::Command`]
/// if the process exits non-zero.
pub fn run(git_dir: &GitDir, args: &[&str]) -> Result<Output, Error> {
    let mut cmd = Command::new("git");
    cmd.arg(format!("--git-dir={}", git_dir.as_path().display()));
    cmd.args(args);
    // Isolation: do not inherit the caller's hooks for inspection commands.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    if !output.status.success() {
        return Err(Error::Command {
            args: args.join(" "),
            status: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        });
    }
    Ok(output)
}

/// Run `git -C <abs work tree> <args>`.
///
/// # Errors
///
/// Returns [`Error::GitDirNotAbsolute`] when `work_tree` is relative,
/// [`Error::Spawn`] if `git` cannot be started, or [`Error::Command`] if the
/// process exits non-zero.
pub fn run_c(work_tree: &Path, args: &[&str]) -> Result<Output, Error> {
    if !work_tree.is_absolute() {
        return Err(Error::GitDirNotAbsolute(work_tree.to_path_buf()));
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(work_tree);
    cmd.args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    if !output.status.success() {
        return Err(Error::Command {
            args: args.join(" "),
            status: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        });
    }
    Ok(output)
}

/// `git init --bare <path>` where `path` is absolute.
///
/// # Errors
///
/// Returns [`Error::GitDirNotAbsolute`] when `path` is relative,
/// [`Error::Spawn`] if parent dirs cannot be created or `git` cannot be
/// started, or [`Error::Command`] if `git init --bare` exits non-zero.
pub fn init_bare(path: &Path) -> Result<GitDir, Error> {
    let git_dir = GitDir::new(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Spawn)?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("init").arg("--bare").arg(path);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    if !output.status.success() {
        return Err(Error::Command {
            args: format!("init --bare {}", path.display()),
            status: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        });
    }
    Ok(git_dir)
}

/// `git worktree add --detach <path> <sha>` from a bare (or main) git dir.
///
/// # Errors
///
/// Returns git errors if the path is relative or `worktree add` fails.
pub fn worktree_add_detach(git_dir: &GitDir, path: &Path, sha: &str) -> Result<(), Error> {
    if !path.is_absolute() {
        return Err(Error::GitDirNotAbsolute(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Spawn)?;
    }
    let path_s = path.to_string_lossy();
    run(
        git_dir,
        &["worktree", "add", "--detach", path_s.as_ref(), sha],
    )?;
    Ok(())
}

/// `git worktree remove --force <path>`.
///
/// # Errors
///
/// Returns git errors if remove fails. Missing worktrees may still error;
/// callers often ignore that after a best-effort filesystem delete.
pub fn worktree_remove_force(git_dir: &GitDir, path: &Path) -> Result<(), Error> {
    let path_s = path.to_string_lossy();
    run(git_dir, &["worktree", "remove", "--force", path_s.as_ref()])?;
    Ok(())
}

/// `git fetch <remote> <refspec>` with absolute `--git-dir`.
///
/// # Errors
///
/// Returns git errors if fetch fails (including unreachable remote).
pub fn fetch(git_dir: &GitDir, remote: &str, refspec: &str) -> Result<(), Error> {
    run(git_dir, &["fetch", remote, refspec])?;
    Ok(())
}

/// `git -C <work> rebase <onto>`.
///
/// # Errors
///
/// Returns [`Error::Command`] on conflict or other rebase failure.
pub fn rebase(work_tree: &Path, onto: &str) -> Result<(), Error> {
    run_c(work_tree, &["rebase", onto])?;
    Ok(())
}

/// `git -C <work> rebase --abort`.
///
/// # Errors
///
/// Returns git errors if abort fails (e.g. no rebase in progress).
pub fn rebase_abort(work_tree: &Path) -> Result<(), Error> {
    run_c(work_tree, &["rebase", "--abort"])?;
    Ok(())
}

/// `git -C <work> reset --hard <rev>`.
///
/// # Errors
///
/// Returns git errors if reset fails.
pub fn reset_hard(work_tree: &Path, rev: &str) -> Result<(), Error> {
    run_c(work_tree, &["reset", "--hard", rev])?;
    Ok(())
}

/// Resolve a revision to a full SHA (`git rev-parse`).
///
/// # Errors
///
/// Returns git errors if the rev does not resolve.
pub fn rev_parse(git_dir: &GitDir, rev: &str) -> Result<String, Error> {
    let out = run(git_dir, &["rev-parse", rev])?;
    Ok(stdout_trim(&out))
}

/// Resolve a revision from a work tree (`git -C … rev-parse`).
///
/// # Errors
///
/// Returns git errors if the rev does not resolve.
pub fn rev_parse_c(work_tree: &Path, rev: &str) -> Result<String, Error> {
    let out = run_c(work_tree, &["rev-parse", rev])?;
    Ok(stdout_trim(&out))
}

/// `git merge-base --is-ancestor <maybe_ancestor> <tip>` (exit 0 = yes).
///
/// # Errors
///
/// Returns [`Error::Spawn`] on I/O failure. Non-zero exit from git that is not
/// status 1 is returned as [`Error::Command`]. Status 1 means "not an ancestor".
pub fn is_ancestor(work_tree: &Path, maybe_ancestor: &str, tip: &str) -> Result<bool, Error> {
    if !work_tree.is_absolute() {
        return Err(Error::GitDirNotAbsolute(work_tree.to_path_buf()));
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(work_tree);
    cmd.args(["merge-base", "--is-ancestor", maybe_ancestor, tip]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Command {
            args: format!("merge-base --is-ancestor {maybe_ancestor} {tip}"),
            status: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        }),
    }
}

/// Whether `git diff --quiet <range>` reports no difference.
///
/// Diff command failures (bad range, etc.) are errors — not treated as empty.
///
/// # Errors
///
/// Returns git errors other than exit status 1 (differences found).
pub fn diff_is_empty(work_tree: &Path, range: &str) -> Result<bool, Error> {
    if !work_tree.is_absolute() {
        return Err(Error::GitDirNotAbsolute(work_tree.to_path_buf()));
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(work_tree);
    cmd.args(["diff", "--quiet", range]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Command {
            args: format!("diff --quiet {range}"),
            status: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        }),
    }
}

/// Trimmed stdout from a successful `git` invocation.
#[must_use]
pub fn stdout_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn redact(stderr: &str) -> String {
    // Keep logs free of credential material if git ever echoes a URL.
    stderr
        .lines()
        .map(|line| {
            if line.contains("://") && (line.contains('@') || line.contains("password")) {
                "<redacted>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reserved for correction-commit isolation (empty hooksPath). Unused in M1.
#[must_use]
pub fn timeout_placeholder() -> Duration {
    Duration::from_secs(60)
}

/// Convert a path-like value into an `OsString` for `Command` args.
#[must_use]
pub fn arg_os(s: impl AsRef<OsStr>) -> std::ffi::OsString {
    s.as_ref().to_os_string()
}
