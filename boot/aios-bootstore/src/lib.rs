//! The AIOS persistent boot store: a two-slot, crash-safe home for boot
//! payloads on the EFI System Partition.
//!
//! The store occupies `/aios/store`:
//!
//! ```text
//! /aios/store/manifest.a   manifest copy A
//! /aios/store/manifest.b   manifest copy B
//! /aios/store/slot0/header slot 0 commit record
//! /aios/store/slot0/...    payload files of slot 0
//! /aios/store/slot1/header slot 1 commit record
//! /aios/store/slot1/...    payload files of slot 1
//! ```
//!
//! A manifest lists the module name, kind, size and CRC32 of every payload and
//! binds it to a slot. Both the manifest and the slot headers carry their own
//! CRC32, and two manifest copies are kept so at least one survives an
//! interrupted write.
//!
//! # Commit protocol
//!
//! 1. Pick the slot that is not active and write its payload files there.
//! 2. Write `slotN/header` with [`SlotState::Staging`] and the CRC of the new
//!    manifest. The old manifest still names the old slot, so a crash here is
//!    harmless.
//! 3. Rewrite the header with [`SlotState::Active`].
//! 4. Write the new manifest into the copy with the **lower sequence number**
//!    ([`Store::stale_copy`]), naming the new slot as active and the old one as
//!    previous.
//! 5. Mark the old slot header [`SlotState::Retired`], carrying the **new**
//!    sequence and manifest CRC so the loader can still validate it.
//!
//! A crash before step 4 leaves the old manifest valid; a crash after step 4
//! leaves the new manifest valid. The loader tries [`Manifest::active_slot`]
//! first and falls back to [`Manifest::previous_slot`], which is the rollback
//! path; step 5 is what keeps that fallback verifiable.
//!
//! # Boundaries
//!
//! This crate is pure format and policy: it does not read files or touch a disk.
//! `aios-boot` implements the reader over the UEFI filesystem, `aios-image`
//! writes the initial generation, and a future on-disk updater implements
//! [`UpdatePlan`].

#![cfg_attr(not(test), no_std)]

pub mod crc32;
mod manifest;
mod slot;

pub use manifest::{
    Entry, Manifest, ENTRY_BYTES, FORMAT_VERSION, MANIFEST_BYTES, MANIFEST_HEADER_BYTES,
    MANIFEST_MAGIC, MAX_ENTRIES, NAME_LEN,
};
pub use slot::{SlotHeader, SlotState, SLOT_HEADER_BYTES, SLOT_MAGIC};

/// Number of A/B slots.
pub const SLOT_COUNT: u8 = 2;
/// Directory that holds the store, POSIX form.
pub const STORE_DIR: &str = "/aios/store";
/// The two manifest copy file names, in copy order.
pub const MANIFEST_FILES: [&str; 2] = ["manifest.a", "manifest.b"];
/// The two slot directory names, in slot order.
pub const SLOT_DIRS: [&str; 2] = ["slot0", "slot1"];
/// File name of a slot header inside its slot directory.
pub const SLOT_HEADER_FILE: &str = "header";

/// Why a store structure or plan was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The buffer is shorter than the fixed structure.
    TooShort,
    /// The magic value does not match.
    BadMagic,
    /// The layout version is not understood.
    UnsupportedVersion,
    /// The recorded header length is not the expected one.
    BadLength,
    /// The CRC32 does not match the contents.
    BadChecksum,
    /// A slot header carries an unknown state.
    BadState,
    /// There are more entries than [`MAX_ENTRIES`].
    TooManyEntries,
    /// An entry name is empty, absolute or escapes its directory.
    InvalidName,
    /// Two entries share a name, ignoring case.
    DuplicateName,
    /// A slot number is not 0 or 1.
    InvalidSlot,
    /// A payload does not fit in the 32-bit size field.
    PayloadTooLarge,
    /// The sequence counter cannot be incremented.
    CounterOverflow,
    /// Neither manifest copy is valid.
    NoValidManifest,
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::TooShort => "buffer is shorter than the fixed structure",
            Self::BadMagic => "magic does not match",
            Self::UnsupportedVersion => "unsupported layout version",
            Self::BadLength => "unexpected header length",
            Self::BadChecksum => "CRC32 mismatch",
            Self::BadState => "unknown slot state",
            Self::TooManyEntries => "too many manifest entries",
            Self::InvalidName => "invalid entry name",
            Self::DuplicateName => "duplicate entry name",
            Self::InvalidSlot => "invalid slot number",
            Self::PayloadTooLarge => "payload is larger than 4 GiB",
            Self::CounterOverflow => "sequence counter overflow",
            Self::NoValidManifest => "no valid manifest copy",
        };
        formatter.write_str(message)
    }
}

