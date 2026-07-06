use crate::util::truncate_for_display;
use crate::{mp3, MetadataEntry, MetadataInfo};

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const WAVE_MAGIC: &[u8; 4] = b"WAVE";

pub fn is_wav(data: &[u8]) -> bool {
    data.len() >= 12 && &data[0..4] == RIFF_MAGIC && &data[8..12] == WAVE_MAGIC
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    let riff_end = declared_riff_end(data)?;

    let mut offset = 12;
    let mut has_fmt = false;
    let mut has_data = false;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((_, _, padded_end)) = chunk_bounds(data, offset, riff_end) else {
            return Err("Invalid WAV chunk length".to_string());
        };
        has_fmt |= chunk_type == b"fmt ";
        has_data |= chunk_type == b"data";
        offset = padded_end;
    }

    if offset != riff_end {
        return Err("Invalid WAV chunk length".to_string());
    }
    if !has_fmt || !has_data {
        return Err("WAV missing fmt or data chunk".to_string());
    }
    Ok(())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;

    if !is_wav(data) {
        return MetadataInfo {
            file_type: "wav".to_string(),
            metadata_found: entries,
            total_metadata_bytes: total_bytes,
        };
    }

    let riff_end = declared_riff_end(data).unwrap_or(data.len());
    let mut offset = 12;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = chunk_bounds(data, offset, riff_end) else {
            break;
        };
        let payload = &data[data_start..data_end];
        let chunk_total_len = padded_end - offset;

        if !is_kept_chunk(chunk_type) {
            total_bytes += chunk_total_len;
            collect_chunk_metadata(chunk_type, payload, &mut entries);
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
        file_type: "wav".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let riff_end = declared_riff_end(data)?;

    let mut result = Vec::with_capacity(data.len());
    result.extend_from_slice(RIFF_MAGIC);
    result.extend_from_slice(&0u32.to_le_bytes());
    result.extend_from_slice(WAVE_MAGIC);

    let mut offset = 12;
    let mut has_fmt = false;
    let mut has_data = false;
    while offset + 8 <= riff_end {
        let chunk_type = &data[offset..offset + 4];
        let Some((data_start, data_end, padded_end)) = chunk_bounds(data, offset, riff_end) else {
            return Err("Invalid WAV chunk length".to_string());
        };
        has_fmt |= chunk_type == b"fmt ";
        has_data |= chunk_type == b"data";

        if is_kept_chunk(chunk_type) {
            let payload = &data[data_start..data_end];
            let size =
                u32::try_from(payload.len()).map_err(|_| "WAV chunk is too large".to_string())?;
            result.extend_from_slice(chunk_type);
            result.extend_from_slice(&size.to_le_bytes());
            result.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                result.push(0);
            }
        }
        offset = padded_end;
    }

    if offset != riff_end {
        return Err("Invalid WAV chunk length".to_string());
    }
    if !has_fmt || !has_data {
        return Err("WAV missing fmt or data chunk".to_string());
    }

    let riff_size = u32::try_from(result.len().saturating_sub(8))
        .map_err(|_| "Cleaned WAV file is too large".to_string())?;
    result[4..8].copy_from_slice(&riff_size.to_le_bytes());
    Ok(result)
}

fn declared_riff_end(data: &[u8]) -> Result<usize, String> {
    if !is_wav(data) {
        return Err("Not a valid WAV file".to_string());
    }

    let riff_size = read_u32_le(data, 4).ok_or_else(|| "Not a valid WAV file".to_string())?;
    if riff_size < WAVE_MAGIC.len() {
        return Err("Invalid WAV RIFF size".to_string());
    }
    let riff_end = 8usize
        .checked_add(riff_size)
        .ok_or_else(|| "Invalid WAV RIFF size".to_string())?;
    if riff_end > data.len() {
        return Err("Truncated WAV RIFF data".to_string());
    }
    Ok(riff_end)
}

