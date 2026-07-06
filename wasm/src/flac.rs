use crate::util::truncate_for_display;
use crate::{mp3, MetadataEntry, MetadataInfo};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const MAX_BLOCKS: usize = 1024;
const STREAMINFO_LEN: usize = 34;

const BLOCK_STREAMINFO: u8 = 0;
const BLOCK_PADDING: u8 = 1;
const BLOCK_APPLICATION: u8 = 2;
const BLOCK_SEEKTABLE: u8 = 3;
const BLOCK_VORBIS_COMMENT: u8 = 4;
const BLOCK_CUESHEET: u8 = 5;
const BLOCK_PICTURE: u8 = 6;

#[derive(Clone, Copy, Debug)]
struct FlacBlock {
    block_type: u8,
    payload_start: usize,
    payload_len: usize,
}

impl FlacBlock {
    fn end(&self) -> usize {
        self.payload_start + self.payload_len
    }
}

struct FlacLayout {
    id3v2_size: usize,
    blocks: Vec<FlacBlock>,
    audio_start: usize,
    audio_end: usize,
    tail_tags: Vec<mp3::TailTag>,
}

pub fn looks_like_flac(data: &[u8]) -> bool {
    flac_magic_offset(data).is_some()
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    parse_layout(data).map(|_| ())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;

    let Ok(layout) = parse_layout(data) else {
        return MetadataInfo {
            file_type: "flac".to_string(),
            metadata_found: entries,
            total_metadata_bytes: total_bytes,
        };
    };

    if layout.id3v2_size > 0 {
        total_bytes += layout.id3v2_size;
        entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: "ID3v2 tag prepended to FLAC".to_string(),
            value: format!("{} bytes", layout.id3v2_size),
        });
        if let Some(header) = mp3::parse_id3v2_header(data) {
            mp3::collect_id3v2_frames(data, header, &mut entries);
        }
    }

    for block in &layout.blocks {
        if !is_removed_block(block.block_type) {
            continue;
        }
        total_bytes += 4 + block.payload_len;
        let payload = &data[block.payload_start..block.end()];
        match block.block_type {
            BLOCK_VORBIS_COMMENT => collect_vorbis_comments(payload, &mut entries),
            BLOCK_PICTURE => entries.push(picture_entry(payload)),
            BLOCK_APPLICATION => entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: "Application block".to_string(),
                value: application_label(payload),
            }),
            BLOCK_CUESHEET => entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: "CD cue sheet".to_string(),
                value: format!("{} bytes; removed during FLAC cleaning", block.payload_len),
            }),
            _ => entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: format!("Metadata block type {}", block.block_type),
                value: format!("{} bytes; removed during FLAC cleaning", block.payload_len),
            }),
        }
    }

    for tag in &layout.tail_tags {
        total_bytes += tag.size();
        entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: tag.tag_name().to_string(),
            value: format!("{} bytes; removed during FLAC cleaning", tag.size()),
        });
        if tag.tag_name() == "ID3v1 tag" {
            mp3::collect_id3v1_fields(&data[tag.start()..tag.start() + tag.size()], &mut entries);
        }
    }

    MetadataInfo {
        file_type: "flac".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let layout = parse_layout(data)?;
    let kept: Vec<&FlacBlock> = layout
        .blocks
        .iter()
        .filter(|block| !is_removed_block(block.block_type))
        .collect();
    if kept.is_empty() || kept[0].block_type != BLOCK_STREAMINFO {
        return Err("FLAC missing STREAMINFO block".to_string());
    }

    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(FLAC_MAGIC);
    for (index, block) in kept.iter().enumerate() {
        let is_last = index == kept.len() - 1;
        let header_byte = block.block_type | if is_last { 0x80 } else { 0 };
        out.push(header_byte);
        let len = u32::try_from(block.payload_len)
            .map_err(|_| "Invalid FLAC block length".to_string())?;
        out.extend_from_slice(&len.to_be_bytes()[1..4]);
        out.extend_from_slice(&data[block.payload_start..block.end()]);
    }
    out.extend_from_slice(&data[layout.audio_start..layout.audio_end]);
    Ok(out)
}

