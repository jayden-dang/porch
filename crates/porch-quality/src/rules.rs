//! Language / hygiene rule packs loaded as data (YAML).

use std::path::Path;

use regex::Regex;
use serde::Deserialize;

use crate::Error;
use crate::diff::FileDiff;

const PACK_GENERIC: &str = include_str!("../data/packs/generic.yaml");
const PACK_RUST: &str = include_str!("../data/packs/rust.yaml");
const PACK_JS: &str = include_str!("../data/packs/js.yaml");

/// One emitted comment before relocate / JSON map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawComment {
    pub path: String,
    pub content: String,
    pub existing_code: String,
    pub start_line: u32,
    pub end_line: u32,
    pub category: String,
    pub severity: String,
    /// Stable `pack/rule` identity; copied onto JSON `rule_id` for porch criterion mapping.
    pub rule_id: String,
}

#[derive(Debug, Deserialize)]
struct PackFile {
    id: String,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    exclude_globs: Vec<String>,
    #[serde(default)]
    rules: Vec<RuleFile>,
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    id: String,
    category: String,
    severity: String,
    message: String,
    #[serde(rename = "match")]
    match_pat: String,
    #[serde(default)]
    exclude_globs: Vec<String>,
}

/// Compiled rule pack.
#[derive(Debug)]
pub struct RulePack {
    pub id: String,
    globs: Vec<GlobPat>,
    exclude_globs: Vec<GlobPat>,
    rules: Vec<CompiledRule>,
}

#[derive(Debug)]
struct CompiledRule {
    id: String,
    category: String,
    severity: String,
    message: String,
    re: Regex,
    exclude_globs: Vec<GlobPat>,
}

#[derive(Debug, Clone)]
struct GlobPat {
    raw: String,
}

impl GlobPat {
    fn new(raw: impl Into<String>) -> Self {
        Self { raw: raw.into() }
    }

    fn matches(&self, path: &str) -> bool {
        glob_match(&self.raw, path)
    }
}

/// Load embedded default packs (generic + rust + js).
///
/// # Errors
///
/// Returns [`Error::Pack`] when embedded YAML or regex is invalid.
pub fn load_builtin_packs() -> Result<Vec<RulePack>, Error> {
    Ok(vec![
        compile_pack(PACK_GENERIC)?,
        compile_pack(PACK_RUST)?,
        compile_pack(PACK_JS)?,
    ])
}

/// Parse and compile one pack YAML body.
///
/// # Errors
///
/// Returns [`Error::Pack`] on YAML or regex errors.
pub fn compile_pack(yaml: &str) -> Result<RulePack, Error> {
    let file: PackFile =
        serde_yaml::from_str(yaml).map_err(|e| Error::Pack(format!("yaml: {e}")))?;
    let mut rules = Vec::with_capacity(file.rules.len());
    for r in file.rules {
        let re = Regex::new(&r.match_pat)
            .map_err(|e| Error::Pack(format!("rule {}: regex: {e}", r.id)))?;
        rules.push(CompiledRule {
            id: r.id,
            category: r.category,
            severity: r.severity,
            message: r.message,
            re,
            exclude_globs: r.exclude_globs.into_iter().map(GlobPat::new).collect(),
        });
    }
    Ok(RulePack {
        id: file.id,
        globs: file.globs.into_iter().map(GlobPat::new).collect(),
        exclude_globs: file.exclude_globs.into_iter().map(GlobPat::new).collect(),
        rules,
    })
}

/// Run all packs against one file's added lines.
#[must_use]
pub fn apply_packs(packs: &[RulePack], path: &str, file_diff: &FileDiff) -> Vec<RawComment> {
    let mut out = Vec::new();
    for pack in packs {
        if !pack_applies(pack, path) {
            continue;
        }
        for rule in &pack.rules {
            if rule.exclude_globs.iter().any(|g| g.matches(path)) {
                continue;
            }
            for added in &file_diff.added_lines {
                if rule.re.is_match(&added.text) {
                    out.push(RawComment {
                        path: path.to_string(),
                        content: format!("[{}/{}] {}", pack.id, rule.id, rule.message),
                        existing_code: added.text.clone(),
                        start_line: added.line_no,
                        end_line: added.line_no,
                        category: rule.category.clone(),
                        severity: rule.severity.clone(),
                        rule_id: format!("{}/{}", pack.id, rule.id),
                    });
                }
            }
        }
    }
    out
}

