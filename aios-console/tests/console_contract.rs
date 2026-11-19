use aios_console::backend::{dispatch, CommandBackend};
use aios_console::capability::{BackendKind, Capability, CapabilitySet};
use aios_console::output::{CommandOutput, ExitCode, OutputControl};
use aios_console::parser::{parse_line, tokenize};
use aios_console::registry::{command_ids, help, resolve, ResolveError, SPECS};
use aios_console::spec::CommandGroup;

struct EchoBackend {
    kind: BackendKind,
    capabilities: CapabilitySet,
    seen: Vec<String>,
}

impl EchoBackend {
    fn host() -> Self {
        Self {
            kind: BackendKind::Host,
            capabilities: CapabilitySet::all(),
            seen: Vec::new(),
        }
    }

    fn kernel() -> Self {
        Self {
            kind: BackendKind::Kernel,
            capabilities: CapabilitySet::all(),
            seen: Vec::new(),
        }
    }
}

impl CommandBackend for EchoBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    fn name(&self) -> &'static str {
        "echo-test"
    }

    fn execute(&mut self, invocation: &aios_console::registry::Invocation) -> CommandOutput {
        self.seen.push(invocation.display_name());
        match invocation.spec.id {
            "clear" => CommandOutput::clear(),
            "exit" => CommandOutput::exit(
                invocation
                    .first_arg()
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0),
            ),
            _ => {
                let mut output = CommandOutput::builder();
                output.push_line(invocation.display_name());
                for argument in &invocation.args {
                    output.push_line(argument.clone());
                }
                for (key, value) in &invocation.flags {
                    output.push_line(format!("--{key}={}", value.as_deref().unwrap_or("")));
                }
                output
            }
        }
    }
}

#[test]
fn parser_lowercases_only_the_command_head() {
    let parsed = parse_line("NET SET IP=10.0.0.5 Host=MiXeD").unwrap();
    assert_eq!(parsed.name, "net");
    assert_eq!(parsed.positionals, ["SET", "IP=10.0.0.5", "Host=MiXeD"]);
}

