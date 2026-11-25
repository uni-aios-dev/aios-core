//! Kernel implementation of the unified text console backend.
//!
//! The backend is intentionally narrow: it wires the commands that already
//! have a real kernel implementation and reports every other registered
//! command as unavailable instead of inventing a result.

use aios_console::backend::{dispatch, CommandBackend};
use aios_console::capability::{BackendKind, CapabilitySet};
use aios_console::legacy::kernel_net_recheck_text;
use aios_console::net_text::{self, AddressLease, AddressSettings};
use aios_console::output::{CommandOutput, OutputControl};
use aios_console::registry::{help, Invocation};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Text console backend for the `x86_64-unknown-none` kernel shell.
pub(crate) struct KernelBackend {
    capabilities: CapabilitySet,
}

impl KernelBackend {
    /// Creates a backend with all kernel capabilities granted.
    pub(crate) fn new() -> Self {
        Self {
            capabilities: CapabilitySet::all(),
        }
    }
}

impl Default for KernelBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandBackend for KernelBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Kernel
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    fn name(&self) -> &'static str {
        "kernel"
    }

    fn execute(&mut self, invocation: &Invocation) -> CommandOutput {
        match (invocation.spec.id, invocation.subcommand) {
            ("help", _) => CommandOutput::ok(help(invocation.first_arg())),
            ("version", _) => CommandOutput::ok(crate::tui::run_command("ver")),
            ("clear", _) => {
                crate::tui::shell_clear();
                CommandOutput::clear()
            }
            ("echo", _) => CommandOutput::text(invocation.args.join(" ")),
            ("kernel", Some("info")) => CommandOutput::ok(crate::tui::run_command("info")),
            ("kernel", Some("ver")) => CommandOutput::ok(crate::tui::run_command("ver")),
            ("faults", Some("stats") | None) => {
                CommandOutput::ok(crate::tui::run_command("faults"))
            }
            ("faults", Some("history")) => {
                let mut lines = Vec::new();
                for record in crate::fault::history_records() {
                    lines.push(format!(
                        "pid {} {} (crash #{}) rip={:#x}",
                        record.snapshot.pid,
                        record.kind.name(),
                        record.count,
                        record.snapshot.rip
                    ));
                }
                if lines.is_empty() {
                    lines.push("no fault history".to_string());
                }
                CommandOutput::ok(lines)
            }
            ("faults", Some("pending")) => {
                let mut lines = Vec::new();
                for record in crate::fault::pending_records() {
                    lines.push(format!(
                        "pid {} {} (crash #{})",
                        record.snapshot.pid,
                        record.kind.name(),
                        record.count
                    ));
                }
                if lines.is_empty() {
                    lines.push("no pending faults".to_string());
                }
                CommandOutput::ok(lines)
            }
            ("fault", Some("resolve")) => self.fault_resolve(invocation),
            ("wifi", None) | ("wifi", Some("status")) => {
                CommandOutput::ok(crate::tui::run_command("wifi"))
            }
            ("wifi", Some("sim")) => CommandOutput::ok(crate::tui::run_command("wifi sim")),
            ("wifi", Some("real")) => CommandOutput::ok(crate::tui::run_command("wifi real")),
            ("wifi", Some("scan")) => match crate::wifi::start_scan() {
                Ok(()) => CommandOutput::text("Wi-Fi scan started"),
                Err(error) => CommandOutput::error(error),
            },
            ("wifi", Some("connect")) => {
                let Some(ssid) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let pass = invocation.arg(1).unwrap_or("");
                match crate::wifi::connect(ssid.as_bytes(), pass.as_bytes()) {
                    Ok(()) => CommandOutput::text(format!("associating with '{ssid}'")),
                    Err(error) => CommandOutput::error(error),
                }
            }
            ("wifi", Some("disconnect")) => {
                crate::wifi::disconnect();
                CommandOutput::text("Wi-Fi disconnected")
            }
            ("udp", _) => CommandOutput::ok(crate::tui::cmd_udp(&invocation.args.join(" "))),
            ("http", _) => CommandOutput::ok(crate::tui::cmd_http(&invocation.args.join(" "))),
            ("socktest", _) => CommandOutput::ok(crate::tui::cmd_socktest()),
            ("net", Some("show")) | ("net", Some("config")) => {
                CommandOutput::ok(net_text::format_settings(&Self::net_settings()))
            }
            ("net", Some("status")) => {
                let settings = Self::net_settings();
                let lease = Self::net_lease();
                CommandOutput::ok(net_text::format_status(
                    crate::net::state().name(),
                    crate::net::nic_kind(),
                    crate::net::nic_mac(),
                    &settings,
                    lease.as_ref(),
                ))
            }
            ("net", Some("lease")) => {
                let lease = Self::net_lease().unwrap_or_default();
                CommandOutput::ok(net_text::format_lease(&lease))
            }
            ("net", Some("set")) => {
                if invocation.args.is_empty() {
                    return CommandOutput::usage(invocation.spec.usage);
                }
                let mut next = Self::net_settings();
                for arg in &invocation.args {
                    if let Err(error) = net_text::apply_update(&mut next, arg) {
                        return CommandOutput::error(error);
                    }
                }
                if let Err(error) = net_text::validate(&next) {
                    return CommandOutput::error(error);
                }
                Self::apply_net_settings(&next);
                CommandOutput::ok(net_text::format_settings(&next))
            }
            ("net", Some("apply")) => {
                crate::net::apply(&crate::net::settings());
                CommandOutput::text("network settings reapplied")
            }
            ("net", Some("validate")) => match net_text::validate(&Self::net_settings()) {
                Ok(()) => CommandOutput::text("network configuration valid"),
                Err(error) => CommandOutput::error(error),
            },
            ("net", Some("reset")) => {
                let next = AddressSettings::default();
                Self::apply_net_settings(&next);
                CommandOutput::ok(net_text::format_settings(&next))
            }
            ("net", Some("adapters")) => {
                let mut lines = Vec::new();
                match crate::net::nic_mac() {
                    Some(mac) => lines.push(format!(
                        "{:<10} {:<12} {}",
                        "wired",
                        crate::net::nic_kind(),
                        net_text::format_mac(mac)
                    )),
                    None => lines.push("wired      none".to_string()),
                }
                let wifi = crate::tui::run_command("wifi");
                lines.push(format!(
                    "{:<10} {}",
                    "wifi",
                    wifi.first()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string())
                ));
                CommandOutput::ok(lines)
            }
            ("net", Some("recheck")) => {
                crate::net::recheck();
                CommandOutput::text(kernel_net_recheck_text())
            }
            _ => CommandOutput::unavailable(&format!(
                "'{}' has no kernel backend yet",
                invocation.display_name()
            )),
        }
    }
}

