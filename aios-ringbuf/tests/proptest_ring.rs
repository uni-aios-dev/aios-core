use aios_ringbuf::{RingBuffer, RingBufferConfig};
use proptest::prelude::*;

proptest! {
    #[test]
    fn ring_write_read_preserves_data(
        capacity in 128usize..=131072,
        data in prop::collection::vec(any::<u8>(), 1..=1024),
    ) {
        let config = RingBufferConfig { capacity, zero_copy: true };
        let ring = RingBuffer::new(config).unwrap();
        let usable = capacity - 1;
        let write_len = data.len().min(usable);
        let slice = &data[..write_len];
        let written = ring.write(slice).unwrap();
        prop_assume!(written > 0, "ring accepted some data");

        let mut buf = vec![0u8; written];
        let read = ring.read(&mut buf).unwrap();
        prop_assert_eq!(read, written);
        prop_assert_eq!(&buf[..read], &slice[..written]);
    }

    #[test]
    fn ring_capacity_never_exceeded(
        capacity in 128usize..=131072,
        data in prop::collection::vec(any::<u8>(), 1..=4096),
    ) {
        let config = RingBufferConfig { capacity, zero_copy: true };
        let ring = RingBuffer::new(config).unwrap();
        let usable = capacity - 1;
        let write_len = data.len().min(usable);
        let written = ring.write(&data[..write_len]).unwrap();
        prop_assert!(
            written <= usable,
            "written {} exceeded usable capacity {}",
            written,
            usable,
        );
    }

    #[test]
    fn ring_available_read_after_write(
        capacity in 128usize..=131072,
        data in prop::collection::vec(any::<u8>(), 1..=1024),
    ) {
        let config = RingBufferConfig { capacity, zero_copy: true };
        let ring = RingBuffer::new(config).unwrap();
        let usable = capacity - 1;
        let write_len = data.len().min(usable);
        let slice = &data[..write_len];
        let written = ring.write(slice).unwrap();
        prop_assert!(
            ring.available_read() >= written,
            "available_read {} < written {}",
            ring.available_read(),
            written,
        );
    }

    #[test]
    fn ring_multiple_sequential_writes(
        capacity in 128usize..=8192,
        chunks in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 1..=32),
            1..=8,
        ),
    ) {
        let config = RingBufferConfig { capacity, zero_copy: true };
        let ring = RingBuffer::new(config).unwrap();
        let usable = capacity - 1;
        let mut total_written = 0usize;
        for chunk in &chunks {
            let space = usable.saturating_sub(total_written);
            if space == 0 {
                break;
            }
            let write_len = chunk.len().min(space);
            let w = ring.write(&chunk[..write_len]).unwrap();
            total_written += w;
        }
        prop_assert!(
            ring.available_read() >= total_written.min(usable),
            "available_read {} should be >= total_written {}",
            ring.available_read(),
            total_written,
        );
    }

    #[test]
    fn ring_zero_copy_write_read(
        capacity in 128usize..=8192,
        data in prop::collection::vec(any::<u8>(), 1..=64),
    ) {
        let config = RingBufferConfig { capacity, zero_copy: true };
        let ring = RingBuffer::new(config).unwrap();

        let (ptr, available) = ring.write_ptr();
        let len = data.len().min(available);
        if len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
            }
            ring.advance_write(len).unwrap();

            let (rptr, rlen) = ring.read_ptr();
            let read_len = len.min(rlen);
            let mut buf = vec![0u8; read_len];
            unsafe {
                std::ptr::copy_nonoverlapping(rptr, buf.as_mut_ptr(), read_len);
            }
            ring.advance_read(read_len).unwrap();

            prop_assert_eq!(&buf[..read_len], &data[..read_len]);
        }
    }
}
