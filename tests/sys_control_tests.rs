//! Integration tests for the `aios-sys-control` stack (v2.29.0 Block 1).
//!
//! Covers the four services (DHCP, net manager, layout switcher, power
//! governor, keyring vault) plus the [`SysControlHub`] facade end-to-end.
//! Everything runs against simulated/mock backends — no hardware or real
//! Wi-Fi adapters are required.

use std::net::Ipv4Addr;
use std::time::Duration;

use aios_llm::{BackendKind, CloudProvider};
use aios_sys_control::dhcp::{
    build_discover, build_request, parse_ack, parse_offer, random_mac, DhcpClient,
};
use aios_sys_control::input_i18n::{Hotkey, KeyCode, Layout, LayoutManager, Modifiers};
use aios_sys_control::keyring::{KeyringVault, UnlockPolicy};
use aios_sys_control::net_manager::{LinkState, LinkStatus, NetManager, WifiBackend, WifiBand};
use aios_sys_control::power_mgr::{
    MockPower, PowerBackend, PowerManager, ThermalGovernor, ThrottleDecision,
};
use aios_sys_control::{SysControlHub, SysStatusSnapshot};

// ============================================================
// Section 1: DHCP wire format (pure builders/parsers)
// ============================================================

#[test]
fn test_discover_packet_has_magic_cookie_and_type53() {
    let p = build_discover(0xDEADBEEF, &[2, 0, 0, 0, 0, 1]);
    assert_eq!(p[0], 1, "op=BOOTREQUEST");
    assert_eq!(&p[3..7], &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(p[236..240], [99, 130, 83, 99]);
    assert!(p.windows(3).any(|w| w == [53, 1, 1]), "opt 53=DHCPDISCOVER");
}

#[test]
fn test_request_packet_selects_server_and_address() {
    let srv = Ipv4Addr::new(192, 168, 4, 1);
    let ip = Ipv4Addr::new(192, 168, 4, 77);
    let p = build_request(42, &random_mac(), srv, ip);
    let find_opt = |code: u8| -> Vec<u8> {
        let mut i = 240;
        loop {
            let c = p[i];
            if c == 255 {
                return Vec::new();
            }
            let len = p[i + 1] as usize;
            if c == code {
                return p[i + 2..i + 2 + len].to_vec();
            }
            i += 2 + len;
        }
    };
    assert_eq!(find_opt(54), srv.octets().to_vec(), "option 54 = server id");
    assert_eq!(
        find_opt(50),
        ip.octets().to_vec(),
        "option 50 = requested ip"
    );
}

#[test]
fn test_offer_roundtrip_through_parser() {
    let mut offer = vec![0u8; 240];
    offer[0] = 2;
    offer[16..20].copy_from_slice(&[10, 1, 2, 3]);
    offer[236..240].copy_from_slice(&[99, 130, 83, 99]);
    offer.extend_from_slice(&[53, 1, 2]);
    offer.extend_from_slice(&[54, 4, 10, 1, 2, 254]);
    offer.push(255);
    let parsed = parse_offer(&offer).expect("valid offer");
    assert_eq!(parsed.yiaddr, Ipv4Addr::new(10, 1, 2, 3));
    assert_eq!(parsed.server_id, Some(Ipv4Addr::new(10, 1, 2, 254)));
}

#[test]
fn test_ack_yields_complete_lease() {
    let mut ack = vec![0u8; 240];
    ack[0] = 2;
    ack[16..20].copy_from_slice(&[172, 16, 0, 9]);
    ack[236..240].copy_from_slice(&[99, 130, 83, 99]);
    ack.extend_from_slice(&[53, 1, 5]);
    ack.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
    ack.extend_from_slice(&[3, 4, 172, 16, 0, 1]);
    ack.extend_from_slice(&[51, 4, 0, 0, 14, 16]);
    let lease = parse_ack(&ack).expect("valid ack");
    assert_eq!(lease.ip, Ipv4Addr::new(172, 16, 0, 9));
    assert_eq!(lease.gateway, Some(Ipv4Addr::new(172, 16, 0, 1)));
    assert_eq!(lease.lease_secs, 3600);
}

#[test]
fn test_dhcp_client_timeout_without_server() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = DhcpClient::with_mac([2, 0, 0, 0, 0, 9]);
    let result = rt.block_on(client.acquire(Duration::from_millis(120)));
    assert!(result.is_err(), "no DHCP server in CI -> timeout expected");
}

