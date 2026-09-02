//! Review engine profiles (`agent`, `quality`, `generic`, `ocr`).

use std::fmt;
use std::path::{Path, PathBuf};

/// PATH names tried for [`EngineKind::Agent`] (same family as the fixer, D8).
pub const AGENT_DETECT_BINS: &[&str] = &["claude", "codex"];

/// Known review engines for first-run setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Session-free coding-agent turn (claude/codex); workflow default when quality absent.
    Agent,
    /// Floor-only shape (`engine: quality`). The floor itself is the `porch-quality` sibling of `porch`, not PATH.
    Quality,
    /// Binary already speaks porch argv (`--from --to --format json --output`).
    Generic,
    /// `ocr` CLI (legacy/optional): wrapper prefixes `review`.
    Ocr,
}

impl EngineKind {
    /// Parse `agent` / `quality` / `generic` / `ocr` (case-insensitive).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "agent" => Some(Self::Agent),
            "quality" => Some(Self::Quality),
            "generic" => Some(Self::Generic),
            "ocr" => Some(Self::Ocr),
            _ => None,
        }
    }

    /// Config / CLI wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Quality => "quality",
            Self::Generic => "generic",
            Self::Ocr => "ocr",
        }
    }

    /// PATH binary to detect for this engine (`agent` uses [`AGENT_DETECT_BINS`]).
    #[must_use]
    pub const fn detect_bin(self) -> &'static str {
        match self {
            Self::Agent => "claude",
            Self::Quality => "porch-quality",
            Self::Generic => "review",
            Self::Ocr => "ocr",
        }
    }

    /// Argv prefix inserted by the porch-owned wrapper before operator args.
    #[must_use]
    pub const fn wrapper_prefix(self) -> &'static [&'static str] {
        match self {
            Self::Ocr => &["review"],
            Self::Agent | Self::Generic | Self::Quality => &[],
        }
    }

    /// Whether this engine uses a `$PORCH_HOME/bin/review` argv wrapper.
    #[must_use]
    pub const fn uses_wrapper(self) -> bool {
        matches!(self, Self::Ocr | Self::Generic | Self::Quality)
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One detected engine candidate on PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedEngine {
    pub kind: EngineKind,
    pub bin: PathBuf,
}

/// Binaries tried when detecting the agent engine.
#[must_use]
pub fn agent_detect_bins() -> &'static [&'static str] {
    AGENT_DETECT_BINS
}

/// Build the shell wrapper body for `engine` targeting absolute `backend`.
///
/// Agent review does not use this wrapper; callers should skip via
/// [`EngineKind::uses_wrapper`].
#[must_use]
pub fn wrapper_script(engine: EngineKind, backend: &Path) -> String {
    let backend = backend.display();
    match engine {
        EngineKind::Ocr => format!("#!/bin/sh\nexec {backend} review \"$@\"\n"),
        EngineKind::Generic | EngineKind::Quality => {
            format!("#!/bin/sh\nexec {backend} \"$@\"\n")
        }
        EngineKind::Agent => {
            "#!/bin/sh\necho \"porch: agent engine has no review wrapper\" >&2\nexit 2\n"
                .to_string()
        }
    }
}

/// Whether `body` is an acceptable porch-owned wrapper for `engine` + `backend`.
#[must_use]
pub fn wrapper_body_matches(engine: EngineKind, backend: &Path, body: &str) -> bool {
    if !engine.uses_wrapper() {
        return false;
    }
    let expected = wrapper_script(engine, backend);
    normalize_script(body) == normalize_script(&expected)
}

fn normalize_script(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// Registry of engines porch knows how to set up (quality preferred, agent workflow, ocr legacy).
#[must_use]
pub fn known_engines() -> &'static [EngineKind] {
    &[
        EngineKind::Quality,
        EngineKind::Agent,
        EngineKind::Generic,
        EngineKind::Ocr,
    ]
}
