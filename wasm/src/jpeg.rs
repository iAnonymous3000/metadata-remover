use crate::util::contains_ascii_ci;
use crate::{MetadataEntry, MetadataInfo};

// JPEG marker constants
const MARKER_PREFIX: u8 = 0xFF;
const SOI: u8 = 0xD8; // Start of Image
const EOI: u8 = 0xD9; // End of Image
const SOS: u8 = 0xDA; // Start of Scan
const APP0: u8 = 0xE0; // JFIF
const APP1: u8 = 0xE1; // EXIF/XMP
const APP2: u8 = 0xE2; // ICC Profile
const APP11: u8 = 0xEB; // JUMBF / Content Credentials
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

fn is_content_credentials_segment(marker: u8, segment_data: &[u8]) -> bool {
    marker == APP11
        && (segment_data.starts_with(b"JUMBF")
            || contains_ascii_ci(segment_data, b"c2pa")
            || contains_ascii_ci(segment_data, b"jumb")
            || contains_ascii_ci(segment_data, b"Content Credentials"))
}

fn marker_label(marker: u8) -> String {
    match marker {
        APP0 => "APP0/JFIF".to_string(),
        APP1 => "APP1".to_string(),
        APP2 => "APP2".to_string(),
        APP11 => "APP11/JUMBF".to_string(),
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

fn tiff_type_size(format: u16) -> Option<usize> {
    match format {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

fn tiff_value_range(
    tiff_data: &[u8],
    entry_offset: usize,
    format: u16,
    count: u32,
    big_endian: bool,
) -> Option<std::ops::Range<usize>> {
    let data_size = (count as usize).checked_mul(tiff_type_size(format)?)?;
    let value_offset = if data_size <= 4 {
        entry_offset.checked_add(8)?
    } else {
        read_tiff_u32(tiff_data, entry_offset + 8, big_endian)? as usize
    };
    let end = value_offset.checked_add(data_size)?;
    (end <= tiff_data.len()).then_some(value_offset..end)
}

fn read_tiff_rational(data: &[u8], offset: usize, big_endian: bool) -> Option<f64> {
    let numerator = read_tiff_u32(data, offset, big_endian)? as f64;
    let denominator = read_tiff_u32(data, offset + 4, big_endian)? as f64;
    (denominator != 0.0).then_some(numerator / denominator)
}

fn parse_gps_ref(data: &[u8]) -> Option<char> {
    data.iter()
        .copied()
        .find(|byte| byte.is_ascii_alphabetic())
        .map(|byte| (byte as char).to_ascii_uppercase())
}

fn parse_gps_rationals(data: &[u8], big_endian: bool) -> Option<[f64; 3]> {
    if data.len() < 24 {
        return None;
    }

    Some([
        read_tiff_rational(data, 0, big_endian)?,
        read_tiff_rational(data, 8, big_endian)?,
        read_tiff_rational(data, 16, big_endian)?,
    ])
}

fn gps_decimal(parts: [f64; 3], reference: char) -> Option<f64> {
    if !parts.iter().all(|value| value.is_finite()) {
        return None;
    }

    let decimal = parts[0] + parts[1] / 60.0 + parts[2] / 3600.0;
    match reference {
        'N' | 'E' => Some(decimal),
        'S' | 'W' => Some(-decimal),
        _ => None,
    }
}

fn parse_gps_ifd(tiff_data: &[u8], gps_ifd_offset: usize, big_endian: bool) -> Option<String> {
    let entry_count = read_tiff_u16(tiff_data, gps_ifd_offset, big_endian)? as usize;
    let mut lat_ref = None;
    let mut lat_parts = None;
    let mut lon_ref = None;
    let mut lon_parts = None;

    for i in 0..entry_count {
        let entry_offset = gps_ifd_offset
            .checked_add(2)?
            .checked_add(i.checked_mul(12)?)?;
        if entry_offset.checked_add(12)? > tiff_data.len() {
            return None;
        }

        let tag = read_tiff_u16(tiff_data, entry_offset, big_endian)?;
        let format = read_tiff_u16(tiff_data, entry_offset + 2, big_endian)?;
        let count = read_tiff_u32(tiff_data, entry_offset + 4, big_endian)?;
        let Some(range) = tiff_value_range(tiff_data, entry_offset, format, count, big_endian)
        else {
            continue;
        };
        let value_data = &tiff_data[range];

        match tag {
            0x0001 if format == 2 => lat_ref = parse_gps_ref(value_data),
            0x0002 if format == 5 && count == 3 => {
                lat_parts = parse_gps_rationals(value_data, big_endian)
            }
            0x0003 if format == 2 => lon_ref = parse_gps_ref(value_data),
            0x0004 if format == 5 && count == 3 => {
                lon_parts = parse_gps_rationals(value_data, big_endian)
            }
            _ => {}
        }
    }

    let latitude = gps_decimal(lat_parts?, lat_ref?)?;
    let longitude = gps_decimal(lon_parts?, lon_ref?)?;
    Some(format!("{latitude:.6}, {longitude:.6}"))
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

        if is_content_credentials_segment(marker, segment_data) {
            total_bytes += length + 2;
            entries.push(MetadataEntry {
                category: "Content Credentials".to_string(),
                name: "JPEG APP11/JUMBF".to_string(),
                value: format!("{} bytes", length - 2),
            });
            offset += 2 + length;
            continue;
        }

        match marker {
            APP1 => {
                if segment_data.starts_with(b"Exif\0\0") {
                    if !is_minimal_orientation_exif(segment_data) {
                        total_bytes += length + 2;
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
                    total_bytes += length + 2;
                    entries.push(MetadataEntry {
                        category: "XMP".to_string(),
                        name: "XMP Data".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                } else {
                    total_bytes += length + 2;
                    entries.push(MetadataEntry {
                        category: "APP1".to_string(),
                        name: "APP1 Segment".to_string(),
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
                if !segment_data.starts_with(b"ICC_PROFILE") {
                    total_bytes += length + 2;
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
                } else {
                    entries.push(MetadataEntry {
                        category: "APP13".to_string(),
                        name: "APP13 Segment".to_string(),
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

    // Get IFD0 offset
    if tiff_data.len() < 8 {
        return;
    }
    let Some(ifd_offset) = read_tiff_u32(tiff_data, 4, big_endian).map(|value| value as usize)
    else {
        return;
    };

    if ifd_offset + 2 > tiff_data.len() {
        return;
    }

    let Some(entry_count) =
        read_tiff_u16(tiff_data, ifd_offset, big_endian).map(|value| value as usize)
    else {
        return;
    };
    let mut exif_ifd_offset = None;

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

        let Some(tag) = read_tiff_u16(tiff_data, entry_offset, big_endian) else {
            continue;
        };
        let Some(format) = read_tiff_u16(tiff_data, entry_offset + 2, big_endian) else {
            continue;
        };
        let Some(count) = read_tiff_u32(tiff_data, entry_offset + 4, big_endian) else {
            continue;
        };

        if tag == 0x8825 {
            let value = if format == 4 && count == 1 {
                let gps_ifd_offset =
                    read_tiff_u32(tiff_data, entry_offset + 8, big_endian).unwrap_or(0) as usize;
                parse_gps_ifd(tiff_data, gps_ifd_offset, big_endian)
                    .unwrap_or_else(|| "Present in EXIF metadata".to_string())
            } else {
                "Present in EXIF metadata".to_string()
            };

            entries.push(MetadataEntry {
                category: "Location".to_string(),
                name: "GPS / Location data".to_string(),
                value,
            });
            continue;
        }

        if tag == 0x8769 {
            if format == 4 && count == 1 {
                exif_ifd_offset = read_tiff_u32(tiff_data, entry_offset + 8, big_endian)
                    .map(|value| value as usize);
            }
            continue;
        }

        let tag_name = match tag {
            0x010F => "Camera Make",
            0x0110 => "Camera Model",
            0x011A => "X Resolution",
            0x011B => "Y Resolution",
            0x0131 => "Software",
            0x0132 => "DateTime",
            0x013B => "Artist",
            0x8298 => "Copyright",
            _ => continue,
        };

        let Some(value_range) =
            tiff_value_range(tiff_data, entry_offset, format, count, big_endian)
        else {
            continue;
        };

        let value_data = &tiff_data[value_range];

        let value = tiff_display_value(value_data, format, count, big_endian);

        if !value.is_empty() && value != "[0 bytes]" {
            entries.push(MetadataEntry {
                category: "EXIF".to_string(),
                name: tag_name.to_string(),
                value,
            });
        }
    }

    if let Some(offset) = exif_ifd_offset {
        parse_exif_sub_ifd(tiff_data, offset, big_endian, entries);
    }
}

fn parse_exif_sub_ifd(
    tiff_data: &[u8],
    ifd_offset: usize,
    big_endian: bool,
    entries: &mut Vec<MetadataEntry>,
) {
    let Some(entry_count) =
        read_tiff_u16(tiff_data, ifd_offset, big_endian).map(|value| value as usize)
    else {
        return;
    };

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

        let Some(tag) = read_tiff_u16(tiff_data, entry_offset, big_endian) else {
            continue;
        };
        let Some(format) = read_tiff_u16(tiff_data, entry_offset + 2, big_endian) else {
            continue;
        };
        let Some(count) = read_tiff_u32(tiff_data, entry_offset + 4, big_endian) else {
            continue;
        };

        let tag_name = match tag {
            0x829A => "Exposure Time",
            0x829D => "F-Number",
            0x8827 => "ISO Speed",
            0x9003 => "DateTimeOriginal",
            0x9004 => "DateTimeDigitized",
            0x920A => "Focal Length",
            0xA002 => "Pixel X Dimension",
            0xA003 => "Pixel Y Dimension",
            0xA405 => "Focal Length In 35mm Film",
            0xA420 => "Image Unique ID",
            0xA431 => "Camera Serial Number",
            0xA433 => "Lens Make",
            0xA434 => "Lens Model",
            0xA435 => "Lens Serial Number",
            _ => continue,
        };

        let Some(value_range) =
            tiff_value_range(tiff_data, entry_offset, format, count, big_endian)
        else {
            continue;
        };

        let value_data = &tiff_data[value_range];
        let value = tiff_display_value(value_data, format, count, big_endian);

        if !value.is_empty() && value != "[0 bytes]" {
            entries.push(MetadataEntry {
                category: "EXIF".to_string(),
                name: tag_name.to_string(),
                value,
            });
        }
    }
}

fn tiff_display_value(value_data: &[u8], format: u16, count: u32, big_endian: bool) -> String {
    match format {
        2 => String::from_utf8_lossy(value_data)
            .trim_end_matches('\0')
            .to_string(),
        3 if count == 1 => read_tiff_u16(value_data, 0, big_endian)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        4 if count == 1 => read_tiff_u32(value_data, 0, big_endian)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        5 if count == 1 => read_tiff_rational(value_data, 0, big_endian)
            .map(format_decimal)
            .unwrap_or_default(),
        _ => format!("[{} bytes]", value_data.len()),
    }
}

fn format_decimal(value: f64) -> String {
    let formatted = format!("{value:.4}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn scan_data_end(data: &[u8], scan_start: usize) -> Result<(usize, bool), String> {
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
            EOI => return Ok((pos + 2, true)),
            _ => return Ok((pos, false)),
        }
    }

    Err("JPEG missing EOI marker".to_string())
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

    let (next_offset, reached_eoi) = scan_data_end(data, scan_start)?;
    result.extend_from_slice(&data[scan_start..next_offset]);
    Ok((next_offset, reached_eoi))
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    if data.len() < 4 {
        return Err("File too small".to_string());
    }

    if data[0] != MARKER_PREFIX || data[1] != SOI {
        return Err("Not a valid JPEG file".to_string());
    }

    let mut offset = 2;
    while offset + 2 <= data.len() {
        if data[offset] != MARKER_PREFIX {
            offset += 1;
            continue;
        }

        let marker = data[offset + 1];
        if marker == MARKER_PREFIX {
            offset += 1;
            continue;
        }

        if marker == EOI {
            return Ok(());
        }

        if is_standalone_marker(marker) {
            offset += 2;
            continue;
        }

        if offset + 4 > data.len() {
            return Err("Truncated JPEG segment".to_string());
        }

        let length = read_u16_be(data, offset + 2) as usize;
        if length < 2 || offset + 2 + length > data.len() {
            return Err("Invalid segment length".to_string());
        }

        if marker == SOS {
            let scan_start = offset + 2 + length;
            let (next_offset, reached_eoi) = scan_data_end(data, scan_start)?;
            if reached_eoi {
                return Ok(());
            }
            offset = next_offset;
            continue;
        }

        offset += 2 + length;
    }

    Err("JPEG missing EOI marker".to_string())
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
    let mut reached_eoi = false;

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
            reached_eoi = true;
            break;
        }

        if is_standalone_marker(marker) {
            result.push(MARKER_PREFIX);
            result.push(marker);
            offset += 2;
            continue;
        }

        if offset + 4 > data.len() {
            return Err("Truncated JPEG segment".to_string());
        }

        let length = read_u16_be(data, offset + 2) as usize;
        if length < 2 || offset + 2 + length > data.len() {
            return Err("Invalid segment length".to_string());
        }

        // Copy entropy-coded image data but stop at the real EOI marker so
        // appended tracking data is not preserved.
        if marker == SOS {
            let (next_offset, scan_reached_eoi) = copy_scan_segment(data, offset, &mut result)?;
            if scan_reached_eoi {
                reached_eoi = true;
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

    if reached_eoi {
        Ok(result)
    } else {
        Err("JPEG missing EOI marker".to_string())
    }
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

    fn push_ifd_entry(out: &mut Vec<u8>, tag: u16, format: u16, count: u32, value: [u8; 4]) {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&format.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&value);
    }

    fn rational(numerator: u32, denominator: u32) -> [u8; 8] {
        let mut out = [0; 8];
        out[..4].copy_from_slice(&numerator.to_be_bytes());
        out[4..].copy_from_slice(&denominator.to_be_bytes());
        out
    }

    fn build_gps_exif() -> Vec<u8> {
        let gps_ifd_offset = 26u32;
        let lat_values_offset = 80u32;
        let lon_values_offset = 104u32;

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM");
        tiff.extend_from_slice(&42u16.to_be_bytes());
        tiff.extend_from_slice(&8u32.to_be_bytes());

        tiff.extend_from_slice(&1u16.to_be_bytes());
        push_ifd_entry(&mut tiff, 0x8825, 4, 1, gps_ifd_offset.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes());

        assert_eq!(tiff.len(), gps_ifd_offset as usize);
        tiff.extend_from_slice(&4u16.to_be_bytes());
        push_ifd_entry(&mut tiff, 0x0001, 2, 2, [b'N', 0, 0, 0]);
        push_ifd_entry(&mut tiff, 0x0002, 5, 3, lat_values_offset.to_be_bytes());
        push_ifd_entry(&mut tiff, 0x0003, 2, 2, [b'W', 0, 0, 0]);
        push_ifd_entry(&mut tiff, 0x0004, 5, 3, lon_values_offset.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes());

        assert_eq!(tiff.len(), lat_values_offset as usize);
        for value in [rational(37, 1), rational(46, 1), rational(741, 25)] {
            tiff.extend_from_slice(&value);
        }

        assert_eq!(tiff.len(), lon_values_offset as usize);
        for value in [rational(122, 1), rational(25, 1), rational(246, 25)] {
            tiff.extend_from_slice(&value);
        }

        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&tiff);
        exif
    }

    fn build_capture_exif() -> Vec<u8> {
        let modified = b"2024:01:02 03:04:05\0";
        let original = b"2023:12:31 23:59:58\0";
        let lens_model = b"SecretLens 50\0";
        let root_data_offset = 8 + 2 + 2 * 12 + 4;
        let modified_offset = root_data_offset as u32;
        let exif_ifd_offset = modified_offset + modified.len() as u32;
        let exif_value_offset = exif_ifd_offset + 2 + 2 * 12 + 4;
        let original_offset = exif_value_offset;
        let lens_model_offset = original_offset + original.len() as u32;

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM");
        tiff.extend_from_slice(&42u16.to_be_bytes());
        tiff.extend_from_slice(&8u32.to_be_bytes());

        tiff.extend_from_slice(&2u16.to_be_bytes());
        push_ifd_entry(
            &mut tiff,
            0x0132,
            2,
            modified.len() as u32,
            modified_offset.to_be_bytes(),
        );
        push_ifd_entry(&mut tiff, 0x8769, 4, 1, exif_ifd_offset.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes());

        assert_eq!(tiff.len(), modified_offset as usize);
        tiff.extend_from_slice(modified);

        assert_eq!(tiff.len(), exif_ifd_offset as usize);
        tiff.extend_from_slice(&2u16.to_be_bytes());
        push_ifd_entry(
            &mut tiff,
            0x9003,
            2,
            original.len() as u32,
            original_offset.to_be_bytes(),
        );
        push_ifd_entry(
            &mut tiff,
            0xA434,
            2,
            lens_model.len() as u32,
            lens_model_offset.to_be_bytes(),
        );
        tiff.extend_from_slice(&0u32.to_be_bytes());

        assert_eq!(tiff.len(), original_offset as usize);
        tiff.extend_from_slice(original);

        assert_eq!(tiff.len(), lens_model_offset as usize);
        tiff.extend_from_slice(lens_model);

        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&tiff);
        exif
    }

    #[test]
    fn test_extract_metadata_decodes_gps_location() {
        let exif = build_gps_exif();
        let mut data = vec![MARKER_PREFIX, SOI, MARKER_PREFIX, APP1];
        data.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&exif);
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);
        let gps = info
            .metadata_found
            .iter()
            .find(|entry| entry.name == "GPS / Location data")
            .expect("GPS location entry should be present");

        assert_eq!(gps.category, "Location");
        assert_eq!(gps.value, "37.774900, -122.419400");
        assert!(!info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "GPS IFD"));
    }

    #[test]
    fn test_extract_metadata_reads_exif_sub_ifd_capture_time_and_lens() {
        let exif = build_capture_exif();
        let mut data = vec![MARKER_PREFIX, SOI, MARKER_PREFIX, APP1];
        data.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&exif);
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);

        assert_eq!(
            info.metadata_found
                .iter()
                .find(|entry| entry.name == "DateTime")
                .map(|entry| entry.value.as_str()),
            Some("2024:01:02 03:04:05")
        );
        assert_eq!(
            info.metadata_found
                .iter()
                .find(|entry| entry.name == "DateTimeOriginal")
                .map(|entry| entry.value.as_str()),
            Some("2023:12:31 23:59:58")
        );
        assert_eq!(
            info.metadata_found
                .iter()
                .find(|entry| entry.name == "Lens Model")
                .map(|entry| entry.value.as_str()),
            Some("SecretLens 50")
        );
        assert!(!info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "EXIF IFD"));
    }

    #[test]
    fn test_validate_rejects_invalid_segment_length() {
        let data = vec![MARKER_PREFIX, SOI, MARKER_PREFIX, COM, 0, 20, b'x'];

        assert_eq!(validate(&data), Err("Invalid segment length".to_string()));
    }

    #[test]
    fn test_validate_rejects_truncated_scan_without_eoi() {
        let data = vec![
            MARKER_PREFIX,
            SOI,
            MARKER_PREFIX,
            SOS,
            0,
            4,
            0x03,
            0x00,
            0x11,
            0x22,
        ];

        assert_eq!(validate(&data), Err("JPEG missing EOI marker".to_string()));
        assert_eq!(
            remove_metadata(&data),
            Err("JPEG missing EOI marker".to_string())
        );
    }

    #[test]
    fn test_validate_rejects_header_without_eoi() {
        let data = vec![
            MARKER_PREFIX,
            SOI,
            MARKER_PREFIX,
            COM,
            0,
            8,
            b'S',
            b'e',
            b'c',
            b'r',
            b'e',
            b't',
        ];

        assert_eq!(validate(&data), Err("JPEG missing EOI marker".to_string()));
        assert_eq!(
            remove_metadata(&data),
            Err("JPEG missing EOI marker".to_string())
        );
    }

    #[test]
    fn test_extract_metadata_does_not_count_preserved_color_segments() {
        let mut data = vec![MARKER_PREFIX, SOI];
        let orientation = build_minimal_orientation_exif(6);
        data.extend_from_slice(&[MARKER_PREFIX, APP1]);
        data.extend_from_slice(&((orientation.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(&orientation);
        data.extend_from_slice(&[MARKER_PREFIX, APP2, 0, 16]);
        data.extend_from_slice(b"ICC_PROFILE\0\x01\x01");
        data.extend_from_slice(&[MARKER_PREFIX, APP14, 0, 7]);
        data.extend_from_slice(b"Adobe");
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);

        assert!(info.metadata_found.is_empty());
        assert_eq!(info.total_metadata_bytes, 0);
    }

    #[test]
    fn test_extract_metadata_reports_generic_stripped_app_segments() {
        let mut data = vec![MARKER_PREFIX, SOI];
        data.extend_from_slice(&[MARKER_PREFIX, APP1, 0, 6]);
        data.extend_from_slice(b"ABCD");
        data.extend_from_slice(&[MARKER_PREFIX, APP13, 0, 6]);
        data.extend_from_slice(b"DATA");
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);

        assert_eq!(info.metadata_found.len(), 2);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "APP1 Segment"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "APP13 Segment"));
        assert_eq!(info.total_metadata_bytes, 16);
    }

    #[test]
    fn test_extract_and_remove_content_credentials_app11_segment() {
        let mut data = vec![MARKER_PREFIX, SOI];
        let payload = b"JUMBF c2pa Content Credentials manifest";
        data.extend_from_slice(&[MARKER_PREFIX, APP11]);
        data.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(&[MARKER_PREFIX, EOI]);

        let info = extract_metadata(&data);
        assert_eq!(info.metadata_found.len(), 1);
        assert_eq!(info.metadata_found[0].category, "Content Credentials");
        assert_eq!(info.metadata_found[0].name, "JPEG APP11/JUMBF");

        let cleaned = remove_metadata(&data).unwrap();
        assert!(!cleaned.windows(b"c2pa".len()).any(|w| w == b"c2pa"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
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
