//! AI KV-Cache & State Compression System
//!
//! Provides runtime memory quantization and compression for:
//! - FP8/INT4 quantization for idle AI Orchestrator buffers
//! - ZSTD compression for inactive system state tables
//! - Lazy decompression with LRU cache

pub mod cache;
pub mod compressor;
pub mod quantizer;

pub use cache::CompressionCache;
pub use compressor::StateCompressor;
pub use quantizer::Quantizer;

/// Compression strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMode {
    /// No compression
    None,
    /// ZSTD compression (default: level 3)
    Zstd(i32),
    /// FP8 quantization (AI buffers)
    FP8Quantize,
    /// INT4 quantization (memory-heavy state)
    INT4Quantize,
}

impl Default for CompressionMode {
    fn default() -> Self {
        CompressionMode::Zstd(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_mode_default() {
        let mode = CompressionMode::default();
        assert_eq!(mode, CompressionMode::Zstd(3));
    }
}
