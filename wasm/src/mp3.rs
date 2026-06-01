use crate::{MetadataEntry, MetadataInfo};

const ID3V2_MAGIC: &[u8; 3] = b"ID3";
const ID3V1_MAGIC: &[u8; 3] = b"TAG";
const APEV2_MAGIC: &[u8; 8] = b"APETAGEX";
const LYRICS3_BEGIN: &[u8; 11] = b"LYRICSBEGIN";
const LYRICS3_V1_END: &[u8; 9] = b"LYRICSEND";
const LYRICS3_V2_END: &[u8; 9] = b"LYRICS200";
const ID3V1_SIZE: usize = 128;
const APEV2_HEADER_SIZE: usize = 32;
const LYRICS3_V2_FOOTER_SIZE: usize = 15;
const LYRICS3_V1_MAX_BYTES: usize = 5100;

#[derive(Clone, Copy, Debug)]
struct Id3v2Header {
    version_major: u8,
    flags: u8,
    payload_size: usize,
    total_size: usize,
}

#[derive(Clone, Copy, Debug)]
struct TailTag {
    start: usize,
    size: usize,
    name: &'static str,
}

pub fn looks_like_mp3(data: &[u8]) -> bool {
    parse_id3v2_header(data).is_some() || has_mpeg_frame_sync_at(data, 0)
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;
    let tail_tags = tail_metadata_tags(data);

    if let Some(header) = parse_id3v2_header(data) {
        total_bytes += header.total_size;
        entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: format!("ID3v2.{} tag", header.version_major),
            value: format!("{} bytes", header.total_size),
        });
        collect_id3v2_frames(data, header, &mut entries);
    }

    if let Some(tag) = tail_tags.iter().find(|tag| tag.name == "ID3v1 tag") {
        let start = tag.start;
        total_bytes += ID3V1_SIZE;
        entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: "ID3v1 tag".to_string(),
            value: format!("{} bytes", ID3V1_SIZE),
        });
        collect_id3v1_fields(&data[start..start + ID3V1_SIZE], &mut entries);
    }

    collect_tail_tags(&tail_tags, &mut entries, &mut total_bytes);

    MetadataInfo {
        file_type: "mp3".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    validate(data)?;

    let start = parse_id3v2_header(data)
        .map(|header| header.total_size)
        .unwrap_or(0);
    let end = audio_end_without_tail_metadata(data);
    if start > end {
        return Err("Invalid MP3 metadata layout".to_string());
    }

    Ok(data[start..end].to_vec())
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    if data.len() < 2 {
        return Err("File too small".to_string());
    }

    let audio_start = parse_id3v2_header(data)
        .map(|header| header.total_size)
        .unwrap_or(0);
    let audio_end = audio_end_without_tail_metadata(data);

    if audio_start >= audio_end || audio_start + 2 > data.len() {
        return Err("MP3 missing MPEG audio frames".to_string());
    }

    if has_mpeg_frame_sync_at(data, audio_start) {
        Ok(())
    } else {
        Err("MP3 missing MPEG audio frames".to_string())
    }
}

fn parse_id3v2_header(data: &[u8]) -> Option<Id3v2Header> {
    if data.len() < 10 || &data[0..3] != ID3V2_MAGIC {
        return None;
    }

    let version_major = data[3];
    if !(2..=4).contains(&version_major) || data[4] == 0xff {
        return None;
    }

    let payload_size = synchsafe_u32_to_usize(&data[6..10])?;
    let footer_size = if version_major == 4 && data[5] & 0x10 != 0 {
        10
    } else {
        0
    };
    let total_size = 10usize
        .checked_add(payload_size)?
        .checked_add(footer_size)?;
    if total_size > data.len() {
        return None;
    }

    Some(Id3v2Header {
        version_major,
        flags: data[5],
        payload_size,
        total_size,
    })
}

fn synchsafe_u32_to_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != 4 || bytes.iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    Some(
        ((bytes[0] as usize) << 21)
            | ((bytes[1] as usize) << 14)
            | ((bytes[2] as usize) << 7)
            | (bytes[3] as usize),
    )
}

fn be_u32_to_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != 4 {
        return None;
    }
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

fn le_u32_to_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

