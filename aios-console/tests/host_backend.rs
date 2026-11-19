use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aios_console::backend::dispatch;
use aios_console::capability::CapabilitySet;
use aios_console::host::{HostBackend, HostBackendConfig};
use aios_console::output::{ExitCode, OutputControl};
use aios_hal::hardware::HardwareProfile;

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

fn script_path(label: &str) -> PathBuf {
    let unique = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "aios-console-script-{label}-{}-{nanos}-{unique}.txt",
        std::process::id()
    ))
}

fn script(label: &str, contents: &str) -> PathBuf {
    let path = script_path(label);
    std::fs::write(&path, contents).expect("write script");
    path
}

fn source_line(path: &Path) -> String {
    format!("source \"{}\"", path.to_string_lossy())
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
        "update history",
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

#[test]
fn host_source_executes_a_script_file() {
    let mut host = backend("source");
    let path = script(
        "basic",
        "# a comment\n\necho MiXeD Case\nversion\n  # indented comment\n",
    );
    let line = source_line(&path);

    let output = dispatch(&mut host, &line);
    assert_ok("source", &output);
    assert!(output.lines.iter().any(|line| line == "MiXeD Case"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("aios-console host")));
    assert!(output.lines.last().unwrap().contains("2 command(s)"));

    assert!(host
        .log_entries()
        .iter()
        .any(|line| line.contains("sourced")));
    let _ = std::fs::remove_file(path);
}

#[test]
fn host_source_rejects_a_missing_argument_and_file() {
    let mut host = backend("source-args");

    assert_eq!(dispatch(&mut host, "source").code, ExitCode::Usage);

    let missing = script_path("source-missing");
    let line = source_line(&missing);
    let output = dispatch(&mut host, &line);
    assert_eq!(output.code, ExitCode::BackendError);
    assert!(output.lines[0].contains("read"));
}

#[test]
fn host_source_propagates_controls_and_stops_on_exit() {
    let mut host = backend("source-control");
    let path = script("control", "echo before\nexit 5\necho after\n");
    let line = source_line(&path);

    let output = dispatch(&mut host, &line);
    assert_eq!(output.control, OutputControl::Exit(5));
    assert!(output.lines.iter().any(|line| line == "before"));
    assert!(!output.lines.iter().any(|line| line == "after"));
    assert!(output.lines.last().unwrap().contains("2 command(s)"));

    let path = script("clear", "clear\necho after-clear\n");
    let line = source_line(&path);
    let output = dispatch(&mut host, &line);
    assert_eq!(output.control, OutputControl::Clear);
    assert!(output.lines.iter().any(|line| line == "after-clear"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn host_source_reports_the_first_failure_and_keeps_running() {
    let mut host = backend("source-failure");
    let path = script("failure", "echo ok\nprocess kill 999\necho ok2\n");
    let line = source_line(&path);

    let output = dispatch(&mut host, &line);
    assert_eq!(output.code, ExitCode::BackendError);
    assert!(output.lines.iter().any(|line| line == "ok"));
    assert!(output.lines.iter().any(|line| line == "ok2"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn host_source_guards_recursive_scripts() {
    let mut host = backend("source-recursion");
    let path = script_path("source-recursion");
    let self_path = path.to_string_lossy().into_owned();
    std::fs::write(&path, format!("echo loop\nsource \"{self_path}\"\n"))
        .expect("write recursive script");
    let line = source_line(&path);

    let output = dispatch(&mut host, &line);
    assert_eq!(output.code, ExitCode::BackendError);
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("nesting limit")));
    let _ = std::fs::remove_file(path);
}

#[test]
fn host_metrics_reports_real_counters() {
    let mut host = backend("metrics");
    assert_ok(
        "process spawn worker --ram=32",
        &dispatch(&mut host, "process spawn worker --ram=32"),
    );
    assert_ok(
        "block mock demo --version=2.3.4",
        &dispatch(&mut host, "block mock demo --version=2.3.4"),
    );

    let output = dispatch(&mut host, "metrics");
    assert_ok("metrics", &output);
    assert!(output.lines.iter().any(|line| line.starts_with("metrics")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("processes       1 (") && line.contains("1 ready")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("blocks          1 (")));
    assert!(output.lines.iter().any(|line| line.contains("/512 MB")));
}

#[test]
fn host_metrics_json_is_parseable() {
    let mut host = backend("metrics-json");
    assert_ok(
        "process spawn worker",
        &dispatch(&mut host, "process spawn worker"),
    );

    let output = dispatch(&mut host, "metrics --json");
    assert_ok("metrics --json", &output);
    let value: serde_json::Value =
        serde_json::from_str(&output.as_text()).expect("metrics JSON must parse");
    assert_eq!(value["processes"]["total"], 1);
    assert_eq!(value["processes"]["ready"], 1);
    assert_eq!(value["ram"]["total_mb"], 512);
    assert_eq!(value["blocks"]["total"], 0);
    assert_eq!(value["restarts"], 0);
    assert_eq!(value["max_restarts"], 2);
}

#[test]
fn host_metrics_prometheus_uses_exposition_format() {
    let mut host = backend("metrics-prometheus");
    assert_ok("block mock demo", &dispatch(&mut host, "block mock demo"));

    let output = dispatch(&mut host, "metrics --prometheus");
    assert_ok("metrics --prometheus", &output);
    let text = output.as_text();
    assert!(text.contains("# HELP aios_uptime_seconds"));
    assert!(text.contains("# TYPE aios_processes gauge"));
    assert!(text.contains("aios_processes_total 0"));
    assert!(text.contains("aios_blocks_total 1"));
    assert!(text.contains("aios_ram_total_mb 512"));
}

#[test]
fn host_health_is_healthy_for_an_idle_backend() {
    let mut host = backend("health-healthy");

    let output = dispatch(&mut host, "health");
    assert_ok("health", &output);
    assert!(output.lines[0].contains("HEALTHY"));
    assert!(output
        .lines
        .last()
        .unwrap()
        .contains("no problems detected"));
}

#[test]
fn host_health_is_degraded_when_a_process_is_suspended() {
    let mut host = backend("health-degraded");
    dispatch(&mut host, "process spawn worker");
    assert_ok(
        "process suspend 1",
        &dispatch(&mut host, "process suspend 1"),
    );

    let output = dispatch(&mut host, "health");
    assert_ok("health", &output);
    assert!(output.lines[0].contains("DEGRADED"));
    assert!(!output.lines[0].contains("UNHEALTHY"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("is Suspended")));
}

#[test]
fn host_health_is_unhealthy_at_the_restart_limit() {
    let mut host = backend("health-unhealthy");
    assert_ok("wd reset", &dispatch(&mut host, "wd reset"));
    assert_ok("wd reset", &dispatch(&mut host, "wd reset"));

    let output = dispatch(&mut host, "health");
    assert_ok("health", &output);
    assert!(output.lines[0].contains("UNHEALTHY"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("restart limit")));
    assert!(!output
        .lines
        .iter()
        .any(|line| line.contains("watchdog restart")));
}

#[test]
fn host_telemetry_record_query_and_latest_work() {
    let mut host = backend("telemetry-record");

    assert_ok(
        "telemetry record cpu 75.5 --ram=1024 --block=7 --process=worker",
        &dispatch(
            &mut host,
            "telemetry record cpu 75.5 --ram=1024 --block=7 --process=worker",
        ),
    );
    assert_ok(
        "telemetry record gpu 90 --ram=2048",
        &dispatch(&mut host, "telemetry record gpu 90 --ram=2048"),
    );

    let query = dispatch(&mut host, "telemetry query cpu");
    assert_ok("telemetry query cpu", &query);
    assert_eq!(query.lines.len(), 2);
    assert!(query.lines[1].contains("cpu"));
    assert!(query.lines[1].contains("75.50"));
    assert!(query.lines[1].contains("1024"));
    assert!(query.lines[1].contains("7"));
    assert!(query.lines[1].contains("worker"));

    let all = dispatch(&mut host, "telemetry query");
    assert_ok("telemetry query", &all);
    assert_eq!(all.lines.len(), 3);
    assert!(all.lines.iter().any(|line| line.contains("gpu")));

    let latest = dispatch(&mut host, "telemetry latest");
    assert_ok("telemetry latest", &latest);
    assert_eq!(latest.lines.len(), 2);
    assert!(latest.lines[1].contains("gpu"));
}

#[test]
fn host_telemetry_avg_and_peak_work() {
    let mut host = backend("telemetry-avg");
    dispatch(&mut host, "telemetry record cpu 75 --ram=1024");
    dispatch(&mut host, "telemetry record cpu 80 --ram=2048");
    dispatch(&mut host, "telemetry record gpu 40 --ram=512");

    let avg = dispatch(&mut host, "telemetry avg cpu");
    assert_ok("telemetry avg cpu", &avg);
    assert_eq!(avg.lines, ["cpu avg: 77.50"]);

    let overall = dispatch(&mut host, "telemetry avg");
    assert_ok("telemetry avg", &overall);
    assert_eq!(overall.lines, ["overall avg: 65.00 over 3 sample(s)"]);

    let peak = dispatch(&mut host, "telemetry peak");
    assert_ok("telemetry peak", &peak);
    assert!(peak.lines.iter().any(|line| line.contains("2048 MB")));
    assert!(peak.lines.iter().any(|line| line.contains("cpu = 80.00")));
}

#[test]
fn host_telemetry_clear_and_empty_ring_are_reported() {
    let mut host = backend("telemetry-clear");

    assert_eq!(
        dispatch(&mut host, "telemetry query").lines,
        ["no telemetry entries"]
    );
    assert_eq!(
        dispatch(&mut host, "telemetry latest").lines,
        ["no telemetry entries"]
    );
    assert_eq!(
        dispatch(&mut host, "telemetry avg").lines,
        ["no telemetry entries"]
    );
    assert_eq!(
        dispatch(&mut host, "telemetry avg cpu").lines,
        ["no samples for 'cpu'"]
    );

    dispatch(&mut host, "telemetry record cpu 50 --ram=128");
    let cleared = dispatch(&mut host, "telemetry clear");
    assert_ok("telemetry clear", &cleared);
    assert!(cleared.lines[0].contains("cleared 1 telemetry entries"));
    assert_eq!(
        dispatch(&mut host, "telemetry query").lines,
        ["no telemetry entries"]
    );
    assert!(cleared.lines[0].contains("cleared"));
    assert!(host
        .log_entries()
        .iter()
        .any(|line| line.contains("cleared 1 telemetry entries")));
}

#[test]
fn host_telemetry_record_defaults_ram_from_the_scheduler() {
    let mut host = backend("telemetry-ram");
    dispatch(&mut host, "process spawn worker --ram=64");

    assert_ok(
        "telemetry record cpu 10",
        &dispatch(&mut host, "telemetry record cpu 10"),
    );
    let latest = dispatch(&mut host, "telemetry latest");
    assert_ok("telemetry latest", &latest);
    assert!(latest.lines[1].contains("64"));
}

#[test]
fn host_telemetry_record_rejects_invalid_arguments() {
    let mut host = backend("telemetry-args");

    assert_eq!(
        dispatch(&mut host, "telemetry record").code,
        ExitCode::Usage
    );
    assert_eq!(
        dispatch(&mut host, "telemetry record cpu nope").code,
        ExitCode::Usage
    );
    assert_eq!(
        dispatch(&mut host, "telemetry record cpu 1 --block=nope").code,
        ExitCode::Usage
    );
    assert_eq!(dispatch(&mut host, "telemetry bogus").code, ExitCode::Usage);
}

#[test]
fn host_telemetry_missing_subcommand_is_a_usage_error() {
    let mut host = backend("telemetry-missing");
    let output = dispatch(&mut host, "telemetry");
    assert_eq!(output.code, ExitCode::Usage);
    assert!(output.lines[0].contains("telemetry record|query|latest|avg|peak|clear"));
}

#[test]
fn host_ipc_stats_reflect_real_bus_traffic() {
    let mut host = backend("ipc-stats");
    let empty = dispatch(&mut host, "ipc stats");
    assert_ok("ipc stats", &empty);
    assert!(empty
        .lines
        .iter()
        .any(|line| line.contains("queued         0")));

    assert_ok(
        "ipc send 1 2 first",
        &dispatch(&mut host, "ipc send 1 2 first"),
    );
    assert_ok(
        "ipc send 3 4 second",
        &dispatch(&mut host, "ipc send 3 4 second"),
    );

    let stats = dispatch(&mut host, "ipc stats");
    assert_ok("ipc stats", &stats);
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("queued         2")));
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("sent           2")));
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("peak depth     2")));
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("received       0")));
}

