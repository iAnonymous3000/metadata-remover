use crate::{MetadataEntry, MetadataInfo};

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

// Critical chunks that must be preserved
const CRITICAL_CHUNKS: [&[u8; 4]; 4] = [b"IHDR", b"PLTE", b"IDAT", b"IEND"];

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    ((data[offset] as u32) << 24)
        | ((data[offset + 1] as u32) << 16)
        | ((data[offset + 2] as u32) << 8)
        | (data[offset + 3] as u32)
}

fn is_critical_chunk(chunk_type: &[u8]) -> bool {
    for critical in &CRITICAL_CHUNKS {
        if chunk_type == *critical {
            return true;
        }
    }
    false
}

fn chunk_type_to_string(chunk_type: &[u8]) -> String {
    String::from_utf8_lossy(chunk_type).to_string()
}

fn decode_latin1(data: &[u8]) -> String {
    data.iter()
        .filter(|&&b| b >= 0x20 && b != 0x7F)
        .map(|&b| b as char)
        .collect()
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;

    if data.len() < 8 || &data[0..8] != PNG_SIGNATURE {
        return MetadataInfo {
            file_type: "png".to_string(),
            metadata_found: entries,
            total_metadata_bytes: 0,
        };
    }

    let mut offset = 8;

    while offset + 12 <= data.len() {
        let length = read_u32_be(data, offset) as usize;
        let chunk_type = &data[offset + 4..offset + 8];

        if offset + 12 + length > data.len() {
            break;
        }

        let chunk_data = &data[offset + 8..offset + 8 + length];

        match chunk_type {
            b"tEXt" => {
                total_bytes += length + 12;
                // Format: keyword\0value
                if let Some(null_pos) = chunk_data.iter().position(|&b| b == 0) {
                    let keyword = decode_latin1(&chunk_data[..null_pos]);
                    let value = decode_latin1(&chunk_data[null_pos + 1..]);
                    entries.push(MetadataEntry {
                        category: "Text".to_string(),
                        name: keyword,
                        value: if value.len() > 100 {
                            format!("{}...", &value[..100])
                        } else {
                            value
                        },
                    });
                }
            }
            b"zTXt" => {
                total_bytes += length + 12;
                if let Some(null_pos) = chunk_data.iter().position(|&b| b == 0) {
                    let keyword = decode_latin1(&chunk_data[..null_pos]);
                    entries.push(MetadataEntry {
                        category: "Text".to_string(),
                        name: keyword,
                        value: format!("[compressed, {} bytes]", length - null_pos - 2),
                    });
                }
            }
            b"iTXt" => {
                total_bytes += length + 12;
                if let Some(null_pos) = chunk_data.iter().position(|&b| b == 0) {
                    let keyword = decode_latin1(&chunk_data[..null_pos]);
                    entries.push(MetadataEntry {
                        category: "Text".to_string(),
                        name: keyword,
                        value: format!("[international text, {} bytes]", length),
                    });
                }
            }
            b"eXIf" => {
                total_bytes += length + 12;
                entries.push(MetadataEntry {
                    category: "EXIF".to_string(),
                    name: "EXIF Data".to_string(),
                    value: format!("{} bytes", length),
                });
            }
            b"tIME" => {
                total_bytes += length + 12;
                if length >= 7 {
                    let year = ((chunk_data[0] as u16) << 8) | (chunk_data[1] as u16);
                    let month = chunk_data[2];
                    let day = chunk_data[3];
                    let hour = chunk_data[4];
                    let minute = chunk_data[5];
                    let second = chunk_data[6];
                    entries.push(MetadataEntry {
                        category: "Time".to_string(),
                        name: "Last Modified".to_string(),
                        value: format!(
                            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                            year, month, day, hour, minute, second
                        ),
                    });
                }
            }
            b"iCCP" => {
                total_bytes += length + 12;
                if let Some(null_pos) = chunk_data.iter().position(|&b| b == 0) {
                    let profile_name = decode_latin1(&chunk_data[..null_pos]);
                    entries.push(MetadataEntry {
                        category: "ICC".to_string(),
                        name: "ICC Profile".to_string(),
                        value: profile_name,
                    });
                }
            }
            b"sRGB" => {
                total_bytes += length + 12;
                let intent = if !chunk_data.is_empty() {
                    match chunk_data[0] {
                        0 => "Perceptual",
                        1 => "Relative colorimetric",
                        2 => "Saturation",
                        3 => "Absolute colorimetric",
                        _ => "Unknown",
                    }
                } else {
                    "Unknown"
                };
                entries.push(MetadataEntry {
                    category: "Color".to_string(),
                    name: "sRGB".to_string(),
                    value: intent.to_string(),
                });
            }
            b"gAMA" => {
                total_bytes += length + 12;
                if length >= 4 {
                    let gamma = read_u32_be(chunk_data, 0) as f64 / 100000.0;
                    entries.push(MetadataEntry {
                        category: "Color".to_string(),
                        name: "Gamma".to_string(),
                        value: format!("{:.4}", gamma),
                    });
                }
            }
            b"cHRM" => {
                total_bytes += length + 12;
                entries.push(MetadataEntry {
                    category: "Color".to_string(),
                    name: "Chromaticity".to_string(),
                    value: format!("{} bytes", length),
                });
            }
            b"pHYs" => {
                total_bytes += length + 12;
                if length >= 9 {
                    let ppux = read_u32_be(chunk_data, 0);
                    let ppuy = read_u32_be(chunk_data, 4);
                    let unit = chunk_data[8];
                    let unit_str = if unit == 1 { "meter" } else { "unknown" };
                    entries.push(MetadataEntry {
                        category: "Physical".to_string(),
                        name: "Pixel Dimensions".to_string(),
                        value: format!("{}x{} per {}", ppux, ppuy, unit_str),
                    });
                }
            }
            _ => {
                // Check for other ancillary (lowercase first letter) non-critical chunks
                if !is_critical_chunk(chunk_type) && chunk_type[0] >= b'a' {
                    total_bytes += length + 12;
                    entries.push(MetadataEntry {
                        category: "Other".to_string(),
                        name: chunk_type_to_string(chunk_type),
                        value: format!("{} bytes", length),
                    });
                }
            }
        }

        // Move to next chunk (length + type + data + CRC)
        offset += 12 + length;
    }

    MetadataInfo {
        file_type: "png".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 8 || &data[0..8] != PNG_SIGNATURE {
        return Err("Not a valid PNG file".to_string());
    }

    let mut result = Vec::with_capacity(data.len());

    // Write PNG signature
    result.extend_from_slice(&PNG_SIGNATURE);

    let mut offset = 8;

    while offset + 12 <= data.len() {
        let length = read_u32_be(data, offset) as usize;
        let chunk_type = &data[offset + 4..offset + 8];

        if offset + 12 + length > data.len() {
            return Err("Invalid chunk length".to_string());
        }

        // Total chunk size: length(4) + type(4) + data(length) + CRC(4)
        let chunk_size = 12 + length;

        // Only keep critical chunks
        if is_critical_chunk(chunk_type) {
            result.extend_from_slice(&data[offset..offset + chunk_size]);
        }

        offset += chunk_size;

        // Stop after IEND
        if chunk_type == b"IEND" {
            break;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_critical_chunk() {
        assert!(is_critical_chunk(b"IHDR"));
        assert!(is_critical_chunk(b"IDAT"));
        assert!(is_critical_chunk(b"IEND"));
        assert!(!is_critical_chunk(b"tEXt"));
        assert!(!is_critical_chunk(b"eXIf"));
    }
}
