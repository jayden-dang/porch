//! Operator prerequisite check (`porch doctor`).

use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitCode};

use porch_gate::{DaemonCondition, daemon_condition, look, porch_home, socket_path};
use porch_review::floor::FloorState;
use porch_review::{
    REVIEW_BIN_ENV, floor, is_executable, load_home_config, resolve_bin, review_bin, which,
};

const GH_BIN_ENV: &str = "PORCH_GH_BIN";
const FIXER_BIN_ENV: &str = "PORCH_FIXER_BIN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Info,
    Fail,
}

struct Check {
    level: Level,
    name: String,
    detail: String,
}

impl Check {
    fn new(level: Level, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level,
            name: name.into(),
            detail: detail.into(),
        }
    }
}

/// Run doctor checks and print a human report.
///
/// Exit 0 when hard prerequisites for a push are present (`git`).
/// Exit 1 when a hard check fails.
pub fn run() -> io::Result<ExitCode> {
    let checks = collect_checks();
    let mut out = io::stdout();
    writeln!(out, "porch doctor")?;
    writeln!(out, "------------")?;
    for c in &checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Fail => "FAIL",
        };
        writeln!(out, "[{tag}] {}: {}", c.name, c.detail)?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "hint: certify runs in a disposable worktree without node_modules;"
    )?;
    writeln!(
        out,
        "      `bun run format` / similar need `biome` (or your formatter) on PATH."
    )?;

    let hard_fail = checks.iter().any(|c| c.level == Level::Fail);
    Ok(if hard_fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn collect_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(check_porch_bin());
    if let Some(c) = check_cargo_bin_on_path() {
        checks.push(c);
    }
    checks.push(check_git());
    checks.extend(check_home_and_daemon());
    checks.push(check_floor());
    checks.push(check_review());
    checks.push(check_gh());
    checks.push(check_fixer());
    checks.extend(check_repo_tools());
    checks
}

fn check_porch_bin() -> Check {
    match env::current_exe() {
        Ok(p) => Check::new(
            Level::Ok,
            "porch",
            format!("{} (version {})", p.display(), env!("CARGO_PKG_VERSION")),
        ),
        Err(e) => Check::new(
            Level::Warn,
            "porch",
            format!("could not resolve current_exe: {e}"),
        ),
    }
}

/// Warn when `$CARGO_HOME/bin/porch` (default `~/.cargo/bin`) exists but that dir is not on PATH.
fn check_cargo_bin_on_path() -> Option<Check> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cargo")))?;
    let cargo_bin = cargo_home.join("bin");
    let cargo_porch = cargo_bin.join("porch");
    if !cargo_porch.is_file() {
        return None;
    }
    let on_path = env::var_os("PATH").is_some_and(|p| {
        env::split_paths(&p).any(|dir| {
            if dir == cargo_bin {
                return true;
            }
            match (dir.canonicalize(), cargo_bin.canonicalize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        })
    });
    if on_path {
        Some(Check::new(
            Level::Ok,
            "PATH",
            format!(
                "{} on PATH ({})",
                cargo_bin.display(),
                cargo_porch.display()
            ),
        ))
    } else {
        Some(Check::new(
            Level::Warn,
            "PATH",
            format!(
                "`{}` exists but {} is not on PATH — add it to your shell profile \
(e.g. export PATH=\"{}:$PATH\")",
                cargo_porch.display(),
                cargo_bin.display(),
                cargo_bin.display()
            ),
        ))
    }
}

fn check_git() -> Check {
    let bin = porch_git::git_bin();
    let overridden = env::var_os(porch_git::GIT_BIN_ENV).is_some();
    match resolve_bin(&bin) {
        Some(p) if overridden => Check::new(
            Level::Ok,
            "git",
            format!("{} ({})", p.display(), porch_git::GIT_BIN_ENV),
        ),
        Some(p) => Check::new(Level::Ok, "git", p.display().to_string()),
        None => Check::new(
            Level::Fail,
            "git",
            format!("`{bin}` not found (required for push)"),
        ),
    }
}

fn check_home_and_daemon() -> Vec<Check> {
    let home = porch_home();
    if !home.exists() {
        return vec![Check::new(
            Level::Info,
            "PORCH_HOME",
            format!("{} (will be created on `porch init`)", home.display()),
        )];
    }
    let mut out = vec![Check::new(
        Level::Ok,
        "PORCH_HOME",
        format!("{} (exists)", home.display()),
    )];
    // Resolved without starting a daemon, so asking does not change the answer, and
    // bounded, so it answers even when the daemon is wedged (`DFAULT-4.1`).
    out.push(daemon_check(&home));
    out.extend(inspect_checks(&home));
    out
}