#[test]
fn host_ipc_peek_shows_the_front_packet() {
    let mut host = backend("ipc-peek");
    assert_ok(
        "ipc send 1 2 first --priority=4",
        &dispatch(&mut host, "ipc send 1 2 first --priority=4"),
    );
    assert_ok(
        "ipc send 3 4 second",
        &dispatch(&mut host, "ipc send 3 4 second"),
    );

    let peek = dispatch(&mut host, "ipc peek");
    assert_ok("ipc peek", &peek);
    assert!(peek.lines.iter().any(|line| line.contains("1 -> 2")));
    assert!(peek
        .lines
        .iter()
        .any(|line| line.contains("priority     4")));
    assert!(peek.lines.iter().any(|line| line.contains("Text(first)")));
    assert!(!peek.lines.iter().any(|line| line.contains("second")));
}

#[test]
fn host_ipc_send_validates_arguments_and_failure_is_a_backend_error() {
    let mut host = backend("ipc-send");
    assert_eq!(dispatch(&mut host, "ipc send").code, ExitCode::Usage);
    assert_eq!(
        dispatch(&mut host, "ipc send x 2 text").code,
        ExitCode::Usage
    );
    assert_eq!(
        dispatch(&mut host, "ipc send 1 y text").code,
        ExitCode::Usage
    );

    let sent = dispatch(&mut host, "ipc send 1 2 text");
    assert_ok("ipc send 1 2 text", &sent);
    assert!(sent.lines[0].contains("IPC packet"));
    assert!(sent.lines[0].contains("1->2"));
}

