//! Frontend seams shared by text-console hosts.

/// Returns the command text after a case-insensitive `console` prefix.
///
/// `None` means the line is not a console command. `Some("")` represents bare
/// `console`, which frontends should treat as a request for generated help.
pub fn console_prefix(line: &str) -> Option<&str> {
    let mut parts = line.trim().splitn(2, char::is_whitespace);
    let head = parts.next()?;
    if head.eq_ignore_ascii_case("console") {
        Some(parts.next().unwrap_or("").trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::console_prefix;

    #[test]
    fn prefix_is_case_insensitive_and_preserves_argument_case() {
        assert_eq!(console_prefix("console ps"), Some("ps"));
        assert_eq!(console_prefix("CONSOLE   ps aux"), Some("ps aux"));
        assert_eq!(
            console_prefix("  CoNsOlE\t echo MiXeD Case "),
            Some("echo MiXeD Case")
        );
    }

    #[test]
    fn non_console_lines_are_not_captured() {
        assert_eq!(console_prefix("ps"), None);
        assert_eq!(console_prefix("consolett ps"), None);
        assert_eq!(console_prefix(""), None);
    }

    #[test]
    fn bare_and_whitespace_only_forms_request_help() {
        assert_eq!(console_prefix("console"), Some(""));
        assert_eq!(console_prefix("  CONSOLE   "), Some(""));
        assert_eq!(console_prefix("console "), Some(""));
    }
}
