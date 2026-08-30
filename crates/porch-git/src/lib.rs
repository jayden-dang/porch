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

/// Ensure a fetch refspec is force (`+…`) when the caller omitted `+`.
#[must_use]
pub fn force_fetch_refspec(refspec: &str) -> String {
    if refspec.starts_with('+') {
        refspec.to_string()
    } else {
        format!("+{refspec}")
    }
}

/// Argv for `git fetch` after `--git-dir` (prune disabled, force refspec).
#[must_use]
pub fn fetch_git_args(remote: &str, refspec: &str) -> Vec<String> {
    vec![
        "-c".into(),
        "fetch.prune=false".into(),
        "fetch".into(),
        remote.into(),
        force_fetch_refspec(refspec),
    ]
}

/// `git -c fetch.prune=false fetch <remote> +<refspec>` with absolute `--git-dir`.
///
/// Adds a leading `+` when `refspec` does not already include one.
///
/// # Errors
///
/// Returns git errors if fetch fails (including unreachable remote).
pub fn fetch(git_dir: &GitDir, remote: &str, refspec: &str) -> Result<(), Error> {
    let args = fetch_git_args(remote, refspec);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    run(git_dir, &args_ref)?;
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

/// `git show <sha>:<path>` → raw blob bytes.
///
/// Missing path in a readable commit returns `Ok(None)`. An unreadable commit
/// (or other git failure) is an error. Callers that need text must decode UTF-8
/// themselves (fail closed rather than lossy-replace).
///
/// # Errors
///
/// Returns [`Error::Command`] / [`Error::Spawn`] when the commit cannot be read
/// or `git show` fails for a reason other than a missing path.
pub fn show_path_at(git_dir: &GitDir, sha: &str, path: &str) -> Result<Option<Vec<u8>>, Error> {
    // Fail closed if the commit itself is not readable.
    let commitish = format!("{sha}^{{commit}}");
    run(git_dir, &["rev-parse", "--verify", &commitish])?;

    let spec = format!("{sha}:{path}");
    let mut cmd = Command::new("git");
    cmd.arg(format!("--git-dir={}", git_dir.as_path().display()));
    cmd.args(["show", &spec]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().map_err(Error::Spawn)?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    let stderr = redact(&String::from_utf8_lossy(&output.stderr));
    // `git show` reports missing paths this way even when the commit exists.
    if stderr.contains("does not exist in") {
        return Ok(None);
    }
    Err(Error::Command {
        args: format!("show {spec}"),
        status: output.status.code().unwrap_or(-1),
        stderr,
    })
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

/// `git update-ref <ref> <sha>` in a bare/git-dir (creates or moves the ref).
///
/// # Errors
///
/// Returns git errors if the ref cannot be updated.
pub fn update_ref(git_dir: &GitDir, refname: &str, sha: &str) -> Result<(), Error> {
    run(git_dir, &["update-ref", refname, sha])?;
    Ok(())
}

/// Delete a ref (`git update-ref -d <ref>`). Missing refs are ok.
///
/// # Errors
///
/// Returns git errors other than a missing ref.
pub fn delete_ref(git_dir: &GitDir, refname: &str) -> Result<(), Error> {
    match run(git_dir, &["update-ref", "-d", refname]) {
        Ok(_) => Ok(()),
        Err(Error::Command { stderr, .. }) if stderr.contains("unable to resolve") => Ok(()),
        Err(e) => Err(e),
    }
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

/// Paths changed in `range` (`git diff --name-only <range>`).
///
/// # Errors
///
/// Returns git errors if the diff cannot be produced.
pub fn diff_name_only(work_tree: &Path, range: &str) -> Result<Vec<String>, Error> {
    let out = run_c(work_tree, &["diff", "--name-only", range])?;
    let mut files = Vec::new();
    for line in stdout_trim(&out).lines() {
        if !line.is_empty() {
            files.push(line.to_string());
        }
    }
    Ok(files)
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

/// Default duration for long-running git operations that take an explicit timeout.
#[must_use]
pub fn timeout_placeholder() -> Duration {
    Duration::from_secs(60)
}

/// Convert a path-like value into an `OsString` for `Command` args.
#[must_use]
pub fn arg_os(s: impl AsRef<OsStr>) -> std::ffi::OsString {
    s.as_ref().to_os_string()
}

/// Result of observing a remote tip before push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTip {
    /// Ref does not exist on the remote.
    Absent,
    /// Ref resolves to this full SHA.
    Present(String),
}

/// How to update the remote branch given an observed tip and local SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushDecision {
    /// Ref absent: plain `push <sha>:refs/heads/<branch>`.
    NewBranch,
    /// Remote already at `local_sha`: no objects move.
    UpToDate,
    /// Force-with-lease anchored at the just-observed remote SHA.
    Lease { observed_sha: String },
    /// Remote has commits not incorporated into validated history.
    RefuseIncorporate,
}

/// Decide push strategy from `ls-remote` result and whether remote tip is
/// incorporated (`rev-list --cherry-pick --right-only` empty).
///
/// `remote_incorporated` is only consulted when the tip is present and differs
/// from `local_sha`. Callers must fail closed before calling when incorporate
/// cannot be verified.
#[must_use]
pub fn resolve_push_decision(
    tip: &RemoteTip,
    local_sha: &str,
    remote_incorporated: bool,
) -> PushDecision {
    match tip {
        RemoteTip::Absent => PushDecision::NewBranch,
        RemoteTip::Present(remote_sha) if remote_sha == local_sha => PushDecision::UpToDate,
        RemoteTip::Present(remote_sha) if remote_incorporated => PushDecision::Lease {
            observed_sha: remote_sha.clone(),
        },
        RemoteTip::Present(_) => PushDecision::RefuseIncorporate,
    }
}

/// `git ls-remote <remote> <ref>` → tip SHA or absent. Fail closed on git error.
///
/// # Errors
///
/// Returns [`Error::Command`] / [`Error::Spawn`] when the remote cannot be
/// queried. Empty stdout is [`RemoteTip::Absent`], not an error.
pub fn ls_remote_sha(git_dir: &GitDir, remote: &str, refname: &str) -> Result<RemoteTip, Error> {
    let out = run(git_dir, &["ls-remote", remote, refname])?;
    let text = stdout_trim(&out);
    if text.is_empty() {
        return Ok(RemoteTip::Absent);
    }
    // `ls-remote` lines: `<sha>\t<ref>`. Take the first field of the first line.
    let sha = text
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default();
    if sha.len() < 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Command {
            args: format!("ls-remote {remote} {refname}"),
            status: -1,
            stderr: format!("unverifiable ls-remote output: {text}"),
        });
    }
    Ok(RemoteTip::Present(sha.to_string()))
}

/// Whether every commit reachable from `remote_sha` but not `local_sha` is a
/// cherry-pick equivalent of something on the local side (empty
/// `rev-list --cherry-pick --right-only local...remote`).
///
/// Optional `base_sha` excludes history at/before the integration base.
///
/// # Errors
///
/// Returns git errors when objects are missing or `rev-list` fails.
pub fn remote_commits_incorporated(
    git_dir: &GitDir,
    local_sha: &str,
    remote_sha: &str,
    base_sha: Option<&str>,
) -> Result<bool, Error> {
    let range = format!("{local_sha}...{remote_sha}");
    let base_arg = base_sha.map(|base| format!("^{base}"));
    let mut args = vec!["rev-list", "--cherry-pick", "--right-only", range.as_str()];
    if let Some(ref base) = base_arg {
        args.push(base.as_str());
    }
    let out = run(git_dir, &args)?;
    Ok(stdout_trim(&out).is_empty())
}

/// Push exact commit SHA to `remote` as `refname` (`<sha>:<ref>`), never `HEAD`.
///
/// - [`PushDecision::NewBranch`][]: plain push
/// - [`PushDecision::UpToDate`][]: no-op success
/// - [`PushDecision::Lease`][]: `--force-with-lease=<ref>:<observed>`
/// - [`PushDecision::RefuseIncorporate`][]: error without mutating
///
/// Always passes `--no-verify`: deliver runs from the bare gate (or a disposable
/// worktree), and author lefthook/`pre-push` must not re-run after certify. A
/// worktree `lefthook install` can also write `pre-push` into the shared bare
/// hooks dir; without `--no-verify` that blocks origin forward.
///
/// After a mutating push, callers should re-`ls-remote` and require equality.
///
/// # Errors
///
/// Returns [`Error::Command`] when push is refused or git fails.
pub fn push_exact_sha(
    git_dir: &GitDir,
    remote: &str,
    refname: &str,
    exact_sha: &str,
    decision: PushDecision,
) -> Result<(), Error> {
    match decision {
        PushDecision::UpToDate => Ok(()),
        PushDecision::RefuseIncorporate => Err(Error::Command {
            args: format!("push {remote} {exact_sha}:{refname}"),
            status: -1,
            stderr: "refuse: remote has commits not incorporated into validated history".into(),
        }),
        PushDecision::NewBranch => {
            let spec = format!("{exact_sha}:{refname}");
            run(git_dir, &["push", "--no-verify", remote, &spec])?;
            Ok(())
        }
        PushDecision::Lease { observed_sha } => {
            let lease = format!("--force-with-lease={refname}:{observed_sha}");
            let spec = format!("{exact_sha}:{refname}");
            run(git_dir, &["push", "--no-verify", &lease, remote, &spec])?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod push_decision_tests {
    use super::*;

    #[test]
    fn absent_is_new_branch() {
        assert_eq!(
            resolve_push_decision(&RemoteTip::Absent, "abc", true),
            PushDecision::NewBranch
        );
    }

    #[test]
    fn same_sha_is_up_to_date() {
        assert_eq!(
            resolve_push_decision(&RemoteTip::Present("abc".into()), "abc", false),
            PushDecision::UpToDate
        );
    }

    #[test]
    fn incorporated_divergence_is_lease() {
        assert_eq!(
            resolve_push_decision(&RemoteTip::Present("old".into()), "new", true),
            PushDecision::Lease {
                observed_sha: "old".into()
            }
        );
    }

    #[test]
    fn unincorporated_refuses() {
        assert_eq!(
            resolve_push_decision(&RemoteTip::Present("other".into()), "local", false),
            PushDecision::RefuseIncorporate
        );
    }
}
