//! First-run detect / wrapper write / verify for review engines.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::Error;
use crate::engine::{
    DetectedEngine, EngineKind, agent_detect_bins, known_engines, wrapper_body_matches,
    wrapper_script,
};
use crate::home_config::{
    FixerConfig, GithubConfig, HomeConfig, ReviewConfig, ToolsConfig, load_home_config,
    remove_home_config, write_home_config,
};
use crate::pathutil::{chmod_755, is_executable, resolve_bin, which};

/// Relative wrapper path under `$PORCH_HOME` (OCR / generic only).
pub const WRAPPER_REL: &str = "bin/review";

/// JSON result for `porch setup --yes` / `--verify` / `--apply`.
#[derive(Debug, Clone, Serialize)]
pub struct SetupResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_bin: Option<String>,
    pub verified: bool,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Path of the OS service definition when `--install-daemon` / wizard opt-in ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_service: Option<String>,
}

impl SetupResult {
    fn fail(msg: impl Into<String>, warnings: Vec<String>) -> Self {
        Self {
            ok: false,
            engine: None,
            wrapper: None,
            agent_bin: None,
            verified: false,
            warnings,
            error: Some(msg.into()),
            daemon_service: None,
        }
    }
}

/// Absolute path of the porch-owned review wrapper.
#[must_use]
pub fn wrapper_path(porch_home: &Path) -> PathBuf {
    porch_home.join(WRAPPER_REL)
}

/// Detect known engines currently on PATH.
#[must_use]
pub fn detect_engines() -> Vec<DetectedEngine> {
    let mut out = Vec::new();
    if let Some(bin) = detect_agent_bin() {
        out.push(DetectedEngine {
            kind: EngineKind::Agent,
            bin: canonicalize_best_effort(&bin),
        });
    }
    for kind in known_engines() {
        if *kind == EngineKind::Agent {
            continue;
        }
        if let Some(bin) = which(kind.detect_bin()) {
            out.push(DetectedEngine {
                kind: *kind,
                bin: canonicalize_best_effort(&bin),
            });
        }
    }
    out
}

fn detect_agent_bin() -> Option<PathBuf> {
    for name in agent_detect_bins() {
        if let Some(bin) = which(name) {
            return Some(bin);
        }
    }
    None
}

/// Smart default: prefer `quality` when present, then `agent`, then `generic`; OCR last.
#[must_use]
pub fn default_engine(detected: &[DetectedEngine]) -> Option<EngineKind> {
    if detected.is_empty() {
        return None;
    }
    if detected.len() == 1 {
        return Some(detected[0].kind);
    }
    detected
        .iter()
        .find(|d| d.kind == EngineKind::Quality)
        .or_else(|| detected.iter().find(|d| d.kind == EngineKind::Agent))
        .or_else(|| detected.iter().find(|d| d.kind == EngineKind::Generic))
        .or_else(|| detected.iter().find(|d| d.kind == EngineKind::Ocr))
        .or_else(|| detected.first())
        .map(|d| d.kind)
}

/// Detect optional fixer / gh / certify tools on PATH.
#[must_use]
pub fn detect_optional_tools() -> (Option<PathBuf>, Option<PathBuf>, ToolsConfig) {
    let fixer = detect_agent_bin();
    let gh = which("gh");
    let tools = ToolsConfig {
        biome: which("biome").map(|p| p.display().to_string()),
        bun: which("bun").map(|p| p.display().to_string()),
        cargo: which("cargo").map(|p| p.display().to_string()),
        just: which("just").map(|p| p.display().to_string()),
        moon: which("moon").map(|p| p.display().to_string()),
    };
    (fixer, gh, tools)
}