fn flac_magic_offset(data: &[u8]) -> Option<usize> {
    if data.get(..FLAC_MAGIC.len()) == Some(FLAC_MAGIC) {
        return Some(0);
    }
    // Nonstandard but common: taggers prepend an ID3v2 tag to FLAC files.
    let id3_size = mp3::parse_id3v2_header(data)?.total_size();
    (data.get(id3_size..id3_size + FLAC_MAGIC.len()) == Some(FLAC_MAGIC)).then_some(id3_size)
}

fn parse_layout(data: &[u8]) -> Result<FlacLayout, String> {
    let id3v2_size = flac_magic_offset(data).ok_or_else(|| "Not a valid FLAC file".to_string())?;

    let mut blocks = Vec::new();
    let mut offset = id3v2_size + FLAC_MAGIC.len();
    loop {
        if blocks.len() >= MAX_BLOCKS {
            return Err("FLAC file contains too many metadata blocks".to_string());
        }
        let header = data
            .get(offset..offset + 4)
            .ok_or_else(|| "Truncated FLAC metadata block".to_string())?;
        let is_last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        if block_type == 127 {
            return Err("Invalid FLAC metadata block type".to_string());
        }
        let payload_len =
            ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | (header[3] as usize);
        let payload_start = offset + 4;
        let end = payload_start
            .checked_add(payload_len)
            .ok_or_else(|| "Invalid FLAC metadata block length".to_string())?;
        if end > data.len() {
            return Err("Truncated FLAC metadata block".to_string());
        }

        if blocks.is_empty() && (block_type != BLOCK_STREAMINFO || payload_len != STREAMINFO_LEN) {
            return Err("FLAC missing STREAMINFO block".to_string());
        }

        blocks.push(FlacBlock {
            block_type,
            payload_start,
            payload_len,
        });
        offset = end;
        if is_last {
            break;
        }
    }

    let tail_tags = mp3::tail_metadata_tags(data);
    let audio_end = tail_tags
        .last()
        .map(|tag| tag.start())
        .unwrap_or(data.len());
    let audio_start = offset;
    let frame = data.get(audio_start..audio_start + 2);
    let has_frame_sync =
        matches!(frame, Some(frame) if frame[0] == 0xff && frame[1] & 0xfc == 0xf8);
    if audio_start >= audio_end || !has_frame_sync {
        return Err("FLAC missing audio frames".to_string());
    }

    Ok(FlacLayout {
        id3v2_size,
        blocks,
        audio_start,
        audio_end,
        tail_tags,
    })
}

fn is_removed_block(block_type: u8) -> bool {
    !matches!(
        block_type,
        BLOCK_STREAMINFO | BLOCK_PADDING | BLOCK_SEEKTABLE
    )
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<usize> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

fn read_u32_be(data: &[u8], offset: usize) -> Option<usize> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

pub(crate) fn collect_vorbis_comments(payload: &[u8], entries: &mut Vec<MetadataEntry>) {
    entries.push(MetadataEntry {
        category: "Audio metadata".to_string(),
        name: "Vorbis comments".to_string(),
        value: format!("{} bytes", payload.len()),
    });

    let Some(vendor_len) = read_u32_le(payload, 0) else {
        return;
    };
    let vendor_end = 4usize.saturating_add(vendor_len);
    if let Some(vendor) = payload.get(4..vendor_end) {
        let vendor = String::from_utf8_lossy(vendor).trim().to_string();
        if !vendor.is_empty() {
            entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: "Encoder (vendor)".to_string(),
                value: truncate_for_display(&vendor, 120),
            });
        }
    }

    let Some(count) = read_u32_le(payload, vendor_end) else {
        return;
    };
    let mut offset = vendor_end + 4;
    for _ in 0..count.min(256) {
        let Some(length) = read_u32_le(payload, offset) else {
            return;
        };
        offset += 4;
        let Some(comment) = payload.get(offset..offset.saturating_add(length)) else {
            return;
        };
        offset += length;
        let comment = String::from_utf8_lossy(comment);
        let (key, value) = comment.split_once('=').unwrap_or((comment.as_ref(), ""));
        if value.trim().is_empty() {
            continue;
        }
        if key.trim().eq_ignore_ascii_case("METADATA_BLOCK_PICTURE")
            || key.trim().eq_ignore_ascii_case("COVERART")
        {
            entries.push(MetadataEntry {
                category: "Embedded artwork".to_string(),
                name: "Attached picture (Vorbis comment)".to_string(),
                value: format!("{} bytes", value.len()),
            });
        } else {
            entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: truncate_for_display(key.trim(), 60),
                value: truncate_for_display(value.trim(), 180),
            });
        }
    }
}

