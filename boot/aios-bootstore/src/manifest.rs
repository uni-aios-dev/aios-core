//! The store manifest: the named payloads of one generation, written twice.
//!
//! The manifest is a fixed-size, little-endian structure with a CRC32 over the
//! whole buffer (the checksum field itself counts as zero). Two copies live on
//! the ESP, `manifest.a` and `manifest.b`; the writer always replaces the copy
//! with the lower sequence number, so a crash leaves at least one readable
//! manifest. Readers pick the valid copy with the highest [`Manifest::sequence`].
//!
//! Entry names are module names relative to the AIOS root (`blocks/hello.wasm`);
//! the payload file is the last path component inside the slot directory.

use crate::crc32;
use crate::StoreError;

/// Magic value in [`Manifest::encode`]: ASCII `AIMS` little-endian.
pub const MANIFEST_MAGIC: u32 = 0x534D_4941;
/// Layout version of the manifest. Bump on any field change.
pub const FORMAT_VERSION: u16 = 1;
/// Maximum number of entries across both slots.
pub const MAX_ENTRIES: usize = 16;
/// Maximum length of an entry name, not counting a terminator.
pub const NAME_LEN: usize = 48;
/// Encoded size of one [`Entry`].
pub const ENTRY_BYTES: usize = NAME_LEN + 4 + 4 + 1 + 1 + 1 + 1;
/// Encoded size of the manifest header.
pub const MANIFEST_HEADER_BYTES: usize = 40;
/// Encoded size of a whole manifest, always written in full.
pub const MANIFEST_BYTES: usize = MANIFEST_HEADER_BYTES + MAX_ENTRIES * ENTRY_BYTES;

/// Offset of the manifest checksum inside the encoded buffer.
const CHECKSUM_OFFSET: usize = 32;

/// One payload described by the manifest.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Module name relative to the AIOS root, NUL padded (`blocks/hello.wasm`).
    pub name: [u8; NAME_LEN],
    /// Payload size in bytes.
    pub size: u32,
    /// CRC-32/ISO-HDLC of the payload.
    pub crc32: u32,
    /// [`aios_bootinfo::MODULE_CONFIG`]-style kind: 1 config, 2 block, 3 raw.
    pub kind: u8,
    /// Slot directory that holds the payload: 0 or 1.
    pub slot: u8,
    /// Reserved for future flags, always zero.
    pub flags: u8,
    /// Padding, always zero.
    pub reserved: u8,
}

impl Entry {
    /// An empty entry, used to initialise the fixed-size array.
    pub const EMPTY: Self = Self {
        name: [0; NAME_LEN],
        size: 0,
        crc32: 0,
        kind: 0,
        slot: 0,
        flags: 0,
        reserved: 0,
    };

    /// Builds an entry for `data` after checking the name, kind and slot.
    pub fn new(name: &str, kind: u8, slot: u8, data: &[u8]) -> Result<Self, StoreError> {
        let size = u32::try_from(data.len()).map_err(|_| StoreError::PayloadTooLarge)?;
        Self::from_parts(name, kind, slot, size, crc32::ieee(data))
    }

    /// Builds an entry from an already measured payload.
    pub fn from_parts(
        name: &str,
        kind: u8,
        slot: u8,
        size: u32,
        crc32: u32,
    ) -> Result<Self, StoreError> {
        if !valid_name(name) || slot >= crate::SLOT_COUNT {
            return Err(if slot >= crate::SLOT_COUNT {
                StoreError::InvalidSlot
            } else {
                StoreError::InvalidName
            });
        }
        let mut entry = Self {
            size,
            crc32,
            kind,
            slot,
            ..Self::EMPTY
        };
        entry.name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(entry)
    }

    /// The name bytes without the NUL padding.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(NAME_LEN);
        &self.name[..end]
    }

    /// The name if it is valid UTF-8.
    pub fn name_str(&self) -> Option<&str> {
        core::str::from_utf8(self.name_bytes()).ok()
    }

    /// The payload file name: the last component of [`Entry::name`].
    pub fn file_name(&self) -> Option<&str> {
        self.name_str()?.rsplit('/').next()
    }

    /// Whether `data` has the recorded size and checksum.
    pub fn matches(&self, data: &[u8]) -> bool {
        self.size == data.len() as u32 && self.crc32 == crc32::ieee(data)
    }

    /// The same entry pointing at `slot`.
    pub fn with_slot(self, slot: u8) -> Result<Self, StoreError> {
        if slot >= crate::SLOT_COUNT {
            return Err(StoreError::InvalidSlot);
        }
        Ok(Self { slot, ..self })
    }
}

/// The manifest of one store generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Commit counter: the valid copy with the highest value wins.
    pub sequence: u64,
    /// Generation counter, incremented on every commit.
    pub generation: u64,
    /// Slot the loader should boot: 0 or 1.
    pub active_slot: u8,
    /// Slot that can be booted as a rollback target.
    pub previous_slot: u8,
    /// Entries of both slots, used to plan a rollback without rereading files.
    pub entries: [Entry; MAX_ENTRIES],
    /// Number of valid entries in [`Manifest::entries`].
    pub entry_count: u16,
}