/// Whether review setup looks complete for doctor / bare porch.
#[must_use]
pub fn review_setup_ok(porch_home: &Path) -> bool {
    if std::env::var_os(crate::REVIEW_BIN_ENV).is_some() {
        return resolve_bin(&crate::review_bin()).is_some();
    }
    if std::env::var_os(crate::REVIEW_AGENT_BIN_ENV).is_some() {
        return std::env::var(crate::REVIEW_AGENT_BIN_ENV)
            .ok()
            .as_deref()
            .and_then(resolve_bin)
            .is_some();
    }
    match load_home_config(porch_home) {
        Ok(Some(cfg)) => {
            if cfg.review.engine_kind() == Some(EngineKind::Agent) {
                return cfg
                    .review
                    .agent_bin
                    .as_deref()
                    .and_then(resolve_bin)
                    .is_some();
            }
            if let Some(w) = cfg.review.wrapper.as_deref() {
                return is_executable(Path::new(w));
            }
            false
        }
        _ => resolve_bin("review").is_some(),
    }
}

/// Detect, write wrapper + config (or agent config), verify. Fail closed.
///
/// # Errors
///
/// Returns [`Error`] only for unexpected I/O while rolling back; preference is
/// to return a [`SetupResult`] with `ok: false`.
pub fn setup_yes(porch_home: &Path, engine: Option<EngineKind>) -> Result<SetupResult, Error> {
    let mut warnings = Vec::new();
    let detected = detect_engines();
    let Some(kind) = engine.or_else(|| default_engine(&detected)) else {
        return Ok(SetupResult::fail(
            "no judgment engine on PATH — install a coding agent (`claude` or `codex`) \
             (`engine: agent` still requires the porch-quality sibling of porch); \
             `engine: quality` is floor-only. Legacy `ocr` / a binary named `review` also ok. \
             Then re-run `porch setup`",
            warnings,
        ));
    };
    let Some(backend) = detected
        .iter()
        .find(|d| d.kind == kind)
        .map(|d| d.bin.clone())
    else {
        let hint = if kind == EngineKind::Agent {
            "claude or codex".to_string()
        } else {
            kind.detect_bin().to_string()
        };
        return Ok(SetupResult::fail(
            format!("engine `{kind}` requested but `{hint}` not found on PATH"),
            warnings,
        ));
    };

    let (fixer, gh, tools) = detect_optional_tools();
    if fixer.is_none() {
        warnings
            .push("fixer not detected (optional; set later for `porch agent respond fix`)".into());
    }
    if gh.is_none() {
        warnings.push("gh not detected (needed for deliver)".into());
    }

    if kind == EngineKind::Agent {
        return setup_yes_agent(porch_home, &backend, fixer, gh, tools, warnings);
    }

    let previous = load_home_config(porch_home)?;
    let wrap = wrapper_path(porch_home);
    write_wrapper(porch_home, kind, &backend)?;

    if let Err(e) = verify_setup(porch_home, kind, &backend, &wrap) {
        rollback_setup(porch_home, previous.as_ref(), &wrap);
        return Ok(SetupResult {
            ok: false,
            engine: Some(kind.as_str().into()),
            wrapper: Some(wrap.display().to_string()),
            agent_bin: None,
            verified: false,
            warnings,
            error: Some(e.to_string()),
            daemon_service: None,
        });
    }

    let cfg = HomeConfig {
        review: ReviewConfig {
            engine: Some(kind.as_str().into()),
            bin: Some(backend.display().to_string()),
            wrapper: Some(wrap.display().to_string()),
            agent_bin: None,
        },
        fixer: FixerConfig {
            bin: fixer.map(|p| p.display().to_string()),
        },
        github: GithubConfig {
            bin: gh.map(|p| p.display().to_string()),
        },
        tools,
    };
    write_home_config(porch_home, &cfg)?;

    Ok(SetupResult {
        ok: true,
        engine: Some(kind.as_str().into()),
        wrapper: Some(wrap.display().to_string()),
        agent_bin: None,
        verified: true,
        warnings,
        error: None,
        daemon_service: None,
    })
}

