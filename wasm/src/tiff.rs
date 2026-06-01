use crate::{MetadataEntry, MetadataInfo};
use std::collections::BTreeSet;

const MAX_IFDS: usize = 64;
const MAX_IFD_ENTRIES: usize = 2048;

#[derive(Clone, Copy)]
struct Endian {
    big: bool,
}

#[derive(Clone, Debug)]
struct IfdEntry {
    tag: u16,
    format: u16,
    count: u32,
    offset: usize,
    value_range: std::ops::Range<usize>,
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    let endian = endian(data)?;
    if read_u16(data, 2, endian)? != 42 {
        return Err("Not a classic TIFF file".to_string());
    }
    let ifd_offset = read_u32(data, 4, endian)? as usize;
    parse_ifd(data, ifd_offset, endian).map(|_| ())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;
    let Ok(endian) = endian(data) else {
        return empty();
    };
    let Ok(mut ifd_offset) = read_u32(data, 4, endian).map(|value| value as usize) else {
        return empty();
    };
    let mut visited = BTreeSet::new();

    for _ in 0..MAX_IFDS {
        if ifd_offset == 0 || !visited.insert(ifd_offset) {
            break;
        }

        let Ok((ifd_entries, next_ifd)) = parse_ifd(data, ifd_offset, endian) else {
            break;
        };

        collect_ifd_metadata(
            data,
            &ifd_entries,
            endian,
            &mut entries,
            &mut total_bytes,
            &mut visited,
        );
        ifd_offset = next_ifd;
    }

