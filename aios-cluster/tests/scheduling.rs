//! Integration tests for the distributed scheduler.
//!
//! These spin up real `DistributedScheduler` instances, connect them through
//! the in-memory transport (deterministic, single process) and the TCP
//! transport (real loopback sockets), and verify discovery, placement,
//! kill/priority control and failover.
use aios_cluster::executor::{MockProcessExecutor, ProcessExecutor};
use aios_cluster::scheduler::DistributedScheduler;
use aios_cluster::transport::{
    ClusterTransport, InMemoryClusterTransport, MemoryRegistry, TcpClusterTransport,
};
use aios_cluster::types::{
    NodeInfo, NodeMetrics, NodeStatus, PlacementStrategy, RemoteProcessId, RemoteProcessSpec,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn node_info(id: u64, name: &str, addr: String, tier: u8) -> NodeInfo {
    NodeInfo {
        id,
        name: name.to_string(),
        addr,
        tier,
        status: NodeStatus::Online,
        metrics: NodeMetrics::idle(),
    }
}

fn wait_until<T>(mut f: impl FnMut() -> Option<T>, what: &str, timeout: Duration) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} ({}ms)",
            timeout.as_millis()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn event_loop(
    sched: Arc<Mutex<DistributedScheduler>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if sched.lock().unwrap().process_events() == 0 {
                thread::sleep(Duration::from_millis(2));
            }
        }
    })
}

type NodeHandle = Arc<Mutex<DistributedScheduler>>;
type MemoryCluster = (
    MemoryRegistry,
    Vec<NodeHandle>,
    Vec<Arc<MockProcessExecutor>>,
);

fn memory_cluster(ids: &[(u64, &str)], strategy: PlacementStrategy) -> MemoryCluster {
    let registry = MemoryRegistry::new();
    let mut schedulers = Vec::new();
    let mut executors = Vec::new();
    let mut addrs = Vec::new();
    let mut transports = Vec::new();
    for (_id, name) in ids {
        let addr = format!("mem://{name}");
        let transport = InMemoryClusterTransport::new(&addr, registry.clone_arc());
        addrs.push(addr);
        transports.push(Arc::from(transport) as Arc<dyn ClusterTransport>);
    }
    for (idx, (id, name)) in ids.iter().enumerate() {
        let executor = Arc::new(MockProcessExecutor::new(*id));
        let mut sched = DistributedScheduler::new(
            node_info(*id, name, addrs[idx].clone(), 2),
            transports[idx].clone(),
            strategy,
        )
        .with_heartbeat(Duration::from_millis(30))
        .with_failover_threshold(Duration::from_millis(400))
        .with_ack_timeout(Duration::from_secs(3));
        sched.set_executor(executor.clone());
        executors.push(executor);
        schedulers.push(Arc::new(Mutex::new(sched)));
    }
    (registry, schedulers, executors)
}

fn start_mem_nodes(
    schedulers: &[Arc<Mutex<DistributedScheduler>>],
    addrs: &[String],
) -> Vec<Arc<AtomicBool>> {
    let mut stops = Vec::new();
    for (idx, sched) in schedulers.iter().enumerate() {
        let peers: Vec<String> = addrs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx)
            .map(|(_, a)| a.clone())
            .collect();
        sched.lock().unwrap().start(&peers).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        stops.push(stop.clone());
        let _handle = event_loop(sched.clone(), stop);
        // NOTE: handle is detached on purpose; shutdown() is driven by Drop.
    }
    stops
}

fn wait_peers_online(sched: &Arc<Mutex<DistributedScheduler>>, expected: usize, timeout: Duration) {
    wait_until(
        || {
            let s = sched.lock().unwrap();
            let online = s
                .nodes()
                .iter()
                .filter(|n| n.status == NodeStatus::Online)
                .count();
            (online >= expected).then_some(online)
        },
        &format!("{expected} online peers"),
        timeout,
    );
}

#[test]
fn two_node_discovery_spawn_kill() {
    let (_registry, schedulers, executors) =
        memory_cluster(&[(1, "a"), (2, "b")], PlacementStrategy::LeastLoaded);
    let a = &schedulers[0];
    let b = &schedulers[1];
    let exec_b = &executors[1];
    let addrs = vec!["mem://a".to_string(), "mem://b".to_string()];
    let _stops = start_mem_nodes(&schedulers, &addrs);

    wait_peers_online(a, 1, Duration::from_secs(5));
    wait_peers_online(b, 1, Duration::from_secs(5));

    let spec = RemoteProcessSpec::new("gateway", 2, 256);
    let rid = a
        .lock()
        .unwrap()
        .spawn(spec.clone(), None)
        .expect("spawn should succeed");
    assert_eq!(rid.node, 2);
    assert!(rid.pid >= 1);

    // Coordinator tracks it; worker hosts it.
    assert_eq!(a.lock().unwrap().processes().len(), 1);
    let host: Vec<_> = exec_b
        .status()
        .into_iter()
        .filter(|p| p.name == "gateway")
        .collect();
    assert_eq!(host.len(), 1);
    assert_eq!(host[0].id, rid);

    // Kill through the coordinator removes it from both sides.
    a.lock().unwrap().kill(rid).expect("kill should succeed");
    assert!(a.lock().unwrap().processes().is_empty());
    wait_until(
        || (exec_b.status().is_empty()).then_some(()),
        "worker process removed",
        Duration::from_secs(3),
    );
}

