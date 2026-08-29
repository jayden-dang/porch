//! Review engine profiles (`ocr`, `generic`). Adding later = another profile.

use std::fmt;
use std::path::{Path, PathBuf};

/// Known review engines for first-run setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// `ocr` (Open Code Review) CLI: wrapper prefixes `review`.
    Ocr,
    /// Binary already speaks porch argv (`--from --to --format json --output`).
    Generic,
}

impl EngineKind {
    /// Parse `ocr` / `generic` (case-insensitive).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ocr" => Some(Self::Ocr),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }

    /// Config / CLI wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Generic => "generic",
        }
    }

    /// PATH binary to detect for this engine.
    #[must_use]
    pub const fn detect_bin(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Generic => "review",
        }
    }

    /// Argv prefix inserted by the porch-owned wrapper before operator args.
    #[must_use]
    pub const fn wrapper_prefix(self) -> &'static [&'static str] {
        match self {
            Self::Ocr => &["review"],
            Self::Generic => &[],
        }
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

/// Build the shell wrapper body for `engine` targeting absolute `backend`.
#[must_use]
pub fn wrapper_script(engine: EngineKind, backend: &Path) -> String {
    let backend = backend.display();
    match engine {
        EngineKind::Ocr => format!("#!/bin/sh\nexec {backend} review \"$@\"\n"),
        EngineKind::Generic => format!("#!/bin/sh\nexec {backend} \"$@\"\n"),
    }
}

/// Whether `body` is an acceptable porch-owned wrapper for `engine` + `backend`.
#[must_use]
pub fn wrapper_body_matches(engine: EngineKind, backend: &Path, body: &str) -> bool {
    let expected = wrapper_script(engine, backend);
    normalize_script(body) == normalize_script(&expected)
}

fn normalize_script(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// Registry of engines porch knows how to set up (lookahead: two only).
#[must_use]
pub fn known_engines() -> &'static [EngineKind] {
    &[EngineKind::Ocr, EngineKind::Generic]
}