impl KernelBackend {
    fn net_settings() -> AddressSettings {
        let settings = crate::net::settings();
        AddressSettings {
            dhcp: settings.dhcp,
            ip: settings.ip,
            mask: settings.mask,
            gw: settings.gw,
            dns1: settings.dns1,
            dns2: settings.dns2,
        }
    }

    fn net_lease() -> Option<AddressLease> {
        crate::net::lease().map(|lease| AddressLease {
            ip: lease.ip,
            mask: lease.mask,
            gw: lease.gw,
            dns1: lease.dns1,
            dns2: lease.dns2,
            server: lease.server,
            secs: lease.secs,
        })
    }

    fn apply_net_settings(settings: &AddressSettings) {
        crate::net::apply(&crate::net::Settings {
            dhcp: settings.dhcp,
            ip: settings.ip,
            mask: settings.mask,
            gw: settings.gw,
            dns1: settings.dns1,
            dns2: settings.dns2,
        });
    }

    fn fault_resolve(&self, invocation: &Invocation) -> CommandOutput {
        let Some(pid) = invocation
            .first_arg()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let Some(action) = invocation.arg(1) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let recovery = match action.to_ascii_lowercase().as_str() {
            "resume" => crate::fault::Recovery::Resume,
            "terminate" => crate::fault::Recovery::Terminate,
            "rollback" => crate::fault::Recovery::Rollback,
            _ => return CommandOutput::usage(invocation.spec.usage),
        };
        let Some(record) = crate::fault::peek() else {
            return CommandOutput::error("no pending fault to resolve");
        };
        if u64::from(record.snapshot.pid) != pid {
            return CommandOutput::error(&format!(
                "pending fault belongs to pid {}, not {pid}",
                record.snapshot.pid
            ));
        }
        crate::fault::resolve(recovery);
        CommandOutput::text(format!(
            "fault for pid {pid} resolved with {}",
            recovery.name()
        ))
    }
}

/// Dispatches one `console ...` line and returns the lines to show in the shell.
pub(crate) fn dispatch_line(input: &str) -> Vec<String> {
    let mut backend = KernelBackend::new();
    let output = dispatch(&mut backend, input);
    match output.control {
        OutputControl::Clear => crate::tui::shell_clear(),
        OutputControl::Exit(_) | OutputControl::None => {}
    }
    output.lines
}
