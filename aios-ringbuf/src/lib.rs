//! Zero-Copy IPC Ring Buffer for High-Throughput Data Passing
//!
//! Provides lock-free, single-producer/single-consumer ring buffers
//! for O(1) data transmission between AIOS blocks without kernel copies.

pub mod buffer;
pub mod reader;
pub mod writer;

pub use buffer::RingBuffer;
pub use reader::RingBufferReader;
pub use writer::RingBufferWriter;

/// Ring buffer configuration
#[derive(Debug, Clone, Copy)]
pub struct RingBufferConfig {
    /// Total capacity in bytes
    pub capacity: usize,
    /// Enable zero-copy mode (requires unsafe operations)
    pub zero_copy: bool,
}

impl Default for RingBufferConfig {
    fn default() -> Self {
        Self {
            capacity: 65536,
            zero_copy: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ringbuffer_creation() {
        let config = RingBufferConfig::default();
        let rb = RingBuffer::new(config).expect("Failed to create ring buffer");
        assert_eq!(rb.capacity(), 65536);
    }
}
