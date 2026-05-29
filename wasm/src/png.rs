use crate::{MetadataEntry, MetadataInfo};

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

// Critical chunks and visual-state chunks that must be preserved.
const CRITICAL_CHUNKS: [&[u8; 4]; 4] = [b"IHDR", b"PLTE", b"IDAT", b"IEND"];
const VISUAL_CHUNKS: [&[u8; 4]; 5] = [b"tRNS", b"sRGB", b"gAMA", b"cHRM", b"iCCP"];
const APNG_CHUNKS: [&[u8; 4]; 3] = [b"acTL", b"fcTL", b"fdAT"];

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    ((data[offset] as u32) << 24)
        | ((data[offset + 1] as u32) << 16)
        | ((data[offset + 2] as u32) << 8)
        | (data[offset + 3] as u32)
}

fn is_critical_chunk(chunk_type: &[u8]) -> bool {
    CRITICAL_CHUNKS
        .iter()
        .any(|critical| chunk_type == *critical)
}

fn is_preserved_chunk(chunk_type: &[u8]) -> bool {
    is_critical_chunk(chunk_type)
        || VISUAL_CHUNKS.iter().any(|chunk| chunk_type == *chunk)
        || APNG_CHUNKS.iter().any(|chunk| chunk_type == *chunk)
}

fn is_ancillary_chunk(chunk_type: &[u8]) -> bool {
    chunk_type.len() == 4 && (chunk_type[0] & 0x20) != 0
}

fn checked_chunk_end(offset: usize, length: usize, data_len: usize) -> Option<usize> {
    offset
        .checked_add(12)?
        .checked_add(length)
        .filter(|end| *end <= data_len)
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

fn truncate_for_display(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        value.to_string()
    }
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;

    if data.len() < 8 || data[0..8] != PNG_SIGNATURE {
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

        let Some(chunk_end) = checked_chunk_end(offset, length, data.len()) else {
            break;
        };

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
                        value: truncate_for_display(&value, 100),
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
                        value: format!(
                            "[compressed, {} bytes]",
                            length.saturating_sub(null_pos + 2)
                        ),
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
                // ICC profiles affect color rendering, so they are preserved.
            }
            b"sRGB" => {
                // Color-management chunks are preserved to avoid changing appearance.
            }
            b"gAMA" => {
                // Color-management chunks are preserved to avoid changing appearance.
            }
            b"cHRM" => {
                // Color-management chunks are preserved to avoid changing appearance.
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
                if is_ancillary_chunk(chunk_type) && !is_preserved_chunk(chunk_type) {
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
        offset = chunk_end;
    }

    MetadataInfo {
        file_type: "png".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 8 || data[0..8] != PNG_SIGNATURE {
        return Err("Not a valid PNG file".to_string());
    }

    let mut result = Vec::with_capacity(data.len());

    // Write PNG signature
    result.extend_from_slice(&PNG_SIGNATURE);

    let mut offset = 8;
    let mut saw_iend = false;

    while offset + 12 <= data.len() {
        let length = read_u32_be(data, offset) as usize;
        let chunk_type = &data[offset + 4..offset + 8];

        let Some(chunk_end) = checked_chunk_end(offset, length, data.len()) else {
            return Err("Invalid chunk length".to_string());
        };

        // Total chunk size: length(4) + type(4) + data(length) + CRC(4)
        let chunk_size = chunk_end - offset;

        if is_preserved_chunk(chunk_type) {
            result.extend_from_slice(&data[offset..offset + chunk_size]);
        }

        offset = chunk_end;

        // Stop after IEND
        if chunk_type == b"IEND" {
            saw_iend = true;
            break;
        }
    }

    if !saw_iend {
        return Err("PNG missing IEND chunk".to_string());
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

    fn chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(chunk_type);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    #[test]
    fn test_extract_metadata_handles_non_ascii_long_text() {
        let mut text = b"Comment\0A".to_vec();
        text.extend(std::iter::repeat_n(0xFF, 120));

        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&chunk(b"tEXt", &text));
        png.extend_from_slice(&chunk(b"IEND", &[]));

        let info = extract_metadata(&png);

        assert_eq!(info.metadata_found.len(), 1);
        assert!(info.metadata_found[0].value.ends_with("..."));
    }

    #[test]
    fn test_remove_metadata_preserves_transparency_and_color_chunks() {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&chunk(b"IHDR", &[0; 13]));
        png.extend_from_slice(&chunk(b"tRNS", &[1, 2]));
        png.extend_from_slice(&chunk(b"sRGB", &[0]));
        png.extend_from_slice(&chunk(b"tEXt", b"Author\0Secret"));
        png.extend_from_slice(&chunk(b"IDAT", &[9, 9, 9]));
        png.extend_from_slice(&chunk(b"IEND", &[]));

        let cleaned = remove_metadata(&png).unwrap();

        assert!(cleaned.windows(4).any(|w| w == b"tRNS"));
        assert!(cleaned.windows(4).any(|w| w == b"sRGB"));
        assert!(!cleaned.windows(4).any(|w| w == b"tEXt"));
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
    }

    #[test]
    fn test_remove_metadata_preserves_apng_animation_chunks() {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&chunk(b"IHDR", &[0; 13]));
        png.extend_from_slice(&chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        png.extend_from_slice(&chunk(b"fcTL", &[0; 26]));
        png.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3]));
        png.extend_from_slice(&chunk(b"fdAT", &[0, 0, 0, 1, 4, 5, 6]));
        png.extend_from_slice(&chunk(b"tEXt", b"Author\0Secret"));
        png.extend_from_slice(&chunk(b"IEND", &[]));

        let info = extract_metadata(&png);
        let cleaned = remove_metadata(&png).unwrap();

        assert_eq!(info.metadata_found.len(), 1);
        assert!(cleaned.windows(4).any(|w| w == b"acTL"));
        assert!(cleaned.windows(4).any(|w| w == b"fcTL"));
        assert!(cleaned.windows(4).any(|w| w == b"fdAT"));
        assert!(!cleaned.windows(4).any(|w| w == b"tEXt"));
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
    }
}