// ============================================================
// Section 2: Net manager over the simulated ether
// ============================================================

fn sim_manager() -> NetManager {
    NetManager::new(WifiBackend::Simulated(Default::default()), None)
}

#[tokio::test]
async fn test_simulated_scan_lists_three_networks() {
    let nets = sim_manager().scan().await.unwrap();
    let ssids: Vec<&str> = nets.iter().map(|n| n.ssid.as_str()).collect();
    assert!(ssids.contains(&"aios-lab-5g"));
    assert!(ssids.contains(&"home-net"));
    assert!(ssids.contains(&"coffee-shop"));
}

#[tokio::test]
async fn test_connect_secured_network_runs_dhcp_lease() {
    let mut mgr = sim_manager();
    let link = mgr
        .connect("aios-lab-5g", Some("aios-rocks"))
        .await
        .unwrap();
    assert_eq!(link.state, LinkState::Connected);
    assert_eq!(link.ssid.as_deref(), Some("aios-lab-5g"));
    assert!(link.ipv4.is_some(), "simulated DHCP grants an address");
}

#[tokio::test]
async fn test_wrong_passphrase_fails_with_permission_denied() {
    let mut mgr = sim_manager();
    let err = mgr.connect("home-net", Some("nope")).await.unwrap_err();
    assert!(
        format!("{err}").contains("wrong passphrase"),
        "unexpected error: {err}"
    );
    assert!(matches!(mgr.link().state, LinkState::Failed(_)));
}

#[tokio::test]
async fn test_unknown_ssid_is_not_detected() {
    let mut mgr = sim_manager();
    assert!(mgr.connect("ghost-net", None).await.is_err());
}

#[tokio::test]
async fn test_open_network_needs_no_password() {
    let mut mgr = sim_manager();
    let link = mgr.connect("coffee-shop", None).await.unwrap();
    assert_eq!(link.state, LinkState::Connected);
}

#[tokio::test]
async fn test_disconnect_resets_cached_link() {
    let mut mgr = sim_manager();
    mgr.connect("home-net", Some("homenet-pass")).await.unwrap();
    mgr.disconnect().await.unwrap();
    assert_eq!(mgr.status().state, LinkState::Disconnected);
}

#[tokio::test]
async fn test_status_segment_renders_connected_line() {
    let mut mgr = sim_manager();
    mgr.connect("aios-lab-5g", Some("aios-rocks"))
        .await
        .unwrap();
    let seg = mgr.status().status_segment();
    assert!(seg.starts_with("[Wi-Fi:"), "got: {seg}");
    assert!(seg.contains("aios-lab-5g"));
    assert!(seg.contains("(5GHz)"));
}

// ============================================================
// Section 3: Layout switcher
// ============================================================

#[test]
fn test_default_hotkeys_flip_layout_both_ways() {
    let mut lm = LayoutManager::default();
    assert!(lm.apply_hotkey(Hotkey::AltShift));
    assert_eq!(lm.active(), Layout::Russian);
    assert!(lm.apply_hotkey(Hotkey::CtrlShift));
    assert_eq!(lm.active(), Layout::English);
}

#[test]
fn test_unregistered_combo_returns_false() {
    let mut lm = LayoutManager::default();
    assert!(!lm.apply_hotkey(Hotkey::CmdSpace));
    assert_eq!(lm.indicator(), "EN");
}

#[test]
fn test_feed_key_detects_registered_modifier_combos_only() {
    let mut lm = LayoutManager::bare(Layout::English);
    assert!(!lm.feed_key(Modifiers::new(true, false, true, false), KeyCode::Other));
    assert!(lm.feed_key(Modifiers::new(true, false, true, false), KeyCode::Shift));
    assert_eq!(lm.active(), Layout::Russian);
}