#[test]
fn host_ipc_reroute_and_flush_change_the_queue() {
    let mut host = backend("ipc-reroute");
    dispatch(&mut host, "ipc send 1 9 text");
    assert_ok("ipc reroute 9 4", &dispatch(&mut host, "ipc reroute 9 4"));
    let peek = dispatch(&mut host, "ipc peek");
    assert!(peek.lines.iter().any(|line| line.contains("1 -> 4")));

    assert_ok("ipc flush", &dispatch(&mut host, "ipc flush"));
    assert_eq!(dispatch(&mut host, "ipc peek").lines, ["IPC queue empty"]);
    let stats = dispatch(&mut host, "ipc stats");
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("queued         0")));
}

#[test]
fn host_ipc_freeze_and_unfreeze_preserve_packets() {
    let mut host = backend("ipc-freeze");
    dispatch(&mut host, "ipc send 1 2 held");

    let frozen = dispatch(&mut host, "ipc freeze");
    assert_ok("ipc freeze", &frozen);
    assert!(frozen.lines[0].contains("frozen (1 packet(s) held)"));
    assert_eq!(dispatch(&mut host, "ipc peek").lines, ["IPC queue empty"]);

    let blocked = dispatch(&mut host, "ipc send 3 4 blocked");
    assert_eq!(blocked.code, ExitCode::BackendError);
    assert!(blocked.lines[0].contains("frozen"));

    let unfrozen = dispatch(&mut host, "ipc unfreeze");
    assert_ok("ipc unfreeze", &unfrozen);
    assert!(unfrozen.lines[0].contains("unfrozen (1 packet(s) restored)"));
    let peek = dispatch(&mut host, "ipc peek");
    assert!(peek.lines.iter().any(|line| line.contains("Text(held)")));
}

