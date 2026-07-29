//! Lock-free ring buffer implementation

use super::RingBufferConfig;
use aios_core::error::{AIOSException, Result};
use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Shared ring buffer state
pub struct RingBufferState {
    /// Write pointer (producer advances this)
    write_pos: AtomicUsize,
    /// Read pointer (consumer advances this)
    read_pos: AtomicUsize,
    /// Total capacity in bytes
    capacity: usize,
}

/// Thread-safe ring buffer for zero-copy IPC
pub struct RingBuffer {
    /// Raw data buffer
    data: *mut u8,
    /// Shared state (write_pos, read_pos)
    state: Arc<RingBufferState>,
    config: RingBufferConfig,
}

impl RingBuffer {
    /// Create a new ring buffer with given capacity
    pub fn new(config: RingBufferConfig) -> Result<Self> {
        if config.capacity == 0 {
            return Err(AIOSException::ConfigurationError(
                "Ring buffer capacity must be > 0".to_string(),
            ));
        }

        let layout = Layout::from_size_align(config.capacity, 4096).map_err(|_| {
            AIOSException::ConfigurationError("Invalid ring buffer layout".to_string())
        })?;

        let data = unsafe { alloc(layout) };
        if data.is_null() {
            return Err(AIOSException::IPCError(
                "Failed to allocate ring buffer memory".to_string(),
            ));
        }

        Ok(RingBuffer {
            data,
            state: Arc::new(RingBufferState {
                write_pos: AtomicUsize::new(0),
                read_pos: AtomicUsize::new(0),
                capacity: config.capacity,
            }),
            config,
        })
    }

    /// Get total capacity
    pub fn capacity(&self) -> usize {
        self.state.capacity
    }

    /// Check if zero-copy mode is enabled
    pub fn is_zero_copy(&self) -> bool {
        self.config.zero_copy
    }

    /// Get available space for writing
    pub fn available_write(&self) -> usize {
        let write = self.state.write_pos.load(Ordering::Acquire);
        let read = self.state.read_pos.load(Ordering::Acquire);

        if write >= read {
            self.capacity() - (write - read) - 1
        } else {
            read - write - 1
        }
    }

    /// Get available data for reading
    pub fn available_read(&self) -> usize {
        let write = self.state.write_pos.load(Ordering::Acquire);
        let read = self.state.read_pos.load(Ordering::Acquire);

        if write >= read {
            write - read
        } else {
            self.capacity() - (read - write)
        }
    }

    /// Write data to ring buffer (memcpy)
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        let available = self.available_write();
        if available < data.len() {
            return Err(AIOSException::IPCError(format!(
                "Ring buffer full: need {}, have {}",
                data.len(),
                available
            )));
        }

        let write_pos = self.state.write_pos.load(Ordering::Acquire);
        let mut written = 0;

