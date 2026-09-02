//! Operator prerequisite check (`porch doctor`).

use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use porch_gate::{health_check, porch_home, socket_path};
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
    match which("git") {
        Some(p) => Check::new(Level::Ok, "git", p.display().to_string()),
        None => Check::new(Level::Fail, "git", "not found on PATH (required for push)"),
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
    let sock = socket_path(&home);
    out.push(match health_check(&home) {
        Ok(true) => Check::new(Level::Ok, "daemon", format!("healthy ({})", sock.display())),
        _ => Check::new(
            Level::Info,
            "daemon",
            format!(
                "not running (socket {}); started on `porch init` / first push notify",
                sock.display()
            ),
        ),
    });
    out
}

fn floor_sibling_of(exe: &Path) -> Option<PathBuf> {
    floor::sibling_of(exe)
}

fn check_floor() -> Check {
    match env::current_exe() {
        Ok(exe) => match floor_sibling_of(&exe) {
            Some(sibling) => {
                if is_executable(&sibling) {
                    Check::new(
                        Level::Ok,
                        "floor",
                        format!(
                            "{} (mandatory deterministic floor sibling)",
                            sibling.display()
                        ),
                    )
                } else {
                    Check::new(
                        Level::Warn,
                        "floor",
                        format!(
                            "`{}` missing or not executable — `cargo install porch --locked` \
installs both binaries next to each other; every run requires this sibling (not PATH)",
                            sibling.display()
                        ),
                    )
                }
            }
            None => Check::new(
                Level::Warn,
                "floor",
                format!(
                    "running executable {} has no parent directory; cannot locate porch-quality",
                    exe.display()
                ),
            ),
        },
        Err(e) => Check::new(
            Level::Warn,
            "floor",
            format!("could not resolve current_exe: {e}"),
        ),
    }
}

fn check_review() -> Check {
    let home = porch_home();
    let from_env = env::var_os(REVIEW_BIN_ENV).is_some();
    if from_env {
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
    use super::floor_sibling_of;
    use std::path::PathBuf;

    #[test]
    fn floor_sibling_is_porch_quality_next_to_the_running_exe() {
        let exe = PathBuf::from("/opt/porch/bin/porch");
        let sibling = floor_sibling_of(&exe).expect("parent directory");
        let expected = PathBuf::from(format!(
            "/opt/porch/bin/porch-quality{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(sibling, expected);
    }

    #[test]
    fn floor_sibling_is_none_when_the_exe_has_no_parent() {
        let exe = PathBuf::from("/");
        assert!(floor_sibling_of(&exe).is_none());
    }
}