fn setup_yes_agent(
    porch_home: &Path,
    backend: &Path,
    fixer: Option<PathBuf>,
    gh: Option<PathBuf>,
    tools: ToolsConfig,
    warnings: Vec<String>,
) -> Result<SetupResult, Error> {
    let previous = load_home_config(porch_home)?;
    if let Err(e) = verify_agent_bin(backend) {
        return Ok(SetupResult {
            ok: false,
            engine: Some(EngineKind::Agent.as_str().into()),
            wrapper: None,
            agent_bin: Some(backend.display().to_string()),
            verified: false,
            warnings,
            error: Some(e.to_string()),
            daemon_service: None,
        });
    }

    let cfg = HomeConfig {
        review: ReviewConfig {
            engine: Some(EngineKind::Agent.as_str().into()),
            bin: None,
            wrapper: None,
            agent_bin: Some(backend.display().to_string()),
        },
        fixer: FixerConfig {
            bin: fixer
                .or_else(|| Some(backend.to_path_buf()))
                .map(|p| p.display().to_string()),
        },
        github: GithubConfig {
            bin: gh.map(|p| p.display().to_string()),
        },
        tools,
    };
    if let Err(e) = write_home_config(porch_home, &cfg) {
        if let Some(prev) = previous.as_ref() {
            let _ = write_home_config(porch_home, prev);
        } else {
            remove_home_config(porch_home);
        }
        return Err(e);
    }

    let _ = previous;
    Ok(SetupResult {
        ok: true,
        engine: Some(EngineKind::Agent.as_str().into()),
        wrapper: None,
        agent_bin: Some(backend.display().to_string()),
        verified: true,
        warnings,
        error: None,
        daemon_service: None,
    })
}

fn verify_agent_bin(backend: &Path) -> Result<(), Error> {
    if !is_executable(backend) {
        return Err(Error::Msg(format!(
            "agent binary not executable: {}",
            backend.display()
        )));
    }
    let help = Command::new(backend)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::Msg(format!("agent --help spawn failed: {e}")))?;
    if !help.status.success() {
        return Err(Error::Msg(format!(
            "agent --help exited {}: {}",
            help.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&help.stderr).trim()
        )));
    }
    Ok(())
}

/// Re-verify current config without rewriting.
///
/// # Errors
///
/// Returns [`Error`] on I/O while reading config; verification failures are
/// returned as [`SetupResult`] with `ok: false`.
pub fn setup_verify(porch_home: &Path) -> Result<SetupResult, Error> {
    let Some(cfg) = load_home_config(porch_home)? else {
        return Ok(SetupResult::fail(
            "no config.yaml — run `porch setup --yes`",
            Vec::new(),
        ));
    };
    let Some(kind) = cfg.review.engine_kind() else {
        return Ok(SetupResult::fail(
            "config.yaml missing review.engine",
            Vec::new(),
        ));
    };
    if kind == EngineKind::Agent {
        let Some(backend) = cfg.review.agent_bin.as_deref().and_then(resolve_bin) else {
            return Ok(SetupResult::fail(
                "config.yaml review.agent_bin missing or not executable",
                Vec::new(),
            ));
        };
        return match verify_agent_bin(&backend) {
            Ok(()) => Ok(SetupResult {
                ok: true,
                engine: Some(kind.as_str().into()),
                wrapper: None,
                agent_bin: Some(backend.display().to_string()),
                verified: true,
                warnings: Vec::new(),
                error: None,
                daemon_service: None,
            }),
            Err(e) => Ok(SetupResult {
                ok: false,
                engine: Some(kind.as_str().into()),
                wrapper: None,
                agent_bin: Some(backend.display().to_string()),
                verified: false,
                warnings: Vec::new(),
                error: Some(e.to_string()),
                daemon_service: None,
            }),
        };
    }
    let Some(backend) = cfg.review.bin.as_deref().and_then(resolve_bin) else {
        return Ok(SetupResult::fail(
            "config.yaml review.bin missing or not executable",
            Vec::new(),
        ));
    };
    let Some(wrap) = cfg.review.wrapper.as_deref().map(PathBuf::from) else {
        return Ok(SetupResult::fail(
            "config.yaml missing review.wrapper",
            Vec::new(),
        ));
    };
    match verify_setup(porch_home, kind, &backend, &wrap) {
        Ok(()) => Ok(SetupResult {
            ok: true,
            engine: Some(kind.as_str().into()),
            wrapper: Some(wrap.display().to_string()),
            agent_bin: None,
            verified: true,
            warnings: Vec::new(),
            error: None,
            daemon_service: None,
        }),
        Err(e) => Ok(SetupResult {
            ok: false,
            engine: Some(kind.as_str().into()),
            wrapper: Some(wrap.display().to_string()),
            agent_bin: None,
            verified: false,
            warnings: Vec::new(),
            error: Some(e.to_string()),
            daemon_service: None,
        }),
    }
}

