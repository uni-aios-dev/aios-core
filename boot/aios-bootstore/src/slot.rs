//! Slot headers: the per-slot commit record.
//!
//! Every slot directory (`slot0`, `slot1`) holds a fixed-size `header` file.
//! The header binds the payload files of that directory to one manifest
//! generation and carries the slot state, so a half-written update is never
//! booted:
//!
//! ```text
//! Empty   the slot has no payload
//! Staging payload is being written, must not boot
//! Active  the manifest may point here
//! Retired an older generation, kept as a rollback target
//! Bad     known broken, never boot
//! ```

use crate::crc32;
use crate::StoreError;

/// Magic value in [`SlotHeader::encode`]: ASCII `AISL` little-endian.
pub const SLOT_MAGIC: u32 = 0x4C53_4941;
/// Encoded size of a slot header, always written in full.
pub const SLOT_HEADER_BYTES: usize = 32;

/// Offset of the slot-header checksum inside the encoded buffer.
const CHECKSUM_OFFSET: usize = 24;

/// State of one A/B slot.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// The slot has no payload.
    Empty = 0,
    /// Payload is being written and must not be used.
    Staging = 1,
    /// The slot can be booted.
    Active = 2,
    /// An older generation, still bootable as a rollback target.
    Retired = 3,
    /// Known broken, never booted.
    Bad = 4,
}

impl SlotState {
    /// Decodes the on-disk state value.
    pub fn from_u16(value: u16) -> Option<Self> {
        Some(match value {
            0 => Self::Empty,
            1 => Self::Staging,
            2 => Self::Active,
            3 => Self::Retired,
            4 => Self::Bad,
            _ => return None,
        })
    }

    /// Whether a loader may boot the payload in this state.
    pub fn is_bootable(self) -> bool {
        matches!(self, Self::Active | Self::Retired)
    }
}

/// The commit record of one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotHeader {
    /// Current state of the slot.
    pub state: SlotState,
    /// Sequence number of the manifest this payload belongs to.
    pub sequence: u64,
    /// CRC32 of the encoded manifest that names this payload.
    pub manifest_crc: u32,
    /// Number of manifest entries that live in this slot.
    pub entry_count: u16,
}

impl SlotHeader {
    /// A header for an empty slot.
    pub const EMPTY: Self = Self {
        state: SlotState::Empty,
        sequence: 0,
        manifest_crc: 0,
        entry_count: 0,
    };

    /// Builds a header for `state` and a manifest generation.
    pub fn new(state: SlotState, sequence: u64, manifest_crc: u32, entry_count: u16) -> Self {
        Self {
            state,
            sequence,
            manifest_crc,
            entry_count,
        }
    }

    /// Whether this header describes the payload of `slot` in `manifest`.
    ///
    /// `manifest_crc` must be the checksum of [`crate::Manifest::encode`] for
    /// `manifest`, which binds the header to the exact bytes the loader read.
    pub fn matches(&self, slot: u8, manifest: &crate::Manifest, manifest_crc: u32) -> bool {
        self.state.is_bootable()
            && self.sequence == manifest.sequence
            && self.manifest_crc == manifest_crc
            && usize::from(self.entry_count) == manifest.entry_count_for(slot)
            && self.entry_count > 0
    }

    /// Encodes the header into its fixed-size on-disk representation.
    pub fn encode(&self) -> [u8; SLOT_HEADER_BYTES] {
        let mut out = [0u8; SLOT_HEADER_BYTES];
        put_u32(&mut out, 0, SLOT_MAGIC);
        put_u16(&mut out, 4, crate::FORMAT_VERSION);
        put_u16(&mut out, 6, SLOT_HEADER_BYTES as u16);
        put_u16(&mut out, 8, self.state as u16);
        put_u16(&mut out, 10, self.entry_count);
        put_u64(&mut out, 12, self.sequence);
        put_u32(&mut out, 20, self.manifest_crc);
        let checksum = checksum(&out);
        put_u32(&mut out, CHECKSUM_OFFSET, checksum);
        out
    }