    MetadataInfo {
        file_type: "tiff".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    validate(data)?;
    let endian = endian(data)?;
    let mut out = data.to_vec();
    let mut ifd_offset = read_u32(data, 4, endian)? as usize;
    let mut visited = BTreeSet::new();

    for _ in 0..MAX_IFDS {
        if ifd_offset == 0 || !visited.insert(ifd_offset) {
            break;
        }
        let (entries, next_ifd) = parse_ifd(data, ifd_offset, endian)?;
        rewrite_ifd_without_metadata(data, &mut out, ifd_offset, &entries, next_ifd, endian)?;
        ifd_offset = next_ifd;
    }

    Ok(out)
}

fn empty() -> MetadataInfo {
    MetadataInfo {
        file_type: "tiff".to_string(),
        metadata_found: vec![],
        total_metadata_bytes: 0,
    }
}

fn collect_ifd_metadata(
    data: &[u8],
    entries: &[IfdEntry],
    endian: Endian,
    metadata: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
    visited: &mut BTreeSet<usize>,
) {
    for entry in entries {
        if let Some(label) = metadata_tag_label(entry.tag) {
            *total_bytes += entry
                .value_range
                .end
                .saturating_sub(entry.value_range.start);
            let value = display_value(data, entry, endian).unwrap_or_else(|| {
                format!(
                    "{} bytes",
                    entry
                        .value_range
                        .end
                        .saturating_sub(entry.value_range.start)
                )
            });
            metadata.push(MetadataEntry {
                category: metadata_category(entry.tag).to_string(),
                name: label.to_string(),
                value,
            });
        }

        if matches!(entry.tag, 0x8769 | 0x8825 | 0xA005) && entry.format == 4 && entry.count == 1 {
            let Ok(sub_ifd_offset) = read_u32(data, entry.offset + 8, endian).map(|v| v as usize)
            else {
                continue;
            };
            collect_sub_ifd_metadata(data, sub_ifd_offset, endian, metadata, total_bytes, visited);
        }
    }
}

fn collect_sub_ifd_metadata(
    data: &[u8],
    ifd_offset: usize,
    endian: Endian,
    metadata: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
    visited: &mut BTreeSet<usize>,
) {
    if ifd_offset == 0 || visited.len() >= MAX_IFDS || !visited.insert(ifd_offset) {
        return;
    }

    let Ok((entries, next_ifd)) = parse_ifd(data, ifd_offset, endian) else {
        return;
    };
    collect_ifd_metadata(data, &entries, endian, metadata, total_bytes, visited);
    collect_sub_ifd_metadata(data, next_ifd, endian, metadata, total_bytes, visited);
}

fn rewrite_ifd_without_metadata(
    source: &[u8],
    out: &mut [u8],
    ifd_offset: usize,
    entries: &[IfdEntry],
    next_ifd: usize,
    endian: Endian,
) -> Result<(), String> {
    let entry_count = entries.len();
    let old_dir_end = ifd_offset
        .checked_add(2)
        .and_then(|offset| {
            entry_count
                .checked_mul(12)
                .and_then(|size| offset.checked_add(size))
        })
        .and_then(|offset| offset.checked_add(4))
        .ok_or_else(|| "Invalid TIFF IFD".to_string())?;
    if old_dir_end > out.len() {
        return Err("Invalid TIFF IFD".to_string());
    }

    let kept: Vec<&IfdEntry> = entries
        .iter()
        .filter(|entry| !is_metadata_tag(entry.tag))
        .collect();

    for entry in entries {
        if is_metadata_tag(entry.tag) {
            zero_value(source, out, entry, endian);
            if matches!(entry.tag, 0x8769 | 0x8825 | 0xA005)
                && entry.format == 4
                && entry.count == 1
            {
                if let Ok(sub_ifd_offset) = read_u32(source, entry.offset + 8, endian) {
                    zero_ifd_tree(
                        source,
                        out,
                        sub_ifd_offset as usize,
                        endian,
                        &mut BTreeSet::new(),
                    );
                }
            }
        }
    }

    out[ifd_offset..old_dir_end].fill(0);
    write_u16(out, ifd_offset, kept.len() as u16, endian)?;
    let mut write_offset = ifd_offset + 2;
    for entry in kept {
        out[write_offset..write_offset + 12]
            .copy_from_slice(&source[entry.offset..entry.offset + 12]);
        write_offset += 12;
    }
    write_u32(out, write_offset, next_ifd as u32, endian)?;

    Ok(())
}

fn zero_ifd_tree(
    source: &[u8],
    out: &mut [u8],
    ifd_offset: usize,
    endian: Endian,
    visited: &mut BTreeSet<usize>,
) {
    if ifd_offset == 0 || !visited.insert(ifd_offset) {
        return;
    }
    let Ok((entries, next_ifd)) = parse_ifd(source, ifd_offset, endian) else {
        return;
    };
    let Some(dir_end) = ifd_offset
        .checked_add(2)
        .and_then(|offset| {
            entries
                .len()
                .checked_mul(12)
                .and_then(|size| offset.checked_add(size))
        })
        .and_then(|offset| offset.checked_add(4))
    else {
        return;
    };
    if dir_end <= out.len() {
        out[ifd_offset..dir_end].fill(0);
    }
    for entry in entries {
        zero_value(source, out, &entry, endian);
        if matches!(entry.tag, 0x8769 | 0x8825 | 0xA005) && entry.format == 4 && entry.count == 1 {
            if let Ok(sub_ifd_offset) = read_u32(source, entry.offset + 8, endian) {
                zero_ifd_tree(source, out, sub_ifd_offset as usize, endian, visited);
            }
        }
    }
    zero_ifd_tree(source, out, next_ifd, endian, visited);
}

fn zero_value(source: &[u8], out: &mut [u8], entry: &IfdEntry, endian: Endian) {
    if entry.value_range.end <= out.len() {
        out[entry.value_range.clone()].fill(0);
    }

    if entry.format == 4 && entry.count == 1 && matches!(entry.tag, 0x8769 | 0x8825 | 0xA005) {
        if let Ok(pointer) = read_u32(source, entry.offset + 8, endian) {
            let pointer = pointer as usize;
            if pointer < out.len() {
                let end = pointer.saturating_add(2).min(out.len());
                out[pointer..end].fill(0);
            }
        }
    }
}

fn parse_ifd(
    data: &[u8],
    ifd_offset: usize,
    endian: Endian,
) -> Result<(Vec<IfdEntry>, usize), String> {
    let entry_count = read_u16(data, ifd_offset, endian)? as usize;
    if entry_count > MAX_IFD_ENTRIES {
        return Err("TIFF IFD contains too many entries".to_string());
    }
    let entries_start = ifd_offset
        .checked_add(2)
        .ok_or_else(|| "Invalid TIFF IFD".to_string())?;
    let next_ifd_offset = entries_start
        .checked_add(
            entry_count
                .checked_mul(12)
                .ok_or_else(|| "Invalid TIFF IFD".to_string())?,
        )
        .ok_or_else(|| "Invalid TIFF IFD".to_string())?;
    if next_ifd_offset.saturating_add(4) > data.len() {
        return Err("Invalid TIFF IFD".to_string());
    }

    let mut entries = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let offset = entries_start + index * 12;
        let tag = read_u16(data, offset, endian)?;
        let format = read_u16(data, offset + 2, endian)?;
        let count = read_u32(data, offset + 4, endian)?;
        let value_range = value_range(data, offset, format, count, endian)?;
        entries.push(IfdEntry {
            tag,
            format,
            count,
            offset,
            value_range,
        });
    }

