//! Validation and writing of AIOS UEFI disk images.
//!
//! [`aios-image`](../aios-image) produces a raw GPT disk with one FAT32 EFI
//! System Partition. This crate checks that structure before a single byte is
//! written and copies the image verbatim to a raw file or to a physical disk.
//! The OS itself never needs this tool: it only prepares the disk on the build
//! or service machine, and after the copy the target is byte-identical to the
//! image already proven under QEMU/OVMF.
//!
//! # Safety model
//!
//! - [`inspect`] validates the protective MBR, both GPT headers, the ESP entry,
//!   the FAT32 signature and the exact image size before any write.
//! - [`plan`] refuses a target smaller than the image and refuses to use the
//!   image file itself as the target.
//! - [`execute`] writes in 1 MiB chunks and reads every chunk back through a
//!   separate handle, comparing the bytes and the running CRC32. It also
//!   re-inspects the target afterwards.
//! - The CLI is a dry run unless `--write --yes` is given; a physical disk
//!   always needs both flags. Unmount volumes on the target before writing —
//!   `std` has no volume-lock API, so the tool cannot do that for you.

use aios_bootstore::crc32;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bytes per logical sector of an AIOS image.
pub const SECTOR_SIZE: u64 = 512;
/// LBA of the ESP in every image built by `aios-image`.
pub const ESP_FIRST_LBA: u64 = 2048;
/// LBA of the primary GPT partition entry array.
pub const ENTRY_ARRAY_LBA: u64 = 2;
/// Number of GPT partition entries.
pub const ENTRY_COUNT: usize = 128;
/// Size of one GPT partition entry.
pub const ENTRY_SIZE: usize = 128;
/// Sectors occupied by one copy of the partition entry array.
pub const ENTRY_ARRAY_SECTORS: u64 = (ENTRY_COUNT * ENTRY_SIZE) as u64 / SECTOR_SIZE;

/// Partition type GUID of an EFI System Partition, in on-disk byte order.
const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// FAT32 signature at offset 82 of the ESP boot sector.
const FAT32_SIGNATURE: [u8; 8] = *b"FAT32   ";
/// GPT partition name written by `aios-image`.
const ESP_NAME: &str = "AIOS ESP";
/// Bytes copied and read back per step.
const CHUNK_SIZE: usize = 1024 * 1024;

/// Structural facts read from an image without modifying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// Exact image size in bytes, including the backup GPT.
    pub bytes: u64,
    /// LBA of the backup GPT header.
    pub backup_lba: u64,
    /// First LBA of the ESP.
    pub esp_first_lba: u64,
    /// Last LBA of the ESP.
    pub esp_last_lba: u64,
}

/// Whether a target path names a raw file or a physical disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A regular file, safe to use in tests and for file-backed disks.
    File,
    /// A Windows `\\.\PhysicalDriveN` path.
    PhysicalDevice,
}

/// A physical disk found by [`list_devices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Path to pass to `--device`.
    pub path: String,
    /// Device size reported by the OS in bytes, or zero when unavailable.
    pub bytes: u64,
    /// Whether sector 0 has the protective-MBR signature.
    pub protective_mbr: bool,
}

/// A validated, not yet executed installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Image to copy.
    pub image: PathBuf,
    /// Target file or physical disk.
    pub target: PathBuf,
    /// Structure read from the image.
    pub info: ImageInfo,
    /// Size of the target in bytes.
    pub target_bytes: u64,
    /// Kind of target resolved from its path.
    pub kind: TargetKind,
    /// CRC32 of the whole image, recomputed while reading back.
    pub image_crc: u32,
}

/// Result of a completed installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallReport {
    /// Bytes written and read back successfully.
    pub written: u64,
    /// CRC32 of both the source and the verified target prefix.
    pub crc: u32,
    /// Number of sectors re-inspected on the target after the copy.
    pub verified_sectors: u64,
}

