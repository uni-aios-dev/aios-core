//! Concrete reasons why registered commands have no real host backend.
//!
//! Every entry documents a missing library API or privilege, not a successful
//! action. Backends use these strings to replace the generic
//! `has no host backend yet` message with an actionable explanation.

/// Returns the reason a registered command has no host backend.
///
/// `command_id` is the canonical spec identifier and `subcommand` is the
/// lowercase declared subcommand. `None` means the command is outside the
/// investigated set, so callers keep their generic fallback message.
pub fn host_unavailable_reason(command_id: &str, subcommand: Option<&str>) -> Option<&'static str> {
    match (command_id, subcommand) {
        ("jobs", _) | ("fg", _) | ("bg", _) => Some(
            "aios-process-mgr has no job control: no OS process groups, signals or TTY foreground",
        ),
        ("acl", _) => Some(
            "no path-scoped ACL API: AclContext grants are process-global tokens and caps tokens are keyed by block ID",
        ),
        ("ping", _) => Some(
            "aios-net has no ICMP reachability API; raw sockets need CAP_NET_RAW and TCP connect is not an echo reply",
        ),
        ("net", Some("recheck")) => Some(
            "host NetManager exposes cached status only; the real recheck() is kernel-only (no_std)",
        ),
        ("fm", _) => Some(
            "the aios host owns no live FileManager; transfer cancellation needs a host-injected control surface",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_unavailable_commands_have_precise_reasons() {
        let cases = [
            ("jobs", None, "no job control"),
            ("fg", None, "no job control"),
            ("bg", None, "no job control"),
            ("acl", Some("grant"), "no path-scoped ACL API"),
            ("ping", None, "no ICMP reachability API"),
            ("net", Some("recheck"), "recheck() is kernel-only"),
            ("fm", Some("cancel"), "owns no live FileManager"),
        ];
        for (command, subcommand, fragment) in cases {
            let reason = host_unavailable_reason(command, subcommand)
                .unwrap_or_else(|| panic!("no reason for {command} {subcommand:?}"));
            assert!(reason.contains(fragment), "reason for {command}: {reason}");
        }
    }

    #[test]
    fn implemented_and_unknown_commands_have_no_reason() {
        assert_eq!(host_unavailable_reason("block", Some("swap")), None);
        assert_eq!(host_unavailable_reason("net", Some("show")), None);
        assert_eq!(host_unavailable_reason("state", Some("extract")), None);
        assert_eq!(host_unavailable_reason("unknown", None), None);
    }
}
