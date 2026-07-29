use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};

#[test]
fn fuzz_ipc_deserialize_random_bytes() {
    let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;

    for _ in 0..10_000 {
        let len = ((state >> 16) % 512) as usize;
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);

        let mut data = Vec::with_capacity(len);
        for _ in 0..len {
            data.push((state >> 32) as u8);
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        }

        let _ = IpcPacket::deserialize(&data);
    }
}

#[test]
fn fuzz_ipc_roundtrip_various_payloads() {
    let high_bytes: Vec<u8> = vec![0x00, 0x00, 0x00, 0xFF, 0xFE, 0xFD];
    let high_str: String = high_bytes
        .into_iter()
        .cycle()
        .take(300)
        .map(|b| b as char)
        .collect();

    let payloads = vec![
        Payload::Empty,
        Payload::HealthCheck,
        Payload::Text(String::new()),
        Payload::Text("hello world".repeat(100)),
        Payload::Text(high_str),
        Payload::Binary(vec![0u8; 0]),
        Payload::Binary(vec![0xAB; 4096]),
        Payload::SpawnProcess {
            name: "test".into(),
            priority: 5,
            ram_mb: 128,
        },
        Payload::KillProcess { pid: 42 },
        Payload::Custom("tag".into(), vec![1, 2, 3]),
        Payload::RestoreState(vec![0xFF; 1024]),
        Payload::HotSwap {
            block_id: 1,
            new_binary: vec![0xCC; 512],
            new_version: "2.0.0".into(),
        },
        Payload::Rollback { block_id: 1 },
        Payload::UnloadBlock { block_id: 1 },
    ];

    for payload in payloads {
        let pkt = IpcPacket::new(1, 0, CommandId::HealthCheck, payload);
        let serialized = pkt.serialize().unwrap();
        let deserialized = IpcPacket::deserialize(&serialized).unwrap();
        assert!(deserialized.verify_checksum());
    }
}

#[test]
fn fuzz_ipc_edge_case_sizes() {
    for size in &[0, 1, 255, 256, 1023, 1024, 4096, 65535] {
        let data: Vec<u8> = (0..*size).map(|i| (i % 256) as u8).collect();
        let _ = IpcPacket::deserialize(&data);
    }
}

#[test]
fn fuzz_ipc_truncated_inputs() {
    let full = IpcPacket::new(1, 0, CommandId::HealthCheck, Payload::HealthCheck)
        .serialize()
        .unwrap();

    for len in 0..full.len() {
        let truncated = &full[..len];
        let _ = IpcPacket::deserialize(truncated);
    }
}

#[test]
fn fuzz_ipc_all_ones_all_zeros() {
    for size in &[16, 64, 256, 1024] {
        let zeros = vec![0u8; *size];
        let ones = vec![0xFFu8; *size];
        let _ = IpcPacket::deserialize(&zeros);
        let _ = IpcPacket::deserialize(&ones);
    }
}

#[test]
fn fuzz_ipc_stress_serialize_deserialize() {
    for i in 0..5000u32 {
        let payload = Payload::Custom(
            format!("fuzz_{}", i % 100),
            vec![(i % 256) as u8; (i % 512) as usize],
        );
        let pkt = IpcPacket::new(i, i.wrapping_add(1), CommandId::HealthCheck, payload);
        let bytes = pkt.serialize().unwrap();
        let restored = IpcPacket::deserialize(&bytes).unwrap();
        assert_eq!(restored.header.source_block, i);
        assert!(restored.verify_checksum());
    }
}