fn pack_applies(pack: &RulePack, path: &str) -> bool {
    if pack.exclude_globs.iter().any(|g| g.matches(path)) {
        return false;
    }
    if pack.globs.is_empty() {
        return true;
    }
    pack.globs.iter().any(|g| g.matches(path))
}

/// Minimal glob: `**`, `*`, `{a,b}` brace lists, and path separators.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    // Expand simple brace groups: **/*.{js,ts} → try each.
    if let Some(expanded) = expand_braces(&pattern) {
        return expanded.iter().any(|p| glob_match_one(p, &path));
    }
    glob_match_one(&pattern, &path)
}

fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let start = pattern.find('{')?;
    let end = pattern[start..].find('}')? + start;
    let prefix = &pattern[..start];
    let suffix = &pattern[end + 1..];
    let inner = &pattern[start + 1..end];
    if inner.contains('{') {
        return None;
    }
    Some(
        inner
            .split(',')
            .map(|alt| format!("{prefix}{alt}{suffix}"))
            .collect(),
    )
}

fn glob_match_one(pattern: &str, path: &str) -> bool {
    match_segs(&split_segs(pattern), &split_segs(path))
}

fn split_segs(s: &str) -> Vec<&str> {
    s.split('/').filter(|p| !p.is_empty()).collect()
}

fn match_segs(pat: &[&str], path: &[&str]) -> bool {
    let mut pi = 0;
    let mut si = 0;
    while pi < pat.len() {
        if pat[pi] == "**" {
            if pi + 1 == pat.len() {
                return true;
            }
            // consume until next segment matches
            let next = pat[pi + 1];
            while si < path.len() {
                if seg_match(next, path[si]) && match_segs(&pat[pi + 1..], &path[si..]) {
                    return true;
                }
                si += 1;
            }
            return false;
        }
        if si >= path.len() {
            return false;
        }
        if !seg_match(pat[pi], path[si]) {
            return false;
        }
        pi += 1;
        si += 1;
    }
    si == path.len()
}

fn seg_match(pat: &str, seg: &str) -> bool {
    if pat == "*" {
        return true;
    }
    if !pat.contains('*') && !pat.contains('?') {
        return pat == seg;
    }
    let mut re = String::new();
    re.push('^');
    for c in pat.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(c);
            }
            other => re.push(other),
        }
    }
    re.push('$');
    Regex::new(&re).is_ok_and(|r| r.is_match(seg))
}

/// Whether a path looks like a lockfile / binary / generated skip candidate.
#[must_use]
pub fn default_skip_reason(path: &str) -> Option<&'static str> {
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "composer.lock"
            | "gemfile.lock"
            | "poetry.lock"
    ) || Path::new(&lower)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
    {
        return Some("lockfile");
    }
    if matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(""),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "pdf"
            | "zip"
            | "gz"
            | "wasm"
            | "bin"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "o"
            | "a"
    ) {
        return Some("binary");
    }
    if path.contains("/generated/") || path.contains("/.git/") {
        return Some("generated-or-vcs");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::AddedLine;

    #[test]
    fn rust_unwrap_hits_lib_not_test_name() {
        let packs = load_builtin_packs().unwrap();
        let diff = FileDiff {
            path: "src/lib.rs".into(),
            added_lines: vec![AddedLine {
                line_no: 10,
                text: "    let x = foo().unwrap();".into(),
            }],
        };
        let hits = apply_packs(&packs, "src/lib.rs", &diff);
        assert!(
            hits.iter().any(|c| c.rule_id.contains("unwrap-in-lib")),
            "{hits:?}"
        );

        let test_hits = apply_packs(&packs, "src/foo_test.rs", &diff);
        assert!(
            !test_hits
                .iter()
                .any(|c| c.rule_id.contains("unwrap-in-lib")),
            "{test_hits:?}"
        );
    }

    #[test]
    fn glob_brace_js() {
        assert!(glob_match("**/*.{js,ts}", "pkg/a.ts"));
        assert!(glob_match("**/*.{js,ts}", "pkg/a.js"));
        assert!(!glob_match("**/*.{js,ts}", "pkg/a.rs"));
    }

    #[test]
    fn lockfile_skip() {
        assert_eq!(default_skip_reason("Cargo.lock"), Some("lockfile"));
        assert_eq!(default_skip_reason("src/lib.rs"), None);
    }
}
