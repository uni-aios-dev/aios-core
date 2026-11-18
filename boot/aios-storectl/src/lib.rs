//! The AIOS store writer: the reference implementation of the commit protocol
//! on top of [`aios_bootstore`].
//!
//! The tool works on a tree that mirrors the ESP, so it can update a mounted
//! ESP directory, a staged asset tree or a test directory without knowing how
//! the bytes eventually reach the disk. The layout is the one the loader reads:
//!
//! ```text
//! <esp>/aios/store/manifest.a
//! <esp>/aios/store/manifest.b
//! <esp>/aios/store/slot0/header
//! <esp>/aios/store/slot0/<payload>
//! <esp>/aios/store/slot1/header
//! <esp>/aios/store/slot1/<payload>
//! ```
//!
//! # Why the order matters
//!
//! [`commit`] writes the payload files first, then the `Staging` header, the
//! `Active` header, the stale manifest copy and finally the `Retired` header of
//! the old slot. Until the manifest write the old generation is still the one
//! the loader picks, so every interruption before it is harmless. After it the
//! new generation is complete because its payload and both headers were already
//! on disk. [`Step`] names those checkpoints, and `fail_after` in [`commit`] /
//! [`rollback`] stops exactly there, which is how the crash-safety tests (and a
//! QEMU boot) simulate a power loss.
//!
//! # Verification
//!
//! [`verify`] repeats the loader's checks: newest manifest copy, slot header
//! bound to that manifest, and every payload's size and CRC32. It reports the
//! slot the loader would boot, or why no slot is usable.

use aios_bootstore::{
    crc32, Entry, Manifest, SlotHeader, SlotState, Store, StoreError, MANIFEST_FILES, SLOT_DIRS,
    SLOT_HEADER_FILE, STORE_DIR,
};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Directory of the store relative to the ESP root, with `/` separators.
pub const STORE_RELATIVE: &str = "aios/store";

/// Stage that produced the module kind, matching `aios_bootinfo`.
pub use aios_bootinfo::{MODULE_BLOCK, MODULE_CONFIG, MODULE_RAW};

/// Key/value filesystem the writer operates on, rooted at the ESP.
///
/// `path` is always relative to that root and uses `/` separators, e.g.
/// `aios/store/manifest.a`. The trait keeps the protocol testable in memory and
/// leaves room for a raw-partition implementation later.
pub trait StoreIo {
    /// Reads a whole file.
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Writes a whole file, creating parent directories as needed.
    fn write(&self, path: &str, data: &[u8]) -> io::Result<()>;
}

/// A [`StoreIo`] over a directory on the host filesystem.
#[derive(Debug, Clone)]
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    /// Opens `root` as the ESP root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The ESP root this store writes to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, relative: &str) -> PathBuf {
        let mut path = self.root.clone();
        for component in relative.split('/') {
            path.push(component);
        }
        path
    }
}

impl StoreIo for DirStore {
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.path(path))
    }

    fn write(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let path = self.path(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)
    }
}

/// One payload file of a new generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// Module name relative to the AIOS root, e.g. `blocks/new.wasm`.
    pub name: String,
    /// `MODULE_CONFIG`, `MODULE_BLOCK` or `MODULE_RAW`.
    pub kind: u8,
    /// File contents.
    pub data: Vec<u8>,
}

impl Payload {
    /// Builds a block payload.
    pub fn block(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            kind: MODULE_BLOCK as u8,
            data: data.into(),
        }
    }
}

/// A checkpoint of the commit sequence.
///
/// In `fail_after` terms the writer stops **after** the named write, which is
/// the state a power loss at that point leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Payload files copied into the target slot.
    Payloads,
    /// Target slot header written as `Staging`.
    StagingHeader,
    /// Target slot header rewritten as `Active`.
    ActiveHeader,
    /// New manifest written into the stale copy.
    Manifest,
    /// Old slot header rewritten as `Retired`.
    RetiredHeader,
}

impl Step {
    /// Every step, in commit order.
    pub const ALL: [Step; 5] = [
        Step::Payloads,
        Step::StagingHeader,
        Step::ActiveHeader,
        Step::Manifest,
        Step::RetiredHeader,
    ];

    /// Command-line spelling of the step.
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Payloads => "payloads",
            Step::StagingHeader => "staging",
            Step::ActiveHeader => "active",
            Step::Manifest => "manifest",
            Step::RetiredHeader => "retired",
        }
    }

    /// Parses a command-line spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Step::ALL
            .into_iter()
            .find(|step| step.as_str().eq_ignore_ascii_case(value))
    }
}

