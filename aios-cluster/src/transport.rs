//! Pluggable cluster transports.
//!
//! [`ClusterTransport`] decouples the distributed scheduler from the network:
//! [`TcpClusterTransport`] is a real TCP listener/connection per node, while
//! [`InMemoryClusterTransport`] routes messages inside one process for tests
//! and for coordinating multiple schedulers on a single machine.
use crate::protocol::{decode_frame, encode, ClusterMessage};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Message-passing transport used by a node to talk to peers.
pub trait ClusterTransport: Send + Sync {
    /// This node's reachable address.
    fn addr(&self) -> String;
    /// Best-effort delivery of `msg` to the node at `peer`.
    fn send(&self, peer: &str, msg: ClusterMessage) -> io::Result<()>;
    /// Begin listening; decoded inbound messages are pushed to `inbox`.
    fn start(&self, inbox: mpsc::Sender<ClusterMessage>) -> io::Result<()>;
    /// Stop listening and drop any bound socket.
    fn shutdown(&self);
}

/// Shared registry that routes in-memory messages by node address.
#[derive(Default)]
pub struct MemoryRegistry(Arc<Mutex<HashMap<String, mpsc::Sender<ClusterMessage>>>>);

impl MemoryRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone the inner `Arc`.
    pub fn clone_arc(&self) -> Arc<Mutex<HashMap<String, mpsc::Sender<ClusterMessage>>>> {
        self.0.clone()
    }
}

/// In-process transport used by tests and multi-scheduler setups.
pub struct InMemoryClusterTransport {
    addr: String,
    registry: Arc<Mutex<HashMap<String, mpsc::Sender<ClusterMessage>>>>,
    stop: Arc<AtomicBool>,
}

impl InMemoryClusterTransport {
    /// Create a transport for node `addr` that routes through `registry`.
    /// Nodes that share one registry can reach each other.
    pub fn new(
        addr: &str,
        registry: Arc<Mutex<HashMap<String, mpsc::Sender<ClusterMessage>>>>,
    ) -> Self {
        Self {
            addr: addr.to_string(),
            registry,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a transport with a private registry (single-node setups).
    pub fn isolated(addr: &str) -> Self {
        Self::new(addr, Arc::new(Mutex::new(HashMap::new())))
    }
}

impl ClusterTransport for InMemoryClusterTransport {
    fn addr(&self) -> String {
        self.addr.clone()
    }

    fn send(&self, peer: &str, msg: ClusterMessage) -> io::Result<()> {
        let sender = self
            .registry
            .lock()
            .unwrap()
            .get(peer)
            .cloned()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no node registered at {peer}"),
                )
            })?;
        sender
            .send(msg)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, format!("peer {peer} closed")))
    }

    fn start(&self, inbox: mpsc::Sender<ClusterMessage>) -> io::Result<()> {
        self.registry
            .lock()
            .unwrap()
            .insert(self.addr.clone(), inbox);
        Ok(())
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.registry.lock().unwrap().remove(&self.addr);
    }
}

/// Real TCP transport: each node binds a listener and connects to peers on
/// demand, exchanging length-prefixed bincode frames.
pub struct TcpClusterTransport {
    bind: String,
    listener: Arc<Mutex<Option<std::net::TcpListener>>>,
    actual: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

impl TcpClusterTransport {
    /// Create a transport that binds `addr` (`host:port`, port 0 allowed for
    /// ephemeral ports in tests). The actual bound address is reported by
    /// [`ClusterTransport::addr`] after [`ClusterTransport::start`].
    pub fn new(bind: &str) -> Self {
        Self {
            bind: bind.to_string(),
            listener: Arc::new(Mutex::new(None)),
            actual: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ClusterTransport for TcpClusterTransport {
    fn addr(&self) -> String {
        self.actual
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.bind.clone())
    }

    fn send(&self, peer: &str, msg: ClusterMessage) -> io::Result<()> {
        let frame = encode(&msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut stream = std::net::TcpStream::connect(peer)?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(&frame)?;
        stream.flush()
    }

    fn start(&self, inbox: mpsc::Sender<ClusterMessage>) -> io::Result<()> {
        let listener = std::net::TcpListener::bind(&self.bind)?;
        let actual = listener.local_addr()?.to_string();
        *self.listener.lock().unwrap() = Some(listener.try_clone()?);
        *self.actual.lock().unwrap() = Some(actual);
        let stop = self.stop.clone();
        std::thread::Builder::new()
            .name("aios-cluster-tcp-accept".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let inbox = inbox.clone();
                            let stop = stop.clone();
                            std::thread::spawn(move || read_frames(stream, inbox, stop));
                        }
                        Err(_) => {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
            })?;
        Ok(())
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(listener) = self.listener.lock().unwrap().take() {
            drop(listener);
        }
    }
}

/// Read length-prefixed frames from `stream` until EOF, forwarding each decoded
/// message to `inbox`.
fn read_frames(
    mut stream: std::net::TcpStream,
    inbox: mpsc::Sender<ClusterMessage>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut header = [0u8; 4];
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let len = u32::from_le_bytes(header) as usize;
        if len > 64 * 1024 * 1024 {
            return;
        }
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        let frame = [&header[..], &body[..]].concat();
        match decode_frame(&frame) {
            Ok((msg, _)) => {
                if inbox.send(msg).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClusterMessage;
    use crate::types::{NodeInfo, NodeMetrics, NodeStatus};
    use std::sync::mpsc;
    use std::time::Duration;

    fn sample_info(id: u64, addr: &str) -> NodeInfo {
        NodeInfo {
            id,
            name: format!("node-{id}"),
            addr: addr.to_string(),
            tier: 2,
            status: NodeStatus::Online,
            metrics: NodeMetrics::idle(),
        }
    }

    #[test]
    fn test_in_memory_route_and_cleanup() {
        let registry = MemoryRegistry::new();
        let a = InMemoryClusterTransport::new("mem://a", registry.clone_arc());
        let b = InMemoryClusterTransport::new("mem://b", registry.clone_arc());
        let (tx, rx) = mpsc::channel();
        a.start(tx).unwrap();
        b.send("mem://a", ClusterMessage::Hello(sample_info(2, "mem://b")))
            .unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("routed message");
        assert!(matches!(got, ClusterMessage::Hello(_)));
        a.shutdown();
        assert!(
            b.send("mem://a", ClusterMessage::Hello(sample_info(2, "mem://b")))
                .is_err(),
            "shutdown must unregister the node"
        );
        b.shutdown();
    }

    #[test]
    fn test_tcp_loopback_roundtrip() {
        let server = TcpClusterTransport::new("127.0.0.1:0");
        let (tx, rx) = mpsc::channel();
        server.start(tx).unwrap();
        let addr = server.addr();

        let client = TcpClusterTransport::new("127.0.0.1:0");
        client
            .send(
                &addr,
                ClusterMessage::Metrics {
                    id: 1,
                    metrics: NodeMetrics::idle(),
                },
            )
            .unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).expect("tcp frame");
        assert!(matches!(got, ClusterMessage::Metrics { .. }));

        client.shutdown();
        server.shutdown();
    }
}
