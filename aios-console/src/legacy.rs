//! Adapters that keep exact legacy shell text while using canonical console data.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::output::{CommandOutput, ExitCode, OutputControl};

const PROCESS_HEADER: &str = "PID  NAME              PRIO  STATE     RAM";
const PROCESS_SEPARATOR: &str = "---- ----------------  ---- --------- -----";
const BLOCK_HEADER: &str = "ID   NAME         VERSION  STATE";
const BLOCK_SEPARATOR: &str = "---- ------------ -------- --------";
const STATUS_HEADER: &str = "status        host console";
const STATUS_UPTIME: &str = "uptime        ";
const STATUS_PROCESSES: &str = "processes     ";
const STATUS_BLOCKS: &str = "blocks        ";
const STATUS_MEMORY: &str = "memory        ";
const STATUS_RESTARTS: &str = "restarts      ";
const STATUS_LOG: &str = "log           ";

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

/// Converts canonical `block list` output into the exact legacy safe-mode block text.
///
/// Returns `None` when `output` is not a well-formed canonical block list, so a
/// frontend can keep the legacy dispatcher instead of printing a partial table.
pub fn block_list_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    if let [line] = output.lines.as_slice() {
        return (line == "No blocks loaded").then(|| line.clone());
    }

    let (header, separator, rows, total) = match output.lines.as_slice() {
        [header, separator, rows @ .., total] => (header, separator, rows, total),
        _ => return None,
    };
    if header != BLOCK_HEADER || separator != BLOCK_SEPARATOR {
        return None;
    }
    let count = total
        .strip_prefix("Total: ")
        .and_then(|value| value.strip_suffix(" blocks"))
        .and_then(|value| value.parse::<usize>().ok())?;
    if count != rows.len() {
        return None;
    }
    if rows.is_empty() {
        return Some(String::from("No blocks loaded"));
    }

    let mut text = String::from(BLOCK_HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(&legacy_block_row(row)?);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(total);
    Some(text)
}

/// Converts canonical `status` output into the exact legacy safe-mode status text.
///
/// `crash_log_count` comes from the scheduler because the canonical status does
/// not expose the crash log. Returns `None` for an unexpected canonical shape so
/// a frontend can keep the legacy dispatcher instead of printing partial status.
pub fn status_text(output: &CommandOutput, crash_log_count: usize) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    let [status, uptime, processes, blocks, memory, restarts, log] = output.lines.as_slice() else {
        return None;
    };
    if status != STATUS_HEADER
        || !uptime.starts_with(STATUS_UPTIME)
        || !restarts.starts_with(STATUS_RESTARTS)
        || !log.starts_with(STATUS_LOG)
    {
        return None;
    }

    let processes = processes.strip_prefix(STATUS_PROCESSES)?;
    let blocks = blocks.strip_prefix(STATUS_BLOCKS)?;
    let memory = memory.strip_prefix(STATUS_MEMORY)?.strip_suffix(" MB")?;
    let (used, total) = memory.split_once('/')?;
    let used = used.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    let pressure = used as f64 / total.max(1) as f64 * 100.0;

    Some(format!(
        "System: SAFE MODE\nProcesses: {processes}\nBlocks: {blocks} loaded\nRAM: {used}/{total} MB ({pressure:.1}%)\nCrash log: {crash_log_count} entries"
    ))
}

/// Converts canonical `exit` output into the exact legacy safe-mode exit text.
///
/// Returns `None` unless the output is a clean `OutputControl::Exit` with no
/// lines, so a frontend can keep the legacy dispatcher instead of inventing a
/// shutdown message for an unexpected canonical result.
pub fn exit_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok || !output.lines.is_empty() {
        return None;
    }
    match output.control {
        OutputControl::Exit(_) => Some(String::from("Exiting safe mode shell")),
        _ => None,
    }
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

fn legacy_block_row(row: &str) -> Option<String> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return None;
    }

    let id = fields[0].parse::<u64>().ok()?;
    let version = fields[fields.len() - 2];
    let state = fields[fields.len() - 1];
    let name = fields[1..fields.len() - 2].join(" ");
    if name.is_empty() || version.is_empty() || state.is_empty() {
        return None;
    }

    let id = format!("block_{id}");
    Some(format!("{:<5} {:<13} {:<9} {}", id, name, version, state))
}

#[cfg(test)]
mod tests {
    use super::{
        block_list_text, exit_text, process_list_text, status_text, BLOCK_HEADER, BLOCK_SEPARATOR,
        PROCESS_HEADER, PROCESS_SEPARATOR,
    };
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
    fn maps_empty_block_list() {
        let output = CommandOutput::ok(alloc::vec![
            BLOCK_HEADER.into(),
            BLOCK_SEPARATOR.into(),
            "Total: 0 blocks".into(),
        ]);
        assert_eq!(
            block_list_text(&output).as_deref(),
            Some("No blocks loaded")
        );
    }

    #[test]
    fn maps_canonical_block_rows_to_legacy_layout() {
        let row = alloc::format!("{:<4} {:<12} {:<8} {}", 1, "hal", "0.1.0", "Active");
        let output = CommandOutput::ok(alloc::vec![
            BLOCK_HEADER.into(),
            BLOCK_SEPARATOR.into(),
            row,
            "Total: 1 blocks".into(),
        ]);
        let expected = alloc::format!(
            "ID   NAME         VERSION  STATE\n{:<5} {:<13} {:<9} {}\n\nTotal: 1 blocks",
            "block_1",
            "hal",
            "0.1.0",
            "Active"
        );
        assert_eq!(block_list_text(&output).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn maps_canonical_status_to_legacy_layout() {
        let output = CommandOutput::ok(alloc::vec![
            "status        host console".into(),
            "uptime        3s".into(),
            "processes     1 (0 running, 1 ready)".into(),
            "blocks        2".into(),
            "memory        64/65536 MB".into(),
            "restarts      0/3".into(),
            "log           0 entries".into(),
        ]);
        let expected = "System: SAFE MODE\nProcesses: 1 (0 running, 1 ready)\nBlocks: 2 loaded\nRAM: 64/65536 MB (0.1%)\nCrash log: 2 entries";
        assert_eq!(status_text(&output, 2).as_deref(), Some(expected));
    }

    #[test]
    fn maps_canonical_exit_to_legacy_text() {
        assert_eq!(
            exit_text(&CommandOutput::exit(0)).as_deref(),
            Some("Exiting safe mode shell")
        );
        assert_eq!(
            exit_text(&CommandOutput::exit(7)).as_deref(),
            Some("Exiting safe mode shell")
        );
    }

    #[test]
    fn rejects_unexpected_outputs() {
        assert_eq!(process_list_text(&CommandOutput::usage("ps")), None);
        assert_eq!(
            process_list_text(&CommandOutput::ok(alloc::vec!["other".into()])),
            None
        );
        assert_eq!(block_list_text(&CommandOutput::usage("block list")), None);
        assert_eq!(
            block_list_text(&CommandOutput::ok(alloc::vec![
                BLOCK_HEADER.into(),
                BLOCK_SEPARATOR.into(),
                "Total: 1 blocks".into(),
            ])),
            None
        );
        assert_eq!(status_text(&CommandOutput::usage("status"), 0), None);
        assert_eq!(
            status_text(&CommandOutput::ok(alloc::vec!["other".into()]), 0),
            None
        );
        assert_eq!(exit_text(&CommandOutput::ok(Vec::new())), None);
        assert_eq!(
            exit_text(&CommandOutput::text("Exiting safe mode shell")),
            None
        );
    }
}
