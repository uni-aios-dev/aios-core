use aios_net::{RealTcpBlock, RealUdpBlock, TcpConfig, UdpConfig};
use std::thread;
use std::time::Duration;

fn tcp_config(port: u16) -> TcpConfig {
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
fn test_tcp_listen_accept_send_receive() {
    let mut server = RealTcpBlock::new(tcp_config(19200));
    server.start_listening().unwrap();

    let mut client = RealTcpBlock::new(tcp_config(0));
    let client_conn = client.connect("127.0.0.1", 19200).unwrap();
    thread::sleep(Duration::from_millis(50));

    let server_conn = server.accept_pending().unwrap().unwrap();
    assert!(server_conn > 0);

    client
        .send(client_conn, b"integration test data".to_vec())
        .unwrap();
    thread::sleep(Duration::from_millis(30));

    let received = server.receive(server_conn).unwrap().unwrap();
    assert_eq!(received, b"integration test data");
    assert!(server.bytes_received() > 0);

    server.stop();
}

#[test]
fn test_tcp_bidirectional_multimsg() {
    let mut server = RealTcpBlock::new(tcp_config(19201));
    server.start_listening().unwrap();

    let mut client = RealTcpBlock::new(tcp_config(0));
    let cc = client.connect("127.0.0.1", 19201).unwrap();
    thread::sleep(Duration::from_millis(50));
    let sc = server.accept_pending().unwrap().unwrap();

    for i in 0..5 {
        let msg = format!("msg_{}", i);
        client.send(cc, msg.clone().into_bytes()).unwrap();
        thread::sleep(Duration::from_millis(20));

        let data = server.receive(sc).unwrap().unwrap();
        assert_eq!(data, msg.as_bytes());

        let reply = format!("ack_{}", i);
        server.send(sc, reply.clone().into_bytes()).unwrap();
        thread::sleep(Duration::from_millis(20));

        let resp = client.receive(cc).unwrap().unwrap();
        assert_eq!(resp, reply.as_bytes());
    }

    server.stop();
}

#[test]
fn test_tcp_multiple_clients() {
    let mut server = RealTcpBlock::new(tcp_config(19202));
    server.start_listening().unwrap();

    let mut clients: Vec<(RealTcpBlock, u32)> = Vec::new();
    for _ in 0..3 {
        let mut c = RealTcpBlock::new(tcp_config(0));
        let conn = c.connect("127.0.0.1", 19202).unwrap();
        clients.push((c, conn));
    }

    thread::sleep(Duration::from_millis(50));

    let mut accepted = Vec::new();
    while let Some(id) = server.accept_pending().unwrap() {
        accepted.push(id);
    }
    assert_eq!(accepted.len(), 3);

    for (i, (client, conn_id)) in clients.iter_mut().enumerate() {
        let msg = format!("client_{}", i);
        client.send(*conn_id, msg.into_bytes()).unwrap();
    }

    thread::sleep(Duration::from_millis(30));

    for (i, &sc) in accepted.iter().enumerate() {
        let data = server.receive(sc).unwrap().unwrap();
        let expected = format!("client_{}", i);
        assert_eq!(data, expected.as_bytes());
    }

    server.stop();
}

#[test]
fn test_tcp_close_and_reopen() {
    let port = 19203;
    let mut server = RealTcpBlock::new(tcp_config(port));
    server.start_listening().unwrap();

    let mut client = RealTcpBlock::new(tcp_config(0));
    let _cc = client.connect("127.0.0.1", port).unwrap();
    thread::sleep(Duration::from_millis(50));
    let sc = server.accept_pending().unwrap().unwrap();
    server.close_connection(sc).unwrap();
    assert_eq!(server.connection_count(), 0);

    drop(server);
    thread::sleep(Duration::from_millis(50));

    let mut server2 = RealTcpBlock::new(tcp_config(port));
    server2.start_listening().unwrap();

    let mut client2 = RealTcpBlock::new(tcp_config(0));
    client2.connect("127.0.0.1", port).unwrap();
    thread::sleep(Duration::from_millis(50));
    let sc2 = server2.accept_pending().unwrap();
    assert!(sc2.is_some());

    server2.stop();
}

#[test]
fn test_udp_bind_send_receive_loopback() {
    let mut receiver = RealUdpBlock::new(UdpConfig {
        bind_addr: "127.0.0.1".into(),
        port: 0,
        ..Default::default()
    });
    receiver.bind().unwrap();

    let mut sender = RealUdpBlock::new(UdpConfig {
        bind_addr: "127.0.0.1".into(),
        port: 0,
        ..Default::default()
    });
    sender.bind().unwrap();

    let payload = b"udp integration payload";
    sender
        .send_to("127.0.0.1", receiver.port(), payload.to_vec())
        .unwrap();
    assert!(sender.bytes_sent() > 0);

    thread::sleep(Duration::from_millis(50));

    let packet = receiver.receive_packet().unwrap().unwrap();
    assert_eq!(packet.data, payload);
}

#[test]
fn test_udp_multiple_messages() {
    let mut receiver = RealUdpBlock::new(UdpConfig {
        port: 0,
        ..Default::default()
    });
    receiver.bind().unwrap();

    let mut sender = RealUdpBlock::new(UdpConfig {
        port: 0,
        ..Default::default()
    });
    sender.bind().unwrap();

    for i in 0..10 {
        let msg = format!("datagram_{}", i);
        sender
            .send_to("127.0.0.1", receiver.port(), msg.into_bytes())
            .unwrap();
        thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(sender.packets_sent(), 10);

    thread::sleep(Duration::from_millis(50));

    let mut received = 0;
    while let Some(_pkt) = receiver.receive_packet().unwrap() {
        received += 1;
    }
    assert_eq!(received, 10);
}

#[test]
fn test_udp_broadcast_flag() {
    let mut block = RealUdpBlock::new(UdpConfig {
        bind_addr: "127.0.0.1".into(),
        port: 0,
        broadcast: true,
        ..Default::default()
    });
    block.bind().unwrap();
}

#[test]
fn test_udp_close_state() {
    let mut block = RealUdpBlock::new(UdpConfig {
        port: 0,
        ..Default::default()
    });
    block.bind().unwrap();
    assert_eq!(block.state(), aios_net::UdpState::Bound);
    block.close();
    assert_eq!(block.state(), aios_net::UdpState::Closed);
}

#[test]
fn test_tcp_no_data_pending() {
    let mut server = RealTcpBlock::new(tcp_config(19210));
    server.start_listening().unwrap();

    let mut client = RealTcpBlock::new(tcp_config(0));
    client.connect("127.0.0.1", 19210).unwrap();
    thread::sleep(Duration::from_millis(50));
    let sc = server.accept_pending().unwrap().unwrap();

    let data = server.receive(sc).unwrap();
    assert!(data.is_none());

    server.stop();
}

#[test]
fn test_tcp_max_connections_enforced() {
    let mut server = RealTcpBlock::new(TcpConfig {
        max_connections: 2,
        ..tcp_config(19211)
    });
    server.start_listening().unwrap();

    for _ in 0..2 {
        let mut c = RealTcpBlock::new(tcp_config(0));
        let _ = c.connect("127.0.0.1", 19211);
        thread::sleep(Duration::from_millis(30));
        let _ = server.accept_pending();
    }
    assert_eq!(server.connection_count(), 2);

    let mut extra = RealTcpBlock::new(tcp_config(0));
    let _ = extra.connect("127.0.0.1", 19211);
    thread::sleep(Duration::from_millis(30));
    let _ = server.accept_pending();
    assert!(server.connection_count() <= 2);

    server.stop();
}

#[test]
fn test_udp_no_data_returns_none() {
    let mut block = RealUdpBlock::new(UdpConfig {
        port: 0,
        ..Default::default()
    });
    block.bind().unwrap();
    let pkt = block.receive_packet().unwrap();
    assert!(pkt.is_none());
}