impl Manifest {
    /// An empty manifest for `sequence` and `active_slot`.
    pub fn new(sequence: u64, active_slot: u8, previous_slot: u8) -> Self {
        Self {
            sequence,
            generation: sequence,
            active_slot,
            previous_slot,
            entries: [Entry::EMPTY; MAX_ENTRIES],
            entry_count: 0,
        }
    }

    /// The valid prefix of [`Manifest::entries`].
    pub fn entries(&self) -> &[Entry] {
        &self.entries[..usize::from(self.entry_count).min(MAX_ENTRIES)]
    }

    /// The entries that live in `slot`.
    pub fn entries_for(&self, slot: u8) -> impl Iterator<Item = &Entry> {
        self.entries()
            .iter()
            .filter(move |entry| entry.slot == slot)
    }

    /// Number of entries that live in `slot`.
    pub fn entry_count_for(&self, slot: u8) -> usize {
        self.entries_for(slot).count()
    }

    /// Replaces the entries after validating names, slots and duplicates.
    pub fn set_entries(&mut self, entries: &[Entry]) -> Result<(), StoreError> {
        if entries.len() > MAX_ENTRIES {
            return Err(StoreError::TooManyEntries);
        }
        self.entries = [Entry::EMPTY; MAX_ENTRIES];
        self.entry_count = 0;
        for (index, entry) in entries.iter().enumerate() {
            let name = entry.name_str().ok_or(StoreError::InvalidName)?;
            if !valid_name(name) || entry.slot >= crate::SLOT_COUNT {
                return Err(StoreError::InvalidName);
            }
            if entries[..index].iter().any(|other| {
                other
                    .name_str()
                    .is_some_and(|other| other.eq_ignore_ascii_case(name))
            }) {
                return Err(StoreError::DuplicateName);
            }
            self.entries[index] = *entry;
            self.entry_count += 1;
        }
        Ok(())
    }

    /// Encodes the manifest into its fixed-size on-disk representation.
    pub fn encode(&self) -> [u8; MANIFEST_BYTES] {
        let mut out = [0u8; MANIFEST_BYTES];
        put_u32(&mut out, 0, MANIFEST_MAGIC);
        put_u16(&mut out, 4, FORMAT_VERSION);
        put_u16(&mut out, 6, MANIFEST_HEADER_BYTES as u16);
        put_u64(&mut out, 8, self.sequence);
        put_u64(&mut out, 16, self.generation);
        out[24] = self.active_slot;
        out[25] = self.previous_slot;
        out[26] = crate::SLOT_COUNT;
        out[27] = 0;
        put_u16(&mut out, 28, self.entry_count);
        for (index, entry) in self.entries().iter().enumerate() {
            let offset = MANIFEST_HEADER_BYTES + index * ENTRY_BYTES;
            out[offset..offset + NAME_LEN].copy_from_slice(&entry.name);
            put_u32(&mut out, offset + NAME_LEN, entry.size);
            put_u32(&mut out, offset + NAME_LEN + 4, entry.crc32);
            out[offset + NAME_LEN + 8] = entry.kind;
            out[offset + NAME_LEN + 9] = entry.slot;
            out[offset + NAME_LEN + 10] = entry.flags;
            out[offset + NAME_LEN + 11] = 0;
        }
        let checksum = checksum(&out);
        put_u32(&mut out, CHECKSUM_OFFSET, checksum);
        out
    }

    /// Decodes and validates an on-disk manifest.
    pub fn decode(data: &[u8]) -> Result<Self, StoreError> {
        if data.len() < MANIFEST_BYTES {
            return Err(StoreError::TooShort);
        }
        let data = &data[..MANIFEST_BYTES];
        if get_u32(data, 0) != MANIFEST_MAGIC {
            return Err(StoreError::BadMagic);
        }
        if get_u16(data, 4) != FORMAT_VERSION {
            return Err(StoreError::UnsupportedVersion);
        }
        if usize::from(get_u16(data, 6)) != MANIFEST_HEADER_BYTES {
            return Err(StoreError::BadLength);
        }
        if get_u32(data, CHECKSUM_OFFSET) != checksum(data) {
            return Err(StoreError::BadChecksum);
        }
        let slot_count = data[26];
        if slot_count != crate::SLOT_COUNT {
            return Err(StoreError::InvalidSlot);
        }
        let active_slot = data[24];
        let previous_slot = data[25];
        if active_slot >= slot_count || previous_slot >= slot_count {
            return Err(StoreError::InvalidSlot);
        }
        let entry_count = get_u16(data, 28);
        if usize::from(entry_count) > MAX_ENTRIES {
            return Err(StoreError::TooManyEntries);
        }

        let mut manifest = Self {
            sequence: get_u64(data, 8),
            generation: get_u64(data, 16),
            active_slot,
            previous_slot,
            entries: [Entry::EMPTY; MAX_ENTRIES],
            entry_count,
        };
        for index in 0..usize::from(entry_count) {
            let offset = MANIFEST_HEADER_BYTES + index * ENTRY_BYTES;
            let mut entry = Entry::EMPTY;
            entry.name.copy_from_slice(&data[offset..offset + NAME_LEN]);
            entry.size = get_u32(data, offset + NAME_LEN);
            entry.crc32 = get_u32(data, offset + NAME_LEN + 4);
            entry.kind = data[offset + NAME_LEN + 8];
            entry.slot = data[offset + NAME_LEN + 9];
            entry.flags = data[offset + NAME_LEN + 10];
            let name = entry.name_str().ok_or(StoreError::InvalidName)?;
            if !valid_name(name) || entry.slot >= slot_count {
                return Err(StoreError::InvalidName);
            }
            if manifest.entries[..index].iter().any(|other| {
                other
                    .name_str()
                    .is_some_and(|other| other.eq_ignore_ascii_case(name))
            }) {
                return Err(StoreError::DuplicateName);
            }
            manifest.entries[index] = entry;
        }
        Ok(manifest)
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new(0, 0, 0)
    }
}

