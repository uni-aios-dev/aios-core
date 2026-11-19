use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aios_console::backend::dispatch;
use aios_console::capability::CapabilitySet;
use aios_console::host::{HostBackend, HostBackendConfig};
use aios_console::output::ExitCode;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn test_config(label: &str) -> (HostBackendConfig, PathBuf) {
    let unique = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "aios-console-{label}-{}-{nanos}-{unique}",
        std::process::id()
    ));
    let config = HostBackendConfig {
        version: format!("test-{unique}"),
        blocks_dir: base.join("blocks"),
        vfs_root: base.join("vfs"),
        net_config_path: base.join("network.json"),
        total_ram_mb: 512,
        max_restarts: 2,
    };
    (config, base)
}

fn backend(label: &str) -> HostBackend {
    let (config, _) = test_config(label);
    HostBackend::with_config(config)
}

fn assert_ok(input: &str, output: &aios_console::output::CommandOutput) {
    assert_eq!(
        output.code,
        ExitCode::Ok,
        "'{input}' failed: {}",
        output.as_text()
    );
}

#[test]
fn host_session_and_status_commands_work() {
    let mut host = backend("session");

    assert_ok("help", &dispatch(&mut host, "help"));
    let version = dispatch(&mut host, "version");
    assert_ok("version", &version);
    assert!(version.lines[0].contains("test-"));

    let echo = dispatch(&mut host, r#"echo "Hello World" MiXeD"#);
    assert_ok("echo", &echo);
    assert_eq!(echo.lines, ["Hello World MiXeD"]);

    let status = dispatch(&mut host, "status");
    assert_ok("status", &status);
    assert!(status.lines.iter().any(|line| line.starts_with("status")));
    assert!(status
        .lines
        .iter()
        .any(|line| line.starts_with("memory        ")));

    assert_ok("history", &dispatch(&mut host, "history"));
    assert!(dispatch(&mut host, "clear").control == aios_console::output::OutputControl::Clear);
}

#[test]
fn host_process_lifecycle_works() {
    let mut host = backend("process");

    let empty = dispatch(&mut host, "ps");
    assert_ok("ps", &empty);
    assert_eq!(empty.lines, ["No processes running"]);

    let spawned = dispatch(&mut host, "process spawn worker --priority=high --ram=32");
    assert_ok("process spawn", &spawned);
    assert!(spawned.lines[0].contains("worker"));

    let list = dispatch(&mut host, "ps");
    assert_ok("ps", &list);
    assert!(list.lines.iter().any(|line| line.contains("worker")));

    let killed = dispatch(&mut host, "process kill 1");
    assert_ok("process kill", &killed);

    let list = dispatch(&mut host, "ps");
    assert_ok("ps", &list);
    assert!(!list.lines.iter().any(|line| line.contains("worker")));
}

#[test]
fn host_process_invalid_arguments_are_usage_errors() {
    let mut host = backend("process-args");

    assert_eq!(dispatch(&mut host, "process spawn").code, ExitCode::Usage);
    assert_eq!(
        dispatch(&mut host, "process kill nope").code,
        ExitCode::Usage
    );
    assert_eq!(
        dispatch(&mut host, "process renice 1 bogus").code,
        ExitCode::Usage
    );
}

#[test]
fn host_block_mock_list_and_unload_work() {
    let mut host = backend("block");

    let mock = dispatch(&mut host, "block mock demo --version=2.3.4");
    assert_ok("block mock", &mock);
    assert!(mock.lines[0].contains("demo"));

    let list = dispatch(&mut host, "block list");
    assert_ok("block list", &list);
    assert!(list.lines.iter().any(|line| line.contains("demo")));
    assert!(list.lines.iter().any(|line| line.contains("2.3.4")));

    let info = dispatch(&mut host, "block info demo");
    assert_ok("block info", &info);
    assert!(info
        .lines
        .iter()
        .any(|line| line.contains("name:         demo")));

    let unloaded = dispatch(&mut host, "block unload demo");
    assert_ok("block unload", &unloaded);

    let list = dispatch(&mut host, "block list");
    assert_ok("block list", &list);
    assert!(!list.lines.iter().any(|line| line.contains("demo")));
}

#[test]
fn host_block_activation_and_dependencies_work() {
    let mut host = backend("block-activate");

    assert_ok("block mock alpha", &dispatch(&mut host, "block mock alpha"));
    assert_ok(
        "block activate alpha",
        &dispatch(&mut host, "block activate alpha"),
    );
    let deps = dispatch(&mut host, "block deps");
    assert_ok("block deps", &deps);
    assert!(deps.lines.iter().any(|line| line.contains("load order")));

    let verify = dispatch(&mut host, "block verify alpha");
    assert_ok("block verify", &verify);
}

#[test]
fn host_watchdog_reset_is_bounded() {
    let mut host = backend("wd");

    let status = dispatch(&mut host, "wd status");
    assert_ok("wd status", &status);
    assert!(status.lines[0].contains("0/2"));

    assert_ok("wd reset", &dispatch(&mut host, "wd reset"));
    assert_eq!(host.restarts(), 1);
    assert_ok("wd reset", &dispatch(&mut host, "wd reset"));
    assert_eq!(host.restarts(), 2);

    let third = dispatch(&mut host, "wd reset");
    assert_eq!(third.code, ExitCode::BackendError);
    assert!(third.lines[0].contains("max restarts"));
}

#[test]
fn host_network_show_and_reset_work() {
    let mut host = backend("net");

    let shown = dispatch(&mut host, "net show");
    assert_ok("net show", &shown);
    assert!(
        serde_json::from_str::<serde_json::Value>(&shown.lines[0]).is_ok(),
        "net show must emit JSON: {}",
        shown.lines[0]
    );

    let set = dispatch(&mut host, "net set dns=1.1.1.1");
    assert_ok("net set", &set);
    assert_ok("net reset", &dispatch(&mut host, "net reset"));
}

#[test]
fn host_store_list_and_sources_work() {
    let mut host = backend("store");

    let list = dispatch(&mut host, "store list");
    assert_ok("store list", &list);
    assert_eq!(list.lines[0], "NAME                 VERSION    PATH");
    assert!(list.lines.last().unwrap().starts_with("Total:"));

    let sources = dispatch(&mut host, "store sources");
    assert_ok("store sources", &sources);
    assert_eq!(sources.lines[0], "NAME                 KIND       BASE");
}

#[test]
fn host_vfs_file_lifecycle_works() {
    let mut host = backend("vfs");

    assert_ok(
        "vfs mkdir AIOS:///docs",
        &dispatch(&mut host, "vfs mkdir AIOS:///docs"),
    );
    assert_ok(
        "vfs write AIOS:///docs/note.txt hello world",
        &dispatch(&mut host, "vfs write AIOS:///docs/note.txt hello world"),
    );

    let cat = dispatch(&mut host, "vfs cat AIOS:///docs/note.txt");
    assert_ok("vfs cat", &cat);
    assert_eq!(cat.lines, ["hello world"]);

    let ls = dispatch(&mut host, "vfs ls AIOS:///docs");
    assert_ok("vfs ls", &ls);
    assert!(ls.lines.iter().any(|line| line.contains("note.txt")));

    let exists = dispatch(&mut host, "vfs exists AIOS:///docs/note.txt");
    assert_ok("vfs exists", &exists);
    assert_eq!(exists.lines, ["true"]);

    let stat = dispatch(&mut host, "vfs stat AIOS:///docs/note.txt");
    assert_ok("vfs stat", &stat);
    assert!(stat.lines.iter().any(|line| line.starts_with("size:")));

    let resolved = dispatch(&mut host, "vfs resolve AIOS:///docs/note.txt");
    assert_ok("vfs resolve", &resolved);

    assert_ok(
        "vfs rm AIOS:///docs/note.txt",
        &dispatch(&mut host, "vfs rm AIOS:///docs/note.txt"),
    );
    let exists = dispatch(&mut host, "vfs exists AIOS:///docs/note.txt");
    assert_eq!(exists.lines, ["false"]);
}

#[test]
fn host_vfs_copy_and_move_work() {
    let mut host = backend("vfs-copy");

    assert_ok(
        "vfs write AIOS:///a.txt one",
        &dispatch(&mut host, "vfs write AIOS:///a.txt one"),
    );
    assert_ok(
        "vfs cp AIOS:///a.txt AIOS:///b.txt",
        &dispatch(&mut host, "vfs cp AIOS:///a.txt AIOS:///b.txt"),
    );
    let cat = dispatch(&mut host, "vfs cat AIOS:///b.txt");
    assert_eq!(cat.lines, ["one"]);

    assert_ok(
        "vfs mv AIOS:///b.txt AIOS:///c.txt",
        &dispatch(&mut host, "vfs mv AIOS:///b.txt AIOS:///c.txt"),
    );
    let cat = dispatch(&mut host, "vfs cat AIOS:///c.txt");
    assert_eq!(cat.lines, ["one"]);
}

#[test]
fn host_vfs_invalid_path_is_a_backend_error() {
    let mut host = backend("vfs-invalid");

    let output = dispatch(&mut host, "vfs cat relative/path.txt");
    assert_eq!(output.code, ExitCode::BackendError);
    assert!(output.lines[0].contains("invalid path"));
}

#[test]
fn host_capability_denial_blocks_privileged_commands() {
    let mut host = backend("caps");
    host.set_capabilities(CapabilitySet::new());

    let store = dispatch(&mut host, "store list");
    assert_eq!(store.code, ExitCode::Denied);
    assert!(store.lines[0].contains("StoreInstall"));

    let wd = dispatch(&mut host, "wd reset");
    assert_eq!(wd.code, ExitCode::Denied);
    assert!(wd.lines[0].contains("WdControl"));

    host.set_capabilities(CapabilitySet::all());
    assert_eq!(dispatch(&mut host, "store list").code, ExitCode::Ok);
    assert_eq!(dispatch(&mut host, "wd reset").code, ExitCode::Ok);
}

#[test]
fn host_unimplemented_subcommands_are_unavailable() {
    let mut host = backend("unavailable");

    for input in [
        "net validate",
        "block swap",
        "block rollback",
        "store verify",
        "vfs readat",
        "sys info",
        "kernel info",
        "wifi status",
    ] {
        let output = dispatch(&mut host, input);
        assert_eq!(output.code, ExitCode::Unavailable, "input: {input}");
        assert!(
            output.lines[0].starts_with("ERR unavailable:"),
            "input: {input}, output: {}",
            output.as_text()
        );
    }
}

#[test]
fn host_log_reflects_state_changes() {
    let mut host = backend("log");

    assert_ok("wd reset", &dispatch(&mut host, "wd reset"));
    let shown = dispatch(&mut host, "log show");
    assert_ok("log show", &shown);
    assert!(shown
        .lines
        .iter()
        .any(|line| line.contains("orchestrator restart")));

    assert_ok("log clear", &dispatch(&mut host, "log clear"));
    let shown = dispatch(&mut host, "log show");
    assert_eq!(shown.lines, ["no log entries"]);
}
