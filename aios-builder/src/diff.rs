//! Bounded line diff used to show a generated script before the user approves it.
//!
//! The diff is intentionally compact: unchanged lines are kept with a leading
//! space, removals with `-` and additions with `+`. Very long scripts are
//! truncated so the confirmation payload stays bounded even for adversarial
//! LLM output.

use std::fmt::Write as _;

/// Maximum number of lines considered from each side of the diff.
pub const MAX_DIFF_LINES: usize = 512;
/// Maximum characters kept from one line.
pub const MAX_DIFF_LINE_CHARS: usize = 240;

fn push_line(out: &mut String, marker: char, line: &str) {
    let trimmed = line.trim_end();
    if trimmed.chars().count() > MAX_DIFF_LINE_CHARS {
        let prefix: String = trimmed.chars().take(MAX_DIFF_LINE_CHARS).collect();
        let _ = writeln!(out, "{marker} {prefix}…");
    } else {
        let _ = writeln!(out, "{marker} {trimmed}");
    }
}

/// Build a compact line diff between `previous` and `current`.
///
/// Returns an empty string when both snapshots are identical. The result is
/// deterministic and bounded by [`MAX_DIFF_LINES`] per side; truncated input is
/// marked with a final `... truncated` line.
pub fn script_diff(previous: &str, current: &str) -> String {
    let old_lines: Vec<&str> = previous.lines().collect();
    let new_lines: Vec<&str> = current.lines().collect();
    if old_lines == new_lines {
        return String::new();
    }

    let old_truncated = old_lines.len() > MAX_DIFF_LINES;
    let new_truncated = new_lines.len() > MAX_DIFF_LINES;
    let old = &old_lines[..old_lines.len().min(MAX_DIFF_LINES)];
    let new = &new_lines[..new_lines.len().min(MAX_DIFF_LINES)];
    let (n, m) = (old.len(), new.len());

    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            push_line(&mut out, ' ', old[i]);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push_line(&mut out, '-', old[i]);
            i += 1;
        } else {
            push_line(&mut out, '+', new[j]);
            j += 1;
        }
    }
    while i < n {
        push_line(&mut out, '-', old[i]);
        i += 1;
    }
    while j < m {
        push_line(&mut out, '+', new[j]);
        j += 1;
    }
    if old_truncated || new_truncated {
        out.push_str("... truncated\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_scripts_have_no_diff() {
        assert_eq!(script_diff("a\nb", "a\nb"), "");
    }

    #[test]
    fn diff_shows_added_and_removed_lines() {
        let diff = script_diff(
            "browse \"https://old\"\nnotify \"done\"",
            "browse \"https://new\"\nnotify \"done\"",
        );
        assert!(diff.contains("- browse \"https://old\""));
        assert!(diff.contains("+ browse \"https://new\""));
        assert!(diff.contains("  notify \"done\""));
    }

    #[test]
    fn diff_is_bounded_for_long_scripts() {
        let long = "x".repeat(MAX_DIFF_LINE_CHARS + 50);
        let old = vec![long.as_str(); MAX_DIFF_LINES + 2].join("\n");
        let new = vec![long.as_str(); MAX_DIFF_LINES + 3].join("\n");
        let diff = script_diff(&old, &new);
        assert!(diff.contains("... truncated"));
        assert!(diff.lines().count() <= MAX_DIFF_LINES + 3);
        assert!(diff.contains("…"));
    }

    #[test]
    fn empty_sides_are_supported() {
        assert_eq!(script_diff("", "notify \"hi\""), "+ notify \"hi\"\n");
        assert_eq!(script_diff("notify \"hi\"", ""), "- notify \"hi\"\n");
    }
}
