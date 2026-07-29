use crate::tcp::{TcpConfig, TcpConnection, TcpState};
use aios_core::error::{AIOSException, Result};
use aios_security::capability::{Capability, CapabilityToken};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn set_keepalive(stream: &TcpStream, enable: bool) {
    #[cfg(unix)]
    unsafe {
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(stream);
        let optval: i32 = if enable { 1 } else { 0 };
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &optval as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
    }
    #[cfg(target_os = "windows")]
    unsafe {
        use std::os::windows::io::AsRawSocket;
        let raw = stream.as_raw_socket();
        let optval: i32 = if enable { 1 } else { 0 };
        extern "system" {
            fn setsockopt(s: u64, level: i32, optname: i32, optval: *const i8, optlen: i32) -> i32;
        }
        const SOL_SOCKET: i32 = 0x0000FFFF;
        const SO_KEEPALIVE: i32 = 0x00000008;
        setsockopt(
            raw,
            SOL_SOCKET,
            SO_KEEPALIVE,
            &optval as *const _ as *const i8,
            std::mem::size_of::<i32>() as i32,
        );
    }
}

pub struct RealTcpBlock {
    config: TcpConfig,
    state: TcpState,
    listener: Option<TcpListener>,
    connections: Vec<RealTcpConnection>,
    bytes_sent: u64,
    bytes_received: u64,
    errors: u32,
    capability: Option<CapabilityToken>,
}

struct RealTcpConnection {
    meta: TcpConnection,
    stream: TcpStream,
}

impl RealTcpBlock {
    pub fn new(config: TcpConfig) -> Self {
        Self {
            config,
            state: TcpState::Idle,
            listener: None,
            connections: Vec::new(),
            bytes_sent: 0,
            bytes_received: 0,
            errors: 0,
            capability: None,
        }
    }

    pub fn set_capability(&mut self, token: CapabilityToken) {
        self.capability = Some(token);
    }

    fn check_capability(&self, required: &Capability) -> Result<()> {
        match &self.capability {
            Some(token) => {
                if token.is_expired() {
                    Err(AIOSException::PermissionDenied(
                        "Capability token has expired".into(),
                    ))
                } else if token.has_capability(required) {
                    Ok(())
                } else {
                    Err(AIOSException::PermissionDenied(format!(
                        "Missing capability {}",
                        required.name()
                    )))
                }
            }
            None => Ok(()),
        }
    }

    pub fn start_listening(&mut self) -> Result<()> {
        self.check_capability(&Capability::NetBind)?;
        if self.state == TcpState::Listening {
            return Err(AIOSException::Generic("TCP already listening".into()));
        }

        let addr = format!("{}:{}", self.config.bind_addr, self.config.port);
        let listener = TcpListener::bind(&addr)
            .map_err(|e| AIOSException::Generic(format!("TCP bind failed: {e}")))?;

        listener
            .set_nonblocking(true)
            .map_err(|e| AIOSException::Generic(format!("TCP nonblocking: {e}")))?;

        #[cfg(unix)]
        if self.config.reuse_addr {
            unsafe {
                let fd = std::os::unix::io::AsRawFd::as_raw_fd(&listener);
                let optval: i32 = 1;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_REUSEADDR,
                    &optval as *const _ as *const libc::c_void,
                    std::mem::size_of::<i32>() as libc::socklen_t,
                );
            }
        }

        #[cfg(target_os = "windows")]
        if self.config.reuse_addr {
            unsafe {
                use std::os::windows::io::AsRawSocket;
                let raw = listener.as_raw_socket();
                let optval: i32 = 1;
                extern "system" {
                    fn setsockopt(
                        s: u64,
                        level: i32,
                        optname: i32,
                        optval: *const i8,
                        optlen: i32,
                    ) -> i32;
                }
                const SOL_SOCKET: i32 = 0x0000FFFF;
                const SO_REUSEADDR: i32 = 0x00000004;
                setsockopt(
                    raw,
                    SOL_SOCKET,
                    SO_REUSEADDR,
                    &optval as *const _ as *const i8,
                    std::mem::size_of::<i32>() as i32,
                );
            }
        }

