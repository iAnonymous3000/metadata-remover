use crate::{MetadataEntry, MetadataInfo};

const GIF87A: &[u8; 6] = b"GIF87a";
const GIF89A: &[u8; 6] = b"GIF89a";

const IMAGE_SEPARATOR: u8 = 0x2C;
const EXTENSION_INTRODUCER: u8 = 0x21;
const TRAILER: u8 = 0x3B;

const GRAPHIC_CONTROL_LABEL: u8 = 0xF9;
const COMMENT_LABEL: u8 = 0xFE;
const PLAIN_TEXT_LABEL: u8 = 0x01;
const APPLICATION_LABEL: u8 = 0xFF;

const NETSCAPE_LOOP_ID: &[u8; 11] = b"NETSCAPE2.0";
const ANIMEXTS_LOOP_ID: &[u8; 11] = b"ANIMEXTS1.0";
const XMP_APP_ID: &[u8; 11] = b"XMP DataXMP";

fn is_gif(data: &[u8]) -> bool {
    data.len() >= 6 && (&data[0..6] == GIF87A || &data[0..6] == GIF89A)
}

fn color_table_len(packed: u8) -> usize {
    if packed & 0x80 == 0 {
        0
    } else {
        3 * (1usize << ((packed & 0x07) + 1))
    }
}

fn logical_screen_end(data: &[u8]) -> Option<usize> {
    if !is_gif(data) || data.len() < 13 {
        return None;
    }

    13usize
        .checked_add(color_table_len(data[10]))
        .filter(|end| *end <= data.len())
}

fn sub_blocks_end(data: &[u8], mut offset: usize) -> Option<(usize, usize)> {
    let mut payload_len = 0usize;

    loop {
        let size = *data.get(offset)? as usize;
        offset = offset.checked_add(1)?;
        if size == 0 {
            return Some((offset, payload_len));
        }
        payload_len = payload_len.checked_add(size)?;
        offset = offset.checked_add(size)?;
        if offset > data.len() {
            return None;
        }
    }
}

fn image_block_end(data: &[u8], offset: usize) -> Option<usize> {
    let descriptor_end = offset.checked_add(10)?;
    if descriptor_end > data.len() {
        return None;
    }

    let local_color_table_end = descriptor_end.checked_add(color_table_len(data[offset + 9]))?;
    if local_color_table_end >= data.len() {
        return None;
    }

    let image_data_start = local_color_table_end.checked_add(1)?;
    let (end, _) = sub_blocks_end(data, image_data_start)?;
    Some(end)
}

fn extension_end(data: &[u8], offset: usize) -> Option<(usize, usize)> {
    if data.get(offset) != Some(&EXTENSION_INTRODUCER) {
        return None;
    }

    let label = *data.get(offset + 1)?;
    match label {
        GRAPHIC_CONTROL_LABEL => {
            let block_size = *data.get(offset + 2)? as usize;
            let end = offset
                .checked_add(3)?
                .checked_add(block_size)?
                .checked_add(1)?;
            if end <= data.len() && data[end - 1] == 0 {
                Some((end, block_size))
            } else {
                None
            }
        }
        APPLICATION_LABEL => {
            let block_size = *data.get(offset + 2)? as usize;
            let sub_blocks_start = offset.checked_add(3)?.checked_add(block_size)?;
            if sub_blocks_start > data.len() {
                return None;
            }
            let (end, payload_len) = sub_blocks_end(data, sub_blocks_start)?;
            Some((end, block_size + payload_len))
        }
        COMMENT_LABEL | PLAIN_TEXT_LABEL => {
            let (end, payload_len) = sub_blocks_end(data, offset + 2)?;
            Some((end, payload_len))
        }
        _ => {
            let (end, payload_len) = sub_blocks_end(data, offset + 2)?;
            Some((end, payload_len))
        }
    }
}

fn application_id(data: &[u8], offset: usize) -> Option<&[u8]> {
    if data.get(offset) != Some(&EXTENSION_INTRODUCER)
        || data.get(offset + 1) != Some(&APPLICATION_LABEL)
        || data.get(offset + 2) != Some(&11)
    {
        return None;
    }

    data.get(offset + 3..offset + 14)
}