    let next_ifd = read_u32(data, next_ifd_offset, endian)? as usize;
    Ok((entries, next_ifd))
}

fn value_range(
    data: &[u8],
    entry_offset: usize,
    format: u16,
    count: u32,
    endian: Endian,
) -> Result<std::ops::Range<usize>, String> {
    let type_size =
        tiff_type_size(format).ok_or_else(|| "Unsupported TIFF field type".to_string())?;
    let data_size = (count as usize)
        .checked_mul(type_size)
        .ok_or_else(|| "Invalid TIFF field size".to_string())?;
    if data_size <= 4 {
        let start = entry_offset + 8;
        return Ok(start..start + data_size);
    }

    let start = read_u32(data, entry_offset + 8, endian)? as usize;
    let end = start
        .checked_add(data_size)
        .ok_or_else(|| "Invalid TIFF field offset".to_string())?;
    if end > data.len() {
        return Err("Invalid TIFF field offset".to_string());
    }
    Ok(start..end)
}

fn display_value(data: &[u8], entry: &IfdEntry, endian: Endian) -> Option<String> {
    let value = data.get(entry.value_range.clone())?;
    match entry.format {
        2 => Some(
            String::from_utf8_lossy(value)
                .trim_end_matches('\0')
                .trim()
                .to_string(),
        ),
        3 if entry.count == 1 => read_u16(value, 0, endian)
            .ok()
            .map(|value| value.to_string()),
        4 if entry.count == 1 => read_u32(value, 0, endian)
            .ok()
            .map(|value| value.to_string()),
        _ => Some(format!("{} bytes", value.len())),
    }
}

fn metadata_tag_label(tag: u16) -> Option<&'static str> {
    match tag {
        0x010E => Some("Image description"),
        0x010F => Some("Camera make"),
        0x0110 => Some("Camera model"),
        0x011A => Some("X resolution"),
        0x011B => Some("Y resolution"),
        0x0131 => Some("Software"),
        0x0132 => Some("DateTime"),
        0x013B => Some("Artist"),
        0x02BC => Some("XMP data"),
        0x8298 => Some("Copyright"),
        0x83BB => Some("IPTC data"),
        0x8649 => Some("Photoshop data"),
        0x8769 => Some("EXIF IFD"),
        0x8825 => Some("GPS IFD"),
        0xA005 => Some("Interoperability IFD"),
        0x9003 => Some("DateTimeOriginal"),
        0x9004 => Some("DateTimeDigitized"),
        0xA420 => Some("Image unique ID"),
        0xA431 => Some("Camera serial number"),
        0xA433 => Some("Lens make"),
        0xA434 => Some("Lens model"),
        0xA435 => Some("Lens serial number"),
        _ => None,
    }
}

fn metadata_category(tag: u16) -> &'static str {
    match tag {
        0x8825 => "Location",
        0x02BC => "XMP",
        0x83BB | 0x8649 => "IPTC",
        _ => "TIFF metadata",
    }
}

fn is_metadata_tag(tag: u16) -> bool {
    metadata_tag_label(tag).is_some()
}