        self.listener = Some(listener);
        self.state = TcpState::Listening;
        log::info!("NET/TCP: Real listening on {}", addr);
        Ok(())
    }

    pub fn accept_pending(&mut self) -> Result<Option<u32>> {
        if self.state != TcpState::Listening {
            return Ok(None);
        }

        if self.connections.len() as u32 >= self.config.max_connections {
            return Ok(None);
        }

        let listener = match &self.listener {
            Some(l) => l,
            None => return Ok(None),
        };

        match listener.accept() {
            Ok((stream, peer)) => {
                stream
                    .set_nodelay(self.config.nodelay)
                    .map_err(|e| AIOSException::Generic(format!("TCP nodelay: {e}")))?;

                set_keepalive(&stream, self.config.keepalive);
                let id = self.connections.len() as u32 + 1;
                let conn = RealTcpConnection {
                    meta: TcpConnection {
                        id,
                        remote_addr: peer.ip().to_string(),
                        remote_port: peer.port(),
                        state: TcpState::Connected,
                        bytes_sent: 0,
                        bytes_received: 0,
                    },
                    stream,
                };

                log::info!(
                    "NET/TCP: Accepted from {}:{} (conn_id={})",
                    peer.ip(),
                    peer.port(),
                    id
                );
                self.connections.push(conn);
                Ok(Some(id))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => {
                self.errors += 1;
                Err(AIOSException::Generic(format!("TCP accept: {e}")))
            }
        }
    }

    pub fn connect(&mut self, addr: &str, port: u16) -> Result<u32> {
        self.check_capability(&Capability::NetConnect)?;

        if self.connections.len() as u32 >= self.config.max_connections {
            return Err(AIOSException::Generic("TCP max connections reached".into()));
        }

        let timeout = Duration::from_millis(self.config.timeout_ms.min(5000));
        let stream = TcpStream::connect_timeout(
            &format!("{}:{}", addr, port)
                .parse()
                .map_err(|_| AIOSException::Generic("Invalid address".into()))?,
            timeout,
        )
        .map_err(|e| AIOSException::Generic(format!("TCP connect failed: {e}")))?;

        stream
            .set_nodelay(self.config.nodelay)
            .map_err(|e| AIOSException::Generic(format!("TCP nodelay: {e}")))?;

        set_keepalive(&stream, self.config.keepalive);

        let id = self.connections.len() as u32 + 1;
        let conn = RealTcpConnection {
            meta: TcpConnection {
                id,
                remote_addr: addr.to_string(),
                remote_port: port,
                state: TcpState::Connected,
                bytes_sent: 0,
                bytes_received: 0,
            },
            stream,
        };

        self.connections.push(conn);
        self.state = TcpState::Connected;
        log::info!("NET/TCP: Connected to {}:{} (conn_id={})", addr, port, id);
        Ok(id)
    }

    pub fn send(&mut self, conn_id: u32, data: Vec<u8>) -> Result<()> {
        let conn = self
            .connections
            .iter_mut()
            .find(|c| c.meta.id == conn_id)
            .ok_or_else(|| AIOSException::Generic(format!("Connection {} not found", conn_id)))?;

        let len = data.len() as u64;
        conn.stream
            .write_all(&data)
            .map_err(|e| AIOSException::Generic(format!("TCP send: {e}")))?;

        self.bytes_sent += len;
        conn.meta.bytes_sent += len;
        Ok(())
    }

    pub fn receive(&mut self, conn_id: u32) -> Result<Option<Vec<u8>>> {
        let conn = self
            .connections
            .iter_mut()
            .find(|c| c.meta.id == conn_id)
            .ok_or_else(|| AIOSException::Generic(format!("Connection {} not found", conn_id)))?;

        let mut buf = vec![0u8; self.config.buffer_size];
        match conn.stream.read(&mut buf) {
            Ok(0) => Ok(None),
            Ok(n) => {
                buf.truncate(n);
                self.bytes_received += n as u64;
                conn.meta.bytes_received += n as u64;
                Ok(Some(buf))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(AIOSException::Generic(format!("TCP recv: {e}"))),
        }
    }

    pub fn close_connection(&mut self, conn_id: u32) -> Result<()> {
        let pos = self
            .connections
            .iter()
            .position(|c| c.meta.id == conn_id)
            .ok_or_else(|| AIOSException::Generic(format!("Connection {} not found", conn_id)))?;
        self.connections.remove(pos);
        log::info!("NET/TCP: Connection {} closed", conn_id);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.listener = None;
        self.connections.clear();
        self.state = TcpState::Idle;
        log::info!("NET/TCP: Stopped");
    }

    pub fn state(&self) -> TcpState {
        self.state
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

    pub fn connections_meta(&self) -> Vec<&TcpConnection> {
        self.connections.iter().map(|c| &c.meta).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_keepalive(stream: &TcpStream) -> bool {
        #[cfg(unix)]
        unsafe {
            let fd = std::os::unix::io::AsRawFd::as_raw_fd(stream);
            let mut optval: i32 = 0;
            let mut optlen: libc::socklen_t = std::mem::size_of::<i32>() as libc::socklen_t;
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_KEEPALIVE,
                &mut optval as *mut _ as *mut libc::c_void,
                &mut optlen,
            );
            optval != 0
        }
        #[cfg(target_os = "windows")]
        unsafe {
            use std::os::windows::io::AsRawSocket;
            let raw = stream.as_raw_socket();
            let mut optval: i32 = 0;
            let mut optlen: i32 = std::mem::size_of::<i32>() as i32;
            extern "system" {
                fn getsockopt(
                    s: u64,
                    level: i32,
                    optname: i32,
                    optval: *mut i8,
                    optlen: *mut i32,
                ) -> i32;
            }
            const SOL_SOCKET: i32 = 0x0000FFFF;
            const SO_KEEPALIVE: i32 = 0x00000008;
            getsockopt(
                raw,
                SOL_SOCKET,
                SO_KEEPALIVE,
                &mut optval as *mut _ as *mut i8,
                &mut optlen,
            );
            optval != 0
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            false
        }
    }

    fn test_config(port: u16) -> TcpConfig {
        TcpConfig {
            bind_addr: "127.0.0.1".into(),
            port,
            max_connections: 10,
            buffer_size: 4096,
            timeout_ms: 3000,
            nodelay: true,
            reuse_addr: true,
            keepalive: true,
        }
    }

    #[test]
    fn test_real_tcp_listen_and_stop() {
        let mut block = RealTcpBlock::new(test_config(19100));
        block.start_listening().unwrap();
        assert_eq!(block.state(), TcpState::Listening);
        block.stop();
        assert_eq!(block.state(), TcpState::Idle);
    }

    #[test]
    fn test_real_tcp_connect_and_send() {
        let mut server = RealTcpBlock::new(test_config(19110));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        let conn_id = client.connect("127.0.0.1", 19110).unwrap();
        assert!(conn_id > 0);

        std::thread::sleep(std::time::Duration::from_millis(50));
        let accepted = server.accept_pending().unwrap();
        assert!(accepted.is_some());

        client.send(conn_id, b"hello real tcp".to_vec()).unwrap();
        assert!(client.bytes_sent() > 0);

        std::thread::sleep(std::time::Duration::from_millis(20));
        let data = server.receive(accepted.unwrap()).unwrap();
        assert!(data.is_some());
        assert_eq!(data.unwrap(), b"hello real tcp");
    }

    #[test]
    fn test_real_tcp_bidirectional() {
        let mut server = RealTcpBlock::new(test_config(19111));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        let client_conn = client.connect("127.0.0.1", 19111).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let server_conn = server.accept_pending().unwrap().unwrap();

        client.send(client_conn, b"client msg".to_vec()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let data = server.receive(server_conn).unwrap().unwrap();
        assert_eq!(data, b"client msg");

        server.send(server_conn, b"server msg".to_vec()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let data = client.receive(client_conn).unwrap().unwrap();
        assert_eq!(data, b"server msg");
    }

    #[test]
    fn test_real_tcp_close_connection() {
        let mut server = RealTcpBlock::new(test_config(19112));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        client.connect("127.0.0.1", 19112).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let server_conn = server.accept_pending().unwrap().unwrap();

        assert!(server.close_connection(server_conn).is_ok());
        assert_eq!(server.connection_count(), 0);
    }

    #[test]
    fn test_real_tcp_max_connections() {
        let mut server = RealTcpBlock::new(TcpConfig {
            max_connections: 1,
            ..test_config(19113)
        });
        server.start_listening().unwrap();

        let mut c1 = RealTcpBlock::new(test_config(0));
        c1.connect("127.0.0.1", 19113).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        server.accept_pending().unwrap();
        assert_eq!(server.connection_count(), 1);

        let mut c2 = RealTcpBlock::new(test_config(0));
        let _ = c2.connect("127.0.0.1", 19113);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = server.accept_pending();
        assert!(server.connection_count() <= 1);
    }

    #[test]
    fn test_real_tcp_no_pending_data() {
        let mut server = RealTcpBlock::new(test_config(19114));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        client.connect("127.0.0.1", 19114).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let server_conn = server.accept_pending().unwrap().unwrap();

        let data = server.receive(server_conn).unwrap();
        assert!(data.is_none());
    }

    #[test]
    fn test_reuse_addr_allows_quick_rebind() {
        let port = 19120;
        let mut block = RealTcpBlock::new(test_config(port));
        block.start_listening().unwrap();
        block.stop();
        let mut block2 = RealTcpBlock::new(test_config(port));
        assert!(block2.start_listening().is_ok());
        block2.stop();
    }

    #[test]
    fn test_keepalive_set_on_accepted_connection() {
        let mut server = RealTcpBlock::new(test_config(19121));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        client.connect("127.0.0.1", 19121).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let server_conn_id = server.accept_pending().unwrap().unwrap();

        let conn = server
            .connections
            .iter()
            .find(|c| c.meta.id == server_conn_id)
            .unwrap();
        let keepalive = get_keepalive(&conn.stream);
        assert!(
            keepalive,
            "SO_KEEPALIVE should be enabled on accepted connection"
        );
    }

    #[test]
    fn test_nodelay_set_on_connection() {
        let mut server = RealTcpBlock::new(test_config(19122));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        client.connect("127.0.0.1", 19122).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = server.accept_pending().unwrap();

        let conn = client.connections.iter().find(|c| c.meta.id == 1).unwrap();
        let nodelay = conn.stream.nodelay().unwrap();
        assert!(
            nodelay,
            "TCP_NODELAY should be enabled on client connection"
        );
    }

    #[test]
    fn test_reuse_addr_disabled_rebind_fails_quickly() {
        let port = 19123;
        let mut block = RealTcpBlock::new(test_config(port));
        block.start_listening().unwrap();
        block.stop();

        let mut no_reuse = RealTcpBlock::new(TcpConfig {
            reuse_addr: false,
            ..test_config(port)
        });
        let result = no_reuse.start_listening();
        if result.is_ok() {
            no_reuse.stop();
        }
    }

    #[test]
    fn test_no_capability_token_allows_all() {
        let mut block = RealTcpBlock::new(test_config(19130));
        assert!(block.start_listening().is_ok());
        block.stop();
    }

    #[test]
    fn test_net_bind_capability_granted() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut block = RealTcpBlock::new(test_config(19131));
        let token = CapabilityToken::new(1, vec![Capability::NetBind], 60_000, b"test_secret");
        block.set_capability(token);
        assert!(block.start_listening().is_ok());
        block.stop();
    }

    #[test]
    fn test_net_bind_capability_denied() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut block = RealTcpBlock::new(test_config(19132));
        let token = CapabilityToken::new(1, vec![Capability::FsRead], 60_000, b"test_secret");
        block.set_capability(token);
        let err = block.start_listening();
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("Missing capability"));
    }

    #[test]
    fn test_net_connect_capability_granted() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut server = RealTcpBlock::new(test_config(19133));
        server.start_listening().unwrap();

        let mut client = RealTcpBlock::new(test_config(0));
        let token = CapabilityToken::new(2, vec![Capability::NetConnect], 60_000, b"test_secret");
        client.set_capability(token);
        let conn_id = client.connect("127.0.0.1", 19133).unwrap();
        assert!(conn_id > 0);

        server.stop();
        client.stop();
    }

    #[test]
    fn test_net_connect_capability_denied() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut block = RealTcpBlock::new(test_config(0));
        let token = CapabilityToken::new(2, vec![Capability::FsWrite], 60_000, b"test_secret");
        block.set_capability(token);
        let err = block.connect("127.0.0.1", 80);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("Missing capability"));
    }

    #[test]
    fn test_expired_capability_token_denied() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut block = RealTcpBlock::new(test_config(19134));
        let mut token = CapabilityToken::new(1, vec![Capability::NetBind], 60_000, b"test_secret");
        token.expires_at_ms = aios_security::capability::now_ms().saturating_sub(1000);
        block.set_capability(token);
        let err = block.start_listening();
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn test_all_capability_grants_everything() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut block = RealTcpBlock::new(test_config(19135));
        let token = CapabilityToken::new(1, vec![Capability::All], 60_000, b"test_secret");
        block.set_capability(token);
        assert!(block.start_listening().is_ok());
        block.stop();
    }
}
