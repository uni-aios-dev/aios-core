use sha2::{Digest, Sha256};

/// Classification of an AI-preview result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiPreviewKind {
    /// `.wasm` or App Store manifest (`.json`) — capabilities, version, author.
    WasmManifest,
    /// `.log` / `crash.dump` — error cause + refactor / rollback hints.
    LogAnalysis,
    /// `.jsonl` chat / intent history.
    JsonlHistory,
    /// Generic text summary.
    Text,
    /// Binary file that cannot be rendered as text.
    Binary,
}

/// Styling hint for a preview line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiLineKind {
    Info,
    Success,
    Warning,
    Error,
    Muted,
}

/// Result of the smart AI preview for a selected file.
#[derive(Debug, Clone)]
pub struct AiPreview {
    /// What kind of file was analyzed.
    pub kind: AiPreviewKind,
    /// Short title shown above the preview.
    pub title: String,
    /// Rendered lines with a styling hint each.
    pub lines: Vec<(AiLineKind, String)>,
}

impl AiPreview {
    /// Human-readable one-line summary (used for status bar messages).
    pub fn headline(&self) -> String {
        self.lines
            .first()
            .map(|(_, l)| l.clone())
            .unwrap_or_default()
    }
}

/// Analyze a file by name + content and produce a smart preview.
pub fn analyze_file(name: &str, content: &[u8]) -> AiPreview {
    let lower = name.to_lowercase();
    if lower.ends_with(".wasm") {
        analyze_wasm(name, content)
    } else if lower.ends_with(".jsonl") {
        analyze_jsonl(name, content)
    } else if lower.ends_with(".log") || lower.ends_with(".dump") || lower.contains("crash") {
        analyze_log(name, content)
    } else if lower.ends_with(".json") {
        analyze_json(name, content)
    } else if is_printable(content) {
        analyze_text(name, content)
    } else {
        AiPreview {
            kind: AiPreviewKind::Binary,
            title: "AI: binary file".into(),
            lines: vec![
                (
                    AiLineKind::Info,
                    format!("File: {name} — {} bytes", content.len()),
                ),
                (
                    AiLineKind::Muted,
                    "Content is not UTF-8 text; no text analysis possible.".into(),
                ),
            ],
        }
    }
}

fn is_printable(content: &[u8]) -> bool {
    content
        .iter()
        .take(512)
        .all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..=0x7e).contains(&b))
}

/// LEB128 reader for the WASM binary format.
fn read_leb(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let mut result = 0u32;
    let mut shift = 0u32;
    loop {
        let b = *bytes.get(*pos)?;
        *pos += 1;
        result |= u32::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 32 {
            return None;
        }
    }
}

/// Extract the module name from the WASM `name` custom section, if present.
fn wasm_module_name(content: &[u8]) -> Option<String> {
    let mut pos = 8usize;
    while pos < content.len() {
        let section_id = *content.get(pos)?;
        pos += 1;
        let size = read_leb(content, &mut pos)? as usize;
        let section_end = pos.checked_add(size)?;
        if section_end > content.len() {
            return None;
        }
        if section_id == 0 {
            let name_len = read_leb(content, &mut pos)? as usize;
            if name_len == 4 && content.get(pos..pos + 4)? == b"name" {
                pos += 4;
                while pos < section_end {
                    let sub_id = *content.get(pos)?;
                    pos += 1;
                    let sub_size = read_leb(content, &mut pos)? as usize;
                    let sub_end = pos.checked_add(sub_size)?;
                    if sub_end > section_end {
                        return None;
                    }
                    if sub_id == 0 {
                        let str_len = read_leb(content, &mut pos)? as usize;
                        let bytes = content.get(pos..pos.checked_add(str_len)?)?;
                        return std::str::from_utf8(bytes).ok().map(|s| s.to_string());
                    }
                    pos = sub_end;
                }
            }
        }
        pos = section_end;
    }
    None
}