fn picture_entry(payload: &[u8]) -> MetadataEntry {
    let mime = read_u32_be(payload, 4)
        .and_then(|mime_len| payload.get(8..8usize.saturating_add(mime_len)))
        .map(|mime| String::from_utf8_lossy(mime).to_string())
        .filter(|mime| !mime.is_empty());
    let value = match mime {
        Some(mime) => format!("embedded image ({mime}), {} bytes", payload.len()),
        None => format!("embedded image, {} bytes", payload.len()),
    };
    MetadataEntry {
        category: "Embedded artwork".to_string(),
        name: "Attached picture".to_string(),
        value,
    }
}

fn application_label(payload: &[u8]) -> String {
    let id: String = payload
        .iter()
        .take(4)
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            }
        })
        .collect();
    format!(
        "'{id}', {} bytes; removed during FLAC cleaning",
        payload.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streaminfo_block(is_last: bool) -> Vec<u8> {
        let mut out = vec![
            if is_last { 0x80 } else { 0x00 },
            0,
            0,
            STREAMINFO_LEN as u8,
        ];
        out.extend_from_slice(&[0u8; STREAMINFO_LEN]);
        out
    }

    fn block(block_type: u8, payload: &[u8], is_last: bool) -> Vec<u8> {
        let mut out = vec![block_type | if is_last { 0x80 } else { 0 }];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..4]);
        out.extend_from_slice(payload);
        out
    }

    fn vorbis_comment_payload() -> Vec<u8> {
        let vendor = b"reference libFLAC 1.4.3";
        let comments: [&[u8]; 2] = [b"TITLE=Secret Song", b"ARTIST=Secret Artist"];
        let mut out = Vec::new();
        out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        out.extend_from_slice(vendor);
        out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for comment in comments {
            out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            out.extend_from_slice(comment);
        }
        out
    }

    fn audio_frames() -> Vec<u8> {
        vec![0xff, 0xf8, 0x69, 0x18, 0x00, 0x01, 0x02, 0x03]
    }

    fn flac_with_metadata() -> Vec<u8> {
        let mut data = FLAC_MAGIC.to_vec();
        data.extend_from_slice(&streaminfo_block(false));
        data.extend_from_slice(&block(
            BLOCK_VORBIS_COMMENT,
            &vorbis_comment_payload(),
            false,
        ));
        data.extend_from_slice(&block(BLOCK_APPLICATION, b"apppSecret app payload", false));
        data.extend_from_slice(&block(
            BLOCK_PICTURE,
            &{
                let mime = b"image/png";
                let mut payload = Vec::new();
                payload.extend_from_slice(&3u32.to_be_bytes());
                payload.extend_from_slice(&(mime.len() as u32).to_be_bytes());
                payload.extend_from_slice(mime);
                payload.extend_from_slice(&0u32.to_be_bytes());
                payload.extend_from_slice(&[0; 16]);
                payload.extend_from_slice(&8u32.to_be_bytes());
                payload.extend_from_slice(b"PNGDATA!");
                payload
            },
            false,
        ));
        data.extend_from_slice(&block(BLOCK_PADDING, &[0u8; 12], true));
        data.extend_from_slice(&audio_frames());
        data
    }

    #[test]
    fn leaves_clean_flac_unchanged() {
        let mut data = FLAC_MAGIC.to_vec();
        data.extend_from_slice(&streaminfo_block(true));
        data.extend_from_slice(&audio_frames());

        assert!(looks_like_flac(&data));
        validate(&data).unwrap();
        assert!(extract_metadata(&data).metadata_found.is_empty());
        assert_eq!(remove_metadata(&data).unwrap(), data);
    }

    #[test]
    fn removes_vorbis_comments_artwork_and_application_blocks() {
        let data = flac_with_metadata();

        validate(&data).unwrap();
        let info = extract_metadata(&data);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "TITLE" && entry.value == "Secret Song"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Embedded artwork"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Encoder (vendor)"));

        let cleaned = remove_metadata(&data).unwrap();
        for secret in [
            &b"Secret Song"[..],
            b"Secret Artist",
            b"PNGDATA!",
            b"libFLAC",
        ] {
            assert!(
                !cleaned.windows(secret.len()).any(|w| w == secret),
                "cleaned FLAC still contains {}",
                String::from_utf8_lossy(secret)
            );
        }
        assert!(cleaned.windows(2).any(|w| w == [0xff, 0xf8]));
        validate(&cleaned).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_prepended_id3v2_and_trailing_id3v1_tags() {
        let mut id3 = Vec::new();
        id3.extend_from_slice(b"ID3");
        id3.extend_from_slice(&[4, 0, 0]);
        let frame = {
            let mut out = Vec::new();
            out.extend_from_slice(b"TIT2");
            out.extend_from_slice(&[0, 0, 0, 13, 0, 0]);
            out.push(3);
            out.extend_from_slice(b"Secret Title");
            out
        };
        id3.extend_from_slice(&[
            ((frame.len() >> 21) & 0x7f) as u8,
            ((frame.len() >> 14) & 0x7f) as u8,
            ((frame.len() >> 7) & 0x7f) as u8,
            (frame.len() & 0x7f) as u8,
        ]);
        id3.extend_from_slice(&frame);

        let mut data = id3;
        data.extend_from_slice(FLAC_MAGIC);
        data.extend_from_slice(&streaminfo_block(true));
        data.extend_from_slice(&audio_frames());
        let mut id3v1 = [0u8; 128];
        id3v1[0..3].copy_from_slice(b"TAG");
        id3v1[3..14].copy_from_slice(b"Secret Song");
        data.extend_from_slice(&id3v1);

        assert!(looks_like_flac(&data));
        let info = extract_metadata(&data);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ID3v2 tag prepended to FLAC"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ID3v1 tag"));

        let cleaned = remove_metadata(&data).unwrap();
        assert!(cleaned.starts_with(FLAC_MAGIC));
        assert!(!cleaned.windows(6).any(|w| w == b"Secret"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn rejects_flac_without_streaminfo_or_audio() {
        let mut no_streaminfo = FLAC_MAGIC.to_vec();
        no_streaminfo.extend_from_slice(&block(BLOCK_PADDING, &[0u8; 4], true));
        no_streaminfo.extend_from_slice(&audio_frames());
        assert_eq!(
            validate(&no_streaminfo),
            Err("FLAC missing STREAMINFO block".to_string())
        );

        let mut no_audio = FLAC_MAGIC.to_vec();
        no_audio.extend_from_slice(&streaminfo_block(true));
        assert_eq!(
            validate(&no_audio),
            Err("FLAC missing audio frames".to_string())
        );

        let mut truncated = FLAC_MAGIC.to_vec();
        truncated.extend_from_slice(&[0x04, 0xff, 0xff]);
        assert!(validate(&truncated).is_err());
        assert!(remove_metadata(&truncated).is_err());
    }
}
