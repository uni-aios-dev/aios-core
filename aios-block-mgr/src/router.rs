use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::IpcPacket;
use std::collections::HashMap;

type BlockHandler = Box<dyn FnMut(&IpcPacket) -> Result<Option<IpcPacket>> + Send>;

pub struct MessageRouter {
    handlers: HashMap<u32, BlockHandler>,
    routes: HashMap<u32, u32>,
}

impl MessageRouter {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            routes: HashMap::new(),
        }
    }

    pub fn register_handler(&mut self, block_id: u32, handler: BlockHandler) {
        self.handlers.insert(block_id, handler);
    }

    pub fn add_route(&mut self, from: u32, to: u32) {
        self.routes.insert(from, to);
    }

    pub fn route_target(&self, target: u32) -> u32 {
        self.routes.get(&target).copied().unwrap_or(target)
    }

    pub fn dispatch(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>> {
        let target = self.route_target(packet.header.target_block);

        let handler = self
            .handlers
            .get_mut(&target)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("block_{}", target)))?;

        handler(packet)
    }

    pub fn has_handler(&self, block_id: u32) -> bool {
        self.handlers.contains_key(&block_id)
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Payload};

    fn echo_handler() -> BlockHandler {
        Box::new(|pkt: &IpcPacket| {
            Ok(Some(IpcPacket::response_ok(
                pkt.header.target_block,
                pkt.header.source_block,
                pkt.header.packet_id,
                Payload::Binary(b"echo".to_vec()),
            )))
        })
    }

    #[test]
    fn test_dispatch_echo() {
        let mut router = MessageRouter::new();
        router.register_handler(1, echo_handler());

        let pkt = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty);
        let resp = router.dispatch(&pkt).unwrap().unwrap();
        assert_eq!(resp.payload, Payload::Binary(b"echo".to_vec()));
    }

    #[test]
    fn test_dispatch_unknown_block() {
        let mut router = MessageRouter::new();
        let pkt = IpcPacket::new(0, 999, CommandId::HealthCheck, Payload::Empty);
        assert!(router.dispatch(&pkt).is_err());
    }

    #[test]
    fn test_route_redirect() {
        let mut router = MessageRouter::new();
        router.add_route(10, 20);
        assert_eq!(router.route_target(10), 20);
        assert_eq!(router.route_target(30), 30);
    }

    #[test]
    fn test_dispatch_via_route() {
        let mut router = MessageRouter::new();
        router.add_route(10, 20);
        router.register_handler(20, echo_handler());

        let pkt = IpcPacket::new(0, 10, CommandId::HealthCheck, Payload::Empty);
        let resp = router.dispatch(&pkt).unwrap().unwrap();
        assert_eq!(resp.payload, Payload::Binary(b"echo".to_vec()));
    }
}
