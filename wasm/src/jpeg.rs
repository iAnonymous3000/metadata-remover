use crate::{MetadataEntry, MetadataInfo};

// JPEG marker constants
const MARKER_PREFIX: u8 = 0xFF;
const SOI: u8 = 0xD8; // Start of Image
const EOI: u8 = 0xD9; // End of Image
const SOS: u8 = 0xDA; // Start of Scan
const APP0: u8 = 0xE0; // JFIF
const APP1: u8 = 0xE1; // EXIF/XMP
const APP2: u8 = 0xE2; // ICC Profile
const APP12: u8 = 0xEC; // Ducky
const APP13: u8 = 0xED; // IPTC/Photoshop
const APP14: u8 = 0xEE; // Adobe
const COM: u8 = 0xFE; // Comment

fn is_metadata_marker(marker: u8) -> bool {
    matches!(marker, APP0..=0xEF | COM)
}

fn is_standalone_marker(marker: u8) -> bool {
    marker == 0x00 || marker == 0x01 || (0xD0..=0xD7).contains(&marker)
}

fn should_strip_marker(marker: u8, segment_data: &[u8]) -> bool {
    match marker {
        APP2 if segment_data.starts_with(b"ICC_PROFILE") => false,
        APP14 => false,
        _ => is_metadata_marker(marker),
    }
}

fn read_u16_be(data: &[u8], offset: usize) -> u16 {
    ((data[offset] as u16) << 8) | (data[offset + 1] as u16)
}

fn parse_exif_value(data: &[u8]) -> String {
    // Try to interpret as ASCII string, otherwise show hex
    if let Ok(s) = std::str::from_utf8(data) {
        let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    format!("[{} bytes]", data.len())
}

fn marker_label(marker: u8) -> String {
    match marker {
        APP0 => "APP0/JFIF".to_string(),
        APP1 => "APP1".to_string(),
        APP2 => "APP2".to_string(),
        APP12 => "APP12".to_string(),
        APP13 => "APP13".to_string(),
        APP14 => "APP14".to_string(),
        COM => "Comment".to_string(),
        m if (APP0..=0xEF).contains(&m) => format!("APP{}", m - APP0),
        _ => format!("Marker 0x{:02X}", marker),
    }
}

fn read_tiff_u16(data: &[u8], offset: usize, big_endian: bool) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(if big_endian {
        ((bytes[0] as u16) << 8) | (bytes[1] as u16)
    } else {
        ((bytes[1] as u16) << 8) | (bytes[0] as u16)
    })
}

fn read_tiff_u32(data: &[u8], offset: usize, big_endian: bool) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(if big_endian {
        ((bytes[0] as u32) << 24)
            | ((bytes[1] as u32) << 16)
            | ((bytes[2] as u32) << 8)
            | (bytes[3] as u32)
    } else {
        ((bytes[3] as u32) << 24)
            | ((bytes[2] as u32) << 16)
            | ((bytes[1] as u32) << 8)
            | (bytes[0] as u32)
    })
}

fn extract_orientation_from_exif(exif_data: &[u8]) -> Option<u16> {
    if !exif_data.starts_with(b"Exif\0\0") || exif_data.len() < 14 {
        return None;
    }

    let tiff_data = &exif_data[6..];
    let big_endian = match (tiff_data[0], tiff_data[1]) {
        (0x4D, 0x4D) => true,
        (0x49, 0x49) => false,
        _ => return None,
    };

    let ifd_offset = read_tiff_u32(tiff_data, 4, big_endian)? as usize;
    let entry_count = read_tiff_u16(tiff_data, ifd_offset, big_endian)? as usize;

    for i in 0..entry_count {
        let entry_offset = ifd_offset.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        if entry_offset.checked_add(12)? > tiff_data.len() {
            return None;
        }

        let tag = read_tiff_u16(tiff_data, entry_offset, big_endian)?;
        let format = read_tiff_u16(tiff_data, entry_offset + 2, big_endian)?;
        let count = read_tiff_u32(tiff_data, entry_offset + 4, big_endian)?;
        if tag == 0x0112 && format == 3 && count == 1 {
            return read_tiff_u16(tiff_data, entry_offset + 8, big_endian)
                .filter(|value| (1..=8).contains(value));
        }
    }

    None
}