/// Why a validation, plan or write failed.
#[derive(Debug)]
pub enum InstallError {
    /// A file or device could not be opened, read, written or flushed.
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        error: io::Error,
    },
    /// The source is not an AIOS GPT/ESP image.
    InvalidImage(String),
    /// The target cannot hold the image.
    TargetTooSmall {
        /// Target size in bytes.
        target: u64,
        /// Image size in bytes.
        image: u64,
    },
    /// Source and target resolve to the same file.
    SameFile(PathBuf),
    /// A chunk read back differed from the written bytes.
    VerifyMismatch {
        /// Byte offset of the first differing chunk.
        offset: u64,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::InvalidImage(reason) => write!(formatter, "not an AIOS image: {reason}"),
            Self::TargetTooSmall { target, image } => write!(
                formatter,
                "target is too small: {target} bytes, the image needs {image} bytes"
            ),
            Self::SameFile(path) => {
                write!(
                    formatter,
                    "{} is both the image and the target",
                    path.display()
                )
            }
            Self::VerifyMismatch { offset } => {
                write!(
                    formatter,
                    "readback differs from the image at byte {offset}"
                )
            }
        }
    }
}

impl std::error::Error for InstallError {}

/// Resolves the target kind from its path spelling.
pub fn target_kind(path: &Path) -> TargetKind {
    if path.to_string_lossy().starts_with(r"\\.\PhysicalDrive") {
        TargetKind::PhysicalDevice
    } else {
        TargetKind::File
    }
}

fn io_error(path: &Path, error: io::Error) -> InstallError {
    InstallError::Io {
        path: path.to_path_buf(),
        error,
    }
}

fn open_read(path: &Path) -> Result<File, InstallError> {
    File::open(path).map_err(|error| io_error(path, error))
}

fn open_read_write(path: &Path) -> Result<File, InstallError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(path, error))
}

