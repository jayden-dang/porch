//! Operator prerequisite check (`porch doctor`).

use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use porch_gate::{health_check, porch_home, socket_path};

const REVIEW_BIN_ENV: &str = "PORCH_REVIEW_BIN";
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
    checks.push(check_git());
    checks.extend(check_home_and_daemon());
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

fn check_review() -> Check {
    let review_bin = env::var(REVIEW_BIN_ENV).unwrap_or_else(|_| "review".into());
    match resolve_bin(&review_bin) {
        Some(p) => Check::new(
            Level::Ok,
            "review",
            format!("{} ({REVIEW_BIN_ENV} or default `review`)", p.display()),
        ),
        None => Check::new(
            Level::Warn,
            "review",
            format!(
                "`{review_bin}` not found — set {REVIEW_BIN_ENV} or install on PATH (needed for a complete run; init still works)"
            ),
        ),
    }
}

fn check_gh() -> Check {
    let gh_bin = env::var(GH_BIN_ENV).unwrap_or_else(|_| "gh".into());
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
    match env::var(FIXER_BIN_ENV) {
        Ok(bin) if !bin.trim().is_empty() => match resolve_bin(bin.trim()) {
            Some(p) => Check::new(
                Level::Ok,
                "fixer",
                format!("{} ({FIXER_BIN_ENV})", p.display()),
            ),
            None => Check::new(
                Level::Warn,
                "fixer",
                format!(
                    "`{bin}` not found — set {FIXER_BIN_ENV} to a real binary (needed for `porch agent respond fix`)"
                ),
            ),
        },
        _ => Check::new(
            Level::Warn,
            "fixer",
            format!(
                "{FIXER_BIN_ENV} unset — required for `porch agent respond fix` (no default binary)"
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

fn resolve_bin(name_or_path: &str) -> Option<PathBuf> {
    let p = Path::new(name_or_path);
    if p.is_absolute() || name_or_path.contains('/') {
        return p.exists().then(|| p.to_path_buf());
    }
    which(name_or_path)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

/// True when `name` resolves on PATH or as an absolute/relative existing path.
#[must_use]
pub fn bin_on_path(name_or_path: &str) -> bool {
    resolve_bin(name_or_path).is_some()
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