fn tiff_type_size(format: u16) -> Option<usize> {
    match format {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

fn endian(data: &[u8]) -> Result<Endian, String> {
    if data.len() < 8 {
        return Err("File too small".to_string());
    }
    match data.get(0..2) {
        Some(b"MM") => Ok(Endian { big: true }),
        Some(b"II") => Ok(Endian { big: false }),
        _ => Err("Not a valid TIFF file".to_string()),
    }
}

fn read_u16(data: &[u8], offset: usize, endian: Endian) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| "Unexpected end of TIFF file".to_string())?;
    Ok(if endian.big {
        u16::from_be_bytes([bytes[0], bytes[1]])
    } else {
        u16::from_le_bytes([bytes[0], bytes[1]])
    })
}

fn read_u32(data: &[u8], offset: usize, endian: Endian) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "Unexpected end of TIFF file".to_string())?;
    Ok(if endian.big {
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    } else {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    })
}

fn write_u16(data: &mut [u8], offset: usize, value: u16, endian: Endian) -> Result<(), String> {
    let bytes = if endian.big {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    };
    let target = data
        .get_mut(offset..offset + 2)
        .ok_or_else(|| "Unexpected end of TIFF file".to_string())?;
    target.copy_from_slice(&bytes);
    Ok(())
}

fn write_u32(data: &mut [u8], offset: usize, value: u32, endian: Endian) -> Result<(), String> {
    let bytes = if endian.big {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    };
    let target = data
        .get_mut(offset..offset + 4)
        .ok_or_else(|| "Unexpected end of TIFF file".to_string())?;
    target.copy_from_slice(&bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: u16, format: u16, count: u32, value: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&format.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    fn sample_tiff() -> Vec<u8> {
        let secret = b"SecretCam\0";
        let pixels = [0xAA, 0xBB, 0xCC, 0xDD];
        let secret_offset = 8 + 2 + 4 * 12 + 4;
        let pixel_offset = secret_offset + secret.len();

        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&entry(0x0100, 4, 1, 1));
        data.extend_from_slice(&entry(0x0101, 4, 1, 1));
        data.extend_from_slice(&entry(0x0131, 2, secret.len() as u32, secret_offset as u32));
        data.extend_from_slice(&entry(0x0111, 4, 1, pixel_offset as u32));
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(secret);
        data.extend_from_slice(&pixels);
        data
    }

    fn clean_tiff() -> Vec<u8> {
        let pixels = [0xAA, 0xBB, 0xCC, 0xDD];
        let pixel_offset = 8 + 2 + 3 * 12 + 4;

        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&entry(0x0100, 4, 1, 1));
        data.extend_from_slice(&entry(0x0101, 4, 1, 1));
        data.extend_from_slice(&entry(0x0111, 4, 1, pixel_offset as u32));
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&pixels);
        data
    }

    #[test]
    fn leaves_clean_tiff_without_metadata_unchanged() {
        let data = clean_tiff();

        validate(&data).unwrap();
        assert!(extract_metadata(&data).metadata_found.is_empty());
        assert_eq!(remove_metadata(&data).unwrap(), data);
    }

    #[test]
    fn removes_tiff_ifd_metadata_without_touching_image_data() {
        let data = sample_tiff();

        let info = extract_metadata(&data);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Software" && entry.value == "SecretCam"));

        let cleaned = remove_metadata(&data).unwrap();
        assert!(!cleaned
            .windows(b"SecretCam".len())
            .any(|w| w == b"SecretCam"));
        assert!(cleaned.windows(4).any(|w| w == [0xAA, 0xBB, 0xCC, 0xDD]));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn rejects_tiff_with_invalid_value_offset() {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&entry(0x0131, 2, 16, 4096));
        data.extend_from_slice(&0u32.to_le_bytes());

        assert_eq!(
            validate(&data),
            Err("Invalid TIFF field offset".to_string())
        );
        assert!(remove_metadata(&data).is_err());
    }

    #[test]
    fn extract_metadata_handles_cyclic_sub_ifd_pointer() {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&entry(0x8769, 4, 1, 8));
        data.extend_from_slice(&0u32.to_le_bytes());

        validate(&data).unwrap();
        let info = extract_metadata(&data);
        assert_eq!(info.metadata_found.len(), 1);
        assert_eq!(info.metadata_found[0].name, "EXIF IFD");

        let cleaned = remove_metadata(&data).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }
}
