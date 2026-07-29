//! Ring buffer reader for consumer-side operations

use crate::RingBuffer;
use aios_core::error::Result;

/// Reader interface for ring buffer consumers
pub struct RingBufferReader {
    buffer: RingBuffer,
}

impl RingBufferReader {
    /// Create a reader from a ring buffer
    pub fn new(buffer: RingBuffer) -> Self {
        RingBufferReader { buffer }
    }

    /// Read data into a buffer
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        self.buffer.read(buf)
    }

    /// Get zero-copy read pointer
    pub fn read_ptr(&self) -> (*const u8, usize) {
        self.buffer.read_ptr()
    }

    /// Advance read position
    pub fn advance(&self, count: usize) -> Result<()> {
        self.buffer.advance_read(count)
    }

    /// Check available data
    pub fn available(&self) -> usize {
        self.buffer.available_read()
    }

    /// Wait for data (busy-spin with timeout)
    pub fn wait_for_data(&self, timeout_ms: u64) -> Result<bool> {
        let start = std::time::Instant::now();
        loop {
            if self.available() > 0 {
                return Ok(true);
            }
            if start.elapsed().as_millis() as u64 > timeout_ms {
                return Ok(false);
            }
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RingBufferConfig;

    #[test]
    fn test_reader_basic() {
        let config = RingBufferConfig::default();
        let rb = RingBuffer::new(config).unwrap();
        let reader = RingBufferReader::new(rb);

        assert_eq!(reader.available(), 0);
    }

    #[test]
    fn test_reader_wait_timeout() {
        let config = RingBufferConfig::default();
        let rb = RingBuffer::new(config).unwrap();
        let reader = RingBufferReader::new(rb);

        let result = reader.wait_for_data(10).unwrap();
        assert!(!result); // Should timeout
    }
}