/// Rewrite wrapper from current `config.yaml` (`porch setup --apply`).
///
/// # Errors
///
/// Returns [`Error`] on I/O while rewriting; verification failures are
/// returned as [`SetupResult`] with `ok: false` after rollback.
pub fn setup_apply(porch_home: &Path) -> Result<SetupResult, Error> {
    let Some(cfg) = load_home_config(porch_home)? else {
        return Ok(SetupResult::fail(
            "no config.yaml — run `porch setup --yes`",
            Vec::new(),
        ));
    };
    let Some(kind) = cfg.review.engine_kind() else {
        return Ok(SetupResult::fail(
            "config.yaml missing review.engine",
            Vec::new(),
        ));
    };
    if kind == EngineKind::Agent {
        let Some(backend) = cfg.review.agent_bin.as_deref().and_then(resolve_bin) else {
            return Ok(SetupResult::fail(
                "config.yaml review.agent_bin missing or not executable",
                Vec::new(),
            ));
        };
        return match verify_agent_bin(&backend) {
            Ok(()) => Ok(SetupResult {
                ok: true,
                engine: Some(kind.as_str().into()),
                wrapper: None,
                agent_bin: Some(backend.display().to_string()),
                verified: true,
                warnings: Vec::new(),
                error: None,
                daemon_service: None,
            }),
            Err(e) => Ok(SetupResult {
                ok: false,
                engine: Some(kind.as_str().into()),
                wrapper: None,
                agent_bin: Some(backend.display().to_string()),
                verified: false,
                warnings: Vec::new(),
                error: Some(e.to_string()),
                daemon_service: None,
            }),
        };
    }
    let Some(backend) = cfg.review.bin.as_deref().and_then(resolve_bin) else {
        return Ok(SetupResult::fail(
            "config.yaml review.bin missing or not executable",
            Vec::new(),
        ));
    };
    let previous = Some(cfg.clone());
    let wrap = wrapper_path(porch_home);
    write_wrapper(porch_home, kind, &backend)?;
    if let Err(e) = verify_setup(porch_home, kind, &backend, &wrap) {
        rollback_setup(porch_home, previous.as_ref(), &wrap);
        return Ok(SetupResult {
            ok: false,
            engine: Some(kind.as_str().into()),
            wrapper: Some(wrap.display().to_string()),
            agent_bin: None,
            verified: false,
            warnings: Vec::new(),
            error: Some(e.to_string()),
            daemon_service: None,
        });
    }
    let mut next = cfg;
    next.review.wrapper = Some(wrap.display().to_string());
    next.review.bin = Some(backend.display().to_string());
    write_home_config(porch_home, &next)?;
    Ok(SetupResult {
        ok: true,
        engine: Some(kind.as_str().into()),
        wrapper: Some(wrap.display().to_string()),
        agent_bin: None,
        verified: true,
        warnings: Vec::new(),
        error: None,
        daemon_service: None,
    })
}

/// Write `$PORCH_HOME/bin/review` for `engine` → `backend`.
///
/// # Errors
///
/// Returns [`Error`] when the backend is not executable or the wrapper cannot
/// be written / chmod'd. Agent engine should not call this.
pub fn write_wrapper(
    porch_home: &Path,
    engine: EngineKind,
    backend: &Path,
) -> Result<PathBuf, Error> {
    if engine == EngineKind::Agent {
        return Err(Error::Msg(
            "agent engine does not use the review wrapper".into(),
        ));
    }
    if !is_executable(backend) {
        return Err(Error::Msg(format!(
            "backend not executable: {}",
            backend.display()
        )));
    }
    let wrap = wrapper_path(porch_home);
    if let Some(parent) = wrap.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = wrapper_script(engine, backend);
    let tmp = wrap.with_extension("review.tmp");
    fs::write(&tmp, body)?;
    chmod_755(&tmp)?;
    fs::rename(&tmp, &wrap)?;
    chmod_755(&wrap)?;
    Ok(wrap)
}