    /// Decodes and validates an on-disk slot header.
    pub fn decode(data: &[u8]) -> Result<Self, StoreError> {
        if data.len() < SLOT_HEADER_BYTES {
            return Err(StoreError::TooShort);
        }
        let data = &data[..SLOT_HEADER_BYTES];
        if get_u32(data, 0) != SLOT_MAGIC {
            return Err(StoreError::BadMagic);
        }
        if get_u16(data, 4) != crate::FORMAT_VERSION {
            return Err(StoreError::UnsupportedVersion);
        }
        if usize::from(get_u16(data, 6)) != SLOT_HEADER_BYTES {
            return Err(StoreError::BadLength);
        }
        if get_u32(data, CHECKSUM_OFFSET) != checksum(data) {
            return Err(StoreError::BadChecksum);
        }
        let state = SlotState::from_u16(get_u16(data, 8)).ok_or(StoreError::BadState)?;
        Ok(Self {
            state,
            sequence: get_u64(data, 12),
            manifest_crc: get_u32(data, 20),
            entry_count: get_u16(data, 10),
        })
    }
}

impl Default for SlotHeader {
    fn default() -> Self {
        Self::EMPTY
    }
}

fn checksum(data: &[u8]) -> u32 {
    let mut crc = crc32::update(crc32::INIT, &data[..CHECKSUM_OFFSET]);
    crc = crc32::update(crc, &[0u8; 4]);
    crc = crc32::update(crc, &data[CHECKSUM_OFFSET + 4..SLOT_HEADER_BYTES]);
    crc32::finish(crc)
}

fn get_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn get_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn get_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Manifest;

    fn manifest() -> Manifest {
        let mut manifest = Manifest::new(5, 1, 0);
        manifest
            .set_entries(&[
                crate::Entry::new("blocks/a.wasm", 2, 0, b"a").unwrap(),
                crate::Entry::new("blocks/b.wasm", 2, 1, b"b").unwrap(),
                crate::Entry::new("blocks/c.wasm", 2, 1, b"c").unwrap(),
            ])
            .unwrap();
        manifest
    }

    #[test]
    fn header_is_fixed_size() {
        assert_eq!(SLOT_HEADER_BYTES, 32);
        assert_eq!(core::mem::size_of::<SlotHeader>(), 16);
    }

    #[test]
    fn round_trip() {
        let header = SlotHeader::new(SlotState::Active, 5, 0xDEAD_BEEF, 2);
        let bytes = header.encode();
        assert_eq!(SlotHeader::decode(&bytes), Ok(header));
    }

    #[test]
    fn rejects_corruption() {
        let bytes = SlotHeader::new(SlotState::Active, 5, 0xDEAD_BEEF, 2).encode();
        assert_eq!(
            SlotHeader::decode(&bytes[..SLOT_HEADER_BYTES - 1]),
            Err(StoreError::TooShort)
        );

        let mut bad_magic = bytes;
        bad_magic[0] ^= 0xFF;
        assert_eq!(SlotHeader::decode(&bad_magic), Err(StoreError::BadMagic));

        let mut bad_checksum = bytes;
        bad_checksum[12] ^= 0xFF;
        assert_eq!(
            SlotHeader::decode(&bad_checksum),
            Err(StoreError::BadChecksum)
        );

        let mut bad_state = bytes;
        put_u16(&mut bad_state, 8, 9);
        let checksum = checksum(&bad_state);
        put_u32(&mut bad_state, CHECKSUM_OFFSET, checksum);
        assert_eq!(SlotHeader::decode(&bad_state), Err(StoreError::BadState));
    }

    #[test]
    fn binds_to_manifest_generation() {
        let manifest = manifest();
        let crc = crate::crc32::ieee(&manifest.encode());
        let header = SlotHeader::new(SlotState::Active, manifest.sequence, crc, 2);
        assert!(header.matches(1, &manifest, crc));
        assert!(!header.matches(0, &manifest, crc));
        assert!(!header.matches(1, &manifest, crc ^ 1));
        assert!(
            !SlotHeader::new(SlotState::Staging, manifest.sequence, crc, 2)
                .matches(1, &manifest, crc)
        );
    }

    #[test]
    fn only_active_and_retired_boot() {
        assert!(!SlotState::Empty.is_bootable());
        assert!(!SlotState::Staging.is_bootable());
        assert!(SlotState::Active.is_bootable());
        assert!(SlotState::Retired.is_bootable());
        assert!(!SlotState::Bad.is_bootable());
    }
}