#[test]
fn host_ipc_observes_an_externally_shared_bus() {
    use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
    use aios_ipc::bus::IpcBus;
    use std::sync::{Arc, Mutex};

    let mut host = backend("ipc-shared");
    let bus = Arc::new(Mutex::new(IpcBus::new(16)));
    {
        let mut shared = bus.lock().expect("lock shared bus");
        shared
            .send(IpcPacket::new(
                5,
                6,
                CommandId::Custom,
                Payload::Text("external".to_string()),
            ))
            .expect("send on shared bus");
    }
    host.set_ipc_bus(bus);

    let stats = dispatch(&mut host, "ipc stats");
    assert_ok("ipc stats", &stats);
    assert!(stats
        .lines
        .iter()
        .any(|line| line.contains("queued         1")));
    let peek = dispatch(&mut host, "ipc peek");
    assert!(peek.lines.iter().any(|line| line.contains("5 -> 6")));
    assert!(peek
        .lines
        .iter()
        .any(|line| line.contains("Text(external)")));
}

#[test]
fn host_sys_info_reports_injected_hardware_inventory() {
    let mut host = backend("sys-info");
    host.set_hardware_profile(HardwareProfile::mock_modern());

    let output = dispatch(&mut host, "sys info");
    assert_ok("sys info", &output);
    assert!(output.lines[0].starts_with("sys "));
    assert!(output
        .lines
        .iter()
        .any(|line| line == "source         injected"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.starts_with("cpu            ")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.starts_with("memory         ")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.starts_with("gpu            ")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.starts_with("npu            ")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("tier           Tier 1")));
}

