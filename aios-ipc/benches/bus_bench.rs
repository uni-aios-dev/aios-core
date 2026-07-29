use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_ipc::bus::IpcBus;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_bus_send_receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus_send_receive");
    for size in &[64, 256, 1024, 4096] {
        let mut bus = IpcBus::new(1024);
        let payload = Payload::Binary(vec![0xAA; *size]);
        let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, payload);
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("{}b", size), &packet, |b, packet| {
            b.iter(|| {
                bus.send(black_box(packet.clone())).unwrap();
                bus.receive()
            })
        });
    }
    group.finish();
}

fn bench_bus_send_priority(c: &mut Criterion) {
    let mut bus = IpcBus::new(1024);
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, Payload::Empty);
    c.bench_function("bus_send_priority_empty", |b| {
        b.iter(|| {
            bus.send_priority(black_box(packet.clone())).unwrap();
            bus.receive()
        })
    });
}

fn bench_bus_throughput(c: &mut Criterion) {
    let mut bus = IpcBus::new(10240);
    let packet = IpcPacket::new(
        1,
        2,
        CommandId::HealthCheck,
        Payload::Binary(vec![0xBB; 256]),
    );

    c.bench_function("bus_throughput_1k_packets", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                bus.send(black_box(packet.clone())).unwrap();
            }
            for _ in 0..1000 {
                bus.receive();
            }
        })
    });
}

criterion_group!(
    benches,
    bench_bus_send_receive,
    bench_bus_send_priority,
    bench_bus_throughput,
);
criterion_main!(benches);