fn analyze_wasm(name: &str, content: &[u8]) -> AiPreview {
    let mut lines: Vec<(AiLineKind, String)> = Vec::new();
    let magic_ok = content.len() >= 8 && content[0..4] == [0x00, 0x61, 0x73, 0x6d];
    if !magic_ok {
        return AiPreview {
            kind: AiPreviewKind::WasmManifest,
            title: "AI: invalid WASM block".into(),
            lines: vec![(
                AiLineKind::Error,
                format!("{name} is not a valid WASM module (bad magic)"),
            )],
        };
    }
    let version = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    let sha = hex::encode(Sha256::digest(content));
    lines.push((
        AiLineKind::Success,
        format!(
            "WASM block: {name} — {} bytes, version {version}",
            content.len()
        ),
    ));
    match wasm_module_name(content) {
        Some(m) => lines.push((AiLineKind::Info, format!("Module name: {m}"))),
        None => lines.push((AiLineKind::Muted, "No module 'name' section found".into())),
    }
    lines.push((AiLineKind::Muted, format!("SHA-256: {sha}")));
    lines.push((
        AiLineKind::Warning,
        format!(
            "Capabilities are declared in the App Store manifest sidecar ({stem}_<ver>.json)",
            stem = name.trim_end_matches(".wasm")
        ),
    ));
    AiPreview {
        kind: AiPreviewKind::WasmManifest,
        title: "AI: WASM block manifest".into(),
        lines,
    }
}

/// Extract a string field from a JSON manifest.
fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn analyze_json(name: &str, content: &[u8]) -> AiPreview {
    let text = String::from_utf8_lossy(content);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return AiPreview {
            kind: AiPreviewKind::Text,
            title: "AI: text file".into(),
            lines: vec![(
                AiLineKind::Info,
                format!(
                    "{name}: {} bytes, not JSON — showing text summary",
                    content.len()
                ),
            )],
        };
    };
    let has_manifest_keys = ["name", "version", "author", "capabilities", "wasm_sha256"]
        .iter()
        .any(|k| value.get(*k).is_some());
    if !has_manifest_keys {
        return AiPreview {
            kind: AiPreviewKind::Text,
            title: "AI: JSON document".into(),
            lines: vec![
                (
                    AiLineKind::Info,
                    format!("{name}: valid JSON, {} bytes", content.len()),
                ),
                (
                    AiLineKind::Muted,
                    "Not an App Store manifest — no capabilities to summarize.".into(),
                ),
            ],
        };
    }

    let mut lines: Vec<(AiLineKind, String)> = Vec::new();
    let block_name = json_str(&value, "name").unwrap_or_else(|| name.to_string());
    let version = json_str(&value, "version").unwrap_or_else(|| "?".into());
    let author = json_str(&value, "author").unwrap_or_else(|| "?".into());
    let description = json_str(&value, "description").unwrap_or_else(|| "—".into());
    lines.push((
        AiLineKind::Success,
        format!("Block: {block_name} v{version} by {author}"),
    ));
    lines.push((AiLineKind::Info, format!("Description: {description}")));
    if let Some(caps) = value.get("capabilities").and_then(|v| v.as_array()) {
        if caps.is_empty() {
            lines.push((AiLineKind::Warning, "Capabilities: none declared".into()));
        } else {
            lines.push((
                AiLineKind::Info,
                format!("Required capabilities ({}):", caps.len()),
            ));
            for cap in caps.iter().take(12) {
                lines.push((AiLineKind::Warning, format!("  {cap}")));
            }
        }
    }
    if let Some(sha) = json_str(&value, "wasm_sha256") {
        let short: String = sha.chars().take(16).collect();
        lines.push((AiLineKind::Muted, format!("WASM SHA-256: {short}…")));
    }
    lines.push((
        AiLineKind::Info,
        match value.get("signature").is_some() {
            true => "Signature: present (trust verified by kernel)".into(),
            false => "Signature: none — unsigned block".into(),
        },
    ));
    AiPreview {
        kind: AiPreviewKind::WasmManifest,
        title: "AI: App Store manifest".into(),
        lines,
    }
}

