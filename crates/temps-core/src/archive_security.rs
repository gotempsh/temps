// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cheap archive metadata checks that run before allocation-heavy decoders.

use std::io::{Read, Seek, SeekFrom};

pub const MAX_ARCHIVE_ENTRIES: u64 = 20_000;
pub const MAX_ZIP_CENTRAL_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, std::io::Error> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("truncated ZIP end record"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, std::io::Error> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated ZIP end record"))?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| invalid("truncated ZIP end record"))?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, std::io::Error> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("truncated ZIP64 end record"))?;
    Ok(u64::from_le_bytes(
        value
            .try_into()
            .map_err(|_| invalid("truncated ZIP64 end record"))?,
    ))
}

/// Reject ZIPs whose central directory can force excessive decoder allocation.
/// The ordinary or ZIP64 end record is read before constructing `ZipArchive`.
pub fn validate_zip_metadata<R: Read + Seek>(reader: &mut R) -> Result<(), std::io::Error> {
    let archive_len = reader.seek(SeekFrom::End(0))?;
    let tail_len = archive_len.min(65_557) as usize;
    reader.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0; tail_len];
    reader.read_exact(&mut tail)?;

    let eocd = tail
        .windows(4)
        .enumerate()
        .rev()
        .find_map(|(offset, window)| {
            if window != b"PK\x05\x06" || offset + 22 > tail.len() {
                return None;
            }
            let comment_len = u16_at(&tail, offset + 20).ok()? as usize;
            (offset + 22 + comment_len == tail.len()).then_some(offset)
        })
        .ok_or_else(|| invalid("ZIP end-of-central-directory record not found"))?;
    let disk = u16_at(&tail, eocd + 4)?;
    let central_disk = u16_at(&tail, eocd + 6)?;
    let entries_on_disk = u16_at(&tail, eocd + 8)?;
    let entries16 = u16_at(&tail, eocd + 10)?;
    let size32 = u32_at(&tail, eocd + 12)?;
    let offset32 = u32_at(&tail, eocd + 16)?;
    if disk != 0 || central_disk != 0 || entries_on_disk != entries16 {
        return Err(invalid("multi-disk ZIP archives are not supported"));
    }

    let eocd_absolute = archive_len - tail_len as u64 + eocd as u64;
    let (entries, central_size, central_offset, directory_end) =
        if entries16 == u16::MAX || size32 == u32::MAX || offset32 == u32::MAX {
            if eocd < 20 || tail.get(eocd - 20..eocd - 16) != Some(b"PK\x06\x07") {
                return Err(invalid("ZIP64 locator is missing"));
            }
            let zip64_offset = u64_at(&tail, eocd - 12)?;
            if zip64_offset > archive_len.saturating_sub(56) {
                return Err(invalid("ZIP64 end record points outside the archive"));
            }
            reader.seek(SeekFrom::Start(zip64_offset))?;
            let mut record = [0u8; 56];
            reader.read_exact(&mut record)?;
            if &record[..4] != b"PK\x06\x06" {
                return Err(invalid("ZIP64 end record is invalid"));
            }
            (
                u64_at(&record, 32)?,
                u64_at(&record, 40)?,
                u64_at(&record, 48)?,
                zip64_offset,
            )
        } else {
            (
                entries16 as u64,
                size32 as u64,
                offset32 as u64,
                eocd_absolute,
            )
        };

    if entries > MAX_ARCHIVE_ENTRIES {
        return Err(invalid(format!(
            "ZIP declares {entries} entries; maximum is {MAX_ARCHIVE_ENTRIES}"
        )));
    }
    if central_size > MAX_ZIP_CENTRAL_DIRECTORY_BYTES {
        return Err(invalid(format!(
            "ZIP central directory is {central_size} bytes; maximum is {MAX_ZIP_CENTRAL_DIRECTORY_BYTES}"
        )));
    }
    if central_offset
        .checked_add(central_size)
        .is_none_or(|end| end != directory_end)
    {
        return Err(invalid(
            "ZIP central directory does not end at its authoritative end record",
        ));
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn rejects_declared_entry_count_before_decoder_construction() {
        let mut bytes = vec![0u8; 22];
        bytes[..4].copy_from_slice(b"PK\x05\x06");
        bytes[8..10].copy_from_slice(&(MAX_ARCHIVE_ENTRIES as u16 + 1).to_le_bytes());
        bytes[10..12].copy_from_slice(&(MAX_ARCHIVE_ENTRIES as u16 + 1).to_le_bytes());
        let error = validate_zip_metadata(&mut Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("maximum"));
    }

    #[test]
    fn ignores_fake_end_record_signature_inside_comment() {
        let mut bytes = vec![0u8; 22 + 22];
        bytes[..4].copy_from_slice(b"PK\x05\x06");
        let too_many = MAX_ARCHIVE_ENTRIES as u16 + 1;
        bytes[8..10].copy_from_slice(&too_many.to_le_bytes());
        bytes[10..12].copy_from_slice(&too_many.to_le_bytes());
        bytes[20..22].copy_from_slice(&22u16.to_le_bytes());
        bytes[22..26].copy_from_slice(b"PK\x05\x06");
        let error = validate_zip_metadata(&mut Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("authoritative end record"));
    }
}
