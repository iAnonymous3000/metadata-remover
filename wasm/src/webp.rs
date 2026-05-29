use crate::{MetadataEntry, MetadataInfo};

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const WEBP_MAGIC: &[u8; 4] = b"WEBP";

const VP8X_FLAG_EXIF: u8 = 0x08;
const VP8X_FLAG_XMP: u8 = 0x04;

fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(
        (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24),
    )
}

fn checked_chunk_bounds(offset: usize, data: &[u8]) -> Option<(usize, usize, usize)> {
    let size = read_u32_le(data, offset + 4)? as usize;
    let data_start = offset.checked_add(8)?;
    let data_end = data_start.checked_add(size)?;
    let padded_end = data_end.checked_add(size & 1)?;

    if padded_end <= data.len() {
        Some((data_start, data_end, padded_end))
    } else {
        None
    }
}

fn chunk_name(chunk_type: &[u8]) -> String {
    String::from_utf8_lossy(chunk_type).trim().to_string()
}

fn is_visual_chunk(chunk_type: &[u8]) -> bool {
    matches!(
        chunk_type,
        b"VP8 " | b"VP8L" | b"VP8X" | b"ALPH" | b"ANIM" | b"ANMF" | b"ICCP"
    )
}

fn write_chunk(result: &mut Vec<u8>, chunk_type: &[u8], chunk_data: &[u8]) -> Result<(), String> {
    let size =
        u32::try_from(chunk_data.len()).map_err(|_| "WebP chunk is too large".to_string())?;
    result.extend_from_slice(chunk_type);
    result.extend_from_slice(&size.to_le_bytes());
    result.extend_from_slice(chunk_data);
    if chunk_data.len() % 2 == 1 {
        result.push(0);
    }
    Ok(())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;

    if data.len() < 12 || &data[0..4] != RIFF_MAGIC || &data[8..12] != WEBP_MAGIC {
        return MetadataInfo {
            file_type: "webp".to_string(),
            metadata_found: entries,
            total_metadata_bytes: 0,
        };
    }

    let mut offset = 12;
    while offset + 8 <= data.len() {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = checked_chunk_bounds(offset, data) else {
            break;
        };
        let chunk_data_len = data_end - data_start;
        let chunk_total_len = padded_end - offset;

        match chunk_type {
            b"EXIF" => {
                total_bytes += chunk_total_len;
                entries.push(MetadataEntry {
                    category: "EXIF".to_string(),
                    name: "EXIF Data".to_string(),
                    value: format!("{} bytes", chunk_data_len),
                });
            }
            b"XMP " => {
                total_bytes += chunk_total_len;
                entries.push(MetadataEntry {
                    category: "XMP".to_string(),
                    name: "XMP Data".to_string(),
                    value: format!("{} bytes", chunk_data_len),
                });
            }
            _ if !is_visual_chunk(chunk_type) => {
                total_bytes += chunk_total_len;
                entries.push(MetadataEntry {
                    category: "Other".to_string(),
                    name: chunk_name(chunk_type),
                    value: format!("{} bytes", chunk_data_len),
                });
            }
            _ => {}
        }

        offset = padded_end;
    }

    if offset < data.len() {
        let trailing_len = data.len() - offset;
        total_bytes += trailing_len;
        entries.push(MetadataEntry {
            category: "Trailing".to_string(),
            name: "Trailing Data".to_string(),
            value: format!("{} bytes", trailing_len),
        });
    }

    MetadataInfo {
        file_type: "webp".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 12 || &data[0..4] != RIFF_MAGIC || &data[8..12] != WEBP_MAGIC {
        return Err("Not a valid WebP file".to_string());
    }

    let mut result = Vec::with_capacity(data.len());
    result.extend_from_slice(RIFF_MAGIC);
    result.extend_from_slice(&0u32.to_le_bytes());
    result.extend_from_slice(WEBP_MAGIC);

    let mut offset = 12;
    while offset + 8 <= data.len() {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = checked_chunk_bounds(offset, data) else {
            break;
        };
        let chunk_data = &data[data_start..data_end];

        if is_visual_chunk(chunk_type) {
            if chunk_type == b"VP8X" && !chunk_data.is_empty() {
                let mut vp8x = chunk_data.to_vec();
                vp8x[0] &= !(VP8X_FLAG_EXIF | VP8X_FLAG_XMP);
                write_chunk(&mut result, chunk_type, &vp8x)?;
            } else {
                write_chunk(&mut result, chunk_type, chunk_data)?;
            }
        }

        offset = padded_end;
    }

    let riff_size = u32::try_from(result.len().saturating_sub(8))
        .map_err(|_| "Cleaned WebP file is too large".to_string())?;
    result[4..8].copy_from_slice(&riff_size.to_le_bytes());

    Ok(result)
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    if data.len() < 12 || &data[0..4] != RIFF_MAGIC || &data[8..12] != WEBP_MAGIC {
        return Err("Not a valid WebP file".to_string());
    }

    let mut offset = 12;
    while offset + 8 <= data.len() {
        let Some((_, _, padded_end)) = checked_chunk_bounds(offset, data) else {
            break;
        };
        offset = padded_end;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(chunk_type);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn webp(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(RIFF_MAGIC);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(WEBP_MAGIC);
        for chunk in chunks {
            out.extend_from_slice(chunk);
        }
        let riff_size = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff_size.to_le_bytes());
        out
    }

    #[test]
    fn test_extract_metadata_finds_exif_xmp_and_unknown_chunks() {
        let input = webp(&[
            chunk(
                b"VP8X",
                &[VP8X_FLAG_EXIF | VP8X_FLAG_XMP, 0, 0, 0, 1, 0, 0, 1, 0, 0],
            ),
            chunk(b"EXIF", b"camera"),
            chunk(b"XMP ", b"xpacket"),
            chunk(b"abcd", b"private"),
            chunk(b"VP8 ", b"image"),
        ]);

        let info = extract_metadata(&input);

        assert_eq!(info.file_type, "webp");
        assert_eq!(info.metadata_found.len(), 3);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "EXIF"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "XMP"));
        assert!(info.metadata_found.iter().any(|entry| entry.name == "abcd"));
    }

    #[test]
    fn test_remove_metadata_preserves_visual_chunks_and_updates_vp8x() {
        let input = webp(&[
            chunk(
                b"VP8X",
                &[
                    VP8X_FLAG_EXIF | VP8X_FLAG_XMP | 0x10,
                    0,
                    0,
                    0,
                    1,
                    0,
                    0,
                    1,
                    0,
                    0,
                ],
            ),
            chunk(b"ICCP", b"profile"),
            chunk(b"EXIF", b"camera"),
            chunk(b"XMP ", b"xpacket"),
            chunk(b"abcd", b"private"),
            chunk(b"VP8 ", b"image"),
        ]);

        let cleaned = remove_metadata(&input).unwrap();
        let info = extract_metadata(&cleaned);

        assert!(info.metadata_found.is_empty());
        assert!(cleaned.windows(b"profile".len()).any(|w| w == b"profile"));
        assert!(cleaned.windows(b"image".len()).any(|w| w == b"image"));
        assert!(!cleaned.windows(b"camera".len()).any(|w| w == b"camera"));
        assert!(!cleaned.windows(b"xpacket".len()).any(|w| w == b"xpacket"));
        assert!(!cleaned.windows(b"private".len()).any(|w| w == b"private"));

        let vp8x_data_start = 12 + 8;
        assert_eq!(
            cleaned[vp8x_data_start] & (VP8X_FLAG_EXIF | VP8X_FLAG_XMP),
            0
        );
        assert_eq!(
            read_u32_le(&cleaned, 4).unwrap() as usize,
            cleaned.len() - 8
        );
    }

    #[test]
    fn test_remove_metadata_strips_trailing_data_shorter_than_chunk_header() {
        let mut input = webp(&[chunk(b"VP8 ", b"image")]);
        input.extend_from_slice(b"tail");

        let cleaned = remove_metadata(&input).unwrap();
        let info = extract_metadata(&cleaned);

        assert_eq!(extract_metadata(&input).metadata_found.len(), 1);
        assert!(info.metadata_found.is_empty());
        assert!(!cleaned.windows(b"tail".len()).any(|w| w == b"tail"));
    }

    #[test]
    fn test_remove_metadata_strips_trailing_data_that_looks_like_chunk_header() {
        let mut input = webp(&[chunk(b"VP8 ", b"image")]);
        input.extend_from_slice(b"TRACKING");

        assert!(validate(&input).is_ok());

        let cleaned = remove_metadata(&input).unwrap();
        let info = extract_metadata(&cleaned);

        assert_eq!(extract_metadata(&input).metadata_found.len(), 1);
        assert!(info.metadata_found.is_empty());
        assert!(!cleaned.windows(b"TRACKING".len()).any(|w| w == b"TRACKING"));
    }
}
