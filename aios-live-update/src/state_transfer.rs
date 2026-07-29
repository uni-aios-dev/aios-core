use aios_core::error::Result;
use aios_core::ipc_protocol::IpcPacket;
use aios_ipc::bus::IpcBus;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: Vec<u8>,
    pub pending_packets: Vec<IpcPacket>,
}

pub struct StateTransferManager;

impl StateTransferManager {
    pub fn extract_state(queue: &mut IpcBus, state: &[u8]) -> Result<Snapshot> {
        let pending_packets = queue.freeze();
        log::info!(
            "StateTransfer: Extracted {} pending packets ({} bytes state)",
            pending_packets.len(),
            state.len()
        );
        Ok(Snapshot {
            state: state.to_vec(),
            pending_packets,
        })
    }

    pub fn restore_state(queue: &mut IpcBus, snapshot: Snapshot) -> Result<()> {
        let count = snapshot.pending_packets.len();
        queue.unfreeze(snapshot.pending_packets);
        log::info!("StateTransfer: Restored {} packets to queue", count);
        Ok(())
    }

    pub fn reroute_snapshot(snapshot: &mut Snapshot, old_target: u32, new_target: u32) -> usize {
        let mut count = 0;
        for pkt in snapshot.pending_packets.iter_mut() {
            if pkt.header.target_block == old_target {
                pkt.header.target_block = new_target;
                count += 1;
            }
        }
        if count > 0 {
            log::info!(
                "StateTransfer: Rerouted {} packets in snapshot {} → {}",
                count,
                old_target,
                new_target
            );
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Payload};

    fn test_packet(target: u32) -> IpcPacket {
        IpcPacket::new(0, target, CommandId::HealthCheck, Payload::Empty)
    }

    #[test]
    fn test_extract_and_restore() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();

        let state = b"block_state_data";
        let snapshot = StateTransferManager::extract_state(&mut bus, state).unwrap();

        assert!(bus.is_empty());
        assert_eq!(snapshot.pending_packets.len(), 2);
        assert_eq!(snapshot.state, b"block_state_data");

        StateTransferManager::restore_state(&mut bus, snapshot).unwrap();
        assert_eq!(bus.len(), 2);
    }

    #[test]
    fn test_extract_empty() {
        let mut bus = IpcBus::new(10);
        let snapshot = StateTransferManager::extract_state(&mut bus, b"").unwrap();
        assert!(snapshot.pending_packets.is_empty());
        assert!(snapshot.state.is_empty());
    }

    #[test]
    fn test_reroute_snapshot() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        bus.send(test_packet(1)).unwrap();

        let mut snapshot = StateTransferManager::extract_state(&mut bus, b"state").unwrap();
        let count = StateTransferManager::reroute_snapshot(&mut snapshot, 1, 99);
        assert_eq!(count, 2);

        StateTransferManager::restore_state(&mut bus, snapshot).unwrap();
        let p1 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, 99);
        let p2 = bus.receive().unwrap();
        assert_eq!(p2.header.target_block, 2);
        let p3 = bus.receive().unwrap();
        assert_eq!(p3.header.target_block, 99);
    }
}