/// Log analysis: count error classes and offer refactor / rollback hints.
fn analyze_log(name: &str, content: &[u8]) -> AiPreview {
    let text = String::from_utf8_lossy(content);
    let mut panics = 0;
    let mut errors = 0;
    let mut crashes = 0;
    let mut denied = 0;
    let mut timeouts = 0;
    let mut hints: Vec<String> = Vec::new();
    let mut samples: Vec<String> = Vec::new();

    for line in text.lines().take(2000) {
        let lower = line.to_lowercase();
        if lower.contains("panic") {
            panics += 1;
            push_sample(&mut samples, line);
        }
        if lower.contains("error") || lower.contains("exception") {
            errors += 1;
        }
        if lower.contains("crash") || lower.contains("segmentation") {
            crashes += 1;
        }
        if lower.contains("denied") || lower.contains("permission") {
            denied += 1;
        }
        if lower.contains("timeout") || lower.contains("timed out") {
            timeouts += 1;
        }
    }

    let mut lines: Vec<(AiLineKind, String)> = Vec::new();
    lines.push((
        AiLineKind::Info,
        format!("Log: {name} — {} lines scanned", text.lines().count()),
    ));
    lines.push((
        AiLineKind::Error,
        format!("Panics: {panics}  |  Errors/exceptions: {errors}"),
    ));
    lines.push((
        AiLineKind::Error,
        format!("Crashes: {crashes}  |  Permission denials: {denied}  |  Timeouts: {timeouts}"),
    ));

    if panics > 0 {
        lines.push((
            AiLineKind::Warning,
            "Root cause: a panic was raised (unwinding abort).".into(),
        ));
        hints.push(
            "Refactor: wrap the failing block call in a result and log context before panicking."
                .into(),
        );
        hints.push("Rollback: hot-swap the last block back to the previous stable version.".into());
    }
    if denied > 0 {
        lines.push((
            AiLineKind::Warning,
            "Root cause: an operation was blocked by the capability ACL.".into(),
        ));
        hints.push("Grant the missing token (e.g. vfs:host:read) via the kernel ACL.".into());
    }
    if timeouts > 0 {
        lines.push((
            AiLineKind::Warning,
            "Root cause: an operation exceeded its deadline.".into(),
        ));
        hints.push("Raise the scheduler time quantum or throttle background I/O.".into());
    }
    if crashes > 0 {
        hints.push("Inspect crash.dump for the faulting instruction and restore state via restore_state().".into());
    }
    if hints.is_empty() {
        lines.push((
            AiLineKind::Success,
            "No actionable error patterns found — log looks healthy.".into(),
        ));
    } else {
        for (i, hint) in hints.iter().enumerate().take(6) {
            lines.push((AiLineKind::Info, format!("Hint {}: {hint}", i + 1)));
        }
    }
    if !samples.is_empty() {
        lines.push((AiLineKind::Muted, "Sample lines:".into()));
        for s in samples.iter().take(5) {
            lines.push((AiLineKind::Muted, format!("  {s}")));
        }
    }
    AiPreview {
        kind: AiPreviewKind::LogAnalysis,
        title: "AI: log analysis".into(),
        lines,
    }
}

fn push_sample(samples: &mut Vec<String>, line: &str) {
    if samples.len() < 5 && line.len() <= 160 {
        samples.push(line.to_string());
    }
}

/// JSONL chat / intent history parser.
fn analyze_jsonl(name: &str, content: &[u8]) -> AiPreview {
    let text = String::from_utf8_lossy(content);
    let mut messages = 0usize;
    let mut intents = 0usize;
    let mut turns: Vec<(AiLineKind, String)> = Vec::new();
    for line in text.lines().take(1000) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        messages += 1;
        let role = json_str(&value, "role").unwrap_or_default();
        let body = json_str(&value, "text")
            .or_else(|| json_str(&value, "content"))
            .or_else(|| json_str(&value, "message"))
            .unwrap_or_default();
        let intent = json_str(&value, "intent").unwrap_or_default();
        if !intent.is_empty() {
            intents += 1;
            turns.push((AiLineKind::Warning, format!("Intent detected: {intent}")));
        }
        if !role.is_empty() && !body.is_empty() {
            let preview: String = body.chars().take(80).collect();
            let kind = if role == "user" {
                AiLineKind::Info
            } else {
                AiLineKind::Success
            };
            turns.push((kind, format!("{role}: {preview}")));
        }
    }
    let mut lines: Vec<(AiLineKind, String)> = Vec::new();
    lines.push((
        AiLineKind::Info,
        format!("History: {name} — {messages} records, {intents} intent lines"),
    ));
    if turns.is_empty() {
        lines.push((
            AiLineKind::Muted,
            "No role/text or intent records could be parsed.".into(),
        ));
    } else {
        for t in turns.iter().take(40) {
            lines.push(t.clone());
        }
    }
    AiPreview {
        kind: AiPreviewKind::JsonlHistory,
        title: "AI: chat / intent history".into(),
        lines,
    }
}

