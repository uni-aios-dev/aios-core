use crate::udp::{UdpConfig, UdpPacket, UdpState};
use aios_core::error::{AIOSException, Result};
use std::net::UdpSocket;
use std::time::Duration;

pub struct RealUdpBlock {
    config: UdpConfig,
    state: UdpState,
    socket: Option<UdpSocket>,
    bytes_sent: u64,
    bytes_received: u64,
    packets_sent: u64,
    packets_received: u64,
    errors: u32,
}

impl RealUdpBlock {
    pub fn new(config: UdpConfig) -> Self {
        log::info!(
            "NET/UDP-Real: Block created on {}:{} (broadcast={})",
            config.bind_addr,
            config.port,
            config.broadcast
        );

        Self {
            config,
            state: UdpState::Idle,
            socket: None,
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
            errors: 0,
        }
    }

    pub fn bind(&mut self) -> Result<()> {
        if self.state == UdpState::Bound {
            return Err(AIOSException::Generic("UDP already bound".into()));
        }

        let addr = format!("{}:{}", self.config.bind_addr, self.config.port);
        let socket = UdpSocket::bind(&addr)
            .map_err(|e| AIOSException::Generic(format!("UDP bind failed: {e}")))?;

        socket
            .set_nonblocking(true)
            .map_err(|e| AIOSException::Generic(format!("UDP nonblocking: {e}")))?;

        if self.config.broadcast {
            socket
                .set_broadcast(true)
                .map_err(|e| AIOSException::Generic(format!("UDP broadcast enable: {e}")))?;
        }

        self.socket = Some(socket);
        self.state = UdpState::Bound;

        log::info!("NET/UDP-Real: Bound to {}", addr);
        Ok(())
    }

    pub fn close(&mut self) {
        self.socket = None;
        self.state = UdpState::Closed;
        log::info!("NET/UDP-Real: Closed");
    }

    pub fn send_to(&mut self, to_addr: &str, to_port: u16, data: Vec<u8>) -> Result<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AIOSException::Generic("UDP not bound".into()))?;

        let dest = format!("{}:{}", to_addr, to_port);
        match socket.send_to(&data, &dest) {
            Ok(n) => {
                self.bytes_sent += n as u64;
                self.packets_sent += 1;
                log::debug!("NET/UDP-Real: Sent {} bytes to {}", n, dest);
                Ok(())
            }
            Err(e) => {
                self.errors += 1;
                Err(AIOSException::Generic(format!("UDP send_to failed: {e}")))
            }
        }
    }

    pub fn broadcast(&mut self, port: u16, data: Vec<u8>) -> Result<()> {
        if !self.config.broadcast {
            return Err(AIOSException::Generic("UDP broadcast not enabled".into()));
        }
        self.send_to("255.255.255.255", port, data)
    }

    pub fn receive_from(&self) -> Result<Option<(Vec<u8>, String, u16)>> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AIOSException::Generic("UDP not bound".into()))?;

        let mut buf = vec![0u8; self.config.buffer_size];
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                buf.truncate(n);
                let from_str = from.to_string();
                let parts: Vec<&str> = from_str.split(':').collect();
                let from_addr = parts.first().unwrap_or(&"unknown").to_string();
                let from_port = parts
                    .get(1)
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(0);
                Ok(Some((buf, from_addr, from_port)))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(AIOSException::Generic(format!("UDP recv_from failed: {e}"))),
        }
    }

    pub fn receive_packet(&self) -> Result<Option<UdpPacket>> {
        match self.receive_from()? {
            Some((data, from_addr, from_port)) => {
                let packet = UdpPacket {
                    from: format!("{}:{}", from_addr, from_port),
                    to: format!("{}:{}", self.config.bind_addr, self.config.port),
                    data,
                    timestamp: 0,
                };
                Ok(Some(packet))
            }
            None => Ok(None),
        }
    }

    pub fn set_timeout(&self, duration: Duration) -> Result<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AIOSException::Generic("UDP not bound".into()))?;
        socket
            .set_read_timeout(Some(duration))
            .map_err(|e| AIOSException::Generic(format!("UDP set_timeout: {e}")))
    }

    pub fn port(&self) -> u16 {
        self.socket
            .as_ref()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.port())
            .unwrap_or(0)
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

    pub fn errors(&self) -> u32 {
        self.errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_real_udp_bind() {
        let mut block = RealUdpBlock::new(UdpConfig {
            port: 0,
            ..Default::default()
        });
        assert!(block.bind().is_ok());
        assert_eq!(block.state(), UdpState::Bound);
    }

    #[test]
    fn test_real_udp_double_bind_fails() {
        let mut block = RealUdpBlock::new(UdpConfig {
            port: 0,
            ..Default::default()
        });
        block.bind().unwrap();
        assert!(block.bind().is_err());
    }

    #[test]
    fn test_real_udp_send_receive_loopback() {
        let mut sender = RealUdpBlock::new(UdpConfig {
            bind_addr: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        });
        sender.bind().unwrap();

        let mut receiver = RealUdpBlock::new(UdpConfig {
            bind_addr: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        });
        receiver.bind().unwrap();

        let recv_addr = receiver.socket.as_ref().unwrap().local_addr().unwrap();

        sender
            .send_to("127.0.0.1", recv_addr.port(), vec![72, 101, 108, 108, 111])
            .unwrap();

        std::thread::sleep(Duration::from_millis(50));

        let packet = receiver.receive_packet().unwrap();
        assert!(packet.is_some());
        let pkt = packet.unwrap();
        assert_eq!(pkt.data, vec![72, 101, 108, 108, 111]);
    }

    #[test]
    fn test_real_udp_no_data_returns_none() {
        let mut block = RealUdpBlock::new(UdpConfig {
            port: 0,
            ..Default::default()
        });
        block.bind().unwrap();
        let packet = block.receive_packet().unwrap();
        assert!(packet.is_none());
    }

    #[test]
    fn test_real_udp_close() {
        let mut block = RealUdpBlock::new(UdpConfig {
            port: 0,
            ..Default::default()
        });
        block.bind().unwrap();
        block.close();
        assert_eq!(block.state(), UdpState::Closed);
        assert!(block.socket.is_none());
    }

    #[test]
    fn test_real_udp_broadcast_enabled() {
        let mut block = RealUdpBlock::new(UdpConfig {
            bind_addr: "127.0.0.1".into(),
            port: 0,
            broadcast: true,
            ..Default::default()
        });
        block.bind().unwrap();
        assert!(block.socket.as_ref().unwrap().broadcast().unwrap());
    }

    #[test]
    fn test_real_udp_metrics() {
        let mut block = RealUdpBlock::new(UdpConfig {
            port: 0,
            ..Default::default()
        });
        block.bind().unwrap();
        assert_eq!(block.bytes_sent(), 0);
        assert_eq!(block.packets_sent(), 0);
        assert_eq!(block.errors(), 0);
    }
}
