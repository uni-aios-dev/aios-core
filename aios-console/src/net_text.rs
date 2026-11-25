use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Backend-neutral IPv4 settings used by kernel console text commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSettings {
    pub dhcp: bool,
    pub ip: [u8; 4],
    pub mask: [u8; 4],
    pub gw: [u8; 4],
    pub dns1: [u8; 4],
    pub dns2: [u8; 4],
}

impl Default for AddressSettings {
    fn default() -> Self {
        Self {
            dhcp: true,
            ip: [0; 4],
            mask: [0; 4],
            gw: [0; 4],
            dns1: [0; 4],
            dns2: [0; 4],
        }
    }
}

/// Backend-neutral bound lease used by kernel console text commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddressLease {
    pub ip: [u8; 4],
    pub mask: [u8; 4],
    pub gw: [u8; 4],
    pub dns1: [u8; 4],
    pub dns2: [u8; 4],
    pub server: [u8; 4],
    pub secs: u32,
}

impl AddressLease {
    /// Whether an IPv4 address is bound.
    pub fn bound(&self) -> bool {
        self.ip != [0; 4]
    }
}

/// Parses one strict dotted-quad IPv4 address.
pub fn parse_quad(text: &str) -> Option<[u8; 4]> {
    let mut parts = text.split('.');
    let mut out = [0u8; 4];
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse::<u8>().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// Formats an IPv4 address for display.
pub fn format_ip(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

/// Formats a MAC address for display.
pub fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Applies one `key=value` token to a settings snapshot.
pub fn apply_update(settings: &mut AddressSettings, token: &str) -> Result<(), &'static str> {
    let (key, value) = token.split_once('=').ok_or("expected key=value")?;
    let value = value.trim();
    match key.trim().to_ascii_lowercase().as_str() {
        "dhcp" => {
            settings.dhcp = match value.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => return Err("dhcp expects true/false or 1/0"),
            };
        }
        "ip" | "address" => settings.ip = parse_quad(value).ok_or("invalid ip")?,
        "mask" | "netmask" => settings.mask = parse_quad(value).ok_or("invalid mask")?,
        "gw" | "gateway" => settings.gw = parse_quad(value).ok_or("invalid gateway")?,
        "dns1" | "dns_primary" => settings.dns1 = parse_quad(value).ok_or("invalid dns1")?,
        "dns2" | "dns_secondary" => settings.dns2 = parse_quad(value).ok_or("invalid dns2")?,
        _ => return Err("unknown network setting"),
    }
    Ok(())
}

/// Validates a settings snapshot before it is applied to the link.
pub fn validate(settings: &AddressSettings) -> Result<(), &'static str> {
    if !settings.dhcp && settings.ip == [0; 4] {
        return Err("static mode requires ip");
    }
    Ok(())
}

/// Renders settings as stable `key value` rows.
pub fn format_settings(settings: &AddressSettings) -> Vec<String> {
    vec![
        format!("{:<12} {}", "dhcp", yes_no(settings.dhcp)),
        format!("{:<12} {}", "ip", format_ip(settings.ip)),
        format!("{:<12} {}", "mask", format_ip(settings.mask)),
        format!("{:<12} {}", "gateway", format_ip(settings.gw)),
        format!("{:<12} {}", "dns1", format_ip(settings.dns1)),
        format!("{:<12} {}", "dns2", format_ip(settings.dns2)),
    ]
}

/// Renders link state, adapter identity, settings and lease in one snapshot.
pub fn format_status(
    state: &str,
    nic: &str,
    mac: Option<[u8; 6]>,
    settings: &AddressSettings,
    lease: Option<&AddressLease>,
) -> Vec<String> {
    let mut lines = vec![
        format!("{:<12} {}", "state", state),
        format!("{:<12} {}", "nic", nic),
        format!(
            "{:<12} {}",
            "mac",
            mac.map(format_mac).unwrap_or_else(|| String::from("none"))
        ),
    ];
    lines.extend(format_settings(settings));
    match lease {
        Some(lease) if lease.bound() => lines.extend(format_lease(lease)),
        _ => lines.push(format!("{:<12} none", "lease")),
    }
    lines
}

/// Renders a bound lease, or the honest no-lease message when unbound.
pub fn format_lease(lease: &AddressLease) -> Vec<String> {
    if !lease.bound() {
        return vec![
            format!("{:<12} none", "lease"),
            String::from("no DHCP lease acquired in this session"),
        ];
    }
    vec![
        format!("{:<12} bound", "lease"),
        format!("{:<12} {}", "ip", format_ip(lease.ip)),
        format!("{:<12} {}", "mask", format_ip(lease.mask)),
        format!("{:<12} {}", "gateway", format_ip(lease.gw)),
        format!("{:<12} {}", "dns1", format_ip(lease.dns1)),
        format!("{:<12} {}", "dns2", format_ip(lease.dns2)),
        format!("{:<12} {}", "server", format_ip(lease.server)),
        format!("{:<12} {}s", "expires", lease.secs),
    ]
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quad_accepts_only_strict_dotted_quads() {
        assert_eq!(parse_quad("10.0.2.15"), Some([10, 0, 2, 15]));
        assert_eq!(parse_quad("255.255.255.255"), Some([255; 4]));
        assert_eq!(parse_quad("10.0.2"), None);
        assert_eq!(parse_quad("10.0.2.256"), None);
        assert_eq!(parse_quad("10.0.2.15.1"), None);
        assert_eq!(parse_quad("10.0.a.1"), None);
    }

    #[test]
    fn apply_update_supports_partial_dotted_keys() {
        let mut settings = AddressSettings::default();
        apply_update(&mut settings, "dhcp=off").unwrap();
        apply_update(&mut settings, "ip=192.168.1.42").unwrap();
        apply_update(&mut settings, "netmask=255.255.255.0").unwrap();
        apply_update(&mut settings, "gateway=192.168.1.1").unwrap();
        apply_update(&mut settings, "dns_primary=8.8.8.8").unwrap();
        assert_eq!(settings.ip, [192, 168, 1, 42]);
        assert_eq!(settings.mask, [255, 255, 255, 0]);
        assert_eq!(settings.gw, [192, 168, 1, 1]);
        assert_eq!(settings.dns1, [8, 8, 8, 8]);
        assert_eq!(validate(&settings), Ok(()));
        assert_eq!(
            apply_update(&mut settings, "dns3=1.1.1.1"),
            Err("unknown network setting")
        );
    }

    #[test]
    fn formatters_report_lease_honestly() {
        let unbound = AddressLease::default();
        let lines = format_lease(&unbound);
        assert_eq!(lines[0], "lease        none");
        assert_eq!(lines[1], "no DHCP lease acquired in this session");

        let bound = AddressLease {
            ip: [10, 0, 2, 15],
            mask: [255, 255, 255, 0],
            gw: [10, 0, 2, 2],
            secs: 3600,
            ..AddressLease::default()
        };
        let lines = format_status(
            "internet",
            "e1000",
            Some([0x52, 0x54, 0, 0x12, 0x34, 0x56]),
            &AddressSettings::default(),
            Some(&bound),
        );
        assert_eq!(lines[0], "state        internet");
        assert_eq!(lines[1], "nic          e1000");
        assert_eq!(lines[2], "mac          52:54:00:12:34:56");
        assert!(lines.iter().any(|line| line == "lease        bound"));
        assert!(lines.iter().any(|line| line == "expires      3600s"));
    }
}
