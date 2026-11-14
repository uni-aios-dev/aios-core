//! CRC-32/ISO-HDLC, the checksum used by both GPT headers and partition
//! entry arrays.
//!
//! Implemented directly instead of pulling in `crc32fast` so that the image
//! builder has no external dependencies at all.

/// Computes the CRC-32/ISO-HDLC (reflected, polynomial `0xEDB8_8320`) of `data`.
pub fn ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_standard_vectors() {
        assert_eq!(ieee(b""), 0x0000_0000);
        assert_eq!(ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            ieee(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }
}