#[test]
fn test_window_override_pinned_layout_survives_toggles() {
    let mut lm = LayoutManager::default();
    lm.set_window_layout("sudo-prompt", Layout::Russian);
    for _ in 0..3 {
        lm.apply_hotkey(Hotkey::AltShift);
    }
    assert_eq!(lm.effective_layout("sudo-prompt"), Layout::Russian);
    assert_eq!(lm.effective_layout("editor"), Layout::Russian);
}

#[test]
fn test_indicator_pair_active_first() {
    assert_eq!(LayoutManager::default().status_segment(), "[EN/RU]");
    assert_eq!(
        LayoutManager::bare(Layout::Russian).status_segment(),
        "[RU/EN]"
    );
}

// ============================================================
// Section 4: Power / thermal governor for LLM offloading
// ============================================================

#[test]
fn test_cool_laptop_keeps_local_inference() {
    let mut pm = PowerManager::mock(MockPower::default());
    assert_eq!(pm.poll(), ThrottleDecision::KeepCurrentBackend);
    assert!(matches!(pm.llm_backend(), BackendKind::MicroLocal));
}

#[test]
fn test_full_overheat_cycle_cloud_and_back() {
    let mut pm = PowerManager::mock(MockPower {
        cpu_temp_c: 95.0,
        gpu_temp_c: 88.0,
        battery_percent: Some(19),
        ac_online: false,
        ..MockPower::default()
    });
    assert_eq!(pm.poll(), ThrottleDecision::MoveToCloud);
    assert!(matches!(
        pm.llm_backend(),
        BackendKind::Cloud(CloudProvider::Groq)
    ));
    pm.set_mock(MockPower {
        cpu_temp_c: 55.0,
        gpu_temp_c: 60.0,
        ..MockPower::default()
    });
    assert_eq!(pm.poll(), ThrottleDecision::ReturnToLocal);
    assert!(matches!(pm.llm_backend(), BackendKind::MicroLocal));
}

#[test]
fn test_hysteresis_zone_does_not_oscillate() {
    let mut g = ThermalGovernor::default();
    g.update(81.0);
    assert!(g.is_throttled());
    g.update(75.0);
    assert!(g.is_throttled(), "still hot enough to stay on cloud");
    g.update(70.0);
    assert!(!g.is_throttled());
}

#[test]
fn test_custom_thresholds_and_provider() {
    let g = ThermalGovernor::new(60.0, 45.0, CloudProvider::GoogleAiStudio);
    assert!(matches!(g.cloud_provider(), CloudProvider::GoogleAiStudio));
}

#[test]
fn test_battery_and_ac_fields_surface_through_manager() {
    let pm = PowerManager::new(PowerBackend::Mock(MockPower {
        battery_percent: Some(12),
        ac_online: false,
        lid_open: false,
        cpu_temp_c: 40.0,
        gpu_temp_c: 41.0,
    }));
    assert_eq!(pm.battery_percent(), Some(12));
    assert_eq!(pm.ac_online(), Some(false));
    assert_eq!(pm.lid_open(), Some(false));
    assert!((pm.max_temp_c() - 41.0).abs() < 1e-4);
}

// ============================================================
// Section 5: Keyring vault (AES-256-GCM + redb)
// ============================================================