fn application_name(app_id: &[u8]) -> String {
    String::from_utf8_lossy(app_id).trim().to_string()
}

fn should_preserve_application(app_id: &[u8]) -> bool {
    app_id == NETSCAPE_LOOP_ID || app_id == ANIMEXTS_LOOP_ID
}

fn extract_comment(data: &[u8], offset: usize, end: usize) -> String {
    let mut bytes = Vec::new();
    let mut pos = offset + 2;

    while pos < end {
        let size = data[pos] as usize;
        pos += 1;
        if size == 0 || pos + size > end {
            break;
        }
        bytes.extend_from_slice(&data[pos..pos + size]);
        pos += size;
    }

    let text: String = bytes
        .iter()
        .filter(|&&b| b >= 0x20 && b != 0x7F)
        .map(|&b| b as char)
        .collect();

    if text.chars().count() > 100 {
        format!("{}...", text.chars().take(100).collect::<String>())
    } else if text.is_empty() {
        format!("{} bytes", bytes.len())
    } else {
        text
    }
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;

    let Some(mut offset) = logical_screen_end(data) else {
        return MetadataInfo {
            file_type: "gif".to_string(),
            metadata_found: entries,
            total_metadata_bytes: 0,
        };
    };

    while offset < data.len() {
        match data[offset] {
            IMAGE_SEPARATOR => {
                let Some(end) = image_block_end(data, offset) else {
                    break;
                };
                offset = end;
            }
            EXTENSION_INTRODUCER => {
                let Some(label) = data.get(offset + 1).copied() else {
                    break;
                };
                let Some((end, payload_len)) = extension_end(data, offset) else {
                    break;
                };

                match label {
                    COMMENT_LABEL => {
                        total_bytes += end - offset;
                        entries.push(MetadataEntry {
                            category: "Comment".to_string(),
                            name: "Comment".to_string(),
                            value: extract_comment(data, offset, end),
                        });
                    }
                    APPLICATION_LABEL => {
                        let app_id = application_id(data, offset);
                        if app_id == Some(XMP_APP_ID) {
                            total_bytes += end - offset;
                            entries.push(MetadataEntry {
                                category: "XMP".to_string(),
                                name: "XMP Data".to_string(),
                                value: format!("{} bytes", payload_len.saturating_sub(11)),
                            });
                        } else if app_id
                            .map(|id| !should_preserve_application(id))
                            .unwrap_or(true)
                        {
                            total_bytes += end - offset;
                            entries.push(MetadataEntry {
                                category: "Application".to_string(),
                                name: app_id
                                    .map(application_name)
                                    .unwrap_or_else(|| "Unknown Application Extension".to_string()),
                                value: format!("{} bytes", payload_len),
                            });
                        }
                    }
                    label if label != GRAPHIC_CONTROL_LABEL && label != PLAIN_TEXT_LABEL => {
                        total_bytes += end - offset;
                        entries.push(MetadataEntry {
                            category: "Extension".to_string(),
                            name: format!("Extension 0x{:02X}", label),
                            value: format!("{} bytes", payload_len),
                        });
                    }
                    _ => {}
                }

                offset = end;
            }
            TRAILER => {
                if offset + 1 < data.len() {
                    let trailing_len = data.len() - offset - 1;
                    total_bytes += trailing_len;
                    entries.push(MetadataEntry {
                        category: "Trailing".to_string(),
                        name: "Trailing Data".to_string(),
                        value: format!("{} bytes", trailing_len),
                    });
                }
                break;
            }
            _ => break,
        }
    }

    MetadataInfo {
        file_type: "gif".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let Some(mut offset) = logical_screen_end(data) else {
        return Err("Not a valid GIF file".to_string());
    };

    let mut result = Vec::with_capacity(data.len());
    result.extend_from_slice(&data[..offset]);

    while offset < data.len() {
        match data[offset] {
            IMAGE_SEPARATOR => {
                let end = image_block_end(data, offset)
                    .ok_or_else(|| "Invalid GIF image block".to_string())?;
                result.extend_from_slice(&data[offset..end]);
                offset = end;
            }
            EXTENSION_INTRODUCER => {
                let label = *data
                    .get(offset + 1)
                    .ok_or_else(|| "Invalid GIF extension block".to_string())?;
                let (end, _) = extension_end(data, offset)
                    .ok_or_else(|| "Invalid GIF extension block".to_string())?;

                let preserve = match label {
                    GRAPHIC_CONTROL_LABEL | PLAIN_TEXT_LABEL => true,
                    APPLICATION_LABEL => application_id(data, offset)
                        .map(should_preserve_application)
                        .unwrap_or(false),
                    _ => false,
                };

                if preserve {
                    result.extend_from_slice(&data[offset..end]);
                }

                offset = end;
            }
            TRAILER => {
                result.push(TRAILER);
                return Ok(result);
            }
            _ => return Err("Invalid GIF block".to_string()),
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub_blocks(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(data.len() as u8);
        out.extend_from_slice(data);
        out.push(0);
        out
    }

    fn base_gif(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(GIF89A);
        out.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0]);
        for block in blocks {
            out.extend_from_slice(block);
        }
        out.push(TRAILER);
        out
    }

    fn app_extension(app_id: &[u8; 11], payload: &[u8]) -> Vec<u8> {
        let mut out = vec![EXTENSION_INTRODUCER, APPLICATION_LABEL, 11];
        out.extend_from_slice(app_id);
        out.extend_from_slice(&sub_blocks(payload));
        out
    }

    #[test]
    fn test_extract_metadata_finds_comment_xmp_and_unknown_app_extensions() {
        let comment = {
            let mut block = vec![EXTENSION_INTRODUCER, COMMENT_LABEL];
            block.extend_from_slice(&sub_blocks(b"private note"));
            block
        };
        let xmp = app_extension(XMP_APP_ID, b"xpacket");
        let unknown = app_extension(b"PRIVATEAPP1", b"secret");
        let loop_ext = app_extension(NETSCAPE_LOOP_ID, &[1, 0, 0]);
        let input = base_gif(&[comment, xmp, unknown, loop_ext]);

        let info = extract_metadata(&input);

        assert_eq!(info.file_type, "gif");
        assert_eq!(info.metadata_found.len(), 3);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Comment"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "XMP"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "PRIVATEAPP1"));
    }

    #[test]
    fn test_remove_metadata_preserves_animation_loop_and_image_data() {
        let comment = {
            let mut block = vec![EXTENSION_INTRODUCER, COMMENT_LABEL];
            block.extend_from_slice(&sub_blocks(b"private note"));
            block
        };
        let loop_ext = app_extension(NETSCAPE_LOOP_ID, &[1, 0, 0]);
        let image = {
            let mut block = vec![IMAGE_SEPARATOR, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2];
            block.extend_from_slice(&sub_blocks(&[0x4C, 0x01]));
            block
        };
        let input = base_gif(&[comment, loop_ext, image]);

        let cleaned = remove_metadata(&input).unwrap();
        let info = extract_metadata(&cleaned);

        assert!(info.metadata_found.is_empty());
        assert!(cleaned
            .windows(NETSCAPE_LOOP_ID.len())
            .any(|w| w == NETSCAPE_LOOP_ID));
        assert!(cleaned
            .windows([0x4C, 0x01].len())
            .any(|w| w == [0x4C, 0x01]));
        assert!(!cleaned
            .windows(b"private note".len())
            .any(|w| w == b"private note"));
        assert_eq!(*cleaned.last().unwrap(), TRAILER);
    }

    #[test]
    fn test_remove_metadata_strips_trailing_data_after_trailer() {
        let mut input = base_gif(&[]);
        input.extend_from_slice(b"tail");

        let cleaned = remove_metadata(&input).unwrap();
        let info = extract_metadata(&cleaned);

        assert_eq!(extract_metadata(&input).metadata_found.len(), 1);
        assert!(info.metadata_found.is_empty());
        assert_eq!(cleaned, base_gif(&[]));
    }
}