fn chunk_bounds(data: &[u8], offset: usize, limit: usize) -> Option<(usize, usize, usize)> {
    let size = read_u32_le(data, offset + 4)?;
    let data_start = offset.checked_add(8)?;
    let data_end = data_start.checked_add(size)?;
    let padded_end = data_end.checked_add(size & 1)?;

    if padded_end <= limit && padded_end <= data.len() {
        Some((data_start, data_end, padded_end))
    } else {
        None
    }
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<usize> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

fn is_kept_chunk(chunk_type: &[u8]) -> bool {
    matches!(chunk_type, b"fmt " | b"data" | b"fact")
}

fn collect_chunk_metadata(chunk_type: &[u8], payload: &[u8], entries: &mut Vec<MetadataEntry>) {
    match chunk_type {
        b"LIST" => collect_list_chunk(payload, entries),
        b"bext" => collect_bext_chunk(payload, entries),
        b"id3 " | b"ID3 " => {
            entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: "ID3 tag chunk".to_string(),
                value: format!("{} bytes", payload.len()),
            });
            if let Some(header) = mp3::parse_id3v2_header(payload) {
                mp3::collect_id3v2_frames(payload, header, entries);
            }
        }
        b"iXML" | b"axml" | b"_PMX" | b"XMP " => entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: format!("{} data", chunk_name(chunk_type)),
            value: format!("{} bytes", payload.len()),
        }),
        b"cue " | b"smpl" | b"inst" | b"adtl" | b"labl" | b"note" => entries.push(MetadataEntry {
            category: "Audio metadata".to_string(),
            name: format!("{} chunk", chunk_name(chunk_type)),
            value: format!("{} bytes; removed during WAV cleaning", payload.len()),
        }),
        _ => entries.push(MetadataEntry {
            category: "Other".to_string(),
            name: chunk_name(chunk_type),
            value: format!("{} bytes", payload.len()),
        }),
    }
}

fn collect_list_chunk(payload: &[u8], entries: &mut Vec<MetadataEntry>) {
    if payload.get(0..4) == Some(b"INFO") {
        let mut offset = 4;
        while offset + 8 <= payload.len() {
            let sub_type = &payload[offset..offset + 4];
            let Some(size) = read_u32_le(payload, offset + 4) else {
                break;
            };
            let sub_start = offset + 8;
            let Some(sub_end) = sub_start
                .checked_add(size)
                .filter(|end| *end <= payload.len())
            else {
                break;
            };
            let value = decode_riff_text(&payload[sub_start..sub_end]);
            if !value.is_empty() {
                entries.push(MetadataEntry {
                    category: "Audio metadata".to_string(),
                    name: info_field_name(sub_type).to_string(),
                    value: truncate_for_display(&value, 180),
                });
            }
            offset = sub_end + (size & 1);
        }
        return;
    }

    let list_type = payload
        .get(0..4)
        .map(chunk_name)
        .unwrap_or_else(|| "LIST".to_string());
    entries.push(MetadataEntry {
        category: "Audio metadata".to_string(),
        name: format!("LIST {list_type} chunk"),
        value: format!("{} bytes; removed during WAV cleaning", payload.len()),
    });
}

fn collect_bext_chunk(payload: &[u8], entries: &mut Vec<MetadataEntry>) {
    entries.push(MetadataEntry {
        category: "Audio metadata".to_string(),
        name: "Broadcast extension (bext)".to_string(),
        value: format!("{} bytes", payload.len()),
    });

    for (name, range) in [
        ("Description", 0..256),
        ("Originator", 256..288),
        ("Originator reference", 288..320),
        ("Origination date", 320..330),
        ("Origination time", 330..338),
    ] {
        let Some(field) = payload.get(range) else {
            continue;
        };
        let value = decode_riff_text(field);
        if !value.is_empty() {
            entries.push(MetadataEntry {
                category: "Audio metadata".to_string(),
                name: format!("bext {name}"),
                value: truncate_for_display(&value, 180),
            });
        }
    }
}

fn info_field_name(sub_type: &[u8]) -> &'static str {
    match sub_type {
        b"INAM" => "Title",
        b"IART" => "Artist",
        b"IPRD" => "Album",
        b"ICMT" => "Comment",
        b"ICRD" => "Date",
        b"IGNR" => "Genre",
        b"ISFT" => "Encoder",
        b"IENG" => "Engineer",
        b"ICOP" => "Copyright",
        b"ITRK" => "Track",
        b"ITCH" => "Technician",
        b"ISBJ" => "Subject",
        b"ISRC" => "Source",
        b"IKEY" => "Keywords",
        _ => "INFO field",
    }
}