#[test]
fn round_robin_placement_alternates() {
    let (_registry, schedulers, _executors) = memory_cluster(
        &[(1, "a"), (2, "b"), (3, "c")],
        PlacementStrategy::RoundRobin,
    );
    let a = &schedulers[0];
    let addrs = vec![
        "mem://a".to_string(),
        "mem://b".to_string(),
        "mem://c".to_string(),
    ];
    let _stops = start_mem_nodes(&schedulers, &addrs);

    wait_peers_online(a, 2, Duration::from_secs(5));

    let mut nodes = Vec::new();
    for i in 0..4 {
        let spec = RemoteProcessSpec::new(&format!("job-{i}"), 2, 128);
        let rid = a
            .lock()
            .unwrap()
            .spawn(spec, None)
            .expect("spawn should succeed");
        nodes.push(rid.node);
    }
    assert_eq!(nodes, vec![2, 3, 2, 3]);
    assert_eq!(a.lock().unwrap().processes().len(), 4);
}

#[test]
fn least_loaded_prefers_empty_node() {
    let (_registry, schedulers, executors) = memory_cluster(
        &[(1, "a"), (2, "b"), (3, "c")],
        PlacementStrategy::LeastLoaded,
    );
    let a = &schedulers[0];
    let exec_b = &executors[1];
    let addrs = vec![
        "mem://a".to_string(),
        "mem://b".to_string(),
        "mem://c".to_string(),
    ];
    let _stops = start_mem_nodes(&schedulers, &addrs);

    // Pre-load node b with a heavy process so its reported load is higher.
    exec_b
        .spawn(&RemoteProcessSpec::new("heavy", 2, 4096))
        .expect("preload should succeed");

    wait_peers_online(a, 2, Duration::from_secs(5));
    // Let the metrics of the preloaded node propagate.
    thread::sleep(Duration::from_millis(250));

    let spec = RemoteProcessSpec::new("light", 2, 64);
    let rid = a
        .lock()
        .unwrap()
        .spawn(spec, None)
        .expect("spawn should succeed");
    assert_eq!(
        rid.node, 3,
        "least-loaded placement must pick the empty node"
    );
}

#[test]
fn failover_respawns_on_other_node() {
    let (_registry, schedulers, executors) = memory_cluster(
        &[(1, "a"), (2, "b"), (3, "c")],
        PlacementStrategy::RoundRobin,
    );
    let a = &schedulers[0];
    let b = &schedulers[1];
    let exec_c = &executors[2];
    let addrs = vec![
        "mem://a".to_string(),
        "mem://b".to_string(),
        "mem://c".to_string(),
    ];
    let stops = start_mem_nodes(&schedulers, &addrs);

    wait_peers_online(a, 2, Duration::from_secs(5));

    // Two processes explicitly placed on node b.
    let rid1 = a
        .lock()
        .unwrap()
        .spawn(RemoteProcessSpec::new("svc-1", 3, 256), Some(2))
        .expect("spawn on b");
    let rid2 = a
        .lock()
        .unwrap()
        .spawn(RemoteProcessSpec::new("svc-2", 3, 512), Some(2))
        .expect("spawn on b");
    assert_eq!(rid1.node, 2);
    assert_eq!(rid2.node, 2);
    assert_eq!(a.lock().unwrap().processes().len(), 2);

    // Kill node b: stop its heartbeat and its event loop, remove it from the
    // registry so nothing is delivered anymore.
    stops[1].store(true, Ordering::Relaxed);
    b.lock().unwrap().shutdown();

    // Drive failover from node a until the processes are respawned on c.
    let events = wait_until(
        || {
            let evs = a.lock().unwrap().tick();
            let ok = evs.iter().any(|e| e.contains("respawned"));
            ok.then_some(evs)
        },
        "failover respawn",
        Duration::from_secs(5),
    );
    assert!(
        events.iter().any(|e| e.contains("node 2 went offline")),
        "expected offline detection, got: {events:?}"
    );

    // Both original processes now hosted by c.
    let hosted: Vec<_> = exec_c
        .status()
        .into_iter()
        .filter(|p| p.name.starts_with("svc-"))
        .collect();
    assert_eq!(
        hosted.len(),
        2,
        "both processes must be respawned on node c"
    );
    assert!(a.lock().unwrap().processes().iter().all(|p| p.id.node == 3));
}

