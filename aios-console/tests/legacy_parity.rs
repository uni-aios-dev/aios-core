use aios_console::registry::{resolve, ResolveError};

enum Expected<'a> {
    Canonical {
        id: &'a str,
        sub: Option<&'a str>,
        args: &'a [&'a str],
    },
    Ambiguous(&'a str),
    Deferred(&'a str),
}

fn assert_expected(input: &str, expected: &Expected<'_>) {
    match expected {
        Expected::Canonical { id, sub, args } => {
            let invocation =
                resolve(input).unwrap_or_else(|error| panic!("{input} did not resolve: {error:?}"));
            let actual_args: Vec<&str> = invocation.args.iter().map(String::as_str).collect();
            assert_eq!(invocation.spec.id, *id, "input: {input}");
            assert_eq!(invocation.subcommand, *sub, "input: {input}");
            assert_eq!(actual_args, *args, "input: {input}");
        }
        Expected::Ambiguous(fragment) => {
            let error = resolve(input).expect_err("{input} must stay ambiguous");
            assert!(
                matches!(error, ResolveError::Ambiguous { .. }),
                "input: {input}, error: {error:?}"
            );
            assert!(
                error.message().contains(*fragment),
                "input: {input}, message: {}",
                error.message()
            );
        }
        Expected::Deferred(fragment) => {
            let error = resolve(input).expect_err("{input} must stay deferred");
            assert!(
                matches!(error, ResolveError::Deferred { .. }),
                "input: {input}, error: {error:?}"
            );
            assert!(
                error.message().contains(*fragment),
                "input: {input}, message: {}",
                error.message()
            );
        }
    }
}

#[test]
fn safe_mode_surface_maps_to_canonical_commands() {
    let cases: &[(&str, Expected<'_>)] = &[
        (
            "ps",
            Expected::Canonical {
                id: "ps",
                sub: None,
                args: &[],
            },
        ),
        (
            "processes",
            Expected::Canonical {
                id: "ps",
                sub: None,
                args: &[],
            },
        ),
        ("list", Expected::Ambiguous("ps")),
        (
            "blocks",
            Expected::Canonical {
                id: "block",
                sub: Some("list"),
                args: &[],
            },
        ),
        ("ls", Expected::Ambiguous("block list")),
        (
            "kill 42",
            Expected::Canonical {
                id: "process",
                sub: Some("kill"),
                args: &["42"],
            },
        ),
        ("spawn worker 3 256", Expected::Ambiguous("process spawn")),
        ("load my_block 2.0.0", Expected::Ambiguous("block load")),
        (
            "unload 7",
            Expected::Canonical {
                id: "block",
                sub: Some("unload"),
                args: &["7"],
            },
        ),
        (
            "status",
            Expected::Canonical {
                id: "status",
                sub: None,
                args: &[],
            },
        ),
        ("info", Expected::Ambiguous("status")),
        (
            "logs",
            Expected::Canonical {
                id: "log",
                sub: Some("show"),
                args: &[],
            },
        ),
        (
            "log",
            Expected::Canonical {
                id: "log",
                sub: Some("show"),
                args: &[],
            },
        ),
        ("restart", Expected::Ambiguous("wd reset")),
        ("recover", Expected::Ambiguous("wd reset")),
        (
            "help",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        (
            "?",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        (
            "exit",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
        (
            "quit",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
        (
            "q",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
    ];
    for (input, expected) in cases {
        assert_expected(input, expected);
    }
}

#[test]
fn host_tui_surface_maps_to_canonical_commands() {
    let cases: &[(&str, Expected<'_>)] = &[
        (
            "help",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        (
            "clear",
            Expected::Canonical {
                id: "clear",
                sub: None,
                args: &[],
            },
        ),
        (
            "ps",
            Expected::Canonical {
                id: "ps",
                sub: None,
                args: &[],
            },
        ),
        (
            "blocks",
            Expected::Canonical {
                id: "block",
                sub: Some("list"),
                args: &[],
            },
        ),
        (
            "kill 17",
            Expected::Canonical {
                id: "process",
                sub: Some("kill"),
                args: &["17"],
            },
        ),
        ("spawn worker", Expected::Ambiguous("process spawn")),
        (
            "store list",
            Expected::Canonical {
                id: "store",
                sub: Some("list"),
                args: &[],
            },
        ),
        (
            "net show",
            Expected::Canonical {
                id: "net",
                sub: Some("show"),
                args: &[],
            },
        ),
        ("cluster status", Expected::Deferred("cluster control")),
        (
            "status",
            Expected::Canonical {
                id: "status",
                sub: None,
                args: &[],
            },
        ),
        (
            "logs",
            Expected::Canonical {
                id: "log",
                sub: Some("show"),
                args: &[],
            },
        ),
        ("restart", Expected::Ambiguous("wd reset")),
        (
            "source script.txt",
            Expected::Canonical {
                id: "source",
                sub: None,
                args: &["script.txt"],
            },
        ),
    ];
    for (input, expected) in cases {
        assert_expected(input, expected);
    }
}

#[test]
fn kernel_surface_maps_to_canonical_commands() {
    let cases: &[(&str, Expected<'_>)] = &[
        (
            "help",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        ("tabs", Expected::Deferred("tab overview")),
        ("info", Expected::Ambiguous("kernel info")),
        (
            "ver",
            Expected::Canonical {
                id: "kernel",
                sub: Some("ver"),
                args: &[],
            },
        ),
        (
            "clear",
            Expected::Canonical {
                id: "clear",
                sub: None,
                args: &[],
            },
        ),
        ("gui", Expected::Deferred("GUI mode")),
        ("tui", Expected::Deferred("framebuffer view switching")),
        (
            "faults",
            Expected::Canonical {
                id: "faults",
                sub: None,
                args: &[],
            },
        ),
        (
            "wifi",
            Expected::Canonical {
                id: "wifi",
                sub: Some("status"),
                args: &[],
            },
        ),
        (
            "echo ready",
            Expected::Canonical {
                id: "echo",
                sub: None,
                args: &["ready"],
            },
        ),
        (
            "udp 127.0.0.1 9000 ping",
            Expected::Canonical {
                id: "udp",
                sub: None,
                args: &["127.0.0.1", "9000", "ping"],
            },
        ),
        (
            "http 127.0.0.1 80",
            Expected::Canonical {
                id: "http",
                sub: None,
                args: &["127.0.0.1", "80"],
            },
        ),
        (
            "socktest",
            Expected::Canonical {
                id: "socktest",
                sub: None,
                args: &[],
            },
        ),
    ];
    for (input, expected) in cases {
        assert_expected(input, expected);
    }
}

#[test]
fn aios_shell_surface_maps_to_canonical_commands() {
    let cases: &[(&str, Expected<'_>)] = &[
        (
            "exit",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
        (
            "quit",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
        (
            "q",
            Expected::Canonical {
                id: "exit",
                sub: None,
                args: &[],
            },
        ),
        (
            "clear",
            Expected::Canonical {
                id: "clear",
                sub: None,
                args: &[],
            },
        ),
        (
            "cls",
            Expected::Canonical {
                id: "clear",
                sub: None,
                args: &[],
            },
        ),
        (
            "help",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        (
            "?",
            Expected::Canonical {
                id: "help",
                sub: None,
                args: &[],
            },
        ),
        (
            "echo hello",
            Expected::Canonical {
                id: "echo",
                sub: None,
                args: &["hello"],
            },
        ),
        (
            "vfs ls",
            Expected::Canonical {
                id: "vfs",
                sub: Some("ls"),
                args: &[],
            },
        ),
        (
            "vfs cat notes.txt",
            Expected::Canonical {
                id: "vfs",
                sub: Some("cat"),
                args: &["notes.txt"],
            },
        ),
        (
            "exec cargo --version",
            Expected::Canonical {
                id: "exec",
                sub: None,
                args: &["cargo"],
            },
        ),
        (
            "source script.txt",
            Expected::Canonical {
                id: "source",
                sub: None,
                args: &["script.txt"],
            },
        ),
        (
            "alias",
            Expected::Canonical {
                id: "alias",
                sub: None,
                args: &[],
            },
        ),
        (
            "unalias ll",
            Expected::Canonical {
                id: "unalias",
                sub: None,
                args: &["ll"],
            },
        ),
        (
            "jobs",
            Expected::Canonical {
                id: "jobs",
                sub: None,
                args: &[],
            },
        ),
        (
            "fg 1",
            Expected::Canonical {
                id: "fg",
                sub: None,
                args: &["1"],
            },
        ),
        (
            "bg 1",
            Expected::Canonical {
                id: "bg",
                sub: None,
                args: &["1"],
            },
        ),
        (
            "history",
            Expected::Canonical {
                id: "history",
                sub: None,
                args: &[],
            },
        ),
        ("gui", Expected::Deferred("GUI mode")),
        ("tui", Expected::Deferred("framebuffer view switching")),
    ];
    for (input, expected) in cases {
        assert_expected(input, expected);
    }
}

#[test]
fn console_prefix_is_only_a_frontend_seam() {
    assert!(matches!(resolve("console"), Err(ResolveError::Unknown(_))));
    assert!(matches!(
        resolve("console ps"),
        Err(ResolveError::Unknown(_))
    ));

    let invocation = resolve("ps").unwrap();
    assert_eq!(invocation.spec.id, "ps");
}

#[test]
fn deferred_surface_names_stay_deferred_across_legacy_shells() {
    for input in [
        "fetch https://example.test",
        "search kernel fault",
        "open https://example.test",
        "intent free up memory",
        "gui",
        "tui",
        "tabs",
        "cluster status",
        "store publish my_block",
        "store sign my_block",
        "perf-tune-video",
        "memory-compaction",
        "theme mono",
    ] {
        let error = resolve(input).expect_err("{input} must stay deferred");
        assert!(
            matches!(error, ResolveError::Deferred { .. }),
            "input: {input}, error: {error:?}"
        );
    }
}

#[test]
fn conflicting_legacy_names_have_resolvable_canonical_alternatives() {
    let cases: &[(&str, &str)] = &[
        ("spawn", "process spawn worker"),
        ("spawn", "block load module.wasm"),
        ("load", "block mock synthetic"),
        ("restart", "wd reset"),
        ("restart", "block rollback"),
        ("restart", "hw rescan"),
        ("recover", "update recover"),
        ("info", "status"),
        ("info", "sys info"),
        ("info", "kernel info"),
        ("list", "ps"),
        ("list", "block list"),
        ("list", "vfs ls"),
        ("ls", "block list"),
        ("ls", "vfs ls"),
    ];
    for (legacy, canonical) in cases {
        assert!(matches!(
            resolve(legacy),
            Err(ResolveError::Ambiguous { .. })
        ));
        let invocation = resolve(canonical)
            .unwrap_or_else(|error| panic!("{canonical} must resolve: {error:?}"));
        assert!(!invocation.spec.id.is_empty(), "input: {canonical}");
    }
}