fn has_mpeg_frame_sync_at(data: &[u8], offset: usize) -> bool {
    let Some(bytes) = data.get(offset..offset + 2) else {
        return false;
    };
    bytes[0] == 0xff
        && bytes[1] & 0xe0 == 0xe0
        && bytes[1] & 0x18 != 0x08
        && bytes[1] & 0x06 != 0x00
}

fn collect_tail_tags(
    tail_tags: &[TailTag],
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    for tag in tail_tags.iter().filter(|tag| tag.name != "ID3v1 tag") {
        *total_bytes += tag.size;
        entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: tag.name.to_string(),
            value: format!("{} bytes; removed during MP3 cleaning", tag.size),
        });
    }
}

fn audio_end_without_tail_metadata(data: &[u8]) -> usize {
    tail_metadata_tags(data)
        .last()
        .map(|tag| tag.start)
        .unwrap_or(data.len())
}

fn tail_metadata_tags(data: &[u8]) -> Vec<TailTag> {
    let mut tags = Vec::new();
    let mut end = data.len();

    while end > 0 {
        let Some(tag) = tail_metadata_tag_before(data, end) else {
            break;
        };
        if tag.size == 0 || tag.start >= end {
            break;
        }
        end = tag.start;
        tags.push(tag);
    }

    tags
}

fn tail_metadata_tag_before(data: &[u8], end: usize) -> Option<TailTag> {
    id3v1_tag_before(data, end)
        .or_else(|| apev2_tag_before(data, end))
        .or_else(|| lyrics3v2_tag_before(data, end))
        .or_else(|| lyrics3v1_tag_before(data, end))
}

fn id3v1_tag_before(data: &[u8], end: usize) -> Option<TailTag> {
    if end < ID3V1_SIZE || end > data.len() {
        return None;
    }
    let start = end - ID3V1_SIZE;
    (data.get(start..start + ID3V1_MAGIC.len()) == Some(ID3V1_MAGIC)).then_some(TailTag {
        start,
        size: ID3V1_SIZE,
        name: "ID3v1 tag",
    })
}

fn apev2_tag_before(data: &[u8], end: usize) -> Option<TailTag> {
    if end < APEV2_HEADER_SIZE {
        return None;
    }
    let footer_start = end - APEV2_HEADER_SIZE;
    let footer = data.get(footer_start..end)?;
    if footer.get(0..8) != Some(APEV2_MAGIC) {
        return None;
    }

    let version = le_u32_to_usize(footer.get(8..12)?)?;
    if !(1000..=2000).contains(&version) {
        return None;
    }
    let tag_size = le_u32_to_usize(footer.get(12..16)?)?;
    if !(APEV2_HEADER_SIZE..=end).contains(&tag_size) {
        return None;
    }

    let mut start = end - tag_size;
    if start >= APEV2_HEADER_SIZE
        && data.get(start - APEV2_HEADER_SIZE..start - APEV2_HEADER_SIZE + APEV2_MAGIC.len())
            == Some(APEV2_MAGIC)
    {
        start -= APEV2_HEADER_SIZE;
    }

    Some(TailTag {
        start,
        size: end - start,
        name: "APEv2 tag",
    })
}

fn lyrics3v2_tag_before(data: &[u8], end: usize) -> Option<TailTag> {
    if end < LYRICS3_V2_FOOTER_SIZE {
        return None;
    }
    let footer_start = end - LYRICS3_V2_FOOTER_SIZE;
    if data.get(footer_start + 6..end) != Some(LYRICS3_V2_END) {
        return None;
    }

    let declared_size = ascii_decimal_to_usize(data.get(footer_start..footer_start + 6)?)?;
    let candidates = [
        footer_start.checked_sub(declared_size),
        footer_start
            .checked_sub(declared_size)
            .and_then(|start| start.checked_sub(LYRICS3_BEGIN.len())),
    ];

    for start in candidates.into_iter().flatten() {
        if start < footer_start
            && data.get(start..start + LYRICS3_BEGIN.len()) == Some(LYRICS3_BEGIN)
        {
            return Some(TailTag {
                start,
                size: end - start,
                name: "Lyrics3 tag",
            });
        }
    }

    None
}