#[test]
fn set_priority_remote() {
    let (_registry, schedulers, executors) =
        memory_cluster(&[(1, "a"), (2, "b")], PlacementStrategy::LeastLoaded);
    let a = &schedulers[0];
    let exec_b = &executors[1];
    let addrs = vec!["mem://a".to_string(), "mem://b".to_string()];
    let _stops = start_mem_nodes(&schedulers, &addrs);
    wait_peers_online(a, 1, Duration::from_secs(5));

    let rid = a
        .lock()
        .unwrap()
        .spawn(RemoteProcessSpec::new("svc", 1, 128), None)
        .expect("spawn");

    a.lock()
        .unwrap()
        .set_priority(rid, 4)
        .expect("priority change");

    let host: Vec<_> = exec_b
        .status()
        .into_iter()
        .filter(|p| p.id == rid)
        .collect();
    assert_eq!(host.len(), 1);
}

#[test]
fn tcp_transport_spawn_kill() {
    let pa = portpicker::pick_unused_port().expect("no free port for a");
    let pb = portpicker::pick_unused_port().expect("no free port for b");
    let addr_a = format!("127.0.0.1:{pa}");
    let addr_b = format!("127.0.0.1:{pb}");
    let transport_a = TcpClusterTransport::new(&addr_a);
    let transport_b = TcpClusterTransport::new(&addr_b);

    let mut sched_a = DistributedScheduler::new(
        node_info(1, "a", transport_a.addr(), 2),
        Arc::from(transport_a),
        PlacementStrategy::LeastLoaded,
    )
    .with_heartbeat(Duration::from_millis(30))
    .with_failover_threshold(Duration::from_millis(500))
    .with_ack_timeout(Duration::from_secs(3));
    let mut sched_b = DistributedScheduler::new(
        node_info(2, "b", transport_b.addr(), 2),
        Arc::from(transport_b),
        PlacementStrategy::LeastLoaded,
    )
    .with_heartbeat(Duration::from_millis(30))
    .with_failover_threshold(Duration::from_millis(500))
    .with_ack_timeout(Duration::from_secs(3));
    let exec_b = Arc::new(MockProcessExecutor::new(2));
    sched_a.set_executor(Arc::new(MockProcessExecutor::new(1)));
    sched_b.set_executor(exec_b.clone());

    let a = Arc::new(Mutex::new(sched_a));
    let b = Arc::new(Mutex::new(sched_b));
    let mut stops = Vec::new();
    a.lock()
        .unwrap()
        .start(std::slice::from_ref(&addr_b))
        .unwrap();
    b.lock()
        .unwrap()
        .start(std::slice::from_ref(&addr_a))
        .unwrap();
    for sched in [&a, &b] {
        let stop = Arc::new(AtomicBool::new(false));
        stops.push(stop.clone());
        event_loop(sched.clone(), stop);
    }

    wait_peers_online(&a, 1, Duration::from_secs(5));
    wait_peers_online(&b, 1, Duration::from_secs(5));

    let rid = a
        .lock()
        .unwrap()
        .spawn(RemoteProcessSpec::new("edge", 2, 256), None)
        .expect("tcp spawn should succeed");
    assert_eq!(rid.pid, 1);
    let host: Vec<_> = exec_b
        .status()
        .into_iter()
        .filter(|p| p.name == "edge")
        .collect();
    assert_eq!(
        host.len(),
        1,
        "executor on node b must host the remote process"
    );

    a.lock()
        .unwrap()
        .kill(rid)
        .expect("tcp kill should succeed");
    wait_until(
        || (exec_b.status().is_empty()).then_some(()),
        "tcp worker process removed",
        Duration::from_secs(3),
    );

    for stop in stops {
        stop.store(true, Ordering::Relaxed);
    }
    a.lock().unwrap().shutdown();
    b.lock().unwrap().shutdown();
}

#[test]
fn unknown_node_or_no_peers_errors() {
    let transport = InMemoryClusterTransport::isolated("mem://solo");
    let mut sched = DistributedScheduler::new(
        node_info(1, "solo", transport.addr(), 1),
        Arc::from(transport),
        PlacementStrategy::RoundRobin,
    );
    sched.start(&[]).unwrap();
    let err = sched
        .spawn(RemoteProcessSpec::new("x", 2, 64), None)
        .expect_err("no peers must reject");
    assert!(err.contains("no online node"), "unexpected error: {err}");
    assert!(sched.kill(RemoteProcessId { node: 9, pid: 1 }).is_err());
    sched.shutdown();
}
