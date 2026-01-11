use crate::{MetadataEntry, MetadataInfo};

// JPEG marker constants
const MARKER_PREFIX: u8 = 0xFF;
const SOI: u8 = 0xD8;  // Start of Image
const EOI: u8 = 0xD9;  // End of Image
const SOS: u8 = 0xDA;  // Start of Scan
const APP0: u8 = 0xE0; // JFIF
const APP1: u8 = 0xE1; // EXIF/XMP
const APP2: u8 = 0xE2; // ICC Profile
const APP12: u8 = 0xEC; // Ducky
const APP13: u8 = 0xED; // IPTC/Photoshop
const APP14: u8 = 0xEE; // Adobe
const COM: u8 = 0xFE;  // Comment

fn is_metadata_marker(marker: u8) -> bool {
    // APP1-APP15 (except APP0 which is JFIF and needed), and COM
    matches!(marker, APP1..=0xEF | COM)
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

        // End of metadata region
        if marker == SOS || marker == EOI {
            break;
        }

        // Skip markers without length
        if marker == 0x00 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
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
                    entries.push(MetadataEntry {
                        category: "EXIF".to_string(),
                        name: "EXIF Data".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                    // Try to extract some readable EXIF data
                    parse_exif_entries(segment_data, &mut entries);
                } else if segment_data.starts_with(b"http://ns.adobe.com/xap/")
                       || segment_data.starts_with(b"<?xpacket") {
                    entries.push(MetadataEntry {
                        category: "XMP".to_string(),
                        name: "XMP Data".to_string(),
                        value: format!("{} bytes", length - 2),
                    });
                }
            }
            APP2 => {
                total_bytes += length + 2;
                if segment_data.starts_with(b"ICC_PROFILE") {
                    entries.push(MetadataEntry {
                        category: "ICC".to_string(),
                        name: "ICC Color Profile".to_string(),
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
                total_bytes += length + 2;
                entries.push(MetadataEntry {
                    category: "Adobe".to_string(),
                    name: "Adobe Tag".to_string(),
                    value: format!("{} bytes", length - 2),
                });
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
            m if (APP0 + 1..=0xEF).contains(&m) => {
                total_bytes += length + 2;
                entries.push(MetadataEntry {
                    category: format!("APP{}", m - APP0),
                    name: format!("APP{} Segment", m - APP0),
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
        let entry_offset = ifd_offset + 2 + (i * 12);
        if entry_offset + 12 > tiff_data.len() {
            break;
        }

        let tag = read_u16(tiff_data, entry_offset);
        let format = read_u16(tiff_data, entry_offset + 2);
        let count = read_u32(tiff_data, entry_offset + 4) as usize;

        let tag_name = match tag {
            0x010F => "Camera Make",
            0x0110 => "Camera Model",
            0x0112 => "Orientation",
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

        let data_size = count * type_size;
        let value_offset = if data_size <= 4 {
            entry_offset + 8
        } else {
            read_u32(tiff_data, entry_offset + 8) as usize
        };

        if value_offset + data_size > tiff_data.len() {
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

    let mut offset = 2;

    while offset + 2 <= data.len() {
        if data[offset] != MARKER_PREFIX {
            offset += 1;
            continue;
        }

        let marker = data[offset + 1];

        // Handle special markers
        if marker == EOI {
            result.push(MARKER_PREFIX);
            result.push(EOI);
            break;
        }

        if marker == 0x00 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
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

        // Start of Scan - copy everything after this point
        if marker == SOS {
            result.extend_from_slice(&data[offset..]);
            break;
        }

        // Keep non-metadata segments
        if !is_metadata_marker(marker) {
            result.extend_from_slice(&data[offset..offset + 2 + length]);
        }

        offset += 2 + length;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_metadata_marker() {
        assert!(is_metadata_marker(APP1));
        assert!(is_metadata_marker(COM));
        assert!(!is_metadata_marker(APP0));
        assert!(!is_metadata_marker(SOI));
    }
}