/// Whether `name` is a safe relative module name.
pub(crate) fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() >= NAME_LEN || name.starts_with('/') {
        return false;
    }
    name.split('/').all(|component| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && !component.contains('\\')
            && !component.contains('\0')
    })
}

fn checksum(data: &[u8]) -> u32 {
    let mut crc = crc32::update(crc32::INIT, &data[..CHECKSUM_OFFSET]);
    crc = crc32::update(crc, &[0u8; 4]);
    crc = crc32::update(crc, &data[CHECKSUM_OFFSET + 4..MANIFEST_BYTES]);
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
    use crate::SLOT_COUNT;

    fn entry(name: &str, slot: u8, data: &[u8]) -> Entry {
        Entry::new(name, 2, slot, data).unwrap()
    }

    fn manifest() -> Manifest {
        let mut manifest = Manifest::new(7, 0, 1);
        manifest
            .set_entries(&[
                entry("blocks/hello.wasm", 0, b"hello"),
                entry("blocks/empty.wasm", 1, b"empty"),
            ])
            .unwrap();
        manifest
    }

    #[test]
    fn layout_is_fixed() {
        assert_eq!(ENTRY_BYTES, 60);
        assert_eq!(MANIFEST_HEADER_BYTES, 40);
        assert_eq!(MANIFEST_BYTES, 1000);
        assert_eq!(core::mem::size_of::<Entry>(), 60);
    }

    #[test]
    fn round_trip() {
        let original = manifest();
        let bytes = original.encode();
        let decoded = Manifest::decode(&bytes).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.entry_count_for(0), 1);
        assert_eq!(decoded.entry_count_for(1), 1);
        assert_eq!(
            decoded.entries_for(0).next().unwrap().name_str(),
            Some("blocks/hello.wasm")
        );
    }

    #[test]
    fn file_name_is_the_last_component() {
        let entry = entry("blocks/hello.wasm", 0, b"hello");
        assert_eq!(entry.file_name(), Some("hello.wasm"));
        assert!(entry.matches(b"hello"));
        assert!(!entry.matches(b"hellO"));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(Entry::new("", 2, 0, b"x").is_err());
        assert!(Entry::new("/blocks/hello.wasm", 2, 0, b"x").is_err());
        assert!(Entry::new("blocks/../hello.wasm", 2, 0, b"x").is_err());
        assert!(Entry::new("blocks\\hello.wasm", 2, 0, b"x").is_err());
        assert!(Entry::new(&"a".repeat(NAME_LEN), 2, 0, b"x").is_err());
        assert_eq!(SLOT_COUNT, 2);
    }

    #[test]
    fn rejects_duplicate_names_ignoring_case() {
        let mut manifest = Manifest::new(1, 0, 0);
        assert_eq!(
            manifest.set_entries(&[
                entry("blocks/a.wasm", 0, b"a"),
                entry("blocks/A.WASM", 1, b"b"),
            ]),
            Err(StoreError::DuplicateName)
        );
    }

    #[test]
    fn rejects_truncated_bad_magic_and_bad_checksum() {
        let bytes = manifest().encode();
        assert_eq!(
            Manifest::decode(&bytes[..MANIFEST_BYTES - 1]),
            Err(StoreError::TooShort)
        );

        let mut wrong_magic = bytes;
        wrong_magic[0] ^= 0xFF;
        assert_eq!(Manifest::decode(&wrong_magic), Err(StoreError::BadMagic));

        let mut wrong_checksum = bytes;
        wrong_checksum[MANIFEST_HEADER_BYTES] ^= 0xFF;
        assert_eq!(
            Manifest::decode(&wrong_checksum),
            Err(StoreError::BadChecksum)
        );

        let mut wrong_version = bytes;
        wrong_version[4] = 2;
        assert_eq!(
            Manifest::decode(&wrong_version),
            Err(StoreError::UnsupportedVersion)
        );
    }
}