/// Fail-closed verification of backend + porch-owned wrapper.
///
/// # Errors
///
/// Returns [`Error`] when the backend/wrapper is missing, not under
/// `PORCH_HOME`, has an unexpected body, `--help` fails, or (for `ocr`)
/// `--preview` on a tempfile repo fails.
pub fn verify_setup(
    porch_home: &Path,
    engine: EngineKind,
    backend: &Path,
    wrapper: &Path,
) -> Result<(), Error> {
    if engine == EngineKind::Agent {
        return verify_agent_bin(backend);
    }
    if !is_executable(backend) {
        return Err(Error::Msg(format!(
            "backend not executable: {}",
            backend.display()
        )));
    }
    let home_canon = canonicalize_best_effort(porch_home);
    let wrap_canon = canonicalize_best_effort(wrapper);
    if !wrap_canon.starts_with(&home_canon) {
        return Err(Error::Msg(format!(
            "wrapper must live under PORCH_HOME ({})",
            porch_home.display()
        )));
    }
    if !is_executable(wrapper) {
        return Err(Error::Msg(format!(
            "wrapper not executable: {}",
            wrapper.display()
        )));
    }
    let body = fs::read_to_string(wrapper)?;
    if body.contains("curl") || body.contains("| sh") || body.contains("|sh") {
        return Err(Error::Msg("wrapper body looks unsafe (curl|sh)".into()));
    }
    if !wrapper_body_matches(engine, backend, &body) {
        return Err(Error::Msg(
            "wrapper body does not match expected porch-owned script for recorded backend".into(),
        ));
    }

    let help = Command::new(wrapper)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::Msg(format!("wrapper --help spawn failed: {e}")))?;
    if !help.status.success() {
        return Err(Error::Msg(format!(
            "wrapper --help exited {}: {}",
            help.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&help.stderr).trim()
        )));
    }

    if engine == EngineKind::Ocr {
        verify_ocr_preview(backend)?;
    }
    if engine == EngineKind::Quality {
        verify_quality_range(backend)?;
    }
    Ok(())
}