#[test]
fn host_sys_tier_derives_tier_from_the_active_profile() {
    let mut host = backend("sys-tier");
    host.set_hardware_profile(HardwareProfile::mock_modern());

    let output = dispatch(&mut host, "sys tier");
    assert_ok("sys tier", &output);
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("tier           Tier 1")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("max model      70.0 GB")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("batch          64")));

    host.set_hardware_profile(HardwareProfile::mock_legacy_2012());
    let legacy = dispatch(&mut host, "sys tier");
    assert_ok("sys tier", &legacy);
    assert!(legacy
        .lines
        .iter()
        .any(|line| line.contains("tier           Tier 3")));
}

#[test]
fn host_hw_profile_switches_mock_profiles_case_insensitively() {
    let mut host = backend("hw-profile");

    let modern = dispatch(&mut host, "hw profile MoDeRn");
    assert_ok("hw profile MoDeRn", &modern);
    assert!(modern
        .lines
        .iter()
        .any(|line| line.contains("profile        modern")));
    let info = dispatch(&mut host, "sys info");
    assert!(info
        .lines
        .iter()
        .any(|line| line == "source         modern"));

    let legacy = dispatch(&mut host, "hw profile legacy-2012");
    assert_ok("hw profile legacy-2012", &legacy);
    let tier = dispatch(&mut host, "sys tier");
    assert!(tier.lines.iter().any(|line| line.contains("Tier 3")));
}

#[test]
fn host_hw_profile_rejects_unknown_names_with_usage() {
    let mut host = backend("hw-profile-invalid");

    let output = dispatch(&mut host, "hw profile alien");
    assert_eq!(output.code, ExitCode::Usage);
    assert!(output.lines[0]
        .contains("modern|legacy|legacy-2012|nvidia|intel-meteor-lake|qualcomm-x-elite"));
}

#[test]
fn host_hw_detect_reports_the_cached_profile() {
    let mut host = backend("hw-detect");
    host.set_hardware_profile(HardwareProfile::mock_intel_meteor_lake());

    let output = dispatch(&mut host, "hw detect");
    assert_ok("hw detect", &output);
    assert!(output.lines[0].starts_with("hw "));
    assert!(output
        .lines
        .iter()
        .any(|line| line == "source         injected"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("tier           Tier 1")));
}

#[test]
fn host_hw_rescan_forces_fresh_detection() {
    let mut host = backend("hw-rescan");
    host.set_hardware_profile(HardwareProfile::mock_modern());

    let output = dispatch(&mut host, "hw rescan");
    assert_ok("hw rescan", &output);
    assert!(output
        .lines
        .iter()
        .any(|line| line == "source         detected"));
    assert!(output
        .lines
        .iter()
        .any(|line| line == "rescan         complete"));
    assert!(host
        .log_entries()
        .iter()
        .any(|entry| entry == "hardware rescan completed"));
}

#[test]
fn host_hw_fingerprints_uses_the_extracted_device_keys() {
    let mut host = backend("hw-fingerprints");
    host.set_hardware_profile(HardwareProfile::mock_modern());

    let output = dispatch(&mut host, "hw fingerprints");
    assert_ok("hw fingerprints", &output);
    assert!(output.lines[0].starts_with("hw "));
    assert!(output
        .lines
        .iter()
        .any(|line| line == "source         injected"));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("count          4 fingerprint(s)")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("usb.046d.c52b")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("pci.10de.2684")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("driver.nvme.0000.0000")));
    assert!(output
        .lines
        .iter()
        .any(|line| line.contains("actionable=yes")));
}
