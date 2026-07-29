use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_ipc_serialize(c: &mut Criterion) {
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, Payload::Empty);
    c.bench_function("ipc_serialize_empty", |b| {
        b.iter(|| black_box(packet.serialize().unwrap()))
    });

    let payload = Payload::Binary(vec![0xAA; 1024]);
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, payload);
    c.bench_function("ipc_serialize_1k_binary", |b| {
        b.iter(|| black_box(packet.serialize().unwrap()))
    });

    let payload = Payload::Binary(vec![0xBB; 65536]);
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, payload);
    c.bench_function("ipc_serialize_64k_binary", |b| {
        b.iter(|| black_box(packet.serialize().unwrap()))
    });
}

fn bench_ipc_deserialize(c: &mut Criterion) {
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, Payload::Empty);
    let data = packet.serialize().unwrap();
    c.bench_function("ipc_deserialize_empty", |b| {
        b.iter(|| black_box(IpcPacket::deserialize(&data).unwrap()))
    });

    let payload = Payload::Binary(vec![0xAA; 1024]);
    let packet = IpcPacket::new(1, 2, CommandId::HealthCheck, payload);
    let data = packet.serialize().unwrap();
    c.bench_function("ipc_deserialize_1k_binary", |b| {
        b.iter(|| black_box(IpcPacket::deserialize(&data).unwrap()))
    });
}

fn bench_ipc_new(c: &mut Criterion) {
    c.bench_function("ipc_new_empty", |b| {
        b.iter(|| black_box(IpcPacket::new(1, 2, CommandId::HealthCheck, Payload::Empty)))
    });

    c.bench_function("ipc_new_1k_binary", |b| {
        b.iter_batched(
            || Payload::Binary(vec![0xCC; 1024]),
            |payload| black_box(IpcPacket::new(1, 2, CommandId::HealthCheck, payload)),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_ipc_verify_checksum(c: &mut Criterion) {
    let packet = IpcPacket::new(
        1,
        2,
        CommandId::HealthCheck,
        Payload::Binary(vec![0xDD; 4096]),
    );
    c.bench_function("ipc_verify_checksum_4k", |b| {
        b.iter(|| black_box(packet.verify_checksum()))
    });
}

criterion_group!(
    benches,
    bench_ipc_serialize,
    bench_ipc_deserialize,
    bench_ipc_new,
    bench_ipc_verify_checksum,
);
criterion_main!(benches);