#[test]
fn parser_handles_quotes_escapes_and_comments() {
    let tokens = tokenize(r#"echo "a b" 'c d' e\ f # ignored"#).unwrap();
    let text: Vec<&str> = tokens.iter().map(|token| token.text.as_str()).collect();
    assert_eq!(text, ["echo", "a b", "c d", "e f"]);
    assert!(tokens[1].quoted);
    assert!(!tokens[3].quoted);
}

#[test]
fn parser_keeps_windows_path_separators_and_escapes_specials() {
    let tokens = tokenize(r#"source C:\Users\User\script.txt"#).unwrap();
    let text: Vec<&str> = tokens.iter().map(|token| token.text.as_str()).collect();
    assert_eq!(text, ["source", r"C:\Users\User\script.txt"]);

    let tokens = tokenize(r#"source "C:\Program Files\script.txt""#).unwrap();
    assert_eq!(tokens[1].text, r"C:\Program Files\script.txt");

    let tokens = tokenize(r#"echo a\"b"#).unwrap();
    assert_eq!(tokens[1].text, "a\"b");
}

#[test]
fn parser_reports_unclosed_quote_with_offset() {
    let error = tokenize("echo \"oops").unwrap_err();
    assert_eq!(error.message, "unclosed quote");
    assert_eq!(error.offset, 10);
}

#[test]
fn ambiguous_legacy_names_are_rejected() {
    for name in ["spawn", "load", "restart", "recover", "info", "list", "ls"] {
        let result = resolve(name);
        assert!(
            matches!(result, Err(ResolveError::Ambiguous { .. })),
            "{name} must stay ambiguous"
        );
        assert!(result.unwrap_err().message().contains("ambiguous"));
    }
}

#[test]
fn conflicting_semantics_have_distinct_canonical_names() {
    let cases: &[(&str, Option<&str>)] = &[
        ("process spawn", Some("spawn")),
        ("block load", Some("load")),
        ("block mock", Some("mock")),
        ("wd reset", Some("reset")),
        ("block rollback", Some("rollback")),
        ("hw rescan", Some("rescan")),
        ("status", None),
        ("sys info", Some("info")),
        ("kernel info", Some("info")),
        ("hub status", Some("status")),
        ("net show", Some("show")),
        ("net set", Some("set")),
        ("net reset", Some("reset")),
        ("net status", Some("status")),
    ];
    for (input, expected_sub) in cases {
        let invocation = resolve(input).unwrap_or_else(|error| panic!("{input}: {error:?}"));
        assert_eq!(invocation.subcommand, *expected_sub, "input: {input}");
    }
}

#[test]
fn case_and_subcommands_are_resolved_registry_wide() {
    let invocation = resolve("BLOCK LIST").unwrap();
    assert_eq!(invocation.spec.id, "block");
    assert_eq!(invocation.subcommand, Some("list"));

    let invocation = resolve("NeT ReSeT").unwrap();
    assert_eq!(invocation.spec.id, "net");
    assert_eq!(invocation.subcommand, Some("reset"));
}

#[test]
fn unambiguous_aliases_map_to_canonical_commands() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("kill abc", "process", Some("kill")),
        ("blocks", "block", Some("list")),
        ("unload 7", "block", Some("unload")),
        ("logs --tail 5", "log", Some("show")),
        ("net get", "net", Some("show")),
        ("q", "exit", None),
        ("?", "help", None),
    ];
    for (input, expected_id, expected_sub) in cases {
        let invocation = resolve(input).unwrap_or_else(|error| panic!("{input}: {error:?}"));
        assert_eq!(invocation.spec.id, *expected_id, "input: {input}");
        assert_eq!(invocation.subcommand, *expected_sub, "input: {input}");
    }
}

#[test]
fn alias_arguments_keep_their_case() {
    let invocation = resolve("kill MiXeD_Name").unwrap();
    assert_eq!(invocation.first_arg(), Some("MiXeD_Name"));
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    let error = resolve("block frobnicate").unwrap_err();
    assert!(matches!(error, ResolveError::UnknownSubcommand { .. }));
    assert!(
        error.message().contains("block frobnicate")
            || error.message().contains("'frobnicate' for 'block'")
    );

    let output = dispatch(&mut EchoBackend::host(), "block frobnicate");
    assert_eq!(output.code, ExitCode::Usage);
}

#[test]
fn free_argument_commands_do_not_need_subcommands() {
    let invocation = resolve("echo Hello World").unwrap();
    assert_eq!(invocation.spec.id, "echo");
    assert_eq!(invocation.subcommand, None);
    assert_eq!(invocation.args, ["Hello", "World"]);
}

#[test]
fn quotes_and_flags_survive_resolution() {
    let invocation = resolve(r#"echo "Hello World" --tag='MiXeD Case'"#).unwrap();
    assert_eq!(invocation.args, ["Hello World"]);
    assert_eq!(invocation.flag("tag"), Some(Some("MiXeD Case")));
}

#[test]
fn deferred_commands_report_unavailable() {
    for input in [
        "intent open the pod bay doors",
        "fetch http://example.test",
        "gui",
        "store publish myblock",
        "theme mono",
    ] {
        let output = dispatch(&mut EchoBackend::host(), input);
        assert_eq!(output.code, ExitCode::Unavailable, "input: {input}");
        assert!(output.lines[0].starts_with("ERR unavailable:"));
    }
}

#[test]
fn scope_gating_rejects_foreign_backends() {
    let output = dispatch(&mut EchoBackend::kernel(), "store list");
    assert_eq!(output.code, ExitCode::Unavailable);
    assert!(output.lines[0].contains("host backend"));

    let output = dispatch(&mut EchoBackend::host(), "socktest");
    assert_eq!(output.code, ExitCode::Unavailable);
    assert!(output.lines[0].contains("kernel backend"));
}

#[test]
fn capability_gating_rejects_ungranted_sessions() {
    let mut backend = EchoBackend::host();
    backend.capabilities = CapabilitySet::new();
    let output = dispatch(&mut backend, "store list");
    assert_eq!(output.code, ExitCode::Denied);
    assert!(output.lines[0].contains("StoreInstall"));

    backend.capabilities.grant(Capability::StoreInstall);
    assert_eq!(dispatch(&mut backend, "store list").code, ExitCode::Ok);
}

#[test]
fn output_control_implements_clear_and_exit() {
    let mut backend = EchoBackend::host();
    let output = dispatch(&mut backend, "clear");
    assert_eq!(output.control, OutputControl::Clear);
    assert!(output.lines.is_empty());

    let output = dispatch(&mut backend, "exit 3");
    assert_eq!(output.control, OutputControl::Exit(3));
    assert_eq!(output.code, ExitCode::Ok);
}

#[test]
fn unknown_command_is_a_usage_error() {
    let output = dispatch(&mut EchoBackend::host(), "frobnicate --x");
    assert_eq!(output.code, ExitCode::Usage);
    assert!(output.lines[0].contains("unknown command"));
}

#[test]
fn help_is_generated_from_the_registry() {
    let lines = help(None);
    for id in command_ids() {
        assert!(
            lines.iter().any(|line| line.trim_start().starts_with(id)),
            "help is missing canonical command '{id}'"
        );
    }

    let network = help(Some("network"));
    assert!(network.iter().any(|line| line.contains("net ")));
    assert!(network.iter().any(|line| line.contains("wifi ")));
    assert!(!network
        .iter()
        .any(|line| line.trim_start().starts_with("ps ")));

    assert!(help(Some("no-such-topic"))[0].starts_with("ERR usage:"));
}

#[test]
fn registry_has_unique_ids_and_expected_groups() {
    let mut ids: Vec<&str> = command_ids();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total);
    assert_eq!(total, SPECS.len());
    assert!(SPECS
        .iter()
        .any(|spec| spec.group == CommandGroup::Session && spec.id == "help"));
}