/// The readable store state: one validated manifest plus both copy sequences.
#[derive(Debug, Clone)]
pub struct Store {
    manifest: Manifest,
    copy: u8,
    copy_sequences: [Option<u64>; 2],
}

impl Store {
    /// Loads the newest valid manifest from the two copies.
    ///
    /// A copy is valid when it decodes and passes every check in
    /// [`Manifest::decode`]. When only one copy is valid it wins regardless of
    /// its sequence; when both are valid the higher sequence wins and copy A
    /// (`0`) wins a tie, which keeps the choice deterministic.
    pub fn load(copy_a: &[u8], copy_b: &[u8]) -> Result<Self, StoreError> {
        let decoded = [Manifest::decode(copy_a).ok(), Manifest::decode(copy_b).ok()];
        let copy_sequences = [
            decoded[0].as_ref().map(|m| m.sequence),
            decoded[1].as_ref().map(|m| m.sequence),
        ];
        let (copy, manifest) = match (decoded[0].clone(), decoded[1].clone()) {
            (Some(a), Some(b)) => {
                if b.sequence > a.sequence {
                    (1, b)
                } else {
                    (0, a)
                }
            }
            (Some(a), None) => (0, a),
            (None, Some(b)) => (1, b),
            (None, None) => return Err(StoreError::NoValidManifest),
        };
        Ok(Self {
            manifest,
            copy,
            copy_sequences,
        })
    }

    /// The selected manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Index of the manifest copy that was selected (`0` = A, `1` = B).
    pub fn copy(&self) -> u8 {
        self.copy
    }

    /// Sequence number stored in copy `index`, or `None` when it is invalid.
    pub fn copy_sequence(&self, index: usize) -> Option<u64> {
        self.copy_sequences.get(index).copied().flatten()
    }

    /// The slot the loader should try first.
    pub fn active_slot(&self) -> u8 {
        self.manifest.active_slot
    }

    /// The rollback target recorded in the manifest.
    pub fn previous_slot(&self) -> u8 {
        self.manifest.previous_slot
    }

    /// Slots in boot order: active first, then previous when it differs.
    pub fn boot_order(&self) -> [u8; 2] {
        [self.manifest.active_slot, self.manifest.previous_slot]
    }

    /// The manifest copy a writer must replace: the one with the lower sequence.
    ///
    /// Replacing only the stale copy means the newest valid manifest is never
    /// destroyed by an interrupted update.
    pub fn stale_copy(&self) -> u8 {
        match (self.copy_sequences[0], self.copy_sequences[1]) {
            (Some(a), Some(b)) => {
                if a <= b {
                    0
                } else {
                    1
                }
            }
            (None, _) => 0,
            (Some(_), None) => 1,
        }
    }

    /// CRC32 of the selected manifest bytes, used to bind a slot header.
    pub fn manifest_crc(&self) -> u32 {
        crc32::ieee(&self.manifest.encode())
    }

    /// Plans the commit of a new generation into the inactive slot.
    ///
    /// `entries` describe only the new payloads; their slots are replaced with
    /// the inactive slot. Entries of the current active slot stay in the
    /// manifest so [`Store::plan_rollback`] can point at them later.
    pub fn plan_update(&self, entries: &[Entry]) -> Result<UpdatePlan, StoreError> {
        let target = (0..SLOT_COUNT)
            .find(|slot| *slot != self.manifest.active_slot)
            .ok_or(StoreError::InvalidSlot)?;
        let sequence = self
            .manifest
            .sequence
            .checked_add(1)
            .ok_or(StoreError::CounterOverflow)?;

        let mut combined = [Entry::EMPTY; MAX_ENTRIES];
        let mut count = 0usize;
        for entry in self
            .manifest
            .entries()
            .iter()
            .filter(|entry| entry.slot != target)
        {
            if count == MAX_ENTRIES {
                return Err(StoreError::TooManyEntries);
            }
            combined[count] = *entry;
            count += 1;
        }
        for entry in entries {
            if count == MAX_ENTRIES {
                return Err(StoreError::TooManyEntries);
            }
            combined[count] = entry.with_slot(target)?;
            count += 1;
        }

        let mut manifest = Manifest::new(sequence, target, self.manifest.active_slot);
        manifest.generation = self
            .manifest
            .generation
            .checked_add(1)
            .ok_or(StoreError::CounterOverflow)?;
        manifest.set_entries(&combined[..count])?;
        let manifest_bytes = manifest.encode();
        Ok(UpdatePlan {
            target_slot: target,
            manifest,
            manifest_bytes,
            manifest_copy: self.stale_copy(),
        })
    }

