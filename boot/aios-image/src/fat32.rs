//! Minimal FAT32 writer for the AIOS EFI System Partition.
//!
//! The ESP layout is fixed at build time, so this module only implements what
//! the image needs: format a volume, create directories, write files and flush
//! the FAT. Names are stored as VFAT long names with generated 8.3 aliases, so
//! module files keep their natural names (`empty.wasm`, `system.conf`).
//!
//! Everything goes through `Read + Write + Seek`, so the same code writes the
//! final disk image and an in-memory buffer in tests.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// Bytes per logical sector of the ESP.
pub const SECTOR_SIZE: u64 = 512;
/// Sectors per cluster: 4 KiB clusters, the usual ESP cluster size.
pub const SECTORS_PER_CLUSTER: u32 = 8;
/// Reserved sectors ahead of the first FAT.
pub const RESERVED_SECTORS: u32 = 32;
/// Number of FAT copies kept in sync.
pub const NUM_FATS: u32 = 2;
/// Cluster number of the root directory in FAT32.
pub const ROOT_CLUSTER: u32 = 2;
/// End-of-chain marker written into the last cluster of a chain.
const FAT_EOC: u32 = 0x0FFF_FFFF;
/// Directory attribute: subdirectory.
const ATTR_DIRECTORY: u8 = 0x10;
/// Directory attribute: regular file.
const ATTR_ARCHIVE: u8 = 0x20;
/// Directory attribute: long-file-name entry (`READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID`).
const ATTR_LFN: u8 = 0x0F;
/// Size of one FAT directory entry.
const DIR_ENTRY_SIZE: u64 = 32;
/// Fixed creation/modification date: 2026-01-01. FAT has no "unknown" date.
const FAT_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;

/// FAT32 volume geometry derived from the volume size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Sectors per cluster.
    pub sectors_per_cluster: u32,
    /// Reserved sectors before the FATs.
    pub reserved_sectors: u32,
    /// Number of FAT copies.
    pub num_fats: u32,
    /// Size of one FAT, in sectors.
    pub fat_sectors: u32,
    /// Number of data clusters (`cluster 2 ..= cluster_count + 1`).
    pub cluster_count: u32,
}

/// Computes the FAT size that can describe `total_sectors`, iterating until the
/// FAT is large enough for the cluster count it implies.
pub fn geometry(total_sectors: u64) -> io::Result<Geometry> {
    let mut fat_sectors = 1u32;
    let mut cluster_count = 0u32;
    for _ in 0..16 {
        let overhead = u64::from(RESERVED_SECTORS) + u64::from(NUM_FATS) * u64::from(fat_sectors);
        let data_sectors = total_sectors
            .checked_sub(overhead)
            .ok_or_else(|| invalid_input("FAT32 volume is too small"))?;
        cluster_count = u32::try_from(data_sectors / u64::from(SECTORS_PER_CLUSTER))
            .map_err(|_| invalid_input("FAT32 volume is too large"))?;
        let needed = u32::try_from(((u64::from(cluster_count) + 2) * 4).div_ceil(SECTOR_SIZE))
            .map_err(|_| invalid_input("FAT32 volume is too large"))?;
        if needed == fat_sectors {
            break;
        }
        fat_sectors = needed;
    }
    let geometry = Geometry {
        sectors_per_cluster: SECTORS_PER_CLUSTER,
        reserved_sectors: RESERVED_SECTORS,
        num_fats: NUM_FATS,
        fat_sectors,
        cluster_count,
    };
    let used = u64::from(geometry.reserved_sectors)
        + u64::from(geometry.num_fats) * u64::from(geometry.fat_sectors)
        + u64::from(geometry.cluster_count) * u64::from(geometry.sectors_per_cluster);
    if used > total_sectors || geometry.cluster_count == 0 {
        return Err(invalid_input("inconsistent FAT32 geometry"));
    }
    if u64::from(geometry.fat_sectors) * SECTOR_SIZE < (u64::from(geometry.cluster_count) + 2) * 4 {
        return Err(invalid_input("FAT is too small for the cluster count"));
    }
    Ok(geometry)
}

