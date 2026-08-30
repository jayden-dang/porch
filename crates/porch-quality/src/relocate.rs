//! Line relocate with precision bias: unique context match or drop.

/// Result of trying to keep / move a finding onto current file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocateOutcome {
    /// Still at the recorded line (or adjusted within the snippet).
    Kept { line: u32 },
    /// Moved to a unique matching location.
    Relocated { line: u32 },
    /// Ambiguous or missing context — drop (prefer false-negative).
    Dropped { reason: &'static str },
}

/// Relocate a finding using `existing_code` against `file_content`.
///
/// Precision bias: empty / missing `existing_code` → drop; match nowhere or
/// more than once → drop. Prefer a false-negative over an unanchored line.
#[must_use]
pub fn relocate_finding(
    file_content: &str,
    start_line: u32,
    existing_code: Option<&str>,
) -> RelocateOutcome {
    let lines: Vec<&str> = file_content.lines().collect();
    if lines.is_empty() {
        return RelocateOutcome::Dropped {
            reason: "empty file",
        };
    }

    let snippet = existing_code.map_or("", str::trim);
    if snippet.is_empty() {
        return RelocateOutcome::Dropped {
            reason: "empty anchor",
        };
    }

    let needle_lines: Vec<&str> = snippet.lines().map(str::trim_end).collect();
    let first = needle_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or("");
    if first.is_empty() {
        return RelocateOutcome::Dropped {
            reason: "empty anchor",
        };
    }

    // Prefer exact position when the first needle line still matches there.
    if start_line >= 1 {
        let idx = (start_line as usize).saturating_sub(1);
        if idx < lines.len() && lines_match_at(&lines, idx, &needle_lines) {
            return RelocateOutcome::Kept { line: start_line };
        }
    }

    let mut matches: Vec<u32> = Vec::new();
    for (i, _) in lines.iter().enumerate() {
        if lines_match_at(&lines, i, &needle_lines) {
            let Ok(line) = u32::try_from(i + 1) else {
                continue;
            };
            matches.push(line);
        }
    }

    match matches.as_slice() {
        [only] => {
            if *only == start_line {
                RelocateOutcome::Kept { line: *only }
            } else {
                RelocateOutcome::Relocated { line: *only }
            }
        }
        [] => RelocateOutcome::Dropped {
            reason: "anchor not found",
        },
        _ => RelocateOutcome::Dropped {
            reason: "ambiguous anchor",
        },
    }
}

fn lines_match_at(file_lines: &[&str], start_idx: usize, needle: &[&str]) -> bool {
    if start_idx + needle.len() > file_lines.len() {
        return false;
    }
    for (offset, n) in needle.iter().enumerate() {
        let file_line = file_lines[start_idx + offset].trim_end();
        let want = n.trim_end();
        if want.is_empty() {
            continue;
        }
        if file_line == want {
            continue;
        }
        // Single-line anchors may be short fragments; never match empty file lines.
        if needle.len() == 1 {
            let frag = want.trim();
            let hay = file_line.trim();
            if !frag.is_empty() && !hay.is_empty() && hay.contains(frag) {
                continue;
            }
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_when_line_still_matches() {
        let file = "fn a() {}\nlet x = danger();\nfn b() {}\n";
        let out = relocate_finding(file, 2, Some("let x = danger();"));
        assert_eq!(out, RelocateOutcome::Kept { line: 2 });
    }

    #[test]
    fn relocates_on_unique_drift() {
        let file = "fn a() {}\n\nfn b() {}\nlet x = danger();\n";
        let out = relocate_finding(file, 2, Some("let x = danger();"));
        assert_eq!(out, RelocateOutcome::Relocated { line: 4 });
    }

    #[test]
    fn drops_ambiguous() {
        let file = "let x = danger();\nfn mid() {}\nlet x = danger();\n";
        let out = relocate_finding(file, 1, Some("let x = danger();"));
        // Line 1 still matches → Kept (exact position wins before search).
        assert_eq!(out, RelocateOutcome::Kept { line: 1 });
        let out2 = relocate_finding(file, 2, Some("let x = danger();"));
        assert_eq!(
            out2,
            RelocateOutcome::Dropped {
                reason: "ambiguous anchor"
            }
        );
    }

    #[test]
    fn drops_missing() {
        let file = "fn only() {}\n";
        let out = relocate_finding(file, 1, Some("let x = danger();"));
        assert_eq!(
            out,
            RelocateOutcome::Dropped {
                reason: "anchor not found"
            }
        );
    }

    #[test]
    fn drops_empty_existing_code() {
        let file = "fn a() {}\nlet x = 1;\n";
        assert_eq!(
            relocate_finding(file, 2, None),
            RelocateOutcome::Dropped {
                reason: "empty anchor"
            }
        );
        assert_eq!(
            relocate_finding(file, 2, Some("")),
            RelocateOutcome::Dropped {
                reason: "empty anchor"
            }
        );
        assert_eq!(
            relocate_finding(file, 2, Some("   \n  ")),
            RelocateOutcome::Dropped {
                reason: "empty anchor"
            }
        );
    }
}
