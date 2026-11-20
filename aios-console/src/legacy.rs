//! Adapters that keep exact legacy shell text while using canonical console data.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::output::{CommandOutput, ExitCode};

const PROCESS_HEADER: &str = "PID  NAME              PRIO  STATE     RAM";
const PROCESS_SEPARATOR: &str = "---- ----------------  ---- --------- -----";

/// Converts canonical `ps` output into the exact legacy safe-mode process text.
///
/// Returns `None` when `output` is not a well-formed canonical process list, so
/// a frontend can keep the legacy dispatcher instead of printing a partial table.
pub fn process_list_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    if let [line] = output.lines.as_slice() {
        return (line == "No processes running").then(|| line.clone());
    }

    let (header, separator, rows, total) = match output.lines.as_slice() {
        [header, separator, rows @ .., total] => (header, separator, rows, total),
        _ => return None,
    };
    if header != PROCESS_HEADER || separator != PROCESS_SEPARATOR {
        return None;
    }
    if rows.is_empty() || !total.starts_with("Total: ") || !total.ends_with(" processes") {
        return None;
    }

    let mut text = String::from(PROCESS_HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(&legacy_process_row(row)?);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(total);
    Some(text)
}

fn legacy_process_row(row: &str) -> Option<String> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 5 {
        return None;
    }

    let pid = fields[0];
    let name = fields[1..fields.len() - 3].join(" ");
    let priority = fields[fields.len() - 3];
    let state = fields[fields.len() - 2];
    let ram = fields[fields.len() - 1].strip_suffix("MB")?;
    if name.is_empty() || ram.is_empty() {
        return None;
    }

    Some(format!(
        "{:<4} {:<17} {} {} {}MB",
        pid, name, priority, state, ram
    ))
}

#[cfg(test)]
mod tests {
    use super::{process_list_text, PROCESS_HEADER, PROCESS_SEPARATOR};
    use crate::output::CommandOutput;

    #[test]
    fn maps_empty_process_list() {
        let output = CommandOutput::text("No processes running");
        assert_eq!(
            process_list_text(&output).as_deref(),
            Some("No processes running")
        );
    }

    #[test]
    fn maps_canonical_process_rows_to_legacy_widths() {
        let row = alloc::format!(
            "{:<4} {:<17} {} {} {}MB",
            1,
            "ai_orchestrator",
            "High",
            "Ready",
            512
        );
        let output = CommandOutput::ok(alloc::vec![
            PROCESS_HEADER.into(),
            PROCESS_SEPARATOR.into(),
            row,
            "Total: 1 processes".into(),
        ]);
        let expected = alloc::format!(
            "PID  NAME              PRIO  STATE     RAM\n{:<4} {:<17} {} {} {}MB\n\nTotal: 1 processes",
            1,
            "ai_orchestrator",
            "High",
            "Ready",
            512
        );
        assert_eq!(
            process_list_text(&output).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn rejects_unexpected_outputs() {
        assert_eq!(process_list_text(&CommandOutput::usage("ps")), None);
        assert_eq!(
            process_list_text(&CommandOutput::ok(alloc::vec!["other".into()])),
            None
        );
    }
}