fn lyrics3v1_tag_before(data: &[u8], end: usize) -> Option<TailTag> {
    if end < LYRICS3_V1_END.len() {
        return None;
    }
    let footer_start = end - LYRICS3_V1_END.len();
    if data.get(footer_start..end) != Some(LYRICS3_V1_END) {
        return None;
    }

    let search_start = footer_start.saturating_sub(LYRICS3_V1_MAX_BYTES);
    let window = data.get(search_start..footer_start)?;
    let relative_start = window
        .windows(LYRICS3_BEGIN.len())
        .rposition(|candidate| candidate == LYRICS3_BEGIN)?;
    let start = search_start + relative_start;

    Some(TailTag {
        start,
        size: end - start,
        name: "Lyrics3 tag",
    })
}

fn ascii_decimal_to_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }

    bytes.iter().try_fold(0usize, |value, byte| {
        value.checked_mul(10)?.checked_add((byte - b'0') as usize)
    })
}

fn collect_id3v2_frames(data: &[u8], header: Id3v2Header, entries: &mut Vec<MetadataEntry>) {
    let tag_start = 10;
    let tag_end = tag_start + header.payload_size;
    if tag_end > data.len() {
        return;
    }
    let tag_data = &data[tag_start..tag_end];
    let mut offset = id3v2_frame_start(tag_data, header);

    while offset < tag_data.len() {
        let Some((frame_id, frame_size, header_size)) =
            parse_frame_header(tag_data, offset, header.version_major)
        else {
            break;
        };
        if frame_id.bytes().all(|byte| byte == 0) {
            break;
        }
        if !frame_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            break;
        }
        let frame_start = offset + header_size;
        let Some(frame_end) = frame_start.checked_add(frame_size) else {
            break;
        };
        if frame_end > tag_data.len() {
            break;
        }
        if frame_size > 0 {
            entries.push(MetadataEntry {
                category: id3v2_frame_category(&frame_id).to_string(),
                name: id3v2_frame_name(&frame_id).to_string(),
                value: id3v2_frame_value(&frame_id, &tag_data[frame_start..frame_end]),
            });
        }
        offset = frame_end;
    }
}

fn id3v2_frame_start(tag_data: &[u8], header: Id3v2Header) -> usize {
    if header.flags & 0x40 == 0 {
        return 0;
    }

    if header.version_major == 3 {
        let Some(size) = tag_data.get(0..4).and_then(be_u32_to_usize) else {
            return 0;
        };
        4usize.saturating_add(size).min(tag_data.len())
    } else if header.version_major == 4 {
        tag_data
            .get(0..4)
            .and_then(synchsafe_u32_to_usize)
            .unwrap_or(0)
            .min(tag_data.len())
    } else {
        0
    }
}

fn parse_frame_header(
    tag_data: &[u8],
    offset: usize,
    version_major: u8,
) -> Option<(String, usize, usize)> {
    if version_major == 2 {
        let header = tag_data.get(offset..offset + 6)?;
        let frame_id = std::str::from_utf8(&header[0..3]).ok()?.to_string();
        let frame_size =
            ((header[3] as usize) << 16) | ((header[4] as usize) << 8) | header[5] as usize;
        return Some((frame_id, frame_size, 6));
    }

    let header = tag_data.get(offset..offset + 10)?;
    let frame_id = std::str::from_utf8(&header[0..4]).ok()?.to_string();
    let frame_size = if version_major == 4 {
        synchsafe_u32_to_usize(&header[4..8])?
    } else {
        be_u32_to_usize(&header[4..8])?
    };
    Some((frame_id, frame_size, 10))
}

fn id3v2_frame_category(frame_id: &str) -> &'static str {
    match frame_id {
        "APIC" | "PIC" => "Embedded artwork",
        _ => "Audio metadata",
    }
}

fn id3v2_frame_name(frame_id: &str) -> &'static str {
    match frame_id {
        "TIT2" | "TT2" => "Title",
        "TPE1" | "TP1" => "Artist",
        "TALB" | "TAL" => "Album",
        "TDRC" | "TYER" | "TYE" => "Recording date",
        "TCON" | "TCO" => "Genre",
        "TCOP" | "TCR" => "Copyright",
        "TSSE" | "TSS" => "Encoder",
        "TXXX" | "TXX" => "User text",
        "COMM" | "COM" => "Comment",
        "USLT" | "ULT" => "Lyrics",
        "WXXX" | "WXX" => "User URL",
        "UFID" | "UFI" => "Unique file ID",
        "PRIV" => "Private frame",
        "GEOB" | "GEO" => "Embedded object",
        "APIC" | "PIC" => "Attached picture",
        id if id.starts_with('W') => "URL",
        id if id.starts_with('T') => "Text frame",
        _ => "ID3 frame",
    }
}

