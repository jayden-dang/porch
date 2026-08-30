//! Git range diff via CLI (no libgit2).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::Error;

/// One changed path with added-line anchors from the unified diff.
#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub path: String,
    /// 1-based line numbers in the new file that were added or context-touched as `+`.
    pub added_lines: Vec<AddedLine>,
}

/// One added line from the `+` side of a hunk.
#[derive(Debug, Clone)]
pub struct AddedLine {
    pub line_no: u32,
    pub text: String,
}

/// Path → hunk map (`DiffMap` spirit): added lines only.
pub type DiffMap = BTreeMap<String, FileDiff>;

/// `git diff --name-only <from>..<to>` in `work_tree` (absolute preferred).
///
/// # Errors
///
/// Returns [`Error::Git`] when git cannot run or exits non-zero.
pub fn changed_paths(work_tree: &Path, from: &str, to: &str) -> Result<Vec<String>, Error> {
    let range = format!("{from}..{to}");
    let out = git(
        work_tree,
        &["diff", "--name-only", "--diff-filter=ACMR", &range],
    )?;
    let mut paths: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Parse unified diff into a path → added-lines map.
///
/// # Errors
///
/// Returns [`Error::Git`] when git fails.
pub fn build_diff_map(work_tree: &Path, from: &str, to: &str) -> Result<DiffMap, Error> {
    let range = format!("{from}..{to}");
    let out = git(
        work_tree,
        &[
            "diff",
            "--no-color",
            "--unified=3",
            "--diff-filter=ACMR",
            &range,
        ],
    )?;
    Ok(parse_unified_diff(&out))
}

fn git(work_tree: &Path, args: &[&str]) -> Result<String, Error> {
    let mut cmd = Command::new("git");
    if work_tree.as_os_str().is_empty() {
        cmd.args(args);
    } else {
        cmd.arg("-C").arg(work_tree).args(args);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd
        .output()
        .map_err(|e| Error::Git(format!("spawn: {e}")))?;
    if !output.status.success() {
        return Err(Error::Git(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `git diff` unified output into [`DiffMap`].
#[must_use]
pub fn parse_unified_diff(diff: &str) -> DiffMap {
    let mut map = DiffMap::new();
    let mut current: Option<String> = None;
    let mut new_line: u32 = 0;

    for line in diff.lines() {
        if let Some(path) = parse_diff_git_a_b(line) {
            current = Some(path.clone());
            map.entry(path).or_default();
            new_line = 0;
            continue;
        }
        if let Some(path) = parse_plus_plus_plus(line) {
            current = Some(path.clone());
            map.entry(path).or_default();
            continue;
        }
        if line.starts_with("@@") {
            if let Some(n) = parse_hunk_new_start(line) {
                new_line = n;
            }
            continue;
        }
        let Some(path) = current.as_ref() else {
            continue;
        };
        if line.starts_with('+') && !line.starts_with("+++") {
            let text = line[1..].to_string();
            let entry = map.entry(path.clone()).or_default();
            entry.path.clone_from(path);
            entry.added_lines.push(AddedLine {
                line_no: new_line,
                text,
            });
            new_line = new_line.saturating_add(1);
        } else if line.starts_with('-') && !line.starts_with("---") {
            // deleted line: do not advance new-file cursor
        } else if line.starts_with(' ') || line.is_empty() {
            new_line = new_line.saturating_add(1);
        }
    }
    map
}

fn parse_diff_git_a_b(line: &str) -> Option<String> {
    // diff --git a/foo b/foo
    let rest = line.strip_prefix("diff --git ")?;
    let mut parts = rest.split_whitespace();
    let _a = parts.next()?;
    let b = parts.next()?;
    let path = b.strip_prefix("b/").unwrap_or(b);
    Some(path.to_string())
}

fn parse_plus_plus_plus(line: &str) -> Option<String> {
    let rest = line.strip_prefix("+++ ")?;
    if rest.starts_with("/dev/null") {
        return None;
    }
    let path = rest.strip_prefix("b/").unwrap_or(rest);
    let path = path.split('\t').next().unwrap_or(path).trim();
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

fn parse_hunk_new_start(line: &str) -> Option<u32> {
    // @@ -a,b +c,d @@ or @@ -a +c @@
    let plus = line.find('+')?;
    let after = &line[plus + 1..];
    let end = after.find([',', ' ']).unwrap_or(after.len());
    after[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_hunk() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
+    let x = 1;
     println!(\"hi\");
 }
";
        let map = parse_unified_diff(diff);
        let f = map.get("src/a.rs").expect("path");
        assert_eq!(f.added_lines.len(), 1);
        assert_eq!(f.added_lines[0].line_no, 2);
        assert_eq!(f.added_lines[0].text, "    let x = 1;");
    }
}