fn inspect_checks(home: &Path) -> Vec<Check> {
    let cwd = env::current_dir().ok();
    let work = cwd.as_deref().filter(|p| {
        std::process::Command::new("git")
            .current_dir(p)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let report = look(home, work, 1);
    let mut out = Vec::new();
    if report.database.readable {
        out.push(Check::new(
            Level::Ok,
            "state database",
            "readable without writing",
        ));
    } else {
        let reason = report.database.reason.as_deref().unwrap_or("unreadable");
        // A missing DB on a fresh PORCH_HOME is info, not a fail.
        let level = if reason.contains("unable to open")
            || reason.contains("cannot open")
            || reason.contains("no such file")
        {
            Level::Info
        } else {
            Level::Warn
        };
        out.push(Check::new(level, "state database", reason.to_string()));
    }
    if work.is_some() {
        out.push(Check::new(
            Level::Info,
            "recovery tips",
            format!("{} — `porch status` lists them", report.recovery_tips.len()),
        ));
    }
    out
}

fn daemon_check(home: &Path) -> Check {
    let cond = daemon_condition(home);
    let sock = socket_path(home);
    match cond {
        DaemonCondition::Ready { .. } => Check::new(
            Level::Ok,
            "daemon",
            format!("{} ({})", cond.summary(), sock.display()),
        ),
        // A daemon that refuses to start or has gone quiet is a fault the operator has
        // to act on, not a neutral fact about their machine.
        DaemonCondition::Refusing { .. } | DaemonCondition::NotAnswering { .. } => Check::new(
            Level::Fail,
            "daemon",
            format!("{} — {}", cond.summary(), cond.remedy()),
        ),
        // Not running is the ordinary state of a fresh checkout. `porch init` starts a
        // daemon; the post-receive hook does not — it execs `porch daemon notify-push`,
        // which makes a best-effort RPC and logs a warning (`DFAULT-4.3`).
        DaemonCondition::Unreachable { .. } => Check::new(
            Level::Info,
            "daemon",
            format!(
                "not running (socket {}); started by `porch init` or `porch daemon start`",
                sock.display()
            ),
        ),
    }
}

/// Report the floor through the resolver's own rule, and report what was observed.
///
/// Reporting the observed artifact identity is not a verdict on it: porch has no
/// expected value to compare against, and establishing one would extend a guarantee
/// `FLOOR-9.2` declined. Printing what the assurance record will store lets an
/// operator compare two installations without porch deciding which is right.
fn check_floor() -> Check {
    let state = floor::state();
    let remedy = state.remedy();
    match state {
        FloorState::Ready {
            sibling,
            artifact_identity,
        } => Check::new(
            Level::Ok,
            "floor",
            format!(
                "{} (mandatory deterministic floor sibling) identity={artifact_identity}",
                sibling.display()
            ),
        ),
        FloorState::LaunchReplaced { launch } => Check::new(
            Level::Warn,
            "floor",
            format!(
                "porch was replaced while running: `{}` no longer names a file, so the sibling \
next to it is not the floor this porch shipped with — {remedy}",
                launch.display()
            ),
        ),
        FloorState::Unresolved { reason } => Check::new(
            Level::Warn,
            "floor",
            format!("{reason} — {remedy}; every run requires this sibling (not PATH)"),
        ),
    }
}

fn check_review() -> Check {
    let home = porch_home();
    if porch_review::env_override(REVIEW_BIN_ENV).is_some() {
        let bin = review_bin();
        return match resolve_bin(&bin) {
            Some(p) => Check::new(
                Level::Ok,
                "review",
                format!("{} ({REVIEW_BIN_ENV})", p.display()),
            ),
            None => Check::new(
                Level::Warn,
                "review",
                format!("`{bin}` not found ({REVIEW_BIN_ENV}); needed for a complete run"),
            ),
        };
    }
    if porch_review::review_uses_agent(Some(&home)) {
        match porch_review::agent_review_bin(&home) {
            Ok(agent) => {
                if let Some(p) = resolve_bin(&agent) {
                    let engine = load_home_config(&home)
                        .ok()
                        .flatten()
                        .and_then(|c| c.review.engine)
                        .unwrap_or_else(|| "agent".into());
                    return Check::new(
                        Level::Ok,
                        "review",
                        format!("{} (engine={engine}, agent)", p.display()),
                    );
                }
                return Check::new(
                    Level::Warn,
                    "review",
                    format!(
                        "`{agent}` not found — run `porch setup --engine agent` or set PORCH_REVIEW_AGENT_BIN"
                    ),
                );
            }
            Err(e) => {
                return Check::new(Level::Warn, "review", e.to_string());
            }
        }
    }
    let bin = review_bin();
    let resolved = resolve_bin(&bin);
    if let Some(p) = resolved {
        if let Ok(Some(cfg)) = load_home_config(&home) {
            if let Some(engine) = cfg.review.engine.as_deref() {
                let wrap = cfg
                    .review
                    .wrapper
                    .as_deref()
                    .unwrap_or_else(|| p.to_str().unwrap_or(""));
                return Check::new(
                    Level::Ok,
                    "review",
                    format!("{} (engine={engine}, wrapper={wrap})", p.display()),
                );
            }
        }
        return Check::new(
            Level::Ok,
            "review",
            format!("{} (PATH default `review`)", p.display()),
        );
    }
    Check::new(
        Level::Warn,
        "review",
        format!(
            "judgment engine not configured — run `porch setup` (`quality` is floor-only; `agent` still needs the porch-quality sibling) or set {REVIEW_BIN_ENV}"
        ),
    )
}

fn check_gh() -> Check {
    let home = porch_home();
    let from_config = load_home_config(&home)
        .ok()
        .flatten()
        .and_then(|c| c.github.bin);
    let gh_bin = env::var(GH_BIN_ENV)
        .ok()
        .or(from_config)
        .unwrap_or_else(|| "gh".into());
    match resolve_bin(&gh_bin) {
        Some(p) => Check::new(
            Level::Ok,
            "gh",
            format!("{} (needed for deliver)", p.display()),
        ),
        None => Check::new(
            Level::Warn,
            "gh",
            format!(
                "`{gh_bin}` not found — set {GH_BIN_ENV} or install on PATH (needed for deliver)"
            ),
        ),
    }
}

fn check_fixer() -> Check {
    let home = porch_home();
    let from_config = load_home_config(&home)
        .ok()
        .flatten()
        .and_then(|c| c.fixer.bin);
    match env::var(FIXER_BIN_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or(from_config)
    {
        Some(bin) => match resolve_bin(bin.trim()) {
            Some(p) => Check::new(
                Level::Ok,
                "fixer",
                format!("{} ({FIXER_BIN_ENV} or config)", p.display()),
            ),
            None => Check::new(
                Level::Warn,
                "fixer",
                format!(
                    "`{bin}` not found — set {FIXER_BIN_ENV} to a real binary (needed for `porch agent respond fix`)"
                ),
            ),
        },
        None => Check::new(
            Level::Warn,
            "fixer",
            format!(
                "{FIXER_BIN_ENV} unset — required for `porch agent respond fix` (optional at setup)"
            ),
        ),
    }
}

fn check_repo_tools() -> Vec<Check> {
    ["biome", "bun", "cargo", "just", "moon"]
        .iter()
        .map(|tool| match which(tool) {
            Some(p) => Check::new(
                Level::Info,
                *tool,
                format!("{} (repo-specific)", p.display()),
            ),
            None => Check::new(
                Level::Info,
                *tool,
                "not on PATH (repo-specific; only needed if certify commands use it)",
            ),
        })
        .collect()
}

/// True when `name` resolves on PATH or as an absolute/relative existing path.
#[must_use]
pub fn bin_on_path(name_or_path: &str) -> bool {
    resolve_bin(name_or_path).is_some_and(|p| is_executable(&p))
}

/// Best-effort current branch name for init hints (`HEAD` if detached).
#[must_use]
pub fn current_branch(work: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(work)
        .output()
        .ok();
    out.and_then(|o| {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        }
    })
    .unwrap_or_else(|| "HEAD".into())
}

#[cfg(test)]
mod tests {
    use porch_review::floor::sibling_of;
    use std::path::PathBuf;

    // Doctor no longer derives the sibling itself; it reports whatever the resolver
    // resolved. These pin the rule it now depends on.

    #[test]
    fn floor_sibling_is_porch_quality_next_to_the_running_exe() {
        let exe = PathBuf::from("/opt/porch/bin/porch");
        let sibling = sibling_of(&exe).expect("parent directory");
        let expected = PathBuf::from(format!(
            "/opt/porch/bin/porch-quality{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(sibling, expected);
    }

    #[test]
    fn floor_sibling_is_none_when_the_exe_has_no_parent() {
        let exe = PathBuf::from("/");
        assert!(sibling_of(&exe).is_none());
    }
}