/// The exact 8.3 form of `name`, when the name already fits the format.
pub fn exact_short_name(name: &str) -> Option<[u8; 11]> {
    let upper = name.to_ascii_uppercase();
    let (base, extension) = match upper.rsplit_once('.') {
        Some((base, extension)) => (base, extension),
        None => (upper.as_str(), ""),
    };
    if base.is_empty() {
        return None;
    }
    build_short(base, extension)
}

/// The 8.3 alias stored next to a VFAT long name.
///
/// Names that already fit 8.3 keep their exact form. Anything else gets a
/// `~1` suffix, the same scheme Windows uses.
pub fn short_alias(name: &str) -> io::Result<[u8; 11]> {
    if let Some(exact) = exact_short_name(name) {
        return Ok(exact);
    }
    let upper = name.to_ascii_uppercase();
    let (base, extension) = match upper.rsplit_once('.') {
        Some((base, extension)) => (base, extension),
        None => (upper.as_str(), ""),
    };
    let mut base: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '~'))
        .take(6)
        .collect();
    if base.is_empty() {
        base.push_str("FILE");
    }
    let extension: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '~'))
        .take(3)
        .collect();
    build_short(&format!("{base}~1"), &extension)
        .ok_or_else(|| invalid_input("cannot derive an 8.3 alias"))
}

fn build_short(base: &str, extension: &str) -> Option<[u8; 11]> {
    if base.is_empty() || base.len() > 8 || extension.len() > 3 {
        return None;
    }
    let mut short = [b' '; 11];
    for (index, byte) in base.bytes().enumerate() {
        if !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'~') {
            return None;
        }
        short[index] = byte;
    }
    for (index, byte) in extension.bytes().enumerate() {
        if !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'~') {
            return None;
        }
        short[8 + index] = byte;
    }
    Some(short)
}