    /// Plans a rollback to the previous slot without touching payload files.
    ///
    /// Returns `None` when the manifest carries no bootable previous generation.
    pub fn plan_rollback(&self) -> Option<UpdatePlan> {
        let target = self.manifest.previous_slot;
        if target == self.manifest.active_slot || self.manifest.entry_count_for(target) == 0 {
            return None;
        }
        let sequence = self.manifest.sequence.checked_add(1)?;
        let mut manifest = Manifest::new(sequence, target, self.manifest.active_slot);
        manifest.generation = self.manifest.generation.checked_add(1)?;
        manifest.set_entries(self.manifest.entries()).ok()?;
        let manifest_bytes = manifest.encode();
        Some(UpdatePlan {
            target_slot: target,
            manifest,
            manifest_bytes,
            manifest_copy: self.stale_copy(),
        })
    }
}

/// A commit that is ready to be written, produced by [`Store`].
///
/// Writing it is the caller's job: the crate does not know whether the bytes go
/// into a FAT file, a raw partition or an offline image.
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    /// Slot whose payload the new manifest names.
    pub target_slot: u8,
    /// Manifest to write.
    pub manifest: Manifest,
    /// Exact encoded bytes of [`UpdatePlan::manifest`].
    pub manifest_bytes: [u8; MANIFEST_BYTES],
    /// Manifest copy to replace: the older of the two.
    pub manifest_copy: u8,
}

impl UpdatePlan {
    /// The header to write while payload files are still being copied.
    pub fn staging_header(&self) -> SlotHeader {
        SlotHeader::new(
            SlotState::Staging,
            self.manifest.sequence,
            crc32::ieee(&self.manifest_bytes),
            u16::try_from(self.manifest.entry_count_for(self.target_slot)).unwrap_or(u16::MAX),
        )
    }

    /// The header to write once the payload files are complete.
    pub fn active_header(&self) -> SlotHeader {
        SlotHeader::new(
            SlotState::Active,
            self.manifest.sequence,
            crc32::ieee(&self.manifest_bytes),
            u16::try_from(self.manifest.entry_count_for(self.target_slot)).unwrap_or(u16::MAX),
        )
    }

