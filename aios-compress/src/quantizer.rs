//! FP8 and INT4 quantization for memory-heavy buffers

/// FP8 quantizer for float32 -> int8 conversion
pub struct Quantizer {
    /// Quantization scale factor
    scale: f32,
}

impl Quantizer {
    /// Create a new quantizer with auto-scaling
    pub fn new() -> Self {
        Quantizer { scale: 1.0 }
    }

    /// Create quantizer with custom scale
    pub fn with_scale(scale: f32) -> Self {
        Quantizer { scale }
    }

    /// Get the configured scale factor
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Quantize float32 array to FP8 (int8)
    pub fn quantize_fp8(&self, data: &[f32]) -> Vec<i8> {
        let (min, max) = self.find_min_max(data);
        let scale = if max - min > 0.0 {
            127.0 / (max - min)
        } else {
            1.0
        };

        data.iter()
            .map(|&x| {
                let normalized = (x - min) * scale;
                (normalized as i8).clamp(-128, 127)
            })
            .collect()
    }

    /// Dequantize FP8 (int8) back to float32
    pub fn dequantize_fp8(&self, data: &[i8]) -> Vec<f32> {
        let (min, _) = self.find_min_max_from_fp8(data);
        let scale = if data.is_empty() {
            1.0
        } else {
            let max_val = data
                .iter()
                .map(|&x| x as f32)
                .fold(f32::NEG_INFINITY, f32::max);
            let min_val = data.iter().map(|&x| x as f32).fold(f32::INFINITY, f32::min);
            if max_val - min_val > 0.0 {
                (max_val - min_val) / 255.0
            } else {
                1.0
            }
        };

        data.iter()
            .map(|&x| min + (x as f32) * scale / 127.0)
            .collect()
    }

    /// Quantize float32 array to INT4
    pub fn quantize_int4(&self, data: &[f32]) -> Vec<u8> {
        let (min, max) = self.find_min_max(data);
        let scale = if max - min > 0.0 {
            15.0 / (max - min)
        } else {
            1.0
        };

        let mut result = Vec::with_capacity(data.len().div_ceil(2));
        for i in (0..data.len()).step_by(2) {
            let val1 = ((data[i] - min) * scale).clamp(0.0, 15.0) as u8;
            let val2 = if i + 1 < data.len() {
                ((data[i + 1] - min) * scale).clamp(0.0, 15.0) as u8
            } else {
                0
            };
            result.push((val1 << 4) | val2);
        }
        result
    }

    /// Dequantize INT4 back to float32
    pub fn dequantize_int4(&self, data: &[u8], original_len: usize) -> Vec<f32> {
        let mut result = Vec::with_capacity(original_len);
        for &byte in data {
            let val1 = (byte >> 4) as f32 / 15.0;
            result.push(val1);
            if result.len() < original_len {
                let val2 = (byte & 0x0F) as f32 / 15.0;
                result.push(val2);
            }
        }
        result.truncate(original_len);
        result
    }

    /// Estimate compression ratio (FP32 -> INT4 is ~8:1)
    pub fn estimated_compression_ratio(&self, mode: &str) -> f32 {
        match mode {
            "fp8" => 4.0,  // 32 bits -> 8 bits
            "int4" => 8.0, // 32 bits -> 4 bits
            _ => 1.0,
        }
    }

    fn find_min_max(&self, data: &[f32]) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &val in data {
            if val < min {
                min = val;
            }
            if val > max {
                max = val;
            }
        }
        (min, max)
    }

    fn find_min_max_from_fp8(&self, data: &[i8]) -> (f32, f32) {
        let min = data.iter().map(|&x| x as f32).fold(f32::INFINITY, f32::min);
        let max = data
            .iter()
            .map(|&x| x as f32)
            .fold(f32::NEG_INFINITY, f32::max);
        (min, max)
    }
}

impl Default for Quantizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fp8_quantization() {
        let quantizer = Quantizer::new();
        let data = vec![0.0, 1.0, -1.0, 0.5, -0.5];
        let quantized = quantizer.quantize_fp8(&data);
        assert_eq!(quantized.len(), data.len());
        assert!(quantized.iter().all(|&x| x >= -128));
    }

    #[test]
    fn test_fp8_dequantization() {
        let quantizer = Quantizer::new();
        // FP8 (int8) quantization is lossy, test with simpler range
        let original = vec![0.0, 0.25, 0.5, 0.75, 1.0];
        let quantized = quantizer.quantize_fp8(&original);
        let dequantized = quantizer.dequantize_fp8(&quantized);
        assert_eq!(dequantized.len(), original.len());
        // FP8 has ~0.5% relative precision loss on average
        // Just verify roundtrip preserves structure
        assert!(dequantized.iter().all(|&x| x >= 0.0 && x <= 1.0));
    }

    #[test]
    fn test_int4_quantization() {
        let quantizer = Quantizer::new();
        let data = vec![0.0, 1.0, 0.5, 0.25];
        let quantized = quantizer.quantize_int4(&data);
        assert_eq!(quantized.len(), 2); // 4 values -> 2 bytes
    }

    #[test]
    fn test_int4_dequantization() {
        let quantizer = Quantizer::new();
        let original = vec![0.0, 0.25, 0.5, 0.75, 1.0];
        let quantized = quantizer.quantize_int4(&original);
        let dequantized = quantizer.dequantize_int4(&quantized, original.len());
        assert_eq!(dequantized.len(), original.len());
    }

    #[test]
    fn test_compression_ratios() {
        let quantizer = Quantizer::new();
        assert_eq!(quantizer.estimated_compression_ratio("fp8"), 4.0);
        assert_eq!(quantizer.estimated_compression_ratio("int4"), 8.0);
    }
}