fn sector_at<R: Read + Seek>(
    reader: &mut R,
    lba: u64,
) -> Result<[u8; SECTOR_SIZE as usize], InstallError> {
    reader
        .seek(SeekFrom::Start(lba * SECTOR_SIZE))
        .map_err(|error| io_error(Path::new("<image>"), error))?;
    let mut sector = [0u8; SECTOR_SIZE as usize];
    reader
        .read_exact(&mut sector)
        .map_err(|error| io_error(Path::new("<image>"), error))?;
    Ok(sector)
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
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

fn header_crc_ok(sector: &[u8; 512]) -> bool {
    if &sector[0..8] != b"EFI PART" || u32_at(sector, 12) < 92 {
        return false;
    }
    let length = u32_at(sector, 12) as usize;
    let mut copy = *sector;
    copy[16..20].copy_from_slice(&[0, 0, 0, 0]);
    u32_at(sector, 16) == crc32::ieee(&copy[..length])
}

fn esp_entry(entries: &[u8]) -> Option<(u64, u64)> {
    for entry in entries.as_chunks::<ENTRY_SIZE>().0 {
        if entry[0..16] != ESP_TYPE_GUID {
            continue;
        }
        let mut name = Vec::new();
        for index in 0..8 {
            let unit = u16_at(entry, 56 + index * 2);
            if unit == 0 {
                break;
            }
            name.push(unit);
        }
        if String::from_utf16_lossy(&name) == ESP_NAME {
            return Some((u64_at(entry, 32), u64_at(entry, 40)));
        }
    }
    None
}

/// Validates the GPT/ESP structure of `reader` without writing to it.
pub fn inspect<R: Read + Seek>(reader: &mut R) -> Result<ImageInfo, InstallError> {
    let bytes = reader
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error(Path::new("<image>"), error))?;
    if bytes < (ESP_FIRST_LBA + ENTRY_ARRAY_SECTORS + 1) * SECTOR_SIZE {
        return Err(InstallError::InvalidImage(
            "the image is too small for a GPT".into(),
        ));
    }
    if bytes % SECTOR_SIZE != 0 {
        return Err(InstallError::InvalidImage(
            "the image size is not sector-aligned".into(),
        ));
    }

    let mbr = sector_at(reader, 0)?;
    if mbr[510..512] != [0x55, 0xAA] || mbr[446 + 4] != 0xEE {
        return Err(InstallError::InvalidImage(
            "the protective MBR is missing".into(),
        ));
    }

    let primary = sector_at(reader, 1)?;
    if !header_crc_ok(&primary) || u64_at(&primary, 24) != 1 {
        return Err(InstallError::InvalidImage(
            "the primary GPT header is invalid".into(),
        ));
    }
    let backup_lba = u64_at(&primary, 32);
    let first_usable = u64_at(&primary, 40);
    let last_usable = u64_at(&primary, 48);
    let entries_lba = u64_at(&primary, 72);
    if first_usable != ENTRY_ARRAY_LBA + ENTRY_ARRAY_SECTORS
        || backup_lba <= last_usable
        || last_usable + ENTRY_ARRAY_SECTORS + 1 != backup_lba
        || entries_lba != ENTRY_ARRAY_LBA
        || u32_at(&primary, 80) as usize != ENTRY_COUNT
        || u32_at(&primary, 84) as usize != ENTRY_SIZE
        || (backup_lba + 1) * SECTOR_SIZE != bytes
    {
        return Err(InstallError::InvalidImage(
            "the GPT geometry is inconsistent".into(),
        ));
    }

    let mut entries = Vec::new();
    for lba in entries_lba..entries_lba + ENTRY_ARRAY_SECTORS {
        entries.extend_from_slice(&sector_at(reader, lba)?);
    }
    let (esp_first_lba, esp_last_lba) = esp_entry(&entries)
        .ok_or_else(|| InstallError::InvalidImage("the GPT has no AIOS ESP entry".into()))?;
    if esp_first_lba != ESP_FIRST_LBA || esp_last_lba <= esp_first_lba || esp_last_lba > last_usable
    {
        return Err(InstallError::InvalidImage(
            "the ESP entry has an invalid range".into(),
        ));
    }

    let boot = sector_at(reader, esp_first_lba)?;
    if boot[82..90] != FAT32_SIGNATURE {
        return Err(InstallError::InvalidImage("the ESP is not FAT32".into()));
    }

    let backup = sector_at(reader, backup_lba)?;
    if !header_crc_ok(&backup)
        || u64_at(&backup, 24) != backup_lba
        || u64_at(&backup, 32) != 1
        || u64_at(&backup, 72) != backup_lba - ENTRY_ARRAY_SECTORS
    {
        return Err(InstallError::InvalidImage(
            "the backup GPT header is invalid".into(),
        ));
    }

    Ok(ImageInfo {
        bytes,
        backup_lba,
        esp_first_lba,
        esp_last_lba,
    })
}

/// Validates the image at `path` and reads its structure.
pub fn inspect_image(path: &Path) -> Result<ImageInfo, InstallError> {
    let mut reader = open_read(path)?;
    inspect(&mut reader)
}

/// Validates the prefix of `path` occupied by an image, ignoring any spare
/// capacity after it (a physical disk is usually larger than the image).
fn inspect_prefix(path: &Path, size: u64) -> Result<ImageInfo, InstallError> {
    let mut reader = Prefix::new(open_read(path)?, size);
    inspect(&mut reader)
}

/// CRC32 of the whole file, read from the beginning.
pub fn fingerprint(path: &Path) -> Result<u32, InstallError> {
    fingerprint_prefix(path, u64::MAX)
}

fn fingerprint_prefix(path: &Path, size: u64) -> Result<u32, InstallError> {
    let mut reader = Prefix::new(open_read(path)?, size);
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(path, error))?;
    let mut crc = crc32::INIT;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| io_error(path, error))?;
        if count == 0 {
            break;
        }
        crc = crc32::update(crc, &buffer[..count]);
    }
    Ok(crc32::finish(crc))
}

/// A reader that ends at an explicit byte limit, so a larger target disk can
/// be validated using only the prefix occupied by the image.
struct Prefix<R> {
    inner: R,
    size: u64,
    position: u64,
}

