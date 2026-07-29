use aios_compress::compressor::StateCompressor;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn compress_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        data.push(((i * 7 + 13) % 256) as u8);
    }
    data
}

fn bench_compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress");
    for size in &[1024, 65536, 1048576] {
        for level in &[1i32, 3, 12] {
            let compressor = StateCompressor::with_level(*level).unwrap();
            let data = compress_data(*size as usize);
            group.throughput(criterion::Throughput::Bytes(*size as u64));
            group.bench_with_input(
                format!("level{}_{}kb", level, size / 1024),
                &data,
                |b, data| b.iter(|| black_box(compressor.compress(data).unwrap())),
            );
        }
    }
    group.finish();
}

fn bench_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress");
    for size in &[1024, 65536, 1048576] {
        let compressor = StateCompressor::new();
        let data = compress_data(*size as usize);
        let compressed = compressor.compress(&data).unwrap();
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(
            format!("{}kb", size / 1024),
            &compressed,
            |b, compressed| b.iter(|| black_box(compressor.decompress(compressed).unwrap())),
        );
    }
    group.finish();
}

fn bench_compress_ratio(c: &mut Criterion) {
    let compressor = StateCompressor::new();
    let data = compress_data(65536);
    c.bench_function("estimate_ratio_64k", |b| {
        b.iter(|| black_box(compressor.estimate_ratio(&data).unwrap()))
    });
}

fn repetitive_data(size: usize) -> Vec<u8> {
    std::iter::repeat(0xABu8).take(size).collect()
}

fn random_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut state: u64 = 0xDEAD_BEEF;
    for _ in 0..size {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        data.push((state >> 32) as u8);
    }
    data
}

fn telemetry_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut i = 0u64;
    while data.len() < size {
        let metric = format!(
            "cpu_usage: {} ram: {} iops: {}",
            i % 100,
            (i * 7) % 4096,
            (i * 13) % 8192
        );
        data.extend_from_slice(metric.as_bytes());
        data.push(b'\n');
        i += 1;
    }
    data.truncate(size);
    data
}

fn bench_compress_ratio_by_pattern(c: &mut Criterion) {
    let compressor = StateCompressor::new();
    let mut group = c.benchmark_group("compress_ratio");
    let size = 65536;

    let patterns: Vec<(&str, Vec<u8>)> = vec![
        ("repetitive_64k", repetitive_data(size)),
        ("random_64k", random_data(size)),
        ("telemetry_64k", telemetry_data(size)),
    ];

    for (name, data) in &patterns {
        let compressed = compressor.compress(data).unwrap();
        let ratio = data.len() as f64 / compressed.len() as f64;

        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(format!("compress_{}", name), data, |b, data| {
            b.iter(|| black_box(compressor.compress(data).unwrap()))
        });

        eprintln!(
            "  {} ratio: {:.2}x ({} -> {} bytes)",
            name,
            ratio,
            data.len(),
            compressed.len()
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_compress,
    bench_decompress,
    bench_compress_ratio,
    bench_compress_ratio_by_pattern,
);
criterion_main!(benches);
