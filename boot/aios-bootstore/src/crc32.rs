//! CRC-32/ISO-HDLC, the checksum used by every store structure.
//!
//! Implemented directly rather than depending on `crc32fast` so the crate stays
//! `no_std` and dependency-free. [`update`] exposes the running state so a
//! checksum can be computed over a buffer with a field treated as zero without
//! copying the buffer.

/// Initial running value of a CRC-32/ISO-HDLC computation.
pub const INIT: u32 = 0xFFFF_FFFF;

/// Feeds `data` into a running checksum.
///
/// Start with [`INIT`] and pass the result of [`finish`] when done.
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// Converts a running checksum into the published CRC-32 value.
pub fn finish(crc: u32) -> u32 {
    !crc
}

/// Computes the CRC-32/ISO-HDLC of `data` in one call.
pub fn ieee(data: &[u8]) -> u32 {
    finish(update(INIT, data))
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

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"aios boot store";
        let split = 5;
        let mut crc = update(INIT, &data[..split]);
        crc = update(crc, &data[split..]);
        assert_eq!(finish(crc), ieee(data));
    }
}
