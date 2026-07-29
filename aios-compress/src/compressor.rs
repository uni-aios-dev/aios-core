//! ZSTD-based state compression for inactive system state tables

use aios_core::error::{AIOSException, Result};
use std::io::Write;
use zstd::Encoder;

/// State compressor for system state tables
pub struct StateCompressor {
    /// Compression level (1-22, default 3)
    level: i32,
}

impl StateCompressor {
    /// Create compressor with default level (3)
    pub fn new() -> Self {
        StateCompressor { level: 3 }
    }

    /// Create compressor with custom level
    pub fn with_level(level: i32) -> Result<Self> {
        if !(1..=22).contains(&level) {
            return Err(AIOSException::ConfigurationError(
                "Compression level must be 1-22".to_string(),
            ));
        }
        Ok(StateCompressor { level })
    }

    /// Compress data using ZSTD
    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = Encoder::new(Vec::new(), self.level)
            .map_err(|e| AIOSException::SerializationError(format!("Compression error: {}", e)))?;

        encoder.write_all(data).map_err(|e| {
            AIOSException::SerializationError(format!("Compression write error: {}", e))
        })?;

        encoder.finish().map_err(|e| {
            AIOSException::SerializationError(format!("Compression finish error: {}", e))
        })
    }

    /// Decompress data using ZSTD
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        zstd::decode_all(data)
            .map_err(|e| AIOSException::SerializationError(format!("Decompression error: {}", e)))
    }

    /// Estimate compression ratio for given data
    pub fn estimate_ratio(&self, data: &[u8]) -> Result<f32> {
        let compressed = self.compress(data)?;
        Ok(data.len() as f32 / compressed.len() as f32)
    }

    /// Check if compression is worthwhile (ratio > threshold)
    pub fn should_compress(&self, data: &[u8], threshold: f32) -> Result<bool> {
        let ratio = self.estimate_ratio(data)?;
        Ok(ratio > threshold)
    }
}

impl Default for StateCompressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress() {
        let compressor = StateCompressor::new();
        let data = b"Hello, this is a test data that should be compressible".repeat(10);

        let compressed = compressor.compress(&data).unwrap();
        assert!(compressed.len() < data.len());

        let decompressed = compressor.decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_compression_levels() {
        let data = b"test data".repeat(100);

        let fast = StateCompressor::with_level(1).unwrap();
        let fast_compressed = fast.compress(&data).unwrap();

        let slow = StateCompressor::with_level(22).unwrap();
        let slow_compressed = slow.compress(&data).unwrap();

        // Higher levels should compress equally or better
        // (but not always strictly better for small data)
        assert!(
            slow_compressed.len() <= fast_compressed.len() + 10,
            "Compression levels mismatch: fast={}, slow={}",
            fast_compressed.len(),
            slow_compressed.len()
        );
    }

    #[test]
    fn test_invalid_level() {
        let result = StateCompressor::with_level(0);
        assert!(result.is_err());

        let result = StateCompressor::with_level(23);
        assert!(result.is_err());
    }

    #[test]
    fn test_compression_ratio() {
        let compressor = StateCompressor::new();
        let data = b"aaaa".repeat(100); // Highly compressible

        let ratio = compressor.estimate_ratio(&data).unwrap();
        assert!(ratio > 1.0); // Should have positive compression
    }

    #[test]
    fn test_should_compress() {
        let compressor = StateCompressor::new();
        let compressible = b"x".repeat(1000);
        let random = vec![rand::random::<u8>(); 100];

        let compressible_result = compressor.should_compress(&compressible, 1.1).unwrap();
        assert!(compressible_result);

        let random_result = compressor.should_compress(&random, 10.0).unwrap();
        assert!(!random_result);
    }
}
