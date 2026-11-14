//! GUID Partition Table writer.
//!
//! Writes the protective MBR, the primary header and entry array and their
//! backup copies, following the UEFI 2.10 layout. Only what the AIOS image
//! needs is emitted: one EFI System Partition entry.

use std::fs::File;
use std::io;
use std::io::{Seek, SeekFrom, Write};

use crate::crc32;

/// Sector size assumed by the GPT.
pub const SECTOR_SIZE: u64 = 512;
/// Number of partition entries in the array.
pub const ENTRY_COUNT: u32 = 128;
/// Size of one partition entry.
pub const ENTRY_SIZE: u32 = 128;
/// First LBA usable for partitions (after the primary entry array).
pub const FIRST_USABLE_LBA: u64 = 34;
/// Partition type GUID for an EFI System Partition, in on-disk byte order.
pub const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// Disk GUID of every AIOS image, in on-disk byte order.
pub const DISK_GUID: [u8; 16] = [
    0x41, 0x49, 0x4F, 0x53, 0x44, 0x49, 0x53, 0x4B, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];
/// Partition GUID of the AIOS ESP, in on-disk byte order.
pub const ESP_PARTITION_GUID: [u8; 16] = [
    0x41, 0x49, 0x4F, 0x53, 0x45, 0x53, 0x50, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
];

/// Sectors occupied by one copy of the partition entry array.
const ENTRY_ARRAY_SECTORS: u64 = (ENTRY_COUNT as u64 * ENTRY_SIZE as u64) / SECTOR_SIZE;

/// Writes the GPT for a disk of `total_sectors` sectors with one ESP spanning
/// `[esp_first, esp_last]`.
pub fn write(file: &mut File, total_sectors: u64, esp_first: u64, esp_last: u64) -> io::Result<()> {
    let backup_header_lba = total_sectors
        .checked_sub(1)
        .ok_or_else(|| invalid("disk is too small for a GPT"))?;
    let backup_entries_lba = backup_header_lba
        .checked_sub(ENTRY_ARRAY_SECTORS)
        .ok_or_else(|| invalid("disk is too small for a GPT"))?;
    let last_usable_lba = backup_entries_lba - 1;
    if last_usable_lba < FIRST_USABLE_LBA {
        return Err(invalid("disk is too small for a GPT"));
    }
    if esp_first < FIRST_USABLE_LBA || esp_last < esp_first || esp_last > last_usable_lba {
        return Err(invalid("ESP range is outside the usable area"));
    }

    let entries = partition_entries(esp_first, esp_last);
    let entries_crc = crc32::ieee(&entries);
    let mbr = protective_mbr(total_sectors);
    let primary = header(1, backup_header_lba, 2, entries_crc, last_usable_lba);
    let backup = header(
        backup_header_lba,
        1,
        backup_entries_lba,
        entries_crc,
        last_usable_lba,
    );

    write_at(file, 0, &mbr)?;
    write_at(file, SECTOR_SIZE, &primary)?;
    write_at(file, 2 * SECTOR_SIZE, &entries)?;
    write_at(file, backup_entries_lba * SECTOR_SIZE, &entries)?;
    write_at(file, backup_header_lba * SECTOR_SIZE, &backup)
}

/// Builds the protective MBR in LBA 0.
pub fn protective_mbr(total_sectors: u64) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let entry = 446;
    sector[entry + 2] = 0x02;
    sector[entry + 4] = 0xEE;
    sector[entry + 5] = 0xFF;
    sector[entry + 6] = 0xFF;
    sector[entry + 7] = 0xFF;
    put_u32(&mut sector, entry + 8, 1);
    let size = (total_sectors - 1).min(u64::from(u32::MAX));
    put_u32(&mut sector, entry + 12, size as u32);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

