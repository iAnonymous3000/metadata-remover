use crate::{MetadataEntry, MetadataInfo};

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const WEBP_MAGIC: &[u8; 4] = b"WEBP";

const VP8X_FLAG_EXIF: u8 = 0x08;
const VP8X_FLAG_XMP: u8 = 0x04;

fn is_webp(data: &[u8]) -> bool {
    data.len() >= 12 && &data[0..4] == RIFF_MAGIC && &data[8..12] == WEBP_MAGIC
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(
        (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24),
    )
}

fn declared_riff_end(data: &[u8]) -> Result<usize, String> {
    if !is_webp(data) {
        return Err("Not a valid WebP file".to_string());
    }

    let riff_size =
        read_u32_le(data, 4).ok_or_else(|| "Not a valid WebP file".to_string())? as usize;
    if riff_size < WEBP_MAGIC.len() {
        return Err("Invalid WebP RIFF size".to_string());
    }

    let riff_end = 8usize
        .checked_add(riff_size)
        .ok_or_else(|| "Invalid WebP RIFF size".to_string())?;
    if riff_end > data.len() {
        return Err("Truncated WebP RIFF data".to_string());
    }

    Ok(riff_end)
}

fn checked_chunk_bounds(offset: usize, data: &[u8], limit: usize) -> Option<(usize, usize, usize)> {
    let size = read_u32_le(data, offset + 4)? as usize;
    let data_start = offset.checked_add(8)?;
    let data_end = data_start.checked_add(size)?;
    let padded_end = data_end.checked_add(size & 1)?;

    if padded_end <= limit && padded_end <= data.len() {
        Some((data_start, data_end, padded_end))
    } else {
        None
    }
}

fn chunk_name(chunk_type: &[u8]) -> String {
    String::from_utf8_lossy(chunk_type).trim().to_string()
}

fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn is_content_credentials_chunk(chunk_type: &[u8], chunk_data: &[u8]) -> bool {
    matches!(chunk_type, b"JUMB" | b"jumb")
        || contains_ascii_ci(chunk_data, b"c2pa")
        || contains_ascii_ci(chunk_data, b"jumb")
        || contains_ascii_ci(chunk_data, b"Content Credentials")
}

fn is_visual_chunk(chunk_type: &[u8]) -> bool {
    matches!(
        chunk_type,
        b"VP8 " | b"VP8L" | b"VP8X" | b"ALPH" | b"ANIM" | b"ANMF" | b"ICCP"
    )
}

fn is_image_data_chunk(chunk_type: &[u8]) -> bool {
    matches!(chunk_type, b"VP8 " | b"VP8L" | b"ANMF")
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

    if !is_webp(data) {
        return MetadataInfo {
            file_type: "webp".to_string(),
            metadata_found: entries,
            total_metadata_bytes: 0,
        };
    }

    let riff_end = declared_riff_end(data).unwrap_or(data.len());
    let mut offset = 12;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = checked_chunk_bounds(offset, data, riff_end)
        else {
            break;
        };
        let chunk_data_len = data_end - data_start;
        let chunk_total_len = padded_end - offset;

        match chunk_type {
            _ if !is_visual_chunk(chunk_type)
                && is_content_credentials_chunk(chunk_type, &data[data_start..data_end]) =>
            {
                total_bytes += chunk_total_len;
                entries.push(MetadataEntry {
                    category: "Content Credentials".to_string(),
                    name: chunk_name(chunk_type),
                    value: format!("{} bytes", chunk_data_len),
                });
            }
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

    if offset < riff_end {
        let trailing_len = riff_end - offset;
        total_bytes += trailing_len;
        entries.push(MetadataEntry {
            category: "Trailing".to_string(),
            name: "Malformed RIFF Data".to_string(),
            value: format!("{} bytes", trailing_len),
        });
    }

    if riff_end < data.len() {
        let trailing_len = data.len() - riff_end;
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
    let riff_end = declared_riff_end(data)?;

    let mut result = Vec::with_capacity(data.len());
    result.extend_from_slice(RIFF_MAGIC);
    result.extend_from_slice(&0u32.to_le_bytes());
    result.extend_from_slice(WEBP_MAGIC);

    let mut offset = 12;
    let mut has_image_data = false;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = checked_chunk_bounds(offset, data, riff_end)
        else {
            return Err("Invalid WebP chunk length".to_string());
        };
        let chunk_data = &data[data_start..data_end];
        has_image_data |= is_image_data_chunk(chunk_type);

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

    if offset != riff_end {
        return Err("Invalid WebP chunk length".to_string());
    }
    if !has_image_data {
        return Err("WebP missing image data chunk".to_string());
    }

    let riff_size = u32::try_from(result.len().saturating_sub(8))
        .map_err(|_| "Cleaned WebP file is too large".to_string())?;
    result[4..8].copy_from_slice(&riff_size.to_le_bytes());

    Ok(result)
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    let riff_end = declared_riff_end(data)?;

    let mut offset = 12;
    let mut has_image_data = false;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((_, _, padded_end)) = checked_chunk_bounds(offset, data, riff_end) else {
            return Err("Invalid WebP chunk length".to_string());
        };
        has_image_data |= is_image_data_chunk(chunk_type);
        offset = padded_end;
    }

    if offset != riff_end {
        return Err("Invalid WebP chunk length".to_string());
    }
    if !has_image_data {
        return Err("WebP missing image data chunk".to_string());
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
    fn test_extract_and_remove_content_credentials_chunk() {
        let input = webp(&[
            chunk(b"JUMB", b"c2pa Content Credentials manifest"),
            chunk(b"VP8 ", b"image"),
        ]);

        let info = extract_metadata(&input);
        assert_eq!(info.metadata_found.len(), 1);
        assert_eq!(info.metadata_found[0].category, "Content Credentials");
        assert_eq!(info.metadata_found[0].name, "JUMB");

        let cleaned = remove_metadata(&input).unwrap();
        assert!(!cleaned.windows(b"c2pa".len()).any(|w| w == b"c2pa"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_visual_chunks_are_not_content_credentials_false_positives() {
        let input = webp(&[chunk(b"VP8 ", b"visual bytes mention c2pa but are pixels")]);

        assert!(extract_metadata(&input).metadata_found.is_empty());
        let cleaned = remove_metadata(&input).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
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

    #[test]
    fn test_validate_rejects_webp_without_image_data() {
        let input = webp(&[chunk(b"EXIF", b"camera")]);

        assert_eq!(
            validate(&input),
            Err("WebP missing image data chunk".to_string())
        );
        assert_eq!(
            remove_metadata(&input),
            Err("WebP missing image data chunk".to_string())
        );
    }

    #[test]
    fn test_validate_rejects_truncated_declared_riff_data() {
        let mut input = Vec::new();
        input.extend_from_slice(RIFF_MAGIC);
        input.extend_from_slice(&20u32.to_le_bytes());
        input.extend_from_slice(WEBP_MAGIC);
        input.extend_from_slice(b"VP8 ");

        assert_eq!(
            validate(&input),
            Err("Truncated WebP RIFF data".to_string())
        );
    }

    #[test]
    fn test_extract_metadata_uses_declared_riff_boundary_for_trailing_data() {
        let mut input = webp(&[chunk(b"VP8 ", b"image")]);
        input.extend_from_slice(&chunk(b"EXIF", b"camera"));

        let info = extract_metadata(&input);

        assert_eq!(info.metadata_found.len(), 1);
        assert_eq!(info.metadata_found[0].category, "Trailing");
        assert_eq!(info.metadata_found[0].name, "Trailing Data");
    }
}