/// What a finished (or deliberately interrupted) commit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Sequence number of the new generation.
    pub sequence: u64,
    /// Slot that received the payloads.
    pub target_slot: u8,
    /// Manifest copy that was replaced (`0` = A, `1` = B).
    pub manifest_copy: u8,
    /// Module names written into [`Commit::target_slot`].
    pub entries: Vec<String>,
    /// Step after which writing stopped, or `None` for a complete commit.
    pub interrupted: Option<Step>,
}

/// Why the writer refused or failed.
#[derive(Debug)]
pub enum WriteError {
    /// A file could not be read or written.
    Io {
        /// Path relative to the ESP root.
        path: String,
        /// Underlying error.
        error: io::Error,
    },
    /// The store rejected the manifest, the header or an entry.
    Store(StoreError),
    /// There is no previous generation to roll back to.
    NoRollback,
    /// A commit needs at least one payload.
    EmptyPayloads,
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(formatter, "{path}: {error}"),
            Self::Store(error) => write!(formatter, "{error}"),
            Self::NoRollback => formatter.write_str("the manifest has no previous generation"),
            Self::EmptyPayloads => formatter.write_str("a commit needs at least one payload"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<StoreError> for WriteError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Result of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// Sequence of the selected manifest.
    pub sequence: u64,
    /// Manifest copy that was selected (`0` = A, `1` = B).
    pub copy: u8,
    /// Active slot named by the manifest.
    pub active_slot: u8,
    /// Rollback slot named by the manifest.
    pub previous_slot: u8,
    /// Slot the loader would boot, or `None` when every slot is unusable.
    pub boot_slot: Option<u8>,
    /// Number of verified payloads in [`Verification::boot_slot`].
    pub boot_entries: usize,
    /// One reason per rejected slot, in boot order.
    pub rejected: Vec<(u8, String)>,
}

/// The store directory relative to the ESP root.
pub fn store_dir() -> String {
    STORE_RELATIVE.to_string()
}

/// The store directory with the `/aios` prefix the loader logs.
pub fn store_posix() -> &'static str {
    STORE_DIR
}

fn store_file(name: &str) -> String {
    format!("{STORE_RELATIVE}/{name}")
}

fn slot_dir(slot: u8) -> String {
    format!("{STORE_RELATIVE}/{}", SLOT_DIRS[slot as usize])
}

fn slot_file(slot: u8, name: &str) -> String {
    format!("{}/{}", slot_dir(slot), name)
}

