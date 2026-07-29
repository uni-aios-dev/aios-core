use aios_persistence::cow_storage::CopyOnWriteStorage;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tempfile::TempDir;

fn bench_atomic_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("atomic_write");
    for size in &[256, 4096, 65536] {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();
        let data = vec![0xAA; *size];
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("{}b", size), &data, |b, data| {
            b.iter(|| storage.atomic_write(black_box("bench_file"), data).unwrap())
        });
    }
    group.finish();
}

fn bench_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cow_read");
    for size in &[256, 4096, 65536] {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();
        let data = vec![0xBB; *size];
        storage.atomic_write("bench_file", &data).unwrap();
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("{}b", size), &data, |b, _| {
            b.iter(|| black_box(storage.read("bench_file").unwrap()))
        });
    }
    group.finish();
}

fn bench_atomic_write_read_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("atomic_write_read_roundtrip");
    for size in &[256, 4096, 65536] {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();
        let data = vec![0xCC; *size];
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("{}b", size), &data, |b, data| {
            b.iter(|| {
                storage.atomic_write("rt_file", data).unwrap();
                black_box(storage.read("rt_file").unwrap())
            })
        });
    }
    group.finish();
}

fn bench_snapshot_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("cow_snapshot");
    for size in &[4096, 65536, 262144] {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();
        let data = vec![0xDD; *size];
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("snapshot_{}b", size), &data, |b, data| {
            b.iter(|| storage.atomic_write(black_box("state.bin"), data).unwrap())
        });
    }
    group.finish();
}

fn bench_rollback_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("cow_rollback");
    for size in &[256, 4096, 65536] {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        let v1 = vec![0x11; *size];
        let v2 = vec![0x22; *size];
        storage.atomic_write("state.bin", &v1).unwrap();
        storage.atomic_write("state.bin", &v2).unwrap();

        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("rollback_{}b", size), size, |b, _| {
            b.iter(|| black_box(storage.rollback("state.bin").unwrap()))
        });
    }
    group.finish();
}

fn bench_disk_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("cow_disk_overhead");
    let sizes = [1024, 8192, 65536];

    for size in &sizes {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();
        let data = vec![0xEE; *size];

        storage.atomic_write("overhead_test", &data).unwrap();
        let file_size = storage.file_size("overhead_test").unwrap();
        let overhead = file_size as f64 / *size as f64;

        eprintln!(
            "  {}b payload -> {}b on disk ({:.1}x overhead)",
            size, file_size, overhead
        );

        group.bench_function(format!("metadata_{}b", size), |b| {
            b.iter(|| black_box(storage.file_size("overhead_test").unwrap()))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_atomic_write,
    bench_read,
    bench_atomic_write_read_roundtrip,
    bench_snapshot_creation,
    bench_rollback_latency,
    bench_disk_overhead,
);
criterion_main!(benches);