impl<R> Prefix<R> {
    fn new(inner: R, size: u64) -> Self {
        Self {
            inner,
            size,
            position: 0,
        }
    }
}

impl<R: Read> Read for Prefix<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.size || buffer.is_empty() {
            return Ok(0);
        }
        let allowed = (self.size - self.position).min(buffer.len() as u64) as usize;
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.position += count as u64;
        Ok(count)
    }
}

impl<R: Seek> Seek for Prefix<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(value) => Some(value),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => self.size.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek outside the image"))?;
        if target > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek past the image",
            ));
        }
        self.position = self.inner.seek(SeekFrom::Start(target))?;
        Ok(self.position)
    }
}

/// Builds a plan without changing either side.
pub fn plan(image: &Path, target: &Path) -> Result<Plan, InstallError> {
    if let (Ok(source), Ok(destination)) = (image.canonicalize(), target.canonicalize()) {
        if source == destination {
            return Err(InstallError::SameFile(source));
        }
    }
    let info = inspect_image(image)?;
    let image_crc = fingerprint(image)?;
    let mut target_file = open_read(target)?;
    let target_bytes = target_file
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error(target, error))?;
    if target_bytes < info.bytes {
        return Err(InstallError::TargetTooSmall {
            target: target_bytes,
            image: info.bytes,
        });
    }
    Ok(Plan {
        image: image.to_path_buf(),
        target: target.to_path_buf(),
        info,
        target_bytes,
        kind: target_kind(target),
        image_crc,
    })
}

/// Writes a planned image and reads every byte back.
pub fn execute(
    plan: &Plan,
    mut progress: impl FnMut(u64, u64),
) -> Result<InstallReport, InstallError> {
    let mut source = open_read(&plan.image)?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&plan.image, error))?;
    let mut output = open_read_write(&plan.target)?;
    output
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&plan.target, error))?;
    let mut reader = open_read(&plan.target)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&plan.target, error))?;

    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut back = vec![0u8; CHUNK_SIZE];
    let mut written = 0u64;
    let mut crc = crc32::INIT;
    while written < plan.info.bytes {
        let remaining = (plan.info.bytes - written) as usize;
        let want = remaining.min(CHUNK_SIZE);
        let count = source
            .read(&mut buffer[..want])
            .map_err(|error| io_error(&plan.image, error))?;
        if count == 0 {
            return Err(InstallError::InvalidImage(
                "the image ended during the copy".into(),
            ));
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| io_error(&plan.target, error))?;
        reader
            .read_exact(&mut back[..count])
            .map_err(|error| io_error(&plan.target, error))?;
        if back[..count] != buffer[..count] {
            return Err(InstallError::VerifyMismatch { offset: written });
        }
        crc = crc32::update(crc, &buffer[..count]);
        written += count as u64;
        progress(written, plan.info.bytes);
    }
    output
        .flush()
        .map_err(|error| io_error(&plan.target, error))?;
    drop(output);

    let check = inspect_prefix(&plan.target, plan.info.bytes)?;
    if check != plan.info || fingerprint_prefix(&plan.target, plan.info.bytes)? != plan.image_crc {
        return Err(InstallError::VerifyMismatch { offset: 0 });
    }
    Ok(InstallReport {
        written,
        crc: crc32::finish(crc),
        verified_sectors: plan.info.bytes / SECTOR_SIZE,
    })
}

/// Re-checks a written target against a plan without writing to it.
pub fn verify(plan: &Plan) -> Result<InstallReport, InstallError> {
    let info = inspect_prefix(&plan.target, plan.info.bytes)?;
    let crc = fingerprint_prefix(&plan.target, plan.info.bytes)?;
    if info != plan.info || crc != plan.image_crc {
        return Err(InstallError::VerifyMismatch { offset: 0 });
    }
    Ok(InstallReport {
        written: plan.info.bytes,
        crc,
        verified_sectors: info.bytes / SECTOR_SIZE,
    })
}