fn id3v2_frame_value(frame_id: &str, frame_data: &[u8]) -> String {
    match frame_id {
        "APIC" | "PIC" => format!("embedded image, {} bytes", frame_data.len()),
        "COMM" | "COM" | "USLT" | "ULT" => decode_comment_like_frame(frame_data)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{} bytes", frame_data.len())),
        id if id.starts_with('T') => decode_text_frame(frame_data)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{} bytes", frame_data.len())),
        id if id.starts_with('W') => String::from_utf8_lossy(frame_data).trim().to_string(),
        _ => format!("{} bytes", frame_data.len()),
    }
}

fn decode_text_frame(frame_data: &[u8]) -> Option<String> {
    let (&encoding, payload) = frame_data.split_first()?;
    Some(clean_id3_text(match encoding {
        0 => decode_latin1(payload),
        1 => decode_utf16(payload, None),
        2 => decode_utf16(payload, Some(false)),
        3 => String::from_utf8_lossy(payload).to_string(),
        _ => String::new(),
    }))
}

fn decode_comment_like_frame(frame_data: &[u8]) -> Option<String> {
    let (&encoding, payload) = frame_data.split_first()?;
    let text_start = payload.iter().position(|byte| *byte == 0).unwrap_or(3);
    let payload = payload
        .get(text_start.saturating_add(1)..)
        .unwrap_or_default();
    Some(clean_id3_text(match encoding {
        0 => decode_latin1(payload),
        1 => decode_utf16(payload, None),
        2 => decode_utf16(payload, Some(false)),
        3 => String::from_utf8_lossy(payload).to_string(),
        _ => String::new(),
    }))
}

fn decode_latin1(data: &[u8]) -> String {
    data.iter().map(|byte| *byte as char).collect()
}

fn decode_utf16(data: &[u8], forced_little_endian: Option<bool>) -> String {
    if data.len() < 2 {
        return String::new();
    }

    let (little_endian, start) = if let Some(little_endian) = forced_little_endian {
        (little_endian, 0)
    } else if data.starts_with(&[0xff, 0xfe]) {
        (true, 2)
    } else if data.starts_with(&[0xfe, 0xff]) {
        (false, 2)
    } else {
        (false, 0)
    };

    let units = data[start..].chunks_exact(2).map(|chunk| {
        if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        }
    });
    char::decode_utf16(units)
        .map(|item| item.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn clean_id3_text(value: String) -> String {
    value
        .replace('\0', " / ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect_id3v1_fields(tag: &[u8], entries: &mut Vec<MetadataEntry>) {
    for (name, range) in [
        ("Title", 3..33),
        ("Artist", 33..63),
        ("Album", 63..93),
        ("Year", 93..97),
        ("Comment", 97..127),
    ] {
        let value = decode_id3v1_field(&tag[range]);
        if !value.is_empty() {
            entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: name.to_string(),
                value,
            });
        }
    }
}