fn vault_path(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("aios_systest_{name}.redb"));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn test_vault_store_read_delete_cycle() {
    let path = vault_path("cycle");
    let vault = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("pw".into())).unwrap();
    vault.set_secret("llm/openrouter", "sk-or-v1-xyz").unwrap();
    assert_eq!(
        vault.get_secret("llm/openrouter").unwrap().as_deref(),
        Some("sk-or-v1-xyz")
    );
    assert!(vault.delete_secret("llm/openrouter").unwrap());
    assert_eq!(vault.get_secret("llm/openrouter").unwrap(), None);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_vault_reopen_requires_same_password() {
    let path = vault_path("reopen");
    drop(KeyringVault::open(&path, &UnlockPolicy::MasterPassword("alpha".into())).unwrap());
    let ok = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("alpha".into()));
    assert!(ok.is_ok());
    let bad = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("beta".into()));
    assert!(bad.is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_vault_tee_binding_is_portable_across_handles() {
    let path = vault_path("tee");
    let v = KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 9001 }).unwrap();
    v.set_secret("wpa/aios-lab-5g", "aios-rocks").unwrap();
    drop(v);
    let reopened =
        KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 9001 }).unwrap();
    assert_eq!(
        reopened.get_secret("wpa/aios-lab-5g").unwrap().as_deref(),
        Some("aios-rocks")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_vault_lists_keys_sorted_without_values() {
    let path = vault_path("list");
    let v = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("pw".into())).unwrap();
    for k in ["c", "a", "b"] {
        v.set_secret(k, "v").unwrap();
    }
    assert_eq!(v.list_keys().unwrap(), vec!["a", "b", "c"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_password_rotation_keeps_secrets_readable() {
    let path = vault_path("rotate");
    let mut v = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("one".into())).unwrap();
    v.set_secret("k", "survives").unwrap();
    v.change_master_password("two").unwrap();
    drop(v);
    let v2 = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("two".into())).unwrap();
    assert_eq!(v2.get_secret("k").unwrap().as_deref(), Some("survives"));
    let _ = std::fs::remove_file(&path);
}

// ============================================================
// Section 6: SysControlHub facade end-to-end
// ============================================================

#[tokio::test]
async fn test_hub_snapshot_defaults_are_sane() {
    let hub = SysControlHub::defaults();
    let snap = hub.refresh_status().await;
    assert_eq!(snap.layout, "EN");
    assert!(!snap.throttled);
    assert_eq!(hub.snapshot(), snap);
}

#[tokio::test]
async fn test_hub_wifi_connect_updates_status_line() {
    let hub = SysControlHub::new(
        NetManager::new(WifiBackend::Simulated(Default::default()), None),
        LayoutManager::default(),
        PowerManager::mock(MockPower::default()),
    );
    let link = hub
        .connect_wifi("home-net", Some("homenet-pass"))
        .await
        .unwrap();
    assert_eq!(link.state, LinkState::Connected);
    let snap = hub.refresh_status().await;
    let line = snap.status_line();
    assert!(line.contains("[EN/RU]"));
    assert!(line.contains("BAT"));
}

#[tokio::test]
async fn test_hub_layout_toggle_reflected_in_next_snapshot() {
    let hub = SysControlHub::defaults();
    assert!(hub.feed_hotkey(Hotkey::CtrlShift).await);
    assert_eq!(hub.layout_indicator().await, "RU");
    let snap = hub.refresh_status().await;
    assert_eq!(snap.layout, "RU");
    assert!(snap.status_line().contains("[RU/EN]"));
}

#[tokio::test]
async fn test_hub_throttle_flag_appears_when_governor_trips() {
    let mut pm = PowerManager::mock(MockPower {
        cpu_temp_c: 85.0,
        gpu_temp_c: 80.0,
        ..MockPower::default()
    });
    assert_eq!(pm.poll(), ThrottleDecision::MoveToCloud);
    let hub = SysControlHub::new(sim_manager(), LayoutManager::default(), pm);
    let snap = hub.refresh_status().await;
    assert!(snap.throttled);
    assert!(snap.status_line().contains("[LLM: cloud]"));
    assert!(matches!(hub.llm_backend_decision(), BackendKind::Cloud(_)));
}

#[test]
fn test_snapshot_serializes_to_json_for_bridge() {
    let snap = SysStatusSnapshot {
        wifi: Some(LinkStatus {
            state: LinkState::Connected,
            ssid: Some("home-net".into()),
            ipv4: None,
            gateway: None,
            dns: None,
            rssi_dbm: -61,
            band: Some(WifiBand::Ghz24),
        }),
        layout: "RU".into(),
        battery_percent: Some(64),
        ac_online: Some(true),
        max_temp_c: 51.5,
        throttled: false,
    };
    let json = serde_json::to_string(&snap).unwrap();
    assert!(json.contains("\"layout\":\"RU\""));
    assert!(json.contains("\"ssid\":\"home-net\""));
    let back: SysStatusSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
}
