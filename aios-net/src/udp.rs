use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UdpState {
    Idle,
    Bound,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpConfig {
    pub bind_addr: String,
    pub port: u16,
    pub buffer_size: usize,
    pub broadcast: bool,
    pub multicast_ttl: u32,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".into(),
            port: 9000,
            buffer_size: 65535,
            broadcast: false,
            multicast_ttl: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpPacket {
    pub from: String,
    pub to: String,
    pub data: Vec<u8>,
    pub timestamp: u64,
}

pub struct UdpBlock {
    config: UdpConfig,
    state: UdpState,
    incoming: VecDeque<UdpPacket>,
    outgoing: VecDeque<UdpPacket>,
    bytes_sent: u64,
    bytes_received: u64,
    packets_sent: u64,
    packets_received: u64,
}

impl UdpBlock {
    pub fn new(config: UdpConfig) -> Self {
        log::info!(
            "NET/UDP: Block created on {}:{} (broadcast={})",
            config.bind_addr,
            config.port,
            config.broadcast
        );

        Self {
            config,
            state: UdpState::Idle,
            incoming: VecDeque::new(),
            outgoing: VecDeque::new(),
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
        }
    }

    pub fn bind(&mut self) -> Result<()> {
        if self.state == UdpState::Bound {
            return Err(AIOSException::Generic("UDP already bound".into()));
        }

        self.state = UdpState::Bound;
        log::info!(
            "NET/UDP: Bound to {}:{}",
            self.config.bind_addr,
            self.config.port
        );
        Ok(())
    }

    pub fn close(&mut self) {
        self.incoming.clear();
        self.outgoing.clear();
        self.state = UdpState::Closed;
        log::info!("NET/UDP: Closed");
    }

    pub fn send(&mut self, to_addr: &str, to_port: u16, data: Vec<u8>) -> Result<()> {
        if self.state != UdpState::Bound {
            return Err(AIOSException::Generic("UDP not bound".into()));
        }

        let packet = UdpPacket {
            from: format!("{}:{}", self.config.bind_addr, self.config.port),
            to: format!("{}:{}", to_addr, to_port),
            data,
            timestamp: 0,
        };

        self.bytes_sent += packet.data.len() as u64;
        self.packets_sent += 1;
        self.outgoing.push_back(packet);

        Ok(())
    }

    pub fn broadcast(&mut self, port: u16, data: Vec<u8>) -> Result<()> {
        if !self.config.broadcast {
            return Err(AIOSException::Generic("UDP broadcast not enabled".into()));
        }
        self.send("255.255.255.255", port, data)
    }

    pub fn receive(&mut self) -> Option<UdpPacket> {
        let packet = self.incoming.pop_front()?;
        self.bytes_received += packet.data.len() as u64;
        self.packets_received += 1;
        Some(packet)
    }

    pub fn inject_packet(&mut self, packet: UdpPacket) {
        self.incoming.push_back(packet);
    }

    pub fn state(&self) -> UdpState {
        self.state
    }

    pub fn config(&self) -> &UdpConfig {
        &self.config
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    pub fn packets_sent(&self) -> u64 {
        self.packets_sent
    }

    pub fn packets_received(&self) -> u64 {
        self.packets_received
    }

    pub fn pending_outgoing(&self) -> usize {
        self.outgoing.len()
    }

    pub fn pending_incoming(&self) -> usize {
        self.incoming.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_udp_block_creation() {
        let block = UdpBlock::new(UdpConfig::default());
        assert_eq!(block.state(), UdpState::Idle);
    }

    #[test]
    fn test_udp_bind() {
        let mut block = UdpBlock::new(UdpConfig::default());
        assert!(block.bind().is_ok());
        assert_eq!(block.state(), UdpState::Bound);
    }

    #[test]
    fn test_udp_double_bind_fails() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();
        assert!(block.bind().is_err());
    }

    #[test]
    fn test_udp_send() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();
        block.send("10.0.0.1", 5000, vec![1, 2, 3]).unwrap();
        assert_eq!(block.packets_sent(), 1);
        assert_eq!(block.bytes_sent(), 3);
        assert_eq!(block.pending_outgoing(), 1);
    }

    #[test]
    fn test_udp_send_not_bound() {
        let mut block = UdpBlock::new(UdpConfig::default());
        assert!(block.send("10.0.0.1", 5000, vec![1]).is_err());
    }

    #[test]
    fn test_udp_receive() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();

        let packet = UdpPacket {
            from: "10.0.0.1:5000".into(),
            to: "127.0.0.1:9000".into(),
            data: vec![42, 43],
            timestamp: 0,
        };
        block.inject_packet(packet);

        let received = block.receive().unwrap();
        assert_eq!(received.data, vec![42, 43]);
        assert_eq!(block.packets_received(), 1);
        assert_eq!(block.bytes_received(), 2);
    }

    #[test]
    fn test_udp_receive_empty() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();
        assert!(block.receive().is_none());
    }

    #[test]
    fn test_udp_broadcast() {
        let mut block = UdpBlock::new(UdpConfig {
            broadcast: true,
            ..Default::default()
        });
        block.bind().unwrap();
        block.broadcast(5000, vec![1, 2]).unwrap();
        assert_eq!(block.packets_sent(), 1);
    }

    #[test]
    fn test_udp_broadcast_disabled() {
        let mut block = UdpBlock::new(UdpConfig {
            broadcast: false,
            ..Default::default()
        });
        block.bind().unwrap();
        assert!(block.broadcast(5000, vec![1]).is_err());
    }

    #[test]
    fn test_udp_close() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();
        block.send("10.0.0.1", 5000, vec![1]).unwrap();
        block.close();
        assert_eq!(block.state(), UdpState::Closed);
        assert_eq!(block.pending_outgoing(), 0);
    }

    #[test]
    fn test_udp_packet_serialization() {
        let pkt = UdpPacket {
            from: "127.0.0.1:9000".into(),
            to: "10.0.0.1:5000".into(),
            data: vec![10, 20, 30],
            timestamp: 999,
        };
        let bytes = bincode::serialize(&pkt).unwrap();
        let restored: UdpPacket = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.data, vec![10, 20, 30]);
        assert_eq!(restored.timestamp, 999);
    }

    #[test]
    fn test_udp_config_defaults() {
        let config = UdpConfig::default();
        assert_eq!(config.bind_addr, "127.0.0.1");
        assert_eq!(config.port, 9000);
        assert_eq!(config.buffer_size, 65535);
        assert!(!config.broadcast);
    }

    #[test]
    fn test_udp_multiple_packets() {
        let mut block = UdpBlock::new(UdpConfig::default());
        block.bind().unwrap();
        block.send("a", 1, vec![1]).unwrap();
        block.send("b", 2, vec![2, 3]).unwrap();
        assert_eq!(block.packets_sent(), 2);
        assert_eq!(block.bytes_sent(), 3);
    }
}