        // First segment (from write_pos to end of buffer or wrap point)
        let first_seg = std::cmp::min(data.len(), self.capacity() - write_pos);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.data.add(write_pos), first_seg);
        }
        written += first_seg;

        // Second segment (if wrapped around)
        if written < data.len() {
            let remaining = data.len() - written;
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(written), self.data, remaining);
            }
            written += remaining;
        }

        let new_write_pos = (write_pos + written) % self.capacity();
        self.state.write_pos.store(new_write_pos, Ordering::Release);

        Ok(written)
    }

    /// Read data from ring buffer (memcpy)
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let available = self.available_read();
        let to_read = std::cmp::min(buf.len(), available);

        if to_read == 0 {
            return Ok(0);
        }

        let read_pos = self.state.read_pos.load(Ordering::Acquire);
        let mut read_bytes = 0;

        // First segment
        let first_seg = std::cmp::min(to_read, self.capacity() - read_pos);
        unsafe {
            std::ptr::copy_nonoverlapping(self.data.add(read_pos), buf.as_mut_ptr(), first_seg);
        }
        read_bytes += first_seg;

        // Second segment (if wrapped)
        if read_bytes < to_read {
            let remaining = to_read - read_bytes;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.data,
                    buf.as_mut_ptr().add(read_bytes),
                    remaining,
                );
            }
            read_bytes += remaining;
        }

        let new_read_pos = (read_pos + read_bytes) % self.capacity();
        self.state.read_pos.store(new_read_pos, Ordering::Release);

        Ok(read_bytes)
    }

    /// Get read pointer for zero-copy access
    pub fn read_ptr(&self) -> (*const u8, usize) {
        let read_pos = self.state.read_pos.load(Ordering::Acquire);
        let write_pos = self.state.write_pos.load(Ordering::Acquire);

        let continuous = if write_pos > read_pos {
            write_pos - read_pos
        } else if write_pos < read_pos {
            self.capacity() - read_pos
        } else {
            0
        };

        unsafe { (self.data.add(read_pos), continuous) }
    }

    /// Get write pointer for zero-copy access
    pub fn write_ptr(&self) -> (*mut u8, usize) {
        let write_pos = self.state.write_pos.load(Ordering::Acquire);
        let read_pos = self.state.read_pos.load(Ordering::Acquire);

        let available = if write_pos >= read_pos {
            self.capacity() - write_pos + read_pos - 1
        } else {
            read_pos - write_pos - 1
        };

        let continuous = if write_pos >= read_pos {
            self.capacity() - write_pos
        } else {
            read_pos - write_pos - 1
        };

        let cap = std::cmp::min(available, continuous);
        unsafe { (self.data.add(write_pos), cap) }
    }

    /// Advance read position (after zero-copy read)
    pub fn advance_read(&self, count: usize) -> Result<()> {
        let available = self.available_read();
        if count > available {
            return Err(AIOSException::IPCError(
                "Cannot advance read past available data".to_string(),
            ));
        }

        let read_pos = self.state.read_pos.load(Ordering::Acquire);
        let new_read_pos = (read_pos + count) % self.capacity();
        self.state.read_pos.store(new_read_pos, Ordering::Release);
        Ok(())
    }

    /// Advance write position (after zero-copy write)
    pub fn advance_write(&self, count: usize) -> Result<()> {
        let available = self.available_write();
        if count > available {
            return Err(AIOSException::IPCError(
                "Cannot advance write past available space".to_string(),
            ));
        }

        let write_pos = self.state.write_pos.load(Ordering::Acquire);
        let new_write_pos = (write_pos + count) % self.capacity();
        self.state.write_pos.store(new_write_pos, Ordering::Release);
        Ok(())
    }

    /// Clear the ring buffer (reset pointers)
    pub fn clear(&self) {
        self.state.write_pos.store(0, Ordering::Release);
        self.state.read_pos.store(0, Ordering::Release);
    }

    /// Get current fill ratio (0.0 to 1.0)
    pub fn fill_ratio(&self) -> f32 {
        let available = self.available_read();
        available as f32 / self.capacity() as f32
    }

    /// Create a shared reference for cross-thread use
    pub fn shared_state(&self) -> Arc<RingBufferState> {
        self.state.clone()
    }
}

impl Drop for RingBuffer {
    fn drop(&mut self) {
        if !self.data.is_null() {
            let layout = Layout::from_size_align(self.state.capacity, 4096).unwrap();
            unsafe { dealloc(self.data, layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read_basic() {
        let config = RingBufferConfig {
            capacity: 1024,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        let data = b"Hello, Ring Buffer!";
        let written = rb.write(data).unwrap();
        assert_eq!(written, data.len());
        assert_eq!(rb.available_read(), data.len());

        let mut buf = vec![0u8; 32];
        let read = rb.read(&mut buf).unwrap();
        assert_eq!(read, data.len());
        assert_eq!(&buf[..read], data);
        assert_eq!(rb.available_read(), 0);
    }

    #[test]
    fn test_write_read_wraparound() {
        let config = RingBufferConfig {
            capacity: 64,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        // Fill most of buffer
        let data1 = vec![1u8; 50];
        rb.write(&data1).unwrap();

        let mut buf1 = vec![0u8; 50];
        rb.read(&mut buf1).unwrap();

        // Now write data that wraps around
        let data2 = vec![2u8; 40];
        rb.write(&data2).unwrap();

        let mut buf2 = vec![0u8; 40];
        let read = rb.read(&mut buf2).unwrap();
        assert_eq!(read, 40);
        assert!(buf2.iter().all(|&x| x == 2));
    }

    #[test]
    fn test_fill_ratio() {
        let config = RingBufferConfig {
            capacity: 1000,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        let data = vec![0u8; 500];
        rb.write(&data).unwrap();

        let ratio = rb.fill_ratio();
        assert!(ratio > 0.49 && ratio < 0.51, "Fill ratio: {}", ratio);
    }

    #[test]
    fn test_overflow() {
        let config = RingBufferConfig {
            capacity: 100,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        let data = vec![0u8; 150];
        let result = rb.write(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_copy_pointers() {
        let config = RingBufferConfig {
            capacity: 256,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        let (write_ptr, capacity) = rb.write_ptr();
        assert!(!write_ptr.is_null());
        assert_eq!(capacity, 255); // capacity - 1 for empty check

        rb.advance_write(50).unwrap();

        let (read_ptr, available) = rb.read_ptr();
        assert!(!read_ptr.is_null());
        assert_eq!(available, 50);
    }

    #[test]
    fn test_clear() {
        let config = RingBufferConfig {
            capacity: 1024,
            zero_copy: true,
        };
        let rb = RingBuffer::new(config).unwrap();

        let data = b"test";
        rb.write(data).unwrap();
        assert!(rb.available_read() > 0);

        rb.clear();
        assert_eq!(rb.available_read(), 0);
        assert_eq!(rb.available_write(), 1023);
    }
}