/// Generic text summary (line/word stats + first non-empty lines).
fn analyze_text(name: &str, content: &[u8]) -> AiPreview {
    let text = String::from_utf8_lossy(content);
    let lines = text.lines().count();
    let words = text.split_whitespace().count();
    let mut preview_lines: Vec<(AiLineKind, String)> = Vec::new();
    preview_lines.push((
        AiLineKind::Info,
        format!(
            "File: {name} — {lines} lines, {words} words, {} bytes",
            content.len()
        ),
    ));
    for line in text.lines().take(20).filter(|l| !l.trim().is_empty()) {
        let snippet: String = line.chars().take(100).collect();
        preview_lines.push((AiLineKind::Muted, format!("  {snippet}")));
    }
    AiPreview {
        kind: AiPreviewKind::Text,
        title: "AI: quick text summary".into(),
        lines: preview_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_preview() {
        let wasm = b"\x00asm\x01\x00\x00\x00";
        let preview = analyze_file("block.wasm", wasm);
        assert_eq!(preview.kind, AiPreviewKind::WasmManifest);
        assert!(preview.lines.iter().any(|(_, l)| l.contains("version 1")));
    }

    #[test]
    fn test_wasm_module_name_section() {
        // header + custom section (id 0) named "name" with a module-name
        // subsection (id 0) containing "my_block".
        let mut bytes: Vec<u8> = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        // section: id=0
        bytes.push(0x00);
        let mut payload: Vec<u8> = Vec::new();
        payload.push(b"name".len() as u8);
        payload.extend_from_slice(b"name");
        // name subsection id 0
        payload.push(0x00);
        // subsection size = LEB(8) + "my_block"
        let mut body: Vec<u8> = Vec::new();
        body.push(b"my_block".len() as u8);
        body.extend_from_slice(b"my_block");
        payload.push(body.len() as u8);
        payload.extend_from_slice(&body);
        bytes.push(payload.len() as u8);
        bytes.extend_from_slice(&payload);

        assert_eq!(wasm_module_name(&bytes).as_deref(), Some("my_block"));
    }

    #[test]
    fn test_manifest_json_preview() {
        let json = br#"{"name":"net","version":"1.2.3","author":"AIOS Team","capabilities":["NET_BIND","FS_WRITE"],"signature":"abc"}"#;
        let preview = analyze_file("net_1.2.3.json", json);
        assert_eq!(preview.kind, AiPreviewKind::WasmManifest);
        assert!(preview.lines.iter().any(|(_, l)| l.contains("net v1.2.3")));
        assert!(preview.lines.iter().any(|(_, l)| l.contains("NET_BIND")));
    }

    #[test]
    fn test_log_analysis_finds_panic() {
        let log = b"2026-01-01 boot ok\n2026-01-01 thread 'main' panicked at src/main.rs:10\n";
        let preview = analyze_file("crash.dump", log);
        assert_eq!(preview.kind, AiPreviewKind::LogAnalysis);
        assert!(preview.lines.iter().any(|(_, l)| l.contains("Panics: 1")));
        assert!(preview.lines.iter().any(|(_, l)| l.contains("Rollback")));
    }

    #[test]
    fn test_jsonl_history() {
        let jsonl =
            b"{\"role\":\"user\",\"text\":\"hello\"}\n{\"role\":\"assistant\",\"text\":\"hi\"}\n";
        let preview = analyze_file("chat.jsonl", jsonl);
        assert_eq!(preview.kind, AiPreviewKind::JsonlHistory);
        assert!(preview.lines.iter().any(|(_, l)| l.contains("2 records")));
    }

    #[test]
    fn test_binary_detected() {
        let preview = analyze_file("blob.bin", &[0u8, 159, 146, 150]);
        assert_eq!(preview.kind, AiPreviewKind::Binary);
    }
}