fn read_optional(io: &impl StoreIo, path: &str) -> Result<Option<Vec<u8>>, WriteError> {
    match io.read(path) {
        Ok(data) => Ok(Some(data)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(WriteError::Io {
            path: path.to_string(),
            error,
        }),
    }
}

fn write(io: &impl StoreIo, path: &str, data: &[u8]) -> Result<(), WriteError> {
    io.write(path, data).map_err(|error| WriteError::Io {
        path: path.to_string(),
        error,
    })
}

/// Reads and selects the store exactly like the loader does.
pub fn load(io: &impl StoreIo) -> Result<Store, WriteError> {
    let a = read_optional(io, &store_file(MANIFEST_FILES[0]))?.unwrap_or_default();
    let b = read_optional(io, &store_file(MANIFEST_FILES[1]))?.unwrap_or_default();
    Store::load(&a, &b).map_err(WriteError::Store)
}

/// Writes the initial generation into an empty tree.
///
/// Slot 0 receives every payload, slot 1 only gets an `Empty` header and both
/// manifest copies carry the same sequence. `aios-image` uses this through the
/// staged asset tree; a freshly installed ESP uses the same call.
pub fn init(io: &impl StoreIo, payloads: &[Payload], sequence: u64) -> Result<Commit, WriteError> {
    let mut entries = Vec::with_capacity(payloads.len());
    for payload in payloads {
        entries.push(Entry::new(&payload.name, payload.kind, 0, &payload.data)?);
    }
    let mut manifest = Manifest::new(sequence, 0, 1);
    manifest.set_entries(&entries)?;
    let bytes = manifest.encode();
    let crc = crc32::ieee(&bytes);

    for payload in payloads {
        let file = payload
            .name
            .rsplit('/')
            .next()
            .ok_or(WriteError::Store(StoreError::InvalidName))?;
        write(io, &slot_file(0, file), &payload.data)?;
    }
    write(
        io,
        &slot_file(0, SLOT_HEADER_FILE),
        &SlotHeader::new(
            SlotState::Active,
            manifest.sequence,
            crc,
            u16::try_from(entries.len()).unwrap_or(u16::MAX),
        )
        .encode(),
    )?;
    write(
        io,
        &slot_file(1, SLOT_HEADER_FILE),
        &SlotHeader::EMPTY.encode(),
    )?;
    write(io, &store_file(MANIFEST_FILES[0]), &bytes)?;
    write(io, &store_file(MANIFEST_FILES[1]), &bytes)?;
    Ok(Commit {
        sequence: manifest.sequence,
        target_slot: 0,
        manifest_copy: 0,
        entries: entries
            .iter()
            .filter_map(|entry| entry.name_str().map(str::to_string))
            .collect(),
        interrupted: None,
    })
}

/// Commits a new generation into the inactive slot.
///
/// `fail_after` stops the writer after the named step without touching anything
/// later, simulating a power loss. The returned [`Commit::interrupted`] then
/// names that step; the store on disk is whatever the loader would really see.
pub fn commit(
    io: &impl StoreIo,
    payloads: &[Payload],
    fail_after: Option<Step>,
) -> Result<Commit, WriteError> {
    if payloads.is_empty() {
        return Err(WriteError::EmptyPayloads);
    }
    let store = load(io)?;
    let mut entries = Vec::with_capacity(payloads.len());
    for payload in payloads {
        entries.push(Entry::new(&payload.name, payload.kind, 0, &payload.data)?);
    }
    let plan = store.plan_update(&entries)?;
    let target = plan.target_slot;

    for payload in payloads {
        let file = payload
            .name
            .rsplit('/')
            .next()
            .ok_or(WriteError::Store(StoreError::InvalidName))?;
        write(io, &slot_file(target, file), &payload.data)?;
    }
    if let Some(step) = reached(fail_after, Step::Payloads) {
        return Ok(done(&plan, Some(step)));
    }

    write(
        io,
        &slot_file(target, SLOT_HEADER_FILE),
        &plan.staging_header().encode(),
    )?;
    if let Some(step) = reached(fail_after, Step::StagingHeader) {
        return Ok(done(&plan, Some(step)));
    }

    write(
        io,
        &slot_file(target, SLOT_HEADER_FILE),
        &plan.active_header().encode(),
    )?;
    if let Some(step) = reached(fail_after, Step::ActiveHeader) {
        return Ok(done(&plan, Some(step)));
    }

    write(
        io,
        &store_file(MANIFEST_FILES[plan.manifest_copy as usize]),
        &plan.manifest_bytes,
    )?;
    if let Some(step) = reached(fail_after, Step::Manifest) {
        return Ok(done(&plan, Some(step)));
    }

    let previous = plan.manifest.previous_slot;
    write(
        io,
        &slot_file(previous, SLOT_HEADER_FILE),
        &plan.retired_header().encode(),
    )?;
    Ok(done(&plan, None))
}

/// Switches the manifest back to the previous slot without copying payloads.
///
/// The previous slot's header is rewritten as `Active`, the manifest is written
/// into the stale copy and the slot that was active becomes `Retired`. The
/// payload files are untouched, so a rollback needs no extra space.
pub fn rollback(io: &impl StoreIo, fail_after: Option<Step>) -> Result<Commit, WriteError> {
    let store = load(io)?;
    let plan = store.plan_rollback().ok_or(WriteError::NoRollback)?;
    let target = plan.target_slot;

    write(
        io,
        &slot_file(target, SLOT_HEADER_FILE),
        &plan.active_header().encode(),
    )?;
    if let Some(step) = reached(fail_after, Step::ActiveHeader) {
        return Ok(done(&plan, Some(step)));
    }

    write(
        io,
        &store_file(MANIFEST_FILES[plan.manifest_copy as usize]),
        &plan.manifest_bytes,
    )?;
    if let Some(step) = reached(fail_after, Step::Manifest) {
        return Ok(done(&plan, Some(step)));
    }

    write(
        io,
        &slot_file(plan.manifest.previous_slot, SLOT_HEADER_FILE),
        &plan.retired_header().encode(),
    )?;
    Ok(done(&plan, None))
}

fn reached(limit: Option<Step>, step: Step) -> Option<Step> {
    (limit == Some(step)).then_some(step)
}

fn done(plan: &aios_bootstore::UpdatePlan, interrupted: Option<Step>) -> Commit {
    Commit {
        sequence: plan.manifest.sequence,
        target_slot: plan.target_slot,
        manifest_copy: plan.manifest_copy,
        entries: plan
            .manifest
            .entries_for(plan.target_slot)
            .filter_map(|entry| entry.name_str().map(str::to_string))
            .collect(),
        interrupted,
    }
}

/// Repeats the loader's verification and reports the slot it would boot.
pub fn verify(io: &impl StoreIo) -> Result<Verification, WriteError> {
    let store = load(io)?;
    let manifest = store.manifest();
    let crc = store.manifest_crc();
    let order = store.boot_order();
    let mut rejected = Vec::new();

    for (index, slot) in order.iter().enumerate() {
        if index == 1 && order[1] == order[0] {
            continue;
        }
        match verify_slot(io, manifest, crc, *slot) {
            Ok(entries) => {
                return Ok(Verification {
                    sequence: manifest.sequence,
                    copy: store.copy(),
                    active_slot: store.active_slot(),
                    previous_slot: store.previous_slot(),
                    boot_slot: Some(*slot),
                    boot_entries: entries,
                    rejected,
                });
            }
            Err(reason) => rejected.push((*slot, reason)),
        }
    }

    Ok(Verification {
        sequence: manifest.sequence,
        copy: store.copy(),
        active_slot: store.active_slot(),
        previous_slot: store.previous_slot(),
        boot_slot: None,
        boot_entries: 0,
        rejected,
    })
}

fn verify_slot(
    io: &impl StoreIo,
    manifest: &Manifest,
    manifest_crc: u32,
    slot: u8,
) -> Result<usize, String> {
    let header_data = read_optional(io, &slot_file(slot, SLOT_HEADER_FILE))
        .map_err(|error| error.to_string())?
        .ok_or("slot header is unreadable")?;
    let header = SlotHeader::decode(&header_data).map_err(|error| error.to_string())?;
    if !header.matches(slot, manifest, manifest_crc) {
        return Err("slot header does not match the manifest".to_string());
    }

    let mut count = 0;
    for entry in manifest.entries_for(slot) {
        let file = entry.file_name().ok_or("entry has no file name")?;
        let data = read_optional(io, &slot_file(slot, file))
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("{file} is unreadable"))?;
        if !entry.matches(&data) {
            return Err(format!("{file} checksum mismatch"));
        }
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryStore {
        files: RefCell<BTreeMap<String, Vec<u8>>>,
    }

    impl StoreIo for MemoryStore {
        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            self.files
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path))
        }

        fn write(&self, path: &str, data: &[u8]) -> io::Result<()> {
            self.files
                .borrow_mut()
                .insert(path.to_string(), data.to_vec());
            Ok(())
        }
    }

    fn initial() -> MemoryStore {
        let store = MemoryStore::default();
        init(
            &store,
            &[Payload::block("blocks/old.wasm", b"old-bytes")],
            1,
        )
        .unwrap();
        store
    }

    fn block_bytes(store: &MemoryStore, path: &str) -> Vec<u8> {
        store.read(path).unwrap()
    }

    fn header(store: &MemoryStore, slot: u8) -> SlotHeader {
        SlotHeader::decode(&block_bytes(store, &slot_file(slot, SLOT_HEADER_FILE))).unwrap()
    }

    #[test]
    fn init_writes_both_copies_and_the_slot_headers() {
        let store = initial();
        let selected = load(&store).unwrap();
        assert_eq!(selected.manifest().sequence, 1);
        assert_eq!(selected.active_slot(), 0);
        assert_eq!(selected.previous_slot(), 1);
        assert_eq!(header(&store, 0).state, SlotState::Active);
        assert_eq!(header(&store, 1).state, SlotState::Empty);
        assert_eq!(block_bytes(&store, &slot_file(0, "old.wasm")), b"old-bytes");
    }

    #[test]
    fn commit_promotes_the_inactive_slot() {
        let store = initial();
        let commit = commit(
            &store,
            &[Payload::block("blocks/new.wasm", b"new-bytes")],
            None,
        )
        .unwrap();
        assert_eq!(commit.target_slot, 1);
        assert_eq!(commit.sequence, 2);
        assert_eq!(commit.manifest_copy, 0);
        assert_eq!(commit.interrupted, None);

        let verification = verify(&store).unwrap();
        assert_eq!(verification.sequence, 2);
        assert_eq!(verification.active_slot, 1);
        assert_eq!(verification.previous_slot, 0);
        assert_eq!(verification.boot_slot, Some(1));
        assert_eq!(verification.boot_entries, 1);
        assert_eq!(header(&store, 1).state, SlotState::Active);
        assert_eq!(header(&store, 0).state, SlotState::Retired);
        assert_eq!(block_bytes(&store, &slot_file(1, "new.wasm")), b"new-bytes");
    }

    #[test]
    fn crash_before_the_manifest_keeps_the_old_generation() {
        for step in [Step::Payloads, Step::StagingHeader, Step::ActiveHeader] {
            let store = initial();
            let commit = commit(
                &store,
                &[Payload::block("blocks/new.wasm", b"new")],
                Some(step),
            )
            .unwrap();
            assert_eq!(commit.interrupted, Some(step));

            let selected = load(&store).unwrap();
            assert_eq!(selected.manifest().sequence, 1, "after {step:?}");
            assert_eq!(selected.active_slot(), 0, "after {step:?}");
            let verification = verify(&store).unwrap();
            assert_eq!(verification.boot_slot, Some(0), "after {step:?}");
        }
    }

    #[test]
    fn crash_after_the_manifest_leaves_a_complete_new_generation() {
        let store = initial();
        commit(
            &store,
            &[Payload::block("blocks/new.wasm", b"new")],
            Some(Step::Manifest),
        )
        .unwrap();
        let verification = verify(&store).unwrap();
        assert_eq!(verification.sequence, 2);
        assert_eq!(verification.boot_slot, Some(1));
        // The retired header is the only missing piece; the new slot still boots.
        assert_eq!(header(&store, 0).state, SlotState::Active);
    }

    #[test]
    fn corrupt_new_payload_falls_back_to_the_retired_slot() {
        let store = initial();
        commit(&store, &[Payload::block("blocks/new.wasm", b"new")], None).unwrap();
        store.write(&slot_file(1, "new.wasm"), b"tampered").unwrap();

        let verification = verify(&store).unwrap();
        assert_eq!(verification.boot_slot, Some(0));
        assert_eq!(verification.rejected.len(), 1);
        assert_eq!(verification.rejected[0].0, 1);
        assert!(verification.rejected[0].1.contains("checksum"));
    }

    #[test]
    fn rollback_switches_back_without_copying_payloads() {
        let store = initial();
        commit(&store, &[Payload::block("blocks/new.wasm", b"new")], None).unwrap();
        let commit = rollback(&store, None).unwrap();
        assert_eq!(commit.sequence, 3);
        assert_eq!(commit.target_slot, 0);
        assert_eq!(commit.manifest_copy, 1);

        let verification = verify(&store).unwrap();
        assert_eq!(verification.active_slot, 0);
        assert_eq!(verification.boot_slot, Some(0));
        assert_eq!(header(&store, 0).state, SlotState::Active);
        assert_eq!(header(&store, 1).state, SlotState::Retired);
    }

    #[test]
    fn rollback_without_a_previous_generation_is_rejected() {
        let store = MemoryStore::default();
        init(&store, &[Payload::block("blocks/only.wasm", b"x")], 1).unwrap();
        // A fresh store has previous_slot = 1, but slot 1 has no entries.
        assert!(matches!(
            rollback(&store, None),
            Err(WriteError::NoRollback)
        ));
    }

    #[test]
    fn second_update_replaces_the_other_manifest_copy() {
        let store = initial();
        commit(&store, &[Payload::block("blocks/new.wasm", b"new")], None).unwrap();
        let commit = commit(
            &store,
            &[Payload::block("blocks/newer.wasm", b"newer")],
            None,
        )
        .unwrap();
        assert_eq!(commit.sequence, 3);
        assert_eq!(commit.target_slot, 0);
        assert_eq!(commit.manifest_copy, 1);

        let verification = verify(&store).unwrap();
        assert_eq!(verification.boot_slot, Some(0));
        assert_eq!(verification.sequence, 3);
    }

    #[test]
    fn rejects_empty_duplicate_and_invalid_payloads() {
        let store = initial();
        assert!(matches!(
            commit(&store, &[], None),
            Err(WriteError::EmptyPayloads)
        ));
        assert!(matches!(
            commit(
                &store,
                &[
                    Payload::block("blocks/a.wasm", b"a"),
                    Payload::block("blocks/A.wasm", b"b"),
                ],
                None
            ),
            Err(WriteError::Store(StoreError::DuplicateName))
        ));
        assert!(matches!(
            commit(&store, &[Payload::block("../escape.wasm", b"a")], None),
            Err(WriteError::Store(StoreError::InvalidName))
        ));
    }

    #[test]
    fn verify_requires_a_readable_manifest() {
        let store = MemoryStore::default();
        assert!(matches!(
            verify(&store),
            Err(WriteError::Store(StoreError::NoValidManifest))
        ));
    }

    #[test]
    fn steps_round_trip_through_their_names() {
        for step in Step::ALL {
            assert_eq!(Step::parse(step.as_str()), Some(step));
        }
        assert_eq!(Step::parse("nonsense"), None);
    }
}
