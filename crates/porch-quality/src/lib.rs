//! Porch-owned review quality engine (M16).
//!
//! Speaks the stable M3 argv contract via the `porch-quality` binary:
//! `--from <sha> --to <sha> --format json --output <path>` (cwd = worktree).
//! Session-free, no shell, no edits. Rule packs are data; optional agent
//! helpers are not required for unit tests.

mod coverage;
mod diff;
mod group;
mod relocate;
mod rules;

use std::fs;
use std::path::Path;

use serde::Serialize;

pub use coverage::{CoverageEntry, assert_complete, files_list};
pub use diff::{DiffMap, FileDiff, build_diff_map, changed_paths, parse_unified_diff};
pub use group::{Group, MAX_FILES_PER_GROUP, group_paths};
pub use relocate::{RelocateOutcome, relocate_finding};
pub use rules::{
    RawComment, RulePack, apply_packs, compile_pack, default_skip_reason, glob_match,
    load_builtin_packs,
};

/// Engine errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("coverage: changed file `{0}` missing from review manifest without skip")]
    Coverage(String),
    #[error("git: {0}")]
    Git(String),
    #[error("rule pack: {0}")]
    Pack(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
}

/// One JSON comment (porch-review compatible).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommentOut {
    pub path: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

/// One JSON group.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GroupOut {
    pub label: String,
    pub files: Vec<String>,
}

/// Top-level review JSON written to `--output`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewOutput {
    pub comments: Vec<CommentOut>,
    pub files: Vec<String>,
    pub coverage: Vec<CoverageEntry>,
    pub groups: Vec<GroupOut>,
}

/// Options for one quality-engine run.
#[derive(Debug, Clone)]
pub struct RunOpts<'a> {
    pub work_tree: &'a Path,
    pub from_sha: &'a str,
    pub to_sha: &'a str,
    /// When set, override discovered changed paths (tests / callers).
    pub changed_override: Option<&'a [String]>,
    pub packs: &'a [RulePack],
}

/// Run the engine: diff → coverage → group → rules → relocate → JSON model.
///
/// # Errors
///
/// Returns git, pack, coverage, or I/O errors. Coverage is fail-closed.
pub fn run_quality(opts: &RunOpts<'_>) -> Result<ReviewOutput, Error> {
    let changed = if let Some(c) = opts.changed_override {
        c.to_vec()
    } else {
        changed_paths(opts.work_tree, opts.from_sha, opts.to_sha)?
    };
    let diff_map = build_diff_map(opts.work_tree, opts.from_sha, opts.to_sha)?;

    let mut coverage = Vec::with_capacity(changed.len());
    let mut reviewable = Vec::new();
    for path in &changed {
        if let Some(reason) = default_skip_reason(path) {
            coverage.push(CoverageEntry::skip(path.clone(), reason));
        } else {
            coverage.push(CoverageEntry::pass(path.clone()));
            reviewable.push(path.clone());
        }
    }
    assert_complete(&changed, &coverage)?;

    let groups = group_paths(&reviewable);
    let mut raw_comments = Vec::new();
    for g in &groups {
        for path in &g.files {
            let file_diff = diff_map.get(path).cloned().unwrap_or_else(|| FileDiff {
                path: path.clone(),
                added_lines: Vec::new(),
            });
            raw_comments.extend(apply_packs(opts.packs, path, &file_diff));
        }
    }

    let mut comments = Vec::new();
    for raw in raw_comments {
        let file_path = opts.work_tree.join(&raw.path);
        let content = fs::read_to_string(&file_path).unwrap_or_default();
        match relocate_finding(&content, raw.start_line, Some(&raw.existing_code)) {
            RelocateOutcome::Kept { line } | RelocateOutcome::Relocated { line } => {
                comments.push(CommentOut {
                    path: raw.path,
                    content: raw.content,
                    existing_code: Some(raw.existing_code),
                    start_line: Some(line),
                    end_line: Some(line),
                    category: Some(raw.category),
                    severity: Some(raw.severity),
                });
            }
            RelocateOutcome::Dropped { .. } => {
                // Precision bias: do not emit unanchored comments.
            }
        }
    }

    Ok(ReviewOutput {
        files: files_list(&coverage),
        coverage,
        groups: groups
            .into_iter()
            .map(|g| GroupOut {
                label: g.label,
                files: g.files,
            })
            .collect(),
        comments,
    })
}

/// Write [`ReviewOutput`] as pretty JSON to `output`.
///
/// # Errors
///
/// Returns I/O or JSON errors.
pub fn write_output(output: &Path, review: &ReviewOutput) -> Result<(), Error> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let bytes = serde_json::to_vec_pretty(review)?;
    fs::write(output, bytes)?;
    Ok(())
}

#[cfg(test)]
mod integration {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn toy_range_emits_coverage_and_rust_rule() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git(root, &["init"]);
        git(root, &["config", "user.email", "porch@example.com"]);
        git(root, &["config", "user.name", "Porch"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "c1"]);
        let from = git_stdout(root, &["rev-parse", "HEAD"]);

        fs::write(
            root.join("src/lib.rs"),
            "pub fn a() -> i32 { foo().unwrap() }\n",
        )
        .unwrap();
        fs::write(root.join("Cargo.lock"), "# lock2\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "c2"]);
        let to = git_stdout(root, &["rev-parse", "HEAD"]);

        let packs = load_builtin_packs().unwrap();
        let out = run_quality(&RunOpts {
            work_tree: root,
            from_sha: &from,
            to_sha: &to,
            changed_override: None,
            packs: &packs,
        })
        .unwrap();

        assert!(
            out.files.iter().any(|f| f == "src/lib.rs"),
            "files={:?}",
            out.files
        );
        assert!(
            out.files.iter().any(|f| f == "Cargo.lock"),
            "files={:?}",
            out.files
        );
        let lock = out
            .coverage
            .iter()
            .find(|c| c.path == "Cargo.lock")
            .unwrap();
        assert_eq!(lock.status, "skip");
        assert_eq!(lock.reason.as_deref(), Some("lockfile"));
        assert!(
            out.comments
                .iter()
                .any(|c| c.content.contains("unwrap-in-lib")),
            "comments={:?}",
            out.comments
        );
    }
}
