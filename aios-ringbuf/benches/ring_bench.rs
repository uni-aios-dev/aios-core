use aios_ringbuf::{RingBuffer, RingBufferConfig};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn ring_config() -> RingBufferConfig {
    RingBufferConfig {
        capacity: 65536,
        zero_copy: true,
    }
}

fn bench_ring_write_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_write_read");
    for size in &[64, 256, 1024, 4096, 16384] {
        let ring = RingBuffer::new(ring_config()).unwrap();
        let data = vec![0xAA; *size];
        group.throughput(criterion::Throughput::Bytes(*size as u64));
        group.bench_with_input(format!("{}b", size), &data, |b, data| {
            b.iter(|| {
                ring.write(black_box(data)).unwrap();
                let mut buf = vec![0u8; data.len()];
                ring.read(&mut buf).unwrap();
            })
        });
    }
    group.finish();
}

fn bench_ring_throughput(c: &mut Criterion) {
    let ring = RingBuffer::new(ring_config()).unwrap();
    let data = vec![0xBB; 4096];

    c.bench_function("ring_throughput_4k_sequential", |b| {
        b.iter(|| {
            ring.write(black_box(&data)).unwrap();
            let mut buf = vec![0u8; 4096];
            ring.read(&mut buf).unwrap();
        })
    });
}

fn bench_ring_zero_copy(c: &mut Criterion) {
    let ring = RingBuffer::new(ring_config()).unwrap();
    let data = vec![0xCC; 4096];

    c.bench_function("ring_zero_copy_write_read_4k", |b| {
        b.iter(|| {
            let (ptr, available) = ring.write_ptr();
            let len = data.len().min(available);
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
            }
            ring.advance_write(len).unwrap();

            let (ptr, available) = ring.read_ptr();
            let len = data.len().min(available);
            let mut buf = vec![0u8; len];
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), len);
            }
            ring.advance_read(len).unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_ring_write_read,
    bench_ring_throughput,
    bench_ring_zero_copy,
);
criterion_main!(benches);