fn decode_id3v1_field(data: &[u8]) -> String {
    decode_latin1(data)
        .trim_end_matches(['\0', ' '])
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synchsafe_bytes(value: usize) -> [u8; 4] {
        [
            ((value >> 21) & 0x7f) as u8,
            ((value >> 14) & 0x7f) as u8,
            ((value >> 7) & 0x7f) as u8,
            (value & 0x7f) as u8,
        ]
    }

    fn frame(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(id);
        out.extend_from_slice(&synchsafe_bytes(data.len()));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(data);
        out
    }

    fn id3v24(frames: &[Vec<u8>]) -> Vec<u8> {
        let payload_len = frames.iter().map(Vec::len).sum();
        let mut out = Vec::new();
        out.extend_from_slice(ID3V2_MAGIC);
        out.extend_from_slice(&[4, 0, 0]);
        out.extend_from_slice(&synchsafe_bytes(payload_len));
        for frame in frames {
            out.extend_from_slice(frame);
        }
        out
    }

    fn id3v1_tag() -> [u8; ID3V1_SIZE] {
        let mut tag = [0u8; ID3V1_SIZE];
        tag[0..3].copy_from_slice(ID3V1_MAGIC);
        tag[3..15].copy_from_slice(b"Secret Song\0");
        tag[33..46].copy_from_slice(b"Secret Artist");
        tag[93..97].copy_from_slice(b"2026");
        tag
    }

    fn apev2_tag(body: &[u8]) -> Vec<u8> {
        let tag_size = body.len() + APEV2_HEADER_SIZE;
        let mut out = Vec::new();
        out.extend_from_slice(body);
        out.extend_from_slice(APEV2_MAGIC);
        out.extend_from_slice(&2000u32.to_le_bytes());
        out.extend_from_slice(&(tag_size as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out
    }

    fn lyrics3v2_tag(body: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(LYRICS3_BEGIN);
        payload.extend_from_slice(body);
        let size = format!("{:06}", payload.len());
        payload.extend_from_slice(size.as_bytes());
        payload.extend_from_slice(LYRICS3_V2_END);
        payload
    }

    #[test]
    fn test_id3v2_frames_and_embedded_artwork_are_removed() {
        let audio = [0xff, 0xfb, 0x90, 0x64, 0, 1, 2, 3];
        let mut title = vec![3];
        title.extend_from_slice(b"Secret Title");
        let mut artist = vec![3];
        artist.extend_from_slice(b"Secret Artist");
        let mut artwork = vec![0];
        artwork.extend_from_slice(b"image/jpeg\0\x03cover\0JPEGDATA");
        let mut input = id3v24(&[
            frame(b"TIT2", &title),
            frame(b"TPE1", &artist),
            frame(b"APIC", &artwork),
            frame(b"PRIV", b"private-owner\0secret"),
        ]);
        input.extend_from_slice(&audio);

        assert!(looks_like_mp3(&input));
        validate(&input).unwrap();

        let info = extract_metadata(&input);
        assert_eq!(info.file_type, "mp3");
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Title" && entry.value == "Secret Title"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Embedded artwork"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Private frame"));

        let cleaned = remove_metadata(&input).unwrap();
        assert_eq!(cleaned, audio);
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_id3v1_tag_is_removed_from_tail() {
        let mut input = vec![0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4];
        input.extend_from_slice(&id3v1_tag());

        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ID3v1 tag"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value == "Secret Song"));

        let cleaned = remove_metadata(&input).unwrap();
        assert!(has_mpeg_frame_sync_at(&cleaned, 0));
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_apev2_tail_is_removed_with_id3_tail() {
        let mut input = vec![0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4];
        input.extend_from_slice(&apev2_tag(b"title=Secret Song\0artist=Secret Artist"));
        input.extend_from_slice(&id3v1_tag());

        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "APEv2 tag"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ID3v1 tag"));

        let cleaned = remove_metadata(&input).unwrap();
        let verification = extract_metadata(&cleaned);
        assert!(verification.metadata_found.is_empty());
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
    }

    #[test]
    fn test_apev2_tail_after_id3v1_is_removed() {
        let mut input = vec![0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4];
        input.extend_from_slice(&id3v1_tag());
        input.extend_from_slice(&apev2_tag(b"title=Secret Song\0artist=Secret Artist"));

        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "APEv2 tag"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ID3v1 tag"));

        let cleaned = remove_metadata(&input).unwrap();
        assert_eq!(cleaned, vec![0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4]);
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_lyrics3_tail_is_removed() {
        let mut input = vec![0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4];
        input.extend_from_slice(&lyrics3v2_tag(b"LYRSecret lyrics"));

        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Lyrics3 tag"));

        let cleaned = remove_metadata(&input).unwrap();
        let verification = extract_metadata(&cleaned);
        assert!(verification.metadata_found.is_empty());
        assert!(!cleaned.windows(b"Secret".len()).any(|w| w == b"Secret"));
    }

    #[test]
    fn test_rejects_id3_tag_without_audio_frames() {
        let input = id3v24(&[frame(b"TIT2", &[3, b'S', b'e', b'c', b'r', b'e', b't'])]);

        assert_eq!(
            validate(&input),
            Err("MP3 missing MPEG audio frames".to_string())
        );
        assert_eq!(
            remove_metadata(&input),
            Err("MP3 missing MPEG audio frames".to_string())
        );
    }
}