/// Lists physical disks on Windows by probing `\\.\PhysicalDriveN`.
///
/// On other hosts the list is empty: file-backed images are used there. The
/// probe only opens devices for reading, so it never changes their contents.
pub fn list_devices() -> Vec<Device> {
    list_devices_impl()
}

#[cfg(windows)]
fn list_devices_impl() -> Vec<Device> {
    let mut devices = Vec::new();
    for number in 0..16 {
        let path = format!(r"\\.\PhysicalDrive{number}");
        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        let bytes = file.seek(SeekFrom::End(0)).unwrap_or(0);
        let protective_mbr = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| {
                let mut sector = [0u8; SECTOR_SIZE as usize];
                file.read_exact(&mut sector)?;
                Ok(sector[510..512] == [0x55, 0xAA] && sector[446 + 4] == 0xEE)
            })
            .unwrap_or(false);
        devices.push(Device {
            path,
            bytes,
            protective_mbr,
        });
    }
    devices
}

#[cfg(not(windows))]
fn list_devices_impl() -> Vec<Device> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    const TOTAL_SECTORS: u64 = ESP_FIRST_LBA + 64 + ENTRY_ARRAY_SECTORS + 1;

    fn temp_path(tag: &str) -> PathBuf {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "aios-install-{tag}-{}-{unique}.img",
            std::process::id()
        ))
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn gpt_header(current: u64, backup: u64, entries: u64, last_usable: u64) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];
        sector[..8].copy_from_slice(b"EFI PART");
        put_u32(&mut sector, 12, 92);
        put_u64(&mut sector, 24, current);
        put_u64(&mut sector, 32, backup);
        put_u64(&mut sector, 40, ENTRY_ARRAY_LBA + ENTRY_ARRAY_SECTORS);
        put_u64(&mut sector, 48, last_usable);
        put_u64(&mut sector, 72, entries);
        put_u32(&mut sector, 80, ENTRY_COUNT as u32);
        put_u32(&mut sector, 84, ENTRY_SIZE as u32);
        let crc = crc32::ieee(&sector[..92]);
        put_u32(&mut sector, 16, crc);
        sector
    }

    fn synthetic_image() -> Vec<u8> {
        let mut data = vec![0u8; (TOTAL_SECTORS * SECTOR_SIZE) as usize];
        data[446 + 4] = 0xEE;
        data[510] = 0x55;
        data[511] = 0xAA;
        let backup = TOTAL_SECTORS - 1;
        let last_usable = backup - ENTRY_ARRAY_SECTORS - 1;
        let header_lba = SECTOR_SIZE as usize;
        data[header_lba..header_lba + SECTOR_SIZE as usize].copy_from_slice(&gpt_header(
            1,
            backup,
            ENTRY_ARRAY_LBA,
            last_usable,
        ));
        let entries = ENTRY_ARRAY_LBA as usize * SECTOR_SIZE as usize;
        data[entries..entries + 16].copy_from_slice(&ESP_TYPE_GUID);
        put_u64(&mut data, entries + 32, ESP_FIRST_LBA);
        put_u64(&mut data, entries + 40, ESP_FIRST_LBA + 63);
        for (index, unit) in ESP_NAME.encode_utf16().enumerate() {
            data[entries + 56 + index * 2..entries + 58 + index * 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        let esp = ESP_FIRST_LBA as usize * SECTOR_SIZE as usize;
        data[esp + 82..esp + 90].copy_from_slice(&FAT32_SIGNATURE);
        let backup_offset = backup as usize * SECTOR_SIZE as usize;
        data[backup_offset..backup_offset + SECTOR_SIZE as usize].copy_from_slice(&gpt_header(
            backup,
            1,
            backup - ENTRY_ARRAY_SECTORS,
            last_usable,
        ));
        data
    }

    fn write_image(path: &Path, data: &[u8]) {
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn inspect_accepts_an_aios_image() {
        let path = temp_path("good");
        write_image(&path, &synthetic_image());
        let info = inspect_image(&path).unwrap();
        assert_eq!(info.bytes, TOTAL_SECTORS * SECTOR_SIZE);
        assert_eq!(info.backup_lba, TOTAL_SECTORS - 1);
        assert_eq!(info.esp_first_lba, ESP_FIRST_LBA);
        assert_eq!(info.esp_last_lba, ESP_FIRST_LBA + 63);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn inspect_rejects_broken_metadata() {
        let good = synthetic_image();
        let path = temp_path("bad");
        let mut cases = Vec::new();

        let mut bad_mbr = good.clone();
        bad_mbr[510] = 0;
        cases.push(bad_mbr);

        let mut bad_gpt = good.clone();
        bad_gpt[SECTOR_SIZE as usize] = b'X';
        cases.push(bad_gpt);

        let mut bad_crc = good.clone();
        bad_crc[SECTOR_SIZE as usize + 60] ^= 0xFF;
        cases.push(bad_crc);

        let mut bad_fat = good.clone();
        let esp = ESP_FIRST_LBA as usize * SECTOR_SIZE as usize;
        bad_fat[esp + 82] = b'X';
        cases.push(bad_fat);

        let mut bad_size = good;
        bad_size.truncate(bad_size.len() - SECTOR_SIZE as usize);
        cases.push(bad_size);

        for data in cases {
            write_image(&path, &data);
            assert!(inspect_image(&path).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn plan_checks_the_target() {
        let image = temp_path("source");
        let target = temp_path("target");
        let data = synthetic_image();
        write_image(&image, &data);
        write_image(&target, &vec![0u8; data.len() - SECTOR_SIZE as usize]);
        assert!(matches!(
            plan(&image, &target),
            Err(InstallError::TargetTooSmall { .. })
        ));
        assert!(matches!(
            plan(&image, &image),
            Err(InstallError::SameFile(_))
        ));
        std::fs::write(&target, vec![0u8; data.len() + 512]).unwrap();
        let planned = plan(&image, &target).unwrap();
        assert_eq!(planned.kind, TargetKind::File);
        assert_eq!(planned.image_crc, fingerprint(&image).unwrap());
        std::fs::remove_file(image).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn execute_copies_the_image_and_keeps_the_tail() {
        let image = temp_path("source");
        let target = temp_path("target");
        let data = synthetic_image();
        write_image(&image, &data);
        let target_data = vec![0x5Au8; data.len() + 512];
        write_image(&target, &target_data);
        let planned = plan(&image, &target).unwrap();

        let mut ticks = 0;
        let report = execute(&planned, |_, _| ticks += 1).unwrap();
        assert_eq!(report.written, data.len() as u64);
        assert_eq!(report.crc, planned.image_crc);
        assert_eq!(report.verified_sectors, TOTAL_SECTORS);
        assert!(ticks > 0);
        assert_eq!(std::fs::read(&target).unwrap()[..data.len()], data);
        assert_eq!(
            &std::fs::read(&target).unwrap()[data.len()..],
            &target_data[data.len()..]
        );
        assert_eq!(verify(&planned).unwrap(), report);
        std::fs::remove_file(image).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn verify_detects_a_tampered_target() {
        let image = temp_path("source");
        let target = temp_path("target");
        let data = synthetic_image();
        write_image(&image, &data);
        write_image(&target, &data);
        let planned = plan(&image, &target).unwrap();
        let mut target_data = std::fs::read(&target).unwrap();
        target_data[ESP_FIRST_LBA as usize * SECTOR_SIZE as usize + 100] ^= 0xFF;
        std::fs::write(&target, &target_data).unwrap();
        assert!(matches!(
            verify(&planned),
            Err(InstallError::VerifyMismatch { offset: 0 })
        ));
        std::fs::remove_file(image).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn target_kind_recognises_physical_disks() {
        assert_eq!(
            target_kind(Path::new(r"\\.\PhysicalDrive3")),
            TargetKind::PhysicalDevice
        );
        assert_eq!(target_kind(Path::new("disk.img")), TargetKind::File);
    }
}