fn is_minimal_orientation_exif(exif_data: &[u8]) -> bool {
    if exif_data.len() != 32 {
        return false;
    }

    let Some(orientation) = extract_orientation_from_exif(exif_data) else {
        return false;
    };

    exif_data == build_minimal_orientation_exif(orientation).as_slice()
}

fn build_minimal_orientation_exif(orientation: u16) -> Vec<u8> {
    let mut exif = Vec::with_capacity(32);
    exif.extend_from_slice(b"Exif\0\0");
    exif.extend_from_slice(b"MM");
    exif.extend_from_slice(&42u16.to_be_bytes());
    exif.extend_from_slice(&8u32.to_be_bytes());
    exif.extend_from_slice(&1u16.to_be_bytes());
    exif.extend_from_slice(&0x0112u16.to_be_bytes());
    exif.extend_from_slice(&3u16.to_be_bytes());
    exif.extend_from_slice(&1u32.to_be_bytes());
    exif.extend_from_slice(&orientation.to_be_bytes());
    exif.extend_from_slice(&0u16.to_be_bytes());
    exif.extend_from_slice(&0u32.to_be_bytes());
    exif
}

fn push_orientation_segment(result: &mut Vec<u8>, orientation: u16) {
    let exif = build_minimal_orientation_exif(orientation);
    let length = (exif.len() + 2) as u16;
    result.extend_from_slice(&[MARKER_PREFIX, APP1]);
    result.extend_from_slice(&length.to_be_bytes());
    result.extend_from_slice(&exif);
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;
    let mut offset = 2; // Skip SOI

    while offset + 4 < data.len() {
        if data[offset] != MARKER_PREFIX {
            offset += 1;
            continue;
        }

        let marker = data[offset + 1];

        // JPEG permits repeated 0xFF fill bytes before a marker.
        if marker == MARKER_PREFIX {
            offset += 1;
            continue;
        }

        // End of metadata region
        if marker == SOS || marker == EOI {
            break;
        }

        // Skip markers without length
        if is_standalone_marker(marker) {
            offset += 2;
            continue;
        }

        if offset + 4 > data.len() {
            break;
        }

        let length = read_u16_be(data, offset + 2) as usize;
        if length < 2 || offset + 2 + length > data.len() {
            break;
        }

        let segment_data = &data[offset + 4..offset + 2 + length];

        match marker {
            APP1 => {
                total_bytes += length + 2;
                if segment_data.starts_with(b"Exif\0\0") {
                    if !is_minimal_orientation_exif(segment_data) {
                        entries.push(MetadataEntry {
                            category: "EXIF".to_string(),
                            name: "EXIF Data".to_string(),
                            value: format!("{} bytes", length - 2),
                        });
                        parse_exif_entries(segment_data, &mut entries);
                    }
                } else if segment_data.starts_with(b"http://ns.adobe.com/xap/")
                    || segment_data.starts_with(b"<?xpacket")
                {
                    entries.push(MetadataEntry {
                        category: "XMP".to_string(),
                        name: "XMP Data".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                }
            }
            APP0 => {
                total_bytes += length + 2;
                let name = if segment_data.starts_with(b"JFIF") {
                    "JFIF Segment"
                } else if segment_data.starts_with(b"JFXX") {
                    "JFIF Extension Segment"
                } else {
                    "APP0 Segment"
                };
                entries.push(MetadataEntry {
                    category: "APP0".to_string(),
                    name: name.to_string(),
                    value: format!("{} bytes", length - 2),
                });
            }
            APP2 => {
                total_bytes += length + 2;
                if !segment_data.starts_with(b"ICC_PROFILE") {
                    entries.push(MetadataEntry {
                        category: "APP2".to_string(),
                        name: "APP2 Segment".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                }
            }
            APP12 => {
                total_bytes += length + 2;
                entries.push(MetadataEntry {
                    category: "Ducky".to_string(),
                    name: "Ducky Tag".to_string(),
                    value: format!("{} bytes", length - 2),
                });
            }
            APP13 => {
                total_bytes += length + 2;
                if segment_data.starts_with(b"Photoshop 3.0") {
                    entries.push(MetadataEntry {
                        category: "IPTC".to_string(),
                        name: "Photoshop/IPTC Data".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                }
            }
            APP14 => {
                // Adobe APP14 affects color transforms for some JPEGs, so it is preserved.
            }
            COM => {
                total_bytes += length + 2;
                let comment = parse_exif_value(segment_data);
                entries.push(MetadataEntry {
                    category: "Comment".to_string(),
                    name: "Comment".to_string(),
                    value: comment,
                });
            }
            m if (APP0..=0xEF).contains(&m) => {
                total_bytes += length + 2;
                entries.push(MetadataEntry {
                    category: marker_label(m),
                    name: format!("{} Segment", marker_label(m)),
                    value: format!("{} bytes", length - 2),
                });
            }
            _ => {}
        }

        offset += 2 + length;
    }

    MetadataInfo {
        file_type: "jpeg".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

fn parse_exif_entries(exif_data: &[u8], entries: &mut Vec<MetadataEntry>) {
    // Skip "Exif\0\0" header
    if exif_data.len() < 14 {
        return;
    }

    let tiff_data = &exif_data[6..];

    // Check byte order
    let big_endian = match (tiff_data[0], tiff_data[1]) {
        (0x4D, 0x4D) => true,  // "MM" - big endian
        (0x49, 0x49) => false, // "II" - little endian
        _ => return,
    };

    let read_u16 = |data: &[u8], off: usize| -> u16 {
        if big_endian {
            ((data[off] as u16) << 8) | (data[off + 1] as u16)
        } else {
            ((data[off + 1] as u16) << 8) | (data[off] as u16)
        }
    };

    let read_u32 = |data: &[u8], off: usize| -> u32 {
        if big_endian {
            ((data[off] as u32) << 24)
                | ((data[off + 1] as u32) << 16)
                | ((data[off + 2] as u32) << 8)
                | (data[off + 3] as u32)
        } else {
            ((data[off + 3] as u32) << 24)
                | ((data[off + 2] as u32) << 16)
                | ((data[off + 1] as u32) << 8)
                | (data[off] as u32)
        }
    };

    // Get IFD0 offset
    if tiff_data.len() < 8 {
        return;
    }
    let ifd_offset = read_u32(tiff_data, 4) as usize;

    if ifd_offset + 2 > tiff_data.len() {
        return;
    }

    let entry_count = read_u16(tiff_data, ifd_offset) as usize;

    for i in 0..entry_count {
        let Some(entry_offset) = ifd_offset
            .checked_add(2)
            .and_then(|base| i.checked_mul(12).and_then(|delta| base.checked_add(delta)))
        else {
            break;
        };
        if entry_offset + 12 > tiff_data.len() {
            break;
        }

        let tag = read_u16(tiff_data, entry_offset);
        let format = read_u16(tiff_data, entry_offset + 2);
        let count = read_u32(tiff_data, entry_offset + 4) as usize;

        let tag_name = match tag {
            0x010F => "Camera Make",
            0x0110 => "Camera Model",
            0x011A => "X Resolution",
            0x011B => "Y Resolution",
            0x0131 => "Software",
            0x0132 => "DateTime",
            0x013B => "Artist",
            0x8298 => "Copyright",
            0x8769 => "EXIF IFD",
            0x8825 => "GPS IFD",
            _ => continue,
        };

        // Type sizes: 1=byte, 2=ascii, 3=short, 4=long, 5=rational
        let type_size = match format {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue,
        };

        let Some(data_size) = count.checked_mul(type_size) else {
            continue;
        };
        let value_offset = if data_size <= 4 {
            entry_offset + 8
        } else {
            read_u32(tiff_data, entry_offset + 8) as usize
        };

        if value_offset
            .checked_add(data_size)
            .filter(|end| *end <= tiff_data.len())
            .is_none()
        {
            continue;
        }

        let value_data = &tiff_data[value_offset..value_offset + data_size];

        let value = if format == 2 {
            // ASCII string
            String::from_utf8_lossy(value_data)
                .trim_end_matches('\0')
                .to_string()
        } else {
            format!("[{} bytes]", data_size)
        };

        if !value.is_empty() && value != "[0 bytes]" {
            entries.push(MetadataEntry {
                category: "EXIF".to_string(),
                name: tag_name.to_string(),
                value,
            });
        }
    }
}

fn copy_scan_segment(
    data: &[u8],
    offset: usize,
    result: &mut Vec<u8>,
) -> Result<(usize, bool), String> {
    let length = read_u16_be(data, offset + 2) as usize;
    if length < 2 || offset + 2 + length > data.len() {
        return Err("Invalid scan segment length".to_string());
    }

    let scan_start = offset + 2 + length;
    result.extend_from_slice(&data[offset..scan_start]);

    let mut pos = scan_start;
    while pos + 1 < data.len() {
        if data[pos] != MARKER_PREFIX {
            pos += 1;
            continue;
        }

        let marker = data[pos + 1];
        match marker {
            0x00 => pos += 2,
            0xFF => pos += 1,
            0xD0..=0xD7 => pos += 2,
            EOI => {
                result.extend_from_slice(&data[scan_start..pos + 2]);
                return Ok((pos + 2, true));
            }
            _ => {
                result.extend_from_slice(&data[scan_start..pos]);
                return Ok((pos, false));
            }
        }
    }

    result.extend_from_slice(&data[scan_start..]);
    Ok((data.len(), true))
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 4 {
        return Err("File too small".to_string());
    }

    if data[0] != MARKER_PREFIX || data[1] != SOI {
        return Err("Not a valid JPEG file".to_string());
    }

    let mut result = Vec::with_capacity(data.len());

    // Write SOI
    result.push(MARKER_PREFIX);
    result.push(SOI);

    let orientation = extract_orientation(data).filter(|value| *value != 1);
    if let Some(orientation) = orientation {
        push_orientation_segment(&mut result, orientation);
    }

    let mut offset = 2;

    while offset + 2 <= data.len() {
        if data[offset] != MARKER_PREFIX {
            offset += 1;
            continue;
        }

        let marker = data[offset + 1];

        // JPEG permits repeated 0xFF fill bytes before a marker.
        if marker == MARKER_PREFIX {
            offset += 1;
            continue;
        }

        // Handle special markers
        if marker == EOI {
            result.push(MARKER_PREFIX);
            result.push(EOI);
            break;
        }

        if is_standalone_marker(marker) {
            result.push(MARKER_PREFIX);
            result.push(marker);
            offset += 2;
            continue;
        }

        if offset + 4 > data.len() {
            break;
        }

        let length = read_u16_be(data, offset + 2) as usize;
        if length < 2 || offset + 2 + length > data.len() {
            return Err("Invalid segment length".to_string());
        }

        // Copy entropy-coded image data but stop at the real EOI marker so
        // appended tracking data is not preserved.
        if marker == SOS {
            let (next_offset, reached_eoi) = copy_scan_segment(data, offset, &mut result)?;
            if reached_eoi {
                break;
            }
            offset = next_offset;
            continue;
        }

        // Keep non-metadata segments
        let segment_data = &data[offset + 4..offset + 2 + length];
        if !should_strip_marker(marker, segment_data) {
            result.extend_from_slice(&data[offset..offset + 2 + length]);
        }

        offset += 2 + length;
    }

    Ok(result)
}

fn extract_orientation(data: &[u8]) -> Option<u16> {
    let mut offset = 2;

    while offset + 4 < data.len() {
        if data[offset] != MARKER_PREFIX {
            offset += 1;
            continue;
        }

        let marker = data[offset + 1];
        if marker == MARKER_PREFIX {
            offset += 1;
            continue;
        }

        if marker == SOS || marker == EOI {
            break;
        }
        if is_standalone_marker(marker) {
            offset += 2;
            continue;
        }

        let length = read_u16_be(data, offset + 2) as usize;
        if length < 2 || offset + 2 + length > data.len() {
            break;
        }

        let segment_data = &data[offset + 4..offset + 2 + length];
        if marker == APP1 {
            if let Some(orientation) = extract_orientation_from_exif(segment_data) {
                return Some(orientation);
            }
        }

        offset += 2 + length;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_metadata_marker() {
        assert!(is_metadata_marker(APP1));
        assert!(is_metadata_marker(APP0));
        assert!(is_metadata_marker(COM));
        assert!(!is_metadata_marker(SOI));
    }

    #[test]
    fn test_remove_metadata_strips_app0_and_trailing_data() {
        let mut data = vec![MARKER_PREFIX, SOI];
        data.extend_from_slice(&[MARKER_PREFIX, APP0, 0, 7]);
        data.extend_from_slice(b"JFIF\0");
        data.extend_from_slice(&[MARKER_PREFIX, 0xDB, 0, 4, 0xAA, 0xBB]);
        data.extend_from_slice(&[MARKER_PREFIX, SOS, 0, 4, 0x03, 0x00]);
        data.extend_from_slice(&[0x11, MARKER_PREFIX, 0x00, 0x22, MARKER_PREFIX, EOI]);
        data.extend_from_slice(b"SECRET_TRAILING_BYTES");

        let cleaned = remove_metadata(&data).unwrap();

        assert!(!cleaned.windows(4).any(|w| w == [MARKER_PREFIX, APP0, 0, 7]));
        assert!(!cleaned.windows(b"SECRET".len()).any(|w| w == b"SECRET"));
        assert!(cleaned.ends_with(&[MARKER_PREFIX, EOI]));
    }

    #[test]
    fn test_remove_metadata_preserves_orientation_and_color_segments() {
        let mut data = vec![MARKER_PREFIX, SOI];
        let exif = build_minimal_orientation_exif(6);
        data.extend_from_slice(&[MARKER_PREFIX, APP1]);
        data.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&exif);
        data.extend_from_slice(&[MARKER_PREFIX, APP2, 0, 16]);
        data.extend_from_slice(b"ICC_PROFILE\0\x01\x01");
        data.extend_from_slice(&[MARKER_PREFIX, APP14, 0, 7]);
        data.extend_from_slice(b"Adobe");
        data.extend_from_slice(&[MARKER_PREFIX, COM, 0, 8]);
        data.extend_from_slice(b"Secret");
        data.extend_from_slice(&[MARKER_PREFIX, SOS, 0, 4, 0x03, 0x00]);
        data.extend_from_slice(&[0x11, MARKER_PREFIX, EOI]);

        let cleaned = remove_metadata(&data).unwrap();
        let info = extract_metadata(&cleaned);

        assert!(cleaned
            .windows(b"ICC_PROFILE".len())
            .any(|w| w == b"ICC_PROFILE"));
        assert!(cleaned.windows(b"Adobe".len()).any(|w| w == b"Adobe"));
        assert_eq!(extract_orientation(&cleaned), Some(6));
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
        assert!(info.metadata_found.is_empty());
    }

    #[test]
    fn test_extract_metadata_skips_header_fill_bytes() {
        let mut data = vec![MARKER_PREFIX, SOI];
        data.extend_from_slice(&[MARKER_PREFIX, MARKER_PREFIX, COM, 0, 8]);
        data.extend_from_slice(b"Secret");
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);

        assert_eq!(info.metadata_found.len(), 1);
        assert_eq!(info.metadata_found[0].category, "Comment");
        assert_eq!(info.metadata_found[0].value, "Secret");
    }

    #[test]
    fn test_remove_metadata_skips_header_fill_bytes() {
        let mut data = vec![MARKER_PREFIX, SOI];
        data.extend_from_slice(&[MARKER_PREFIX, MARKER_PREFIX, 0xDB, 0, 4, 0xAA, 0xBB]);
        data.extend_from_slice(&[MARKER_PREFIX, MARKER_PREFIX, COM, 0, 8]);
        data.extend_from_slice(b"Secret");
        data.extend_from_slice(&[MARKER_PREFIX, SOS, 0, 4, 0x03, 0x00]);
        data.extend_from_slice(&[0x11, MARKER_PREFIX, EOI]);

        let cleaned = remove_metadata(&data).unwrap();

        assert!(cleaned
            .windows(6)
            .any(|w| w == [MARKER_PREFIX, 0xDB, 0, 4, 0xAA, 0xBB]));
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
        assert!(cleaned.ends_with(&[MARKER_PREFIX, EOI]));
    }
}