fn verify_quality_range(backend: &Path) -> Result<(), Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("porch-setup-quality-{stamp}"));
    fs::create_dir_all(&tmp)?;
    let out_json = tmp.join("out.json");
    let result = (|| {
        git(&tmp, &["init"])?;
        git(&tmp, &["config", "user.email", "porch-setup@example.com"])?;
        git(&tmp, &["config", "user.name", "Porch Setup"])?;
        fs::write(tmp.join("README"), "one\n")?;
        git(&tmp, &["add", "README"])?;
        git(&tmp, &["commit", "-m", "c1"])?;
        fs::write(tmp.join("README"), "two\n")?;
        git(&tmp, &["add", "README"])?;
        git(&tmp, &["commit", "-m", "c2"])?;
        let from = git_stdout(&tmp, &["rev-parse", "HEAD~1"])?;
        let to = git_stdout(&tmp, &["rev-parse", "HEAD"])?;
        let out = Command::new(backend)
            .args([
                "--from",
                from.trim(),
                "--to",
                to.trim(),
                "--format",
                "json",
                "--output",
                out_json
                    .to_str()
                    .ok_or_else(|| Error::Msg("non-utf8 out path".into()))?,
            ])
            .current_dir(&tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| Error::Msg(format!("porch-quality range smoke spawn failed: {e}")))?;
        if !out.status.success() {
            return Err(Error::Msg(format!(
                "porch-quality range smoke exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        if !out_json.is_file() {
            return Err(Error::Msg(
                "porch-quality range smoke produced no output JSON".into(),
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn verify_ocr_preview(backend: &Path) -> Result<(), Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("porch-setup-verify-{stamp}"));
    fs::create_dir_all(&tmp)?;
    let result = (|| {
        git(&tmp, &["init"])?;
        git(&tmp, &["config", "user.email", "porch-setup@example.com"])?;
        git(&tmp, &["config", "user.name", "Porch Setup"])?;
        fs::write(tmp.join("README"), "one\n")?;
        git(&tmp, &["add", "README"])?;
        git(&tmp, &["commit", "-m", "c1"])?;
        fs::write(tmp.join("README"), "two\n")?;
        git(&tmp, &["add", "README"])?;
        git(&tmp, &["commit", "-m", "c2"])?;
        let from = git_stdout(&tmp, &["rev-parse", "HEAD~1"])?;
        let to = git_stdout(&tmp, &["rev-parse", "HEAD"])?;
        let out = Command::new(backend)
            .args([
                "review",
                "--preview",
                "--from",
                from.trim(),
                "--to",
                to.trim(),
            ])
            .current_dir(&tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| Error::Msg(format!("ocr review --preview spawn failed: {e}")))?;
        if !out.status.success() {
            return Err(Error::Msg(format!(
                "ocr review --preview exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn git(dir: &Path, args: &[&str]) -> Result<(), Error> {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Msg(format!("git {} failed: {e}", args.join(" "))))?;
    if !st.success() {
        return Err(Error::Msg(format!("git {} failed", args.join(" "))));
    }
    Ok(())
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| Error::Msg(format!("git {} failed: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(Error::Msg(format!("git {} failed", args.join(" "))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn rollback_setup(porch_home: &Path, previous: Option<&HomeConfig>, wrapper: &Path) {
    if let Some(cfg) = previous {
        match (
            cfg.review.engine_kind(),
            cfg.review.bin.as_deref().map(Path::new),
        ) {
            (Some(engine), Some(bin)) if engine.uses_wrapper() => {
                let _ = write_wrapper(porch_home, engine, bin);
            }
            _ => {
                let _ = fs::remove_file(wrapper);
            }
        }
        let _ = write_home_config(porch_home, cfg);
    } else {
        let _ = fs::remove_file(wrapper);
        remove_home_config(porch_home);
    }
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prefers_agent_over_ocr() {
        let detected = vec![
            DetectedEngine {
                kind: EngineKind::Ocr,
                bin: PathBuf::from("/usr/bin/ocr"),
            },
            DetectedEngine {
                kind: EngineKind::Agent,
                bin: PathBuf::from("/usr/bin/claude"),
            },
        ];
        assert_eq!(default_engine(&detected), Some(EngineKind::Agent));
    }

    #[test]
    fn default_prefers_quality_over_agent() {
        let detected = vec![
            DetectedEngine {
                kind: EngineKind::Agent,
                bin: PathBuf::from("/usr/bin/claude"),
            },
            DetectedEngine {
                kind: EngineKind::Quality,
                bin: PathBuf::from("/usr/bin/porch-quality"),
            },
        ];
        assert_eq!(default_engine(&detected), Some(EngineKind::Quality));
    }

    #[test]
    fn default_ocr_when_agent_absent() {
        let detected = vec![DetectedEngine {
            kind: EngineKind::Ocr,
            bin: PathBuf::from("/usr/bin/ocr"),
        }];
        assert_eq!(default_engine(&detected), Some(EngineKind::Ocr));
    }

    #[test]
    fn default_prefers_generic_over_ocr() {
        let detected = vec![
            DetectedEngine {
                kind: EngineKind::Ocr,
                bin: PathBuf::from("/usr/bin/ocr"),
            },
            DetectedEngine {
                kind: EngineKind::Generic,
                bin: PathBuf::from("/usr/bin/review"),
            },
        ];
        assert_eq!(default_engine(&detected), Some(EngineKind::Generic));
    }

    #[test]
    fn default_none_when_empty() {
        assert_eq!(default_engine(&[]), None);
    }
}
