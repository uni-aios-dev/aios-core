use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload, Response};
use proptest::prelude::*;

fn arb_payload() -> impl Strategy<Value = Payload> {
    prop_oneof![
        Just(Payload::Empty),
        any::<Vec<u8>>().prop_map(Payload::Binary),
        ".*".prop_map(Payload::Text),
        (".*", ".*", any::<Vec<u8>>()).prop_map(|(name, version, binary)| Payload::RegisterBlock {
            name,
            version,
            binary
        }),
        any::<u32>().prop_map(|id| Payload::UnloadBlock { block_id: id }),
        Just(Payload::GetTopology),
        (".*", any::<u8>(), any::<u64>()).prop_map(|(name, priority, ram_mb)| {
            Payload::SpawnProcess {
                name,
                priority,
                ram_mb,
            }
        }),
        any::<u64>().prop_map(|pid| Payload::KillProcess { pid }),
        (any::<u64>(), any::<u8>())
            .prop_map(|(pid, new_priority)| Payload::AdjustPriority { pid, new_priority }),
        Just(Payload::HealthCheck),
        Just(Payload::ExtractState),
        any::<Vec<u8>>().prop_map(Payload::RestoreState),
        (any::<u32>(), any::<Vec<u8>>(), ".*").prop_map(|(block_id, new_binary, new_version)| {
            Payload::HotSwap {
                block_id,
                new_binary,
                new_version,
            }
        }),
        any::<u32>().prop_map(|block_id| Payload::Rollback { block_id }),
        (".*", any::<Vec<u8>>()).prop_map(|(tag, data)| Payload::Custom(tag, data)),
    ]
}

fn arb_command_id() -> impl Strategy<Value = CommandId> {
    prop_oneof![
        Just(CommandId::RegisterBlock),
        Just(CommandId::UnloadBlock),
        Just(CommandId::GetTopology),
        Just(CommandId::SpawnProcess),
        Just(CommandId::KillProcess),
        Just(CommandId::AdjustPriority),
        Just(CommandId::HealthCheck),
        Just(CommandId::ExtractState),
        Just(CommandId::RestoreState),
        Just(CommandId::HotSwap),
        Just(CommandId::Rollback),
        Just(CommandId::IntentCommand),
        Just(CommandId::Custom),
    ]
}

proptest! {
    #[test]
    fn ipc_packet_serialize_deserialize_roundtrip(
        source in any::<u32>(),
        target in any::<u32>(),
        cmd in arb_command_id(),
        payload in arb_payload(),
    ) {
        let packet = IpcPacket::new(source, target, cmd, payload);
        let serialized = packet.serialize().expect("serialize should succeed");
        let deserialized = IpcPacket::deserialize(&serialized).expect("deserialize should succeed");
        prop_assert_eq!(packet.header.source_block, deserialized.header.source_block);
        prop_assert_eq!(packet.header.target_block, deserialized.header.target_block);
        prop_assert_eq!(packet.header.command_id, deserialized.header.command_id);
        prop_assert_eq!(packet.payload, deserialized.payload);
    }

    #[test]
    fn ipc_packet_checksum_always_valid(
        source in any::<u32>(),
        target in any::<u32>(),
        cmd in arb_command_id(),
        payload in arb_payload(),
    ) {
        let packet = IpcPacket::new(source, target, cmd, payload);
        prop_assert!(packet.verify_checksum(), "checksum should be valid after construction");
    }

    #[test]
    fn ipc_packet_new_assigns_unique_ids(
        a_source in any::<u32>(),
        a_target in any::<u32>(),
        b_source in any::<u32>(),
        b_target in any::<u32>(),
    ) {
        let a = IpcPacket::new(a_source, a_target, CommandId::HealthCheck, Payload::Empty);
        let b = IpcPacket::new(b_source, b_target, CommandId::HealthCheck, Payload::Empty);
        prop_assert_ne!(a.header.packet_id, b.header.packet_id, "packet IDs must be unique");
    }

    #[test]
    fn ipc_response_serialize_deserialize_roundtrip(
        payload in arb_payload(),
    ) {
        let response = Response::Success(payload);
        let serialized = bincode::serialize(&response).unwrap();
        let deserialized: Response = bincode::deserialize(&serialized).unwrap();
        prop_assert_eq!(&response, &deserialized);
    }

    #[test]
    fn ipc_response_failure_roundtrip(
        code in any::<u16>(),
        message in ".*",
    ) {
        let response = Response::Failure { code, message };
        let serialized = bincode::serialize(&response).unwrap();
        let deserialized: Response = bincode::deserialize(&serialized).unwrap();
        prop_assert_eq!(&response, &deserialized);
    }

    #[test]
    fn payload_binary_preserves_data(
        data in any::<Vec<u8>>(),
    ) {
        let payload = Payload::Binary(data.clone());
        let serialized = bincode::serialize(&payload).unwrap();
        let deserialized: Payload = bincode::deserialize(&serialized).unwrap();
        match deserialized {
            Payload::Binary(recovered) => prop_assert_eq!(&recovered, &data),
            _ => prop_assert!(false, "expected Binary variant"),
        }
    }

    #[test]
    fn payload_text_preserves_string(
        text in ".*",
    ) {
        let payload = Payload::Text(text.clone());
        let serialized = bincode::serialize(&payload).unwrap();
        let deserialized: Payload = bincode::deserialize(&serialized).unwrap();
        match deserialized {
            Payload::Text(recovered) => prop_assert_eq!(&recovered, &text),
            _ => prop_assert!(false, "expected Text variant"),
        }
    }

    #[test]
    fn payload_serialization_size_nonzero(
        payload in arb_payload(),
    ) {
        let serialized = bincode::serialize(&payload).unwrap();
        prop_assert!(!serialized.is_empty(), "serialized payload should not be empty");
    }
}