    /// The header to write for the slot the plan moved away from.
    ///
    /// The retired header carries the **new** sequence and manifest CRC, not
    /// the old ones: the loader validates a slot against the manifest it just
    /// read, so this is what makes the previous slot a usable automatic
    /// fallback when the active slot fails verification.
    pub fn retired_header(&self) -> SlotHeader {
        SlotHeader::new(
            SlotState::Retired,
            self.manifest.sequence,
            crc32::ieee(&self.manifest_bytes),
            u16::try_from(self.manifest.entry_count_for(self.manifest.previous_slot))
                .unwrap_or(u16::MAX),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, slot: u8, data: &[u8]) -> Entry {
        Entry::new(name, 2, slot, data).unwrap()
    }

    fn manifest(sequence: u64, active: u8, previous: u8) -> Manifest {
        let mut manifest = Manifest::new(sequence, active, previous);
        manifest
            .set_entries(&[
                entry("blocks/old.wasm", previous, b"old"),
                entry("blocks/live.wasm", active, b"live"),
            ])
            .unwrap();
        manifest
    }

    fn store(sequence: u64) -> Store {
        let bytes = manifest(sequence, 0, 1).encode();
        Store::load(&bytes, &bytes).unwrap()
    }

    #[test]
    fn newest_copy_wins() {
        let old = manifest(3, 0, 1).encode();
        let new = manifest(4, 1, 0).encode();
        let store = Store::load(&new, &old).unwrap();
        assert_eq!(store.copy(), 0);
        assert_eq!(store.manifest().sequence, 4);
        assert_eq!(store.active_slot(), 1);
        assert_eq!(store.previous_slot(), 0);

        let store = Store::load(&old, &new).unwrap();
        assert_eq!(store.copy(), 1);
        assert_eq!(store.manifest().sequence, 4);
    }

    #[test]
    fn tie_prefers_copy_a() {
        let bytes = manifest(9, 0, 1).encode();
        let store = Store::load(&bytes, &bytes).unwrap();
        assert_eq!(store.copy(), 0);
    }

    #[test]
    fn corrupt_newest_falls_back_to_oldest() {
        let good = manifest(3, 0, 1).encode();
        let mut bad = manifest(4, 0, 1).encode();
        bad[100] ^= 0xFF;
        let store = Store::load(&good, &bad).unwrap();
        assert_eq!(store.copy(), 0);
        assert_eq!(store.copy_sequence(1), None);
        assert_eq!(store.manifest().sequence, 3);
    }

    #[test]
    fn both_copies_invalid_is_an_error() {
        assert!(matches!(
            Store::load(&[], &[]),
            Err(StoreError::NoValidManifest)
        ));
        assert!(matches!(
            Store::load(&[1, 2, 3], &[4, 5, 6]),
            Err(StoreError::NoValidManifest)
        ));
    }

    #[test]
    fn stale_copy_is_the_lower_sequence() {
        let seq2 = manifest(2, 0, 1).encode();
        let seq3 = manifest(3, 1, 0).encode();
        let store = Store::load(&seq3, &seq2).unwrap();
        assert_eq!(store.stale_copy(), 1);

        let store = Store::load(&seq2, &seq3).unwrap();
        assert_eq!(store.stale_copy(), 0);
    }

    #[test]
    fn boot_order_is_active_then_previous() {
        assert_eq!(store(5).boot_order(), [0, 1]);
    }

    #[test]
    fn update_targets_the_inactive_slot() {
        let store = store(5);
        let plan = store
            .plan_update(&[entry("blocks/new.wasm", 0, b"new")])
            .unwrap();
        assert_eq!(plan.target_slot, 1);
        assert_eq!(plan.manifest.sequence, 6);
        assert_eq!(plan.manifest.active_slot, 1);
        assert_eq!(plan.manifest.previous_slot, 0);
        assert_eq!(plan.manifest.entry_count_for(0), 1);
        assert_eq!(plan.manifest.entry_count_for(1), 1);
        assert_eq!(plan.manifest_copy, store.stale_copy());
        assert!(plan
            .active_header()
            .matches(1, &plan.manifest, crc32::ieee(&plan.manifest_bytes)));
        assert!(!plan.staging_header().matches(
            1,
            &plan.manifest,
            crc32::ieee(&plan.manifest_bytes)
        ));
    }

    #[test]
    fn update_keeps_the_previous_generation() {
        let store = store(5);
        let plan = store
            .plan_update(&[entry("blocks/new.wasm", 1, b"new")])
            .unwrap();
        let previous_names: Vec<_> = plan
            .manifest
            .entries_for(0)
            .filter_map(|entry| entry.name_str())
            .collect();
        assert_eq!(previous_names, ["blocks/live.wasm"]);
        assert_eq!(
            plan.manifest.entries_for(1).next().unwrap().name_str(),
            Some("blocks/new.wasm")
        );
    }

    #[test]
    fn rollback_swaps_active_and_previous() {
        let store = store(5);
        let plan = store.plan_rollback().unwrap();
        assert_eq!(plan.target_slot, 1);
        assert_eq!(plan.manifest.sequence, 6);
        assert_eq!(plan.manifest.active_slot, 1);
        assert_eq!(plan.manifest.previous_slot, 0);
        assert!(plan
            .active_header()
            .matches(1, &plan.manifest, crc32::ieee(&plan.manifest_bytes)));
    }

    #[test]
    fn plan_validates_entries_and_capacity() {
        let store = store(5);
        assert!(matches!(
            store.plan_update(&[
                entry("blocks/a.wasm", 0, b"a"),
                entry("blocks/A.wasm", 0, b"b")
            ]),
            Err(StoreError::DuplicateName)
        ));
        let many: Vec<Entry> = (0..MAX_ENTRIES)
            .map(|index| {
                let name = format!("blocks/{index}.wasm");
                Entry::new(&name, 2, 0, b"x").unwrap()
            })
            .collect();
        // One active-slot entry stays, so only MAX_ENTRIES - 1 new ones fit.
        assert!(matches!(
            store.plan_update(&many),
            Err(StoreError::TooManyEntries)
        ));
    }

    #[test]
    fn retired_header_keeps_the_previous_slot_bootable() {
        let store = store(5);
        let plan = store
            .plan_update(&[entry("blocks/new.wasm", 1, b"new")])
            .unwrap();
        let crc = crc32::ieee(&plan.manifest_bytes);
        let retired = plan.retired_header();
        assert_eq!(retired.sequence, plan.manifest.sequence);
        assert!(retired.matches(0, &plan.manifest, crc));
        assert_eq!(retired.entry_count, 1);
    }

    #[test]
    fn rollback_retires_the_slot_it_moved_away_from() {
        let store = store(5);
        let plan = store.plan_rollback().unwrap();
        let crc = crc32::ieee(&plan.manifest_bytes);
        assert_eq!(plan.manifest.active_slot, 1);
        assert_eq!(plan.manifest.previous_slot, 0);
        assert!(plan.retired_header().matches(0, &plan.manifest, crc));
        assert!(plan.active_header().matches(1, &plan.manifest, crc));
    }

    #[test]
    fn slot_zero_is_bootable_after_rollback() {
        let store = store(5);
        let plan = store.plan_rollback().unwrap();
        let header = plan.active_header();
        assert_eq!(header.state, SlotState::Active);
        assert!(header.state.is_bootable());
        assert_eq!(header.entry_count, 1);
    }
}