fn chunk_name(chunk_type: &[u8]) -> String {
    String::from_utf8_lossy(chunk_type).trim().to_string()
}

fn decode_riff_text(data: &[u8]) -> String {
    data.iter()
        .take_while(|&&byte| byte != 0)
        .filter(|&&byte| byte >= 0x20 && byte != 0x7f)
        .map(|&byte| byte as char)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(chunk_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(chunk_type);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn wav(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(RIFF_MAGIC);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(WAVE_MAGIC);
        for item in chunks {
            out.extend_from_slice(item);
        }
        let riff_size = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff_size.to_le_bytes());
        out
    }

    fn fmt_chunk() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u16.to_le_bytes());
        payload.extend_from_slice(&1u16.to_le_bytes());
        payload.extend_from_slice(&44100u32.to_le_bytes());
        payload.extend_from_slice(&88200u32.to_le_bytes());
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.extend_from_slice(&16u16.to_le_bytes());
        chunk(b"fmt ", &payload)
    }

    fn info_list() -> Vec<u8> {
        let mut payload = b"INFO".to_vec();
        for (id, value) in [
            (&b"INAM"[..], &b"Secret Song\0"[..]),
            (b"IART", b"Secret Artist\0"),
            (b"ISFT", b"Lavf99.0.0\0"),
        ] {
            payload.extend_from_slice(id);
            payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
            payload.extend_from_slice(value);
        }
        chunk(b"LIST", &payload)
    }

    fn bext_chunk() -> Vec<u8> {
        let mut payload = vec![0u8; 602];
        payload[0..18].copy_from_slice(b"Secret recording !");
        payload[256..271].copy_from_slice(b"Secret Recorder");
        payload[320..330].copy_from_slice(b"2026-03-14");
        chunk(b"bext", &payload)
    }

    #[test]
    fn leaves_clean_wav_unchanged() {
        let data = wav(&[fmt_chunk(), chunk(b"data", &[1, 2, 3, 4])]);

        assert!(is_wav(&data));
        validate(&data).unwrap();
        assert!(extract_metadata(&data).metadata_found.is_empty());
        assert_eq!(remove_metadata(&data).unwrap(), data);
    }

    #[test]
    fn removes_info_tags_bext_and_unknown_chunks() {
        let data = wav(&[
            fmt_chunk(),
            info_list(),
            bext_chunk(),
            chunk(b"junk", b"private-tracker-payload"),
            chunk(b"data", &[1, 2, 3, 4]),
        ]);

        validate(&data).unwrap();
        let info = extract_metadata(&data);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Title" && entry.value == "Secret Song"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "bext Originator" && entry.value == "Secret Recorder"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Other" && entry.name == "junk"));

        let cleaned = remove_metadata(&data).unwrap();
        for secret in [
            &b"Secret Song"[..],
            b"Secret Artist",
            b"Secret Recorder",
            b"private-tracker-payload",
            b"Lavf",
        ] {
            assert!(
                !cleaned.windows(secret.len()).any(|w| w == secret),
                "cleaned WAV still contains {}",
                String::from_utf8_lossy(secret)
            );
        }
        assert!(cleaned.windows(4).any(|w| w == [1, 2, 3, 4]));
        validate(&cleaned).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn strips_trailing_data_after_declared_riff_end() {
        let mut data = wav(&[fmt_chunk(), chunk(b"data", &[1, 2, 3, 4])]);
        data.extend_from_slice(b"appended-tracker-data");

        let info = extract_metadata(&data);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Trailing Data"));

        let cleaned = remove_metadata(&data).unwrap();
        assert!(!cleaned.windows(b"appended".len()).any(|w| w == b"appended"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn rejects_wav_without_required_chunks_or_valid_lengths() {
        let no_data = wav(&[fmt_chunk()]);
        assert_eq!(
            validate(&no_data),
            Err("WAV missing fmt or data chunk".to_string())
        );
        assert!(remove_metadata(&no_data).is_err());

        let mut bad_length = wav(&[fmt_chunk(), chunk(b"data", &[1, 2, 3, 4])]);
        let fmt_size_offset = 12 + 4;
        bad_length[fmt_size_offset..fmt_size_offset + 4]
            .copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(
            validate(&bad_length),
            Err("Invalid WAV chunk length".to_string())
        );
        assert!(remove_metadata(&bad_length).is_err());
    }
}