/// Builds the 128-entry partition array with a single ESP entry.
pub fn partition_entries(esp_first: u64, esp_last: u64) -> Vec<u8> {
    let mut entries = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
    let entry = &mut entries[..ENTRY_SIZE as usize];
    entry[0..16].copy_from_slice(&ESP_TYPE_GUID);
    entry[16..32].copy_from_slice(&ESP_PARTITION_GUID);
    put_u64(entry, 32, esp_first);
    put_u64(entry, 40, esp_last);
    put_u64(entry, 48, 0);
    for (index, unit) in "AIOS ESP".encode_utf16().enumerate() {
        put_u16(entry, 56 + index * 2, unit);
    }
    entries
}

/// Builds a 92-byte GPT header padded to one sector, CRC included.
pub fn header(
    current_lba: u64,
    backup_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
    last_usable_lba: u64,
) -> [u8; 512] {
    let mut sector = [0u8; 512];
    sector[0..8].copy_from_slice(b"EFI PART");
    put_u32(&mut sector, 8, 0x0001_0000);
    put_u32(&mut sector, 12, 92);
    put_u32(&mut sector, 16, 0);
    put_u32(&mut sector, 20, 0);
    put_u64(&mut sector, 24, current_lba);
    put_u64(&mut sector, 32, backup_lba);
    put_u64(&mut sector, 40, FIRST_USABLE_LBA);
    put_u64(&mut sector, 48, last_usable_lba);
    sector[56..72].copy_from_slice(&DISK_GUID);
    put_u64(&mut sector, 72, entries_lba);
    put_u32(&mut sector, 80, ENTRY_COUNT);
    put_u32(&mut sector, 84, ENTRY_SIZE);
    put_u32(&mut sector, 88, entries_crc);
    let crc = crc32::ieee(&sector[..92]);
    put_u32(&mut sector, 16, crc);
    sector
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_at(file: &mut File, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: u64 = 2048 + 655_360 + 33;

    #[test]
    fn protective_mbr_marks_the_whole_disk() {
        let mbr = protective_mbr(TOTAL);
        assert_eq!(mbr[446 + 4], 0xEE);
        assert_eq!(
            u32::from_le_bytes(mbr[446 + 8..446 + 12].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(mbr[446 + 12..446 + 16].try_into().unwrap()),
            (TOTAL - 1) as u32
        );
        assert_eq!(mbr[510..512], [0x55, 0xAA]);
    }

    #[test]
    fn header_crc_verifies() {
        let entries = partition_entries(2048, 2048 + 655_360 - 1);
        let entries_crc = crc32::ieee(&entries);
        let header = header(1, TOTAL - 1, 2, entries_crc, TOTAL - 34);
        assert_eq!(&header[0..8], b"EFI PART");
        assert_eq!(u32::from_le_bytes(header[12..16].try_into().unwrap()), 92);
        assert_eq!(u64::from_le_bytes(header[24..32].try_into().unwrap()), 1);
        assert_eq!(
            u64::from_le_bytes(header[32..40].try_into().unwrap()),
            TOTAL - 1
        );
        assert_eq!(
            u32::from_le_bytes(header[88..92].try_into().unwrap()),
            entries_crc
        );
        let mut copy = header;
        copy[16..20].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(header[16..20].try_into().unwrap()),
            crc32::ieee(&copy[..92])
        );
    }

    #[test]
    fn partition_entry_describes_the_esp() {
        let entries = partition_entries(2048, 2048 + 655_360 - 1);
        assert_eq!(entries.len(), (ENTRY_COUNT * ENTRY_SIZE) as usize);
        assert_eq!(&entries[0..16], &ESP_TYPE_GUID);
        assert_eq!(
            u64::from_le_bytes(entries[32..40].try_into().unwrap()),
            2048
        );
        assert_eq!(
            u64::from_le_bytes(entries[40..48].try_into().unwrap()),
            2048 + 655_360 - 1
        );
        let name: Vec<u16> = entries[56..72]
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        assert_eq!(String::from_utf16(&name).unwrap(), "AIOS ESP");
        assert!(entries[ENTRY_SIZE as usize..].iter().all(|byte| *byte == 0));
    }
}
