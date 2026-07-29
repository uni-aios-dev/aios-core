use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TcpState {
    Idle,
    Listening,
    Connecting,
    Connected,
    Closing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpConfig {
    pub bind_addr: String,
    pub port: u16,
    pub max_connections: u32,
    pub buffer_size: usize,
    pub timeout_ms: u64,
    pub nodelay: bool,
    pub reuse_addr: bool,
    pub keepalive: bool,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".into(),
            port: 8080,
            max_connections: 64,
            buffer_size: 8192,
            timeout_ms: 30_000,
            nodelay: true,
            reuse_addr: true,
            keepalive: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpMessage {
    pub from: String,
    pub to: String,
    pub data: Vec<u8>,
    pub timestamp: u64,
}

pub struct TcpBlock {
    config: TcpConfig,
    state: TcpState,
    incoming: VecDeque<TcpMessage>,
    outgoing: VecDeque<TcpMessage>,
    connections: Vec<TcpConnection>,
    bytes_sent: u64,
    bytes_received: u64,
    errors: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpConnection {
    pub id: u32,
    pub remote_addr: String,
    pub remote_port: u16,
    pub state: TcpState,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl TcpBlock {
    pub fn new(config: TcpConfig) -> Self {
        log::info!(
            "NET/TCP: Block created on {}:{} (max_conn={}, buf={})",
            config.bind_addr,
            config.port,
            config.max_connections,
            config.buffer_size
        );

        Self {
            config,
            state: TcpState::Idle,
            incoming: VecDeque::new(),
            outgoing: VecDeque::new(),
            connections: Vec::new(),
            bytes_sent: 0,
            bytes_received: 0,
            errors: 0,
        }
    }

    pub fn start_listening(&mut self) -> Result<()> {
        if self.state == TcpState::Connected || self.state == TcpState::Listening {
            return Err(AIOSException::Generic("TCP block already active".into()));
        }

        self.state = TcpState::Listening;
        log::info!(
            "NET/TCP: Listening on {}:{}",
            self.config.bind_addr,
            self.config.port
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        self.state = TcpState::Closing;
        self.connections.clear();
        self.incoming.clear();
        self.outgoing.clear();
        self.state = TcpState::Idle;
        log::info!("NET/TCP: Stopped");
    }

    pub fn connect(&mut self, addr: &str, port: u16) -> Result<u32> {
        if self.connections.len() as u32 >= self.config.max_connections {
            return Err(AIOSException::Generic("TCP max connections reached".into()));
        }

        let id = self.connections.len() as u32 + 1;
        self.connections.push(TcpConnection {
            id,
            remote_addr: addr.to_string(),
            remote_port: port,
            state: TcpState::Connected,
            bytes_sent: 0,
            bytes_received: 0,
        });

        self.state = TcpState::Connected;
        log::info!("NET/TCP: Connected to {}:{} (conn_id={})", addr, port, id);
        Ok(id)
    }

    pub fn accept_connection(&mut self, remote_addr: &str, remote_port: u16) -> Result<u32> {
        if self.state != TcpState::Listening {
            return Err(AIOSException::Generic("TCP block is not listening".into()));
        }

        if self.connections.len() as u32 >= self.config.max_connections {
            return Err(AIOSException::Generic("TCP max connections reached".into()));
        }

        let id = self.connections.len() as u32 + 1;
        self.connections.push(TcpConnection {
            id,
            remote_addr: remote_addr.to_string(),
            remote_port,
            state: TcpState::Connected,
            bytes_sent: 0,
            bytes_received: 0,
        });

        log::info!(
            "NET/TCP: Accepted connection from {}:{} (conn_id={})",
            remote_addr,
            remote_port,
            id
        );
        Ok(id)
    }

    pub fn send(&mut self, conn_id: u32, data: Vec<u8>) -> Result<()> {
        let conn = self
            .connections
            .iter_mut()
            .find(|c| c.id == conn_id)
            .ok_or_else(|| {
                AIOSException::Generic(format!("TCP connection {} not found", conn_id))
            })?;

        if conn.state != TcpState::Connected {
            return Err(AIOSException::Generic(format!(
                "TCP connection {} not connected",
                conn_id
            )));
        }

        let msg = TcpMessage {
            from: format!("{}:{}", self.config.bind_addr, self.config.port),
            to: format!("{}:{}", conn.remote_addr, conn.remote_port),
            data,
            timestamp: 0,
        };

        self.bytes_sent += msg.data.len() as u64;
        conn.bytes_sent += msg.data.len() as u64;
        self.outgoing.push_back(msg);

        Ok(())
    }

    pub fn receive(&mut self, conn_id: u32) -> Option<TcpMessage> {
        let conn = self.connections.iter().find(|c| c.id == conn_id)?;
        let from = format!("{}:{}", conn.remote_addr, conn.remote_port);

        self.incoming
            .iter()
            .position(|m| m.from == from)
            .map(|pos| {
                let msg = self.incoming.remove(pos).unwrap();
                self.bytes_received += msg.data.len() as u64;
                msg
            })
    }

    pub fn close_connection(&mut self, conn_id: u32) -> Result<()> {
        let pos = self
            .connections
            .iter()
            .position(|c| c.id == conn_id)
            .ok_or_else(|| {
                AIOSException::Generic(format!("TCP connection {} not found", conn_id))
            })?;

        self.connections.remove(pos);
        log::info!("NET/TCP: Connection {} closed", conn_id);
        Ok(())
    }

    pub fn inject_message(&mut self, msg: TcpMessage) {
        self.incoming.push_back(msg);
    }

    pub fn state(&self) -> TcpState {
        self.state
    }

    pub fn config(&self) -> &TcpConfig {
        &self.config
    }

    pub fn connections(&self) -> &[TcpConnection] {
        &self.connections
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    pub fn error_count(&self) -> u32 {
        self.errors
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
    fn test_tcp_block_creation() {
        let block = TcpBlock::new(TcpConfig::default());
        assert_eq!(block.state(), TcpState::Idle);
        assert_eq!(block.connection_count(), 0);
    }

    #[test]
    fn test_tcp_start_listening() {
        let mut block = TcpBlock::new(TcpConfig::default());
        assert!(block.start_listening().is_ok());
        assert_eq!(block.state(), TcpState::Listening);
    }

    #[test]
    fn test_tcp_double_listen_fails() {
        let mut block = TcpBlock::new(TcpConfig::default());
        block.start_listening().unwrap();
        assert!(block.start_listening().is_err());
    }

    #[test]
    fn test_tcp_connect() {
        let mut block = TcpBlock::new(TcpConfig::default());
        let conn_id = block.connect("192.168.1.1", 9000).unwrap();
        assert_eq!(conn_id, 1);
        assert_eq!(block.connection_count(), 1);
        assert_eq!(block.state(), TcpState::Connected);
    }

    #[test]
    fn test_tcp_max_connections() {
        let mut block = TcpBlock::new(TcpConfig {
            max_connections: 2,
            ..Default::default()
        });
        block.connect("a", 1).unwrap();
        block.connect("b", 2).unwrap();
        assert!(block.connect("c", 3).is_err());
    }

    #[test]
    fn test_tcp_send_receive() {
        let mut block = TcpBlock::new(TcpConfig::default());
        let conn_id = block.connect("10.0.0.1", 5000).unwrap();
        block.send(conn_id, b"hello".to_vec()).unwrap();
        assert_eq!(block.pending_outgoing(), 1);
        assert_eq!(block.bytes_sent(), 5);
    }

    #[test]
    fn test_tcp_close_connection() {
        let mut block = TcpBlock::new(TcpConfig::default());
        let conn_id = block.connect("10.0.0.1", 5000).unwrap();
        assert!(block.close_connection(conn_id).is_ok());
        assert_eq!(block.connection_count(), 0);
    }

    #[test]
    fn test_tcp_stop() {
        let mut block = TcpBlock::new(TcpConfig::default());
        block.start_listening().unwrap();
        block.connect("10.0.0.1", 5000).unwrap();
        block.stop();
        assert_eq!(block.state(), TcpState::Idle);
        assert_eq!(block.connection_count(), 0);
    }

    #[test]
    fn test_tcp_message_serialization() {
        let msg = TcpMessage {
            from: "127.0.0.1:8080".into(),
            to: "10.0.0.1:9000".into(),
            data: vec![1, 2, 3],
            timestamp: 12345,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let restored: TcpMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.data, vec![1, 2, 3]);
        assert_eq!(restored.timestamp, 12345);
    }

    #[test]
    fn test_tcp_config_defaults() {
        let config = TcpConfig::default();
        assert_eq!(config.bind_addr, "127.0.0.1");
        assert_eq!(config.port, 8080);
        assert_eq!(config.max_connections, 64);
        assert!(config.nodelay);
    }

    #[test]
    fn test_tcp_connection_not_found() {
        let mut block = TcpBlock::new(TcpConfig::default());
        assert!(block.send(999, vec![1]).is_err());
        assert!(block.close_connection(999).is_err());
    }

    #[test]
    fn test_tcp_inject_receive() {
        let mut block = TcpBlock::new(TcpConfig::default());
        let conn_id = block.connect("10.0.0.1", 5000).unwrap();

        let msg = TcpMessage {
            from: "10.0.0.1:5000".into(),
            to: "127.0.0.1:8080".into(),
            data: vec![42],
            timestamp: 0,
        };
        block.inject_message(msg);

        let received = block.receive(conn_id);
        assert!(received.is_some());
        assert_eq!(received.unwrap().data, vec![42]);
        assert_eq!(block.bytes_received(), 1);
    }

    #[test]
    fn test_tcp_accept_connection() {
        let mut block = TcpBlock::new(TcpConfig::default());
        block.start_listening().unwrap();
        let conn_id = block.accept_connection("10.0.0.1", 5000).unwrap();
        assert_eq!(conn_id, 1);
        assert_eq!(block.connection_count(), 1);
    }

    #[test]
    fn test_tcp_accept_when_not_listening() {
        let mut block = TcpBlock::new(TcpConfig::default());
        assert!(block.accept_connection("10.0.0.1", 5000).is_err());
    }
}
