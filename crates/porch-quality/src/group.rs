//! Simple porch-owned grouping by language / directory.

use std::collections::BTreeMap;
use std::path::Path;

/// Max files per emitted group.
pub const MAX_FILES_PER_GROUP: usize = 10;

/// One review group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    pub files: Vec<String>,
}

/// Group paths by language bucket then top-level directory; cap at [`MAX_FILES_PER_GROUP`].
#[must_use]
pub fn group_paths(paths: &[String]) -> Vec<Group> {
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in paths {
        let key = bucket_key(p);
        buckets.entry(key).or_default().push(p.clone());
    }

    let mut groups = Vec::new();
    let mut gi = 0usize;
    for (bucket, mut files) in buckets {
        files.sort();
        for chunk in files.chunks(MAX_FILES_PER_GROUP) {
            groups.push(Group {
                label: format!("{bucket}-{gi}"),
                files: chunk.to_vec(),
            });
            gi += 1;
        }
    }
    groups
}

fn bucket_key(path: &str) -> String {
    let lang = language_of(path);
    let top = Path::new(path)
        .components()
        .find_map(|c| {
            let s = c.as_os_str().to_str()?;
            if s.is_empty() || s == "." {
                None
            } else {
                Some(s)
            }
        })
        .unwrap_or("root");
    format!("{lang}:{top}")
}

fn language_of(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "js",
        "ts" | "tsx" => "ts",
        "py" => "py",
        "go" => "go",
        "java" | "kt" => "jvm",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "md" => "md",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_lang_and_caps() {
        let mut paths = Vec::new();
        for i in 0..12 {
            paths.push(format!("src/f{i}.rs"));
        }
        paths.push("web/a.ts".into());
        let g = group_paths(&paths);
        assert!(g.len() >= 2, "{g:?}");
        assert!(g.iter().all(|x| x.files.len() <= MAX_FILES_PER_GROUP));
        assert!(g.iter().any(|x| {
            x.files.iter().any(|f| {
                Path::new(f)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
            })
        }));
    }
}
