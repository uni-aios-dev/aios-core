//! Ring buffer writer for producer-side operations

use crate::RingBuffer;
use aios_core::error::Result;

/// Writer interface for ring buffer producers
pub struct RingBufferWriter {
    buffer: RingBuffer,
}

impl RingBufferWriter {
    /// Create a writer from a ring buffer
    pub fn new(buffer: RingBuffer) -> Self {
        RingBufferWriter { buffer }
    }

    /// Write data to ring buffer
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        self.buffer.write(data)
    }

    /// Get zero-copy write pointer
    pub fn write_ptr(&self) -> (*mut u8, usize) {
        self.buffer.write_ptr()
    }

    /// Advance write position
    pub fn advance(&self, count: usize) -> Result<()> {
        self.buffer.advance_write(count)
    }

    /// Check available space
    pub fn available(&self) -> usize {
        self.buffer.available_write()
    }

    /// Wait for space (busy-spin with timeout)
    pub fn wait_for_space(&self, needed: usize, timeout_ms: u64) -> Result<bool> {
        let start = std::time::Instant::now();
        loop {
            if self.available() >= needed {
                return Ok(true);
            }
            if start.elapsed().as_millis() as u64 > timeout_ms {
                return Ok(false);
            }
            std::thread::yield_now();
        }
    }

    /// Flush writes (no-op for ring buffers, but semantically important)
    pub fn flush(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RingBufferConfig;

    #[test]
    fn test_writer_basic() {
        let config = RingBufferConfig::default();
        let rb = RingBuffer::new(config).unwrap();
        let writer = RingBufferWriter::new(rb);

        assert!(writer.available() > 0);
    }

    #[test]
    fn test_writer_wait_timeout() {
        let config = RingBufferConfig {
            capacity: 10,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();
        let writer = RingBufferWriter::new(rb);

        // Fill buffer
        let data = vec![0u8; 9];
        writer.write(&data).unwrap();

        // Try to wait for 100 bytes (should timeout)
        let result = writer.wait_for_space(100, 10).unwrap();
        assert!(!result);
    }
}