/// VFAT long-name entries for a single file, highest sequence number first.
///
/// Returns an empty vector when the name already fits 8.3.
pub fn long_name_entries(name: &str, short: &[u8; 11]) -> io::Result<Vec<u8>> {
    if exact_short_name(name).as_ref() == Some(short) {
        return Ok(Vec::new());
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.is_empty() || units.len() > 255 {
        return Err(invalid_input("long name must be 1..=255 UTF-16 units"));
    }
    let per_entry = 13usize;
    let count = (units.len() + 1).div_ceil(per_entry);
    let checksum = short_name_checksum(short);
    let mut entries = vec![0u8; count * DIR_ENTRY_SIZE as usize];

    for sequence in 0..count {
        let chunk = count - 1 - sequence;
        let entry = &mut entries[sequence * 32..(sequence + 1) * 32];
        let number = chunk + 1;
        entry[0] = (number as u8) | if chunk == 0 { 0x40 } else { 0 };
        entry[11] = ATTR_LFN;
        entry[13] = checksum;
        let slots = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (slot, &offset) in slots.iter().enumerate() {
            let index = chunk * per_entry + slot;
            let value = match index.cmp(&units.len()) {
                core::cmp::Ordering::Less => units[index],
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 0xFFFF,
            };
            entry[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    Ok(entries)
}

/// VFAT checksum of an 8.3 alias, stored in every long-name entry.
pub fn short_name_checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short {
        sum = (sum >> 1).wrapping_add((sum & 1) << 7).wrapping_add(byte);
    }
    sum
}

/// A FAT32 volume being written.
pub struct Fat32<W: Read + Write + Seek> {
    file: W,
    base: u64,
    total_sectors: u64,
    geometry: Geometry,
    fat: Vec<u32>,
    next_cluster: u32,
}

impl<W: Read + Write + Seek> Fat32<W> {
    /// Formats a FAT32 volume of `total_sectors` sectors at byte `base` in `file`.
    ///
    /// The volume must start on a sector boundary. Boot sector, FSInfo, their
    /// backups and both FATs are written; the data area is left untouched, so
    /// the backing file must already read back as zeroes there.
    pub fn format(mut file: W, base: u64, total_sectors: u64) -> io::Result<Self> {
        let geometry = geometry(total_sectors)?;
        if total_sectors > u64::from(u32::MAX) {
            return Err(invalid_input("FAT32 volume exceeds 2 TiB"));
        }
        let boot = boot_sector(&geometry, total_sectors, base / SECTOR_SIZE);
        write_at(&mut file, base, &boot)?;
        write_at(&mut file, base + SECTOR_SIZE, &fsinfo())?;
        write_at(&mut file, base + 6 * SECTOR_SIZE, &boot)?;
        write_at(&mut file, base + 7 * SECTOR_SIZE, &fsinfo())?;

        let fat_bytes = u64::from(geometry.fat_sectors) * SECTOR_SIZE;
        let fat_start = base + u64::from(geometry.reserved_sectors) * SECTOR_SIZE;
        let zeros = vec![0u8; 64 * 1024];
        for copy in 0..u64::from(geometry.num_fats) {
            let mut written = 0u64;
            while written < fat_bytes {
                let chunk = usize::try_from((fat_bytes - written).min(zeros.len() as u64))
                    .map_err(|_| invalid_input("FAT is too large"))?;
                write_at(
                    &mut file,
                    fat_start + copy * fat_bytes + written,
                    &zeros[..chunk],
                )?;
                written += chunk as u64;
            }
        }

        let mut fat = vec![0u32; geometry.cluster_count as usize + 2];
        fat[0] = 0x0FFF_FFF8;
        fat[1] = FAT_EOC;
        fat[ROOT_CLUSTER as usize] = FAT_EOC;

        Ok(Self {
            file,
            base,
            total_sectors,
            geometry,
            fat,
            next_cluster: ROOT_CLUSTER + 1,
        })
    }

    /// Creates a subdirectory `name` inside `parent` and returns its cluster.
    pub fn create_dir(&mut self, parent: u32, name: &str) -> io::Result<u32> {
        let cluster = self.allocate_chain(1)?;
        let offset = self.cluster_offset(cluster);
        let mut block = vec![0u8; self.cluster_bytes()];
        let dot = dot_entries(cluster, parent);
        block[..dot.len()].copy_from_slice(&dot);
        write_at(&mut self.file, offset, &block)?;
        let entries = self.directory_entries(name, ATTR_DIRECTORY, cluster, 0)?;
        self.add_entries(parent, &entries)?;
        Ok(cluster)
    }

    /// Writes a file `name` into `parent`.
    pub fn write_file(&mut self, parent: u32, name: &str, data: &[u8]) -> io::Result<()> {
        let per_cluster = self.cluster_bytes() as u64;
        let clusters = u32::try_from((data.len() as u64).div_ceil(per_cluster))
            .map_err(|_| invalid_input("file is too large for FAT32"))?;
        let first = self.allocate_chain(clusters)?;
        for index in 0..clusters as usize {
            let start = index * per_cluster as usize;
            let end = (start + per_cluster as usize).min(data.len());
            let offset = self.cluster_offset(first + index as u32);
            write_at(&mut self.file, offset, &data[start..end])?;
        }
        let size =
            u32::try_from(data.len()).map_err(|_| invalid_input("file is too large for FAT32"))?;
        let entries = self.directory_entries(name, ATTR_ARCHIVE, first, size)?;
        self.add_entries(parent, &entries)
    }

    /// Flushes both FAT copies and the backing file.
    pub fn finish(mut self) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(self.fat.len() * 4);
        for entry in &self.fat {
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        let fat_start = self.base + u64::from(self.geometry.reserved_sectors) * SECTOR_SIZE;
        let fat_bytes = u64::from(self.geometry.fat_sectors) * SECTOR_SIZE;
        for copy in 0..u64::from(self.geometry.num_fats) {
            write_at(&mut self.file, fat_start + copy * fat_bytes, &bytes)?;
        }
        self.file.flush()
    }

    /// Total size of the volume in bytes.
    pub fn volume_bytes(&self) -> u64 {
        self.total_sectors * SECTOR_SIZE
    }

    /// Bytes per cluster.
    pub fn cluster_bytes(&self) -> usize {
        SECTORS_PER_CLUSTER as usize * SECTOR_SIZE as usize
    }

    fn directory_entries(
        &self,
        name: &str,
        attributes: u8,
        cluster: u32,
        size: u32,
    ) -> io::Result<Vec<u8>> {
        let short = short_alias(name)?;
        let mut entries = long_name_entries(name, &short)?;
        entries.extend_from_slice(&dir_entry(short, attributes, cluster, size));
        Ok(entries)
    }

    fn allocate_chain(&mut self, clusters: u32) -> io::Result<u32> {
        if clusters == 0 {
            return Ok(0);
        }
        let first = self.next_cluster;
        let end = u64::from(first) + u64::from(clusters);
        if end > u64::from(self.geometry.cluster_count) + 2 {
            return Err(invalid_input("ESP is full"));
        }
        self.next_cluster = u32::try_from(end).map_err(|_| invalid_input("ESP is full"))?;
        for index in 0..clusters {
            let cluster = first + index;
            let next = if index + 1 == clusters {
                FAT_EOC
            } else {
                cluster + 1
            };
            self.fat[cluster as usize] = next;
        }
        Ok(first)
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        let sectors = u64::from(self.geometry.reserved_sectors)
            + u64::from(self.geometry.num_fats) * u64::from(self.geometry.fat_sectors)
            + u64::from(cluster - ROOT_CLUSTER) * u64::from(self.geometry.sectors_per_cluster);
        self.base + sectors * SECTOR_SIZE
    }

    fn add_entries(&mut self, parent: u32, entries: &[u8]) -> io::Result<()> {
        let needed = entries.len() / DIR_ENTRY_SIZE as usize;
        let per_cluster = self.cluster_bytes() / DIR_ENTRY_SIZE as usize;
        loop {
            let mut chain = Vec::new();
            let mut cluster = parent;
            loop {
                chain.push(cluster);
                let next = self.fat[cluster as usize];
                if next == FAT_EOC || next == 0 {
                    break;
                }
                cluster = next;
            }

            let mut run = 0usize;
            let mut run_start = (0usize, 0usize);
            let mut found = false;
            'scan: for (chain_index, &cluster) in chain.iter().enumerate() {
                let mut block = vec![0u8; self.cluster_bytes()];
                let offset = self.cluster_offset(cluster);
                read_at(&mut self.file, offset, &mut block)?;
                for slot in 0..per_cluster {
                    if block[slot * DIR_ENTRY_SIZE as usize] == 0 {
                        if run == 0 {
                            run_start = (chain_index, slot);
                        }
                        run += 1;
                        if run == needed {
                            found = true;
                            break 'scan;
                        }
                    } else {
                        run = 0;
                    }
                }
            }

            if !found {
                let fresh = self.allocate_chain(1)?;
                let last = *chain.last().expect("directory chain is never empty");
                self.fat[last as usize] = fresh;
                continue;
            }

            let (chain_index, slot) = run_start;
            let mut remaining = entries;
            for (index, &cluster) in chain.iter().enumerate().skip(chain_index) {
                let start_slot = if index == chain_index { slot } else { 0 };
                let slots = per_cluster - start_slot;
                let take = remaining.len().min(slots * DIR_ENTRY_SIZE as usize);
                let offset =
                    self.cluster_offset(cluster) + (start_slot * DIR_ENTRY_SIZE as usize) as u64;
                write_at(&mut self.file, offset, &remaining[..take])?;
                remaining = &remaining[take..];
                if remaining.is_empty() {
                    return Ok(());
                }
            }
            return Err(invalid_input("directory run does not fit its chain"));
        }
    }
}

/// Builds the 512-byte FAT32 boot sector (BPB) for `geometry`.
pub fn boot_sector(geometry: &Geometry, total_sectors: u64, hidden_sectors: u64) -> [u8; 512] {
    let mut sector = [0u8; 512];
    sector[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    sector[3..11].copy_from_slice(b"AIOSESP ");
    put_u16(&mut sector, 11, SECTOR_SIZE as u16);
    sector[13] = geometry.sectors_per_cluster as u8;
    put_u16(&mut sector, 14, geometry.reserved_sectors as u16);
    sector[16] = geometry.num_fats as u8;
    put_u16(&mut sector, 17, 0);
    put_u16(&mut sector, 19, 0);
    sector[21] = 0xF8;
    put_u16(&mut sector, 22, 0);
    put_u16(&mut sector, 24, 63);
    put_u16(&mut sector, 26, 255);
    put_u32(&mut sector, 28, hidden_sectors as u32);
    put_u32(&mut sector, 32, total_sectors as u32);
    put_u32(&mut sector, 36, geometry.fat_sectors);
    put_u16(&mut sector, 40, 0);
    put_u16(&mut sector, 42, 0);
    put_u32(&mut sector, 44, ROOT_CLUSTER);
    put_u16(&mut sector, 48, 1);
    put_u16(&mut sector, 50, 6);
    sector[64] = 0x80;
    sector[66] = 0x29;
    put_u32(&mut sector, 67, 0x4149_4F53);
    sector[71..82].copy_from_slice(b"AIOS ESP   ");
    sector[82..90].copy_from_slice(b"FAT32   ");
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

/// Builds the 512-byte FSInfo sector.
pub fn fsinfo() -> [u8; 512] {
    let mut sector = [0u8; 512];
    put_u32(&mut sector, 0, 0x4161_5252);
    put_u32(&mut sector, 484, 0x6141_7272);
    put_u32(&mut sector, 488, u32::MAX);
    put_u32(&mut sector, 492, u32::MAX);
    put_u32(&mut sector, 508, 0xAA55_0000);
    sector
}

fn dir_entry(name: [u8; 11], attributes: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut entry = [0u8; 32];
    entry[0..11].copy_from_slice(&name);
    entry[11] = attributes;
    put_u16(&mut entry, 14, 0);
    put_u16(&mut entry, 16, FAT_DATE);
    put_u16(&mut entry, 18, FAT_DATE);
    put_u16(&mut entry, 20, (cluster >> 16) as u16);
    put_u16(&mut entry, 22, 0);
    put_u16(&mut entry, 24, FAT_DATE);
    put_u16(&mut entry, 26, (cluster & 0xFFFF) as u16);
    put_u32(&mut entry, 28, size);
    entry
}

fn dot_entries(self_cluster: u32, parent_cluster: u32) -> [u8; 64] {
    let mut blocks = [0u8; 64];
    let mut current = [b' '; 11];
    current[0] = b'.';
    let mut parent = [b' '; 11];
    parent[0] = b'.';
    parent[1] = b'.';
    let parent_cluster = if parent_cluster == ROOT_CLUSTER {
        0
    } else {
        parent_cluster
    };
    blocks[0..32].copy_from_slice(&dir_entry(current, ATTR_DIRECTORY, self_cluster, 0));
    blocks[32..64].copy_from_slice(&dir_entry(parent, ATTR_DIRECTORY, parent_cluster, 0));
    blocks
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_at<W: Write + Seek>(file: &mut W, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

fn read_at<W: Read + Seek>(file: &mut W, offset: u64, data: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(data)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Decodes the long-name entries written by [`long_name_entries`].
    fn decode_lfn(entries: &[u8]) -> String {
        let mut units: Vec<u16> = Vec::new();
        let checksum = entries[13];
        for entry in entries.chunks(32).rev() {
            assert_eq!(entry[11], ATTR_LFN);
            assert_eq!(entry[13], checksum);
            for offset in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                units.push(u16::from_le_bytes([entry[offset], entry[offset + 1]]));
            }
        }
        while units.last() == Some(&0xFFFF) {
            units.pop();
        }
        if units.last() == Some(&0) {
            units.pop();
        }
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn eight_dot_three_names_are_kept() {
        assert_eq!(exact_short_name("KERNEL.ELF").unwrap(), *b"KERNEL  ELF");
        assert_eq!(exact_short_name("aios").unwrap(), *b"AIOS       ");
        assert_eq!(exact_short_name("boot.conf"), None);
        assert_eq!(exact_short_name("empty.wasm"), None);
        assert_eq!(exact_short_name("toolongname.txt"), None);
        assert_eq!(exact_short_name("bad name.txt"), None);
        assert_eq!(exact_short_name(""), None);
    }

    #[test]
    fn long_names_get_a_unique_alias() {
        assert_eq!(short_alias("empty.wasm").unwrap(), *b"EMPTY~1 WAS");
        assert_eq!(short_alias("system.conf").unwrap(), *b"SYSTEM~1CON");
        assert_eq!(short_alias("boot.conf").unwrap(), *b"BOOT~1  CON");
        assert_eq!(short_alias("KERNEL.ELF").unwrap(), *b"KERNEL  ELF");
    }

    #[test]
    fn long_name_entries_round_trip() {
        let short = short_alias("empty.wasm").unwrap();
        let entries = long_name_entries("empty.wasm", &short).unwrap();
        assert_eq!(entries.len(), 32);
        assert_eq!(decode_lfn(&entries), "empty.wasm");
        assert_eq!(entries[0] & 0x40, 0x40);
        assert_eq!(entries[13], short_name_checksum(&short));

        let short = short_alias("a-very-long-block-name.wasm").unwrap();
        let entries = long_name_entries("a-very-long-block-name.wasm", &short).unwrap();
        assert_eq!(entries.len(), 3 * 32);
        assert_eq!(decode_lfn(&entries), "a-very-long-block-name.wasm");
        assert_eq!(entries[0] & 0x1F, 3);
        assert_eq!(entries[32] & 0x1F, 2);
        assert_eq!(entries[64] & 0x5F, 0x41);
    }

    #[test]
    fn geometry_is_self_consistent() {
        let geometry = geometry(320 * 1024 * 1024 / SECTOR_SIZE).unwrap();
        assert_eq!(geometry.sectors_per_cluster, 8);
        assert!(geometry.cluster_count > 65_525);
        let used = u64::from(geometry.reserved_sectors)
            + u64::from(geometry.num_fats) * u64::from(geometry.fat_sectors)
            + u64::from(geometry.cluster_count) * u64::from(geometry.sectors_per_cluster);
        assert!(used <= 320 * 1024 * 1024 / SECTOR_SIZE);
        assert!(
            u64::from(geometry.fat_sectors) * SECTOR_SIZE
                >= (u64::from(geometry.cluster_count) + 2) * 4
        );
    }

    #[test]
    fn boot_sector_has_the_expected_fields() {
        let geometry = geometry(320 * 1024 * 1024 / SECTOR_SIZE).unwrap();
        let sector = boot_sector(&geometry, 655_360, 2048);
        assert_eq!(sector[0..3], [0xEB, 0x58, 0x90]);
        assert_eq!(u16::from_le_bytes([sector[11], sector[12]]), 512);
        assert_eq!(sector[13], 8);
        assert_eq!(u16::from_le_bytes([sector[14], sector[15]]), 32);
        assert_eq!(sector[16], 2);
        assert_eq!(
            u32::from_le_bytes(sector[32..36].try_into().unwrap()),
            655_360
        );
        assert_eq!(u32::from_le_bytes(sector[44..48].try_into().unwrap()), 2);
        assert_eq!(sector[510..512], [0x55, 0xAA]);
    }

    #[test]
    fn writes_directories_and_files() {
        let total_sectors = 8 * 1024 * 1024 / SECTOR_SIZE;
        let mut volume = vec![0u8; (total_sectors * SECTOR_SIZE) as usize];
        let mut fat = Fat32::format(Cursor::new(&mut volume), 0, total_sectors).unwrap();
        let efi = fat.create_dir(ROOT_CLUSTER, "efi").unwrap();
        let boot = fat.create_dir(efi, "boot").unwrap();
        fat.write_file(boot, "BOOTX64.EFI", b"loader").unwrap();
        fat.write_file(ROOT_CLUSTER, "hello.txt", b"hello").unwrap();
        let root = fat.cluster_offset(ROOT_CLUSTER) as usize;
        let hello = fat.cluster_offset(ROOT_CLUSTER + 4) as usize;
        fat.finish().unwrap();

        assert_eq!(&volume[root..root + 11], b"EFI        ");
        assert_eq!(&volume[root + 32..root + 43], b"HELLO   TXT");
        let size = u32::from_le_bytes(volume[root + 32 + 28..root + 32 + 32].try_into().unwrap());
        assert_eq!(size, 5);
        assert_eq!(&volume[hello..hello + 5], b"hello");
    }
}
