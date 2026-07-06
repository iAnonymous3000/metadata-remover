use crate::util::contains_ascii_ci;
use crate::{MetadataEntry, MetadataInfo};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BOXES: usize = 20_000;
const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug)]
struct BoxInfo {
    start: usize,
    header_size: usize,
    size: usize,
    kind: [u8; 4],
}

#[derive(Clone, Debug)]
struct HeifItem {
    id: u32,
    kind: [u8; 4],
    content_type: String,
    name: String,
}

pub fn detect_file_type(data: &[u8]) -> Option<&'static str> {
    let ftyp = ftyp_box(data)?;
    let brands = ftyp_brands(data, &ftyp)?;
    if brands.iter().any(|brand| is_avif_brand(brand)) {
        Some("avif")
    } else if brands.iter().any(|brand| is_heif_brand(brand)) {
        // Real HEVC-coded files (iPhone captures) list an hei*/hev* brand
        // alongside the generic mif1/msf1 structural brands, so the codec
        // brand decides; only purely generic files are labeled heif.
        if brands.iter().any(|brand| is_heic_codec_brand(brand)) {
            Some("heic")
        } else {
            Some("heif")
        }
    } else if brands.iter().any(|brand| brand.as_slice() == b"qt  ") {
        Some("mov")
    } else if brands.iter().any(|brand| is_mp4_brand(brand)) {
        Some("mp4")
    } else {
        None
    }
}

pub fn validate(data: &[u8], expected_type: &str) -> Result<(), String> {
    let detected =
        detect_file_type(data).ok_or_else(|| "Not a supported ISO-BMFF file".to_string())?;
    if detected != expected_type {
        return Err("ISO-BMFF file type changed during detection".to_string());
    }
    scan_boxes(data, 0, data.len(), 0, &mut |_| Ok(()))
}

pub fn extract_metadata(data: &[u8], file_type: &str) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;

    if matches!(file_type, "avif" | "heic" | "heif") {
        collect_top_level_content_credentials(data, &mut entries, &mut total_bytes);
        collect_heif_metadata(data, &mut entries, &mut total_bytes);
    } else {
        let _ = collect_video_metadata(data, &mut entries, &mut total_bytes);
    }

    MetadataInfo {
        file_type: file_type.to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8], file_type: &str) -> Result<Vec<u8>, String> {
    validate(data, file_type)?;
    let mut out = data.to_vec();
    if matches!(file_type, "avif" | "heic" | "heif") {
        clean_heif(data, &mut out)?;
    } else {
        clean_video_boxes(data, &mut out)?;
    }
    Ok(out)
}

fn collect_video_metadata(
    data: &[u8],
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) -> Result<(), String> {
    collect_video_metadata_range(data, 0, data.len(), 0, entries, total_bytes)
}

fn collect_video_metadata_range(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("ISO-BMFF nesting is too deep".to_string());
    }
    let mut offset = start;
    let mut count = 0usize;
    while offset < end {
        count += 1;
        if count > MAX_BOXES {
            return Err("ISO-BMFF file contains too many boxes".to_string());
        }
        let info = parse_box(data, offset, end)?;
        let kind = box_name(info.kind);
        if is_content_credentials_box(data, &info) {
            *total_bytes += info.size;
            entries.push(content_credentials_entry(&info));
        } else if is_video_metadata_box(info.kind) || is_video_text_metadata_box(data, &info) {
            *total_bytes += info.size;
            entries.push(MetadataEntry {
                category: "Video metadata".to_string(),
                name: format!("{kind} box"),
                value: format!("{} bytes", info.size),
            });
        } else {
            if let Some((timestamp_start, timestamp_end)) = video_timestamp_field_range(data, &info)
            {
                if data[timestamp_start..timestamp_end]
                    .iter()
                    .any(|byte| *byte != 0)
                {
                    *total_bytes += timestamp_end - timestamp_start;
                    entries.push(MetadataEntry {
                        category: "Video metadata".to_string(),
                        name: format!("{kind} timestamps"),
                        value: format!("{} bytes", timestamp_end - timestamp_start),
                    });
                }
            }
            if is_container_box(info.kind) {
                let (child_start, child_end) = child_bounds(&info)?;
                collect_video_metadata_range(
                    data,
                    child_start,
                    child_end,
                    depth + 1,
                    entries,
                    total_bytes,
                )?;
            }
        }
        offset += info.size;
    }
    Ok(())
}

fn clean_video_boxes(source: &[u8], out: &mut [u8]) -> Result<(), String> {
    clean_video_range(source, out, 0, source.len(), 0)
}

fn clean_video_range(
    source: &[u8],
    out: &mut [u8],
    start: usize,
    end: usize,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("ISO-BMFF nesting is too deep".to_string());
    }
    let mut offset = start;
    let mut count = 0usize;
    while offset < end {
        count += 1;
        if count > MAX_BOXES {
            return Err("ISO-BMFF file contains too many boxes".to_string());
        }
        let info = parse_box(source, offset, end)?;
        if is_content_credentials_box(source, &info)
            || is_video_metadata_box(info.kind)
            || is_video_text_metadata_box(source, &info)
        {
            replace_with_free(out, &info)?;
        } else {
            zero_video_timestamp_fields(source, out, &info);
            if is_container_box(info.kind) {
                let (child_start, child_end) = child_bounds(&info)?;
                clean_video_range(source, out, child_start, child_end, depth + 1)?;
            }
        }
        offset += info.size;
    }
    Ok(())
}

fn zero_video_timestamp_fields(source: &[u8], out: &mut [u8], info: &BoxInfo) {
    let Some((start, end)) = video_timestamp_field_range(source, info) else {
        return;
    };
    if source[start..end].iter().any(|byte| *byte != 0) && end <= out.len() {
        out[start..end].fill(0);
    }
}

fn video_timestamp_field_range(data: &[u8], info: &BoxInfo) -> Option<(usize, usize)> {
    if !is_video_timestamp_box(info.kind) {
        return None;
    }
    let version = *data.get(info.start + info.header_size)?;
    let field_len = match version {
        0 => 8,
        1 => 16,
        _ => return None,
    };
    let start = info.start.checked_add(info.header_size)?.checked_add(4)?;
    let end = start.checked_add(field_len)?;
    (end <= info.start + info.size && end <= data.len()).then_some((start, end))
}

fn is_video_timestamp_box(kind: [u8; 4]) -> bool {
    matches!(&kind, b"mvhd" | b"tkhd" | b"mdhd")
}

fn collect_heif_metadata(data: &[u8], entries: &mut Vec<MetadataEntry>, total_bytes: &mut usize) {
    let Some(meta) = find_top_level_box(data, *b"meta") else {
        return;
    };
    let Ok(items) = heif_items(data, &meta) else {
        return;
    };
    let metadata_ids: BTreeSet<u32> = items
        .iter()
        .filter(|item| is_heif_metadata_item(item))
        .map(|item| item.id)
        .collect();
    if metadata_ids.is_empty() {
        return;
    }

    let extent_bytes = heif_metadata_extent_bytes(data, &meta, &metadata_ids);
    for item in items {
        if !is_heif_metadata_item(&item) {
            continue;
        }
        let size = extent_bytes.get(&item.id).copied().unwrap_or(0);
        let label = if item.name.is_empty() {
            format!("item {}", item.id)
        } else {
            item.name.clone()
        };
        entries.push(MetadataEntry {
            category: "HEIF metadata".to_string(),
            name: heif_item_label(&item).to_string(),
            value: if size > 0 {
                format!("{label}, {size} bytes")
            } else {
                label
            },
        });
        *total_bytes += size;
    }
}

fn heif_metadata_extent_bytes(
    data: &[u8],
    meta: &BoxInfo,
    metadata_ids: &BTreeSet<u32>,
) -> BTreeMap<u32, usize> {
    let mut bytes = BTreeMap::new();
    let Ok(extents) = iloc_extents(data, meta) else {
        return bytes;
    };
    for extent in extents {
        if metadata_ids.contains(&extent.item_id) {
            let length = usize::try_from(extent.extent_length).unwrap_or(usize::MAX);
            let total = bytes.entry(extent.item_id).or_insert(0usize);
            *total = total.saturating_add(length);
        }
    }
    bytes
}

fn collect_top_level_content_credentials(
    data: &[u8],
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    let _ = scan_boxes(data, 0, data.len(), 0, &mut |info| {
        if is_content_credentials_box(data, info) {
            *total_bytes += info.size;
            entries.push(content_credentials_entry(info));
        }
        Ok(())
    });
}

fn clean_heif(source: &[u8], out: &mut [u8]) -> Result<(), String> {
    let Some(meta) = find_top_level_box(source, *b"meta") else {
        clean_top_level_metadata_boxes(source, out)?;
        return Ok(());
    };
    let items = heif_items(source, &meta)?;
    let metadata_ids: BTreeSet<u32> = items
        .iter()
        .filter(|item| is_heif_metadata_item(item))
        .map(|item| item.id)
        .collect();

    if metadata_ids.is_empty() {
        clean_top_level_metadata_boxes(source, out)?;
        return Ok(());
    }

    let cleared_ids = zero_heif_item_extents(source, out, &meta, &metadata_ids)?;
    neutralize_heif_item_info(source, out, &meta, &cleared_ids)?;
    clean_top_level_metadata_boxes(source, out)?;
    Ok(())
}

fn clean_top_level_metadata_boxes(source: &[u8], out: &mut [u8]) -> Result<(), String> {
    scan_boxes(source, 0, source.len(), 0, &mut |info| {
        if is_content_credentials_box(source, info) || is_video_text_metadata_box(source, info) {
            replace_with_free(out, info)?;
        }
        Ok(())
    })
}

fn neutralize_heif_item_info(
    source: &[u8],
    out: &mut [u8],
    meta: &BoxInfo,
    metadata_ids: &BTreeSet<u32>,
) -> Result<(), String> {
    let Some(iinf) = find_child_box(source, meta, *b"iinf")? else {
        return Ok(());
    };
    let payload_start = iinf.start + iinf.header_size + 4;
    let payload_end = iinf.start + iinf.size;
    let version = source
        .get(iinf.start + iinf.header_size)
        .copied()
        .ok_or_else(|| "Invalid HEIF iinf box".to_string())?;
    let count_size = if version == 0 { 2 } else { 4 };
    let mut offset = payload_start
        .checked_add(count_size)
        .ok_or_else(|| "Invalid HEIF iinf box".to_string())?;

    while offset < payload_end {
        let infe = parse_box(source, offset, payload_end)?;
        if infe.kind == *b"infe" {
            let Some((item_id, item_type_offset)) = parse_infe_id_and_type_offset(source, &infe)?
            else {
                offset += infe.size;
                continue;
            };
            if metadata_ids.contains(&item_id) {
                out[item_type_offset..item_type_offset + 4].copy_from_slice(b"free");
                zero_infe_strings(out, &infe, item_type_offset + 4);
            }
        }
        offset += infe.size;
    }
    Ok(())
}

fn zero_infe_strings(out: &mut [u8], infe: &BoxInfo, from: usize) {
    let end = infe.start + infe.size;
    if from < end && end <= out.len() {
        for byte in &mut out[from..end] {
            if *byte != 0 {
                *byte = 0;
            }
        }
    }
}

fn zero_heif_item_extents(
    source: &[u8],
    out: &mut [u8],
    meta: &BoxInfo,
    metadata_ids: &BTreeSet<u32>,
) -> Result<BTreeSet<u32>, String> {
    let idat = find_child_box(source, meta, *b"idat")?;
    let mut cleared_ids = BTreeSet::new();
    let mut failed_ids = BTreeSet::new();

    for extent in iloc_extents(source, meta)? {
        if !metadata_ids.contains(&extent.item_id) {
            continue;
        }

        let range = heif_extent_range(
            &idat,
            extent.construction_method,
            extent.base_offset,
            extent.extent_offset,
            extent.extent_length,
        )
        .filter(|(_, end)| *end <= out.len());

        if let Some((start, end)) = range {
            out[start..end].fill(0);
            cleared_ids.insert(extent.item_id);
        } else {
            failed_ids.insert(extent.item_id);
        }
    }

    for item_id in failed_ids {
        cleared_ids.remove(&item_id);
    }

    Ok(cleared_ids)
}

struct IlocExtent {
    item_id: u32,
    construction_method: u64,
    base_offset: u64,
    extent_offset: u64,
    extent_length: u64,
}

fn iloc_extents(source: &[u8], meta: &BoxInfo) -> Result<Vec<IlocExtent>, String> {
    let Some(iloc) = find_child_box(source, meta, *b"iloc")? else {
        return Ok(vec![]);
    };
    let version = source
        .get(iloc.start + iloc.header_size)
        .copied()
        .ok_or_else(|| "Invalid HEIF iloc box".to_string())?;
    let mut offset = iloc.start + iloc.header_size + 4;
    let sizes = *source
        .get(offset)
        .ok_or_else(|| "Invalid HEIF iloc box".to_string())?;
    offset += 1;
    let offset_size = (sizes >> 4) as usize;
    let length_size = (sizes & 0x0f) as usize;
    let base_offset_size = (*source
        .get(offset)
        .ok_or_else(|| "Invalid HEIF iloc box".to_string())?
        >> 4) as usize;
    let index_size = if version == 1 || version == 2 {
        (source[offset] & 0x0f) as usize
    } else {
        0
    };
    offset += 1;
    let item_count = if version < 2 {
        read_be(source, offset, 2)? as usize
    } else {
        read_be(source, offset, 4)? as usize
    };
    offset += if version < 2 { 2 } else { 4 };

    let mut extents = Vec::new();
    for _ in 0..item_count {
        let item_id = if version < 2 {
            read_be(source, offset, 2)? as u32
        } else {
            read_be(source, offset, 4)? as u32
        };
        offset += if version < 2 { 2 } else { 4 };
        let construction_method = if version == 1 || version == 2 {
            let value = read_be(source, offset, 2)?;
            offset += 2;
            value & 0x000f
        } else {
            0
        };
        offset += 2; // data_reference_index
        let base_offset = read_be(source, offset, base_offset_size)?;
        offset += base_offset_size;
        let extent_count = read_be(source, offset, 2)? as usize;
        offset += 2;

        for _ in 0..extent_count {
            if index_size > 0 {
                offset += index_size;
            }
            let extent_offset = read_be(source, offset, offset_size)?;
            offset += offset_size;
            let extent_length = read_be(source, offset, length_size)?;
            offset += length_size;
            extents.push(IlocExtent {
                item_id,
                construction_method,
                base_offset,
                extent_offset,
                extent_length,
            });
        }
    }

    Ok(extents)
}

fn heif_extent_range(
    idat: &Option<BoxInfo>,
    construction_method: u64,
    base_offset: u64,
    extent_offset: u64,
    extent_length: u64,
) -> Option<(usize, usize)> {
    let relative_start = base_offset.checked_add(extent_offset)?;
    match construction_method {
        0 => {
            let end = relative_start.checked_add(extent_length)?;
            if end > usize::MAX as u64 {
                return None;
            }
            Some((relative_start as usize, end as usize))
        }
        1 => {
            let idat = idat.as_ref()?;
            let payload_start = idat.start.checked_add(idat.header_size)? as u64;
            let payload_end = idat.start.checked_add(idat.size)? as u64;
            let start = payload_start.checked_add(relative_start)?;
            let end = start.checked_add(extent_length)?;
            if end > payload_end || end > usize::MAX as u64 {
                return None;
            }
            Some((start as usize, end as usize))
        }
        _ => None,
    }
}

fn heif_items(data: &[u8], meta: &BoxInfo) -> Result<Vec<HeifItem>, String> {
    let Some(iinf) = find_child_box(data, meta, *b"iinf")? else {
        return Ok(vec![]);
    };
    let version = data
        .get(iinf.start + iinf.header_size)
        .copied()
        .ok_or_else(|| "Invalid HEIF iinf box".to_string())?;
    let payload_start = iinf.start + iinf.header_size + 4;
    let payload_end = iinf.start + iinf.size;
    let count_size = if version == 0 { 2 } else { 4 };
    let mut offset = payload_start
        .checked_add(count_size)
        .ok_or_else(|| "Invalid HEIF iinf box".to_string())?;
    let mut items = Vec::new();

    while offset < payload_end {
        let infe = parse_box(data, offset, payload_end)?;
        if infe.kind == *b"infe" {
            if let Some(item) = parse_infe(data, &infe)? {
                items.push(item);
            }
        }
        offset += infe.size;
    }
    Ok(items)
}

fn parse_infe(data: &[u8], infe: &BoxInfo) -> Result<Option<HeifItem>, String> {
    let Some((id, item_type_offset)) = parse_infe_id_and_type_offset(data, infe)? else {
        return Ok(None);
    };
    let kind = read_kind(data, item_type_offset)?;
    // The infe strings are positional (item_name, then content_type for mime
    // items); an empty item_name still occupies its slot, so empty segments
    // must be preserved. Real Apple HEIC writes XMP as `\0application/rdf+xml`.
    let mut strings = data[item_type_offset + 4..infe.start + infe.size].split(|byte| *byte == 0);
    let name = null_string_part(strings.next());
    let content_type = null_string_part(strings.next());
    Ok(Some(HeifItem {
        id,
        kind,
        name,
        content_type,
    }))
}

fn null_string_part(part: Option<&[u8]>) -> String {
    part.and_then(|part| std::str::from_utf8(part).ok())
        .unwrap_or_default()
        .to_string()
}

fn parse_infe_id_and_type_offset(
    data: &[u8],
    infe: &BoxInfo,
) -> Result<Option<(u32, usize)>, String> {
    let full_header = infe.start + infe.header_size;
    let version = *data
        .get(full_header)
        .ok_or_else(|| "Invalid HEIF infe box".to_string())?;
    if version < 2 {
        return Ok(None);
    }
    let mut offset = full_header + 4;
    let id = if version == 2 {
        let value = read_be(data, offset, 2)? as u32;
        offset += 2;
        value
    } else {
        let value = read_be(data, offset, 4)? as u32;
        offset += 4;
        value
    };
    offset += 2; // item_protection_index
    if offset + 4 > infe.start + infe.size {
        return Err("Invalid HEIF infe box".to_string());
    }
    Ok(Some((id, offset)))
}

fn is_heif_metadata_item(item: &HeifItem) -> bool {
    item.kind == *b"Exif"
        || item.kind == *b"mime"
            && matches!(
                item.content_type.as_str(),
                "application/rdf+xml" | "application/xml" | "text/xml"
            )
        || item.name.to_ascii_lowercase().contains("xmp")
}

fn heif_item_label(item: &HeifItem) -> &'static str {
    if item.kind == *b"Exif" {
        "EXIF item"
    } else {
        "XMP item"
    }
}

fn ftyp_box(data: &[u8]) -> Option<BoxInfo> {
    let info = parse_box(data, 0, data.len()).ok()?;
    (info.kind == *b"ftyp").then_some(info)
}

fn ftyp_brands(data: &[u8], ftyp: &BoxInfo) -> Option<Vec<Vec<u8>>> {
    let payload = data.get(ftyp.start + ftyp.header_size..ftyp.start + ftyp.size)?;
    if payload.len() < 8 {
        return None;
    }
    let mut brands = Vec::new();
    brands.push(payload[0..4].to_vec());
    for chunk in payload[8..].chunks_exact(4) {
        brands.push(chunk.to_vec());
    }
    Some(brands)
}

fn is_heif_brand(brand: &[u8]) -> bool {
    matches!(brand, b"mif1" | b"msf1") || is_heic_codec_brand(brand)
}

fn is_heic_codec_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs"
    )
}

fn is_avif_brand(brand: &[u8]) -> bool {
    matches!(brand, b"avif" | b"avis")
}

fn is_mp4_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"isom"
            | b"iso2"
            | b"iso4"
            | b"iso5"
            | b"iso6"
            | b"mp41"
            | b"mp42"
            | b"mp4v"
            | b"M4V "
            | b"M4A "
            | b"3gp4"
            | b"3gp5"
            | b"3g2a"
    )
}

fn is_video_metadata_box(kind: [u8; 4]) -> bool {
    matches!(
        &kind,
        b"udta"
            | b"meta"
            | b"ilst"
            | b"cprt"
            | b"keys"
            | b"XMP_"
            | b"Exif"
            | b"name"
            | b"auth"
            | b"\xA9nam"
            | b"\xA9ART"
            | b"\xA9day"
            | b"\xA9too"
            | b"\xA9xyz"
    )
}

fn content_credentials_entry(info: &BoxInfo) -> MetadataEntry {
    MetadataEntry {
        category: "Content Credentials".to_string(),
        name: format!("{} box", box_name(info.kind)),
        value: format!("{} bytes", info.size),
    }
}

fn box_contains_text_metadata(data: &[u8], info: &BoxInfo) -> bool {
    let Some(payload) = data.get(info.start + info.header_size..info.start + info.size) else {
        return false;
    };
    contains_ascii_ci(payload, b"xmp")
        || contains_ascii_ci(payload, b"exif")
        || contains_ascii_ci(payload, b"com.apple.quicktime.location")
        || contains_ascii_ci(payload, b"com.apple.quicktime.make")
        || contains_ascii_ci(payload, b"com.apple.quicktime.model")
        || contains_ascii_ci(payload, b"com.apple.quicktime.software")
        || contains_ascii_ci(payload, b"gps")
}

fn is_content_credentials_box(data: &[u8], info: &BoxInfo) -> bool {
    if info.kind == *b"jumb" {
        return true;
    }
    if info.kind != *b"uuid" {
        return false;
    }
    let Some(payload) = data.get(info.start + info.header_size..info.start + info.size) else {
        return false;
    };
    contains_ascii_ci(payload, b"c2pa")
        || contains_ascii_ci(payload, b"jumb")
        || contains_ascii_ci(payload, b"Content Credentials")
}

fn is_video_text_metadata_box(data: &[u8], info: &BoxInfo) -> bool {
    info.kind == *b"uuid" && box_contains_text_metadata(data, info)
}

fn replace_with_free(out: &mut [u8], info: &BoxInfo) -> Result<(), String> {
    if info.size < 8 || info.start + info.size > out.len() {
        return Err("Invalid ISO-BMFF box".to_string());
    }
    out[info.start..info.start + info.size].fill(0);
    write_u32(out, info.start, info.size as u32)?;
    out[info.start + 4..info.start + 8].copy_from_slice(b"free");
    Ok(())
}

fn scan_boxes<F>(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    visitor: &mut F,
) -> Result<(), String>
where
    F: FnMut(&BoxInfo) -> Result<(), String>,
{
    if depth > MAX_DEPTH {
        return Err("ISO-BMFF nesting is too deep".to_string());
    }
    let mut offset = start;
    let mut count = 0usize;
    while offset < end {
        count += 1;
        if count > MAX_BOXES {
            return Err("ISO-BMFF file contains too many boxes".to_string());
        }
        let info = parse_box(data, offset, end)?;
        visitor(&info)?;
        offset += info.size;
    }
    Ok(())
}

fn parse_box(data: &[u8], offset: usize, end: usize) -> Result<BoxInfo, String> {
    if offset + 8 > end || offset + 8 > data.len() {
        return Err("Truncated ISO-BMFF box".to_string());
    }
    let small_size = read_be(data, offset, 4)? as usize;
    let kind = read_kind(data, offset + 4)?;
    let (size, header_size) = if small_size == 1 {
        let large_size = read_be(data, offset + 8, 8)?;
        if large_size > usize::MAX as u64 {
            return Err("ISO-BMFF box is too large".to_string());
        }
        (large_size as usize, 16)
    } else if small_size == 0 {
        (end - offset, 8)
    } else {
        (small_size, 8)
    };
    if size < header_size || offset.saturating_add(size) > end {
        return Err("Invalid ISO-BMFF box size".to_string());
    }
    Ok(BoxInfo {
        start: offset,
        header_size,
        size,
        kind,
    })
}

fn child_bounds(info: &BoxInfo) -> Result<(usize, usize), String> {
    let extra = if is_fullbox_container(info.kind) {
        4
    } else {
        0
    };
    let start = info
        .start
        .checked_add(info.header_size)
        .and_then(|value| value.checked_add(extra))
        .ok_or_else(|| "Invalid ISO-BMFF container".to_string())?;
    let end = info.start + info.size;
    if start > end {
        return Err("Invalid ISO-BMFF container".to_string());
    }
    Ok((start, end))
}

fn is_container_box(kind: [u8; 4]) -> bool {
    matches!(
        &kind,
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" | b"meta"
    )
}

fn is_fullbox_container(kind: [u8; 4]) -> bool {
    kind == *b"meta"
}

fn find_top_level_box(data: &[u8], kind: [u8; 4]) -> Option<BoxInfo> {
    let mut found = None;
    let _ = scan_boxes(data, 0, data.len(), 0, &mut |info| {
        if info.kind == kind {
            found = Some(info.clone());
        }
        Ok(())
    });
    found
}

fn find_child_box(data: &[u8], parent: &BoxInfo, kind: [u8; 4]) -> Result<Option<BoxInfo>, String> {
    let (start, end) = child_bounds(parent)?;
    let mut offset = start;
    while offset < end {
        let info = parse_box(data, offset, end)?;
        if info.kind == kind {
            return Ok(Some(info));
        }
        offset += info.size;
    }
    Ok(None)
}

fn read_kind(data: &[u8], offset: usize) -> Result<[u8; 4], String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "Unexpected end of ISO-BMFF file".to_string())?;
    Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_be(data: &[u8], offset: usize, size: usize) -> Result<u64, String> {
    if size > 8 {
        return Err("Invalid ISO-BMFF integer size".to_string());
    }
    let bytes = data
        .get(offset..offset + size)
        .ok_or_else(|| "Unexpected end of ISO-BMFF file".to_string())?;
    let mut value = 0u64;
    for byte in bytes {
        value = (value << 8) | (*byte as u64);
    }
    Ok(value)
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) -> Result<(), String> {
    let target = data
        .get_mut(offset..offset + 4)
        .ok_or_else(|| "Unexpected end of ISO-BMFF file".to_string())?;
    target.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn box_name(kind: [u8; 4]) -> String {
    kind.iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_bytes(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn ftyp(major: &[u8; 4]) -> Vec<u8> {
        ftyp_with_compatible(major, &[*major])
    }

    fn ftyp_with_compatible(major: &[u8; 4], compatible: &[[u8; 4]]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(major);
        payload.extend_from_slice(&0u32.to_be_bytes());
        for brand in compatible {
            payload.extend_from_slice(brand);
        }
        box_bytes(b"ftyp", &payload)
    }

    fn timestamp_payload_v0(creation_time: u32, modification_time: u32) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&creation_time.to_be_bytes());
        payload.extend_from_slice(&modification_time.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload
    }

    fn assert_timestamp_zeroed(data: &[u8], info: &BoxInfo) {
        let (start, end) = video_timestamp_field_range(data, info).unwrap();
        assert!(data[start..end].iter().all(|byte| *byte == 0));
    }

    fn heif_exif_idat_fixture(construction_method: u16) -> Vec<u8> {
        let mut data = ftyp(b"heic");
        let mut infe_payload = Vec::new();
        infe_payload.extend_from_slice(&[2, 0, 0, 0]);
        infe_payload.extend_from_slice(&1u16.to_be_bytes());
        infe_payload.extend_from_slice(&0u16.to_be_bytes());
        infe_payload.extend_from_slice(b"Exif");
        infe_payload.extend_from_slice(b"Exif\0\0");
        let infe = box_bytes(b"infe", &infe_payload);

        let mut iinf_payload = Vec::new();
        iinf_payload.extend_from_slice(&[0, 0, 0, 0]);
        iinf_payload.extend_from_slice(&1u16.to_be_bytes());
        iinf_payload.extend_from_slice(&infe);
        let iinf = box_bytes(b"iinf", &iinf_payload);

        let idat_payload = b"Exif\0\0GPS_SECRET";
        let idat = box_bytes(b"idat", idat_payload);
        let mut iloc_payload = Vec::new();
        iloc_payload.extend_from_slice(&[1, 0, 0, 0]);
        iloc_payload.push(0x44);
        iloc_payload.push(0x40);
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&construction_method.to_be_bytes());
        iloc_payload.extend_from_slice(&0u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u32.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u32.to_be_bytes());
        iloc_payload.extend_from_slice(&(idat_payload.len() as u32).to_be_bytes());
        let iloc = box_bytes(b"iloc", &iloc_payload);

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&iinf);
        meta_payload.extend_from_slice(&iloc);
        meta_payload.extend_from_slice(&idat);
        data.extend_from_slice(&box_bytes(b"meta", &meta_payload));
        data
    }

    #[test]
    fn accepts_clean_heif_without_metadata_items() {
        let mut data = ftyp(b"mif1");
        data.extend_from_slice(&box_bytes(b"mdat", b"IMAGE"));

        assert_eq!(detect_file_type(&data), Some("heif"));
        validate(&data, "heif").unwrap();
        assert!(extract_metadata(&data, "heif").metadata_found.is_empty());
        assert_eq!(remove_metadata(&data, "heif").unwrap(), data);
    }

    #[test]
    fn detects_common_mp4_compatible_brands() {
        assert_eq!(detect_file_type(&ftyp(b"mp4v")), Some("mp4"));
        assert_eq!(detect_file_type(&ftyp(b"3gp5")), Some("mp4"));
    }

    #[test]
    fn detects_avif_before_generic_heif_brands() {
        let data = ftyp_with_compatible(b"mif1", &[*b"mif1", *b"avif"]);

        assert_eq!(detect_file_type(&data), Some("avif"));
        validate(&data, "avif").unwrap();
    }

    #[test]
    fn removes_mp4_metadata_box_in_place() {
        let mut data = ftyp(b"mp42");
        let metadata = box_bytes(b"XMP_", b"<x:xmpmeta>GPS_SECRET</x:xmpmeta>");
        let moov = box_bytes(b"moov", &metadata);
        let mdat_payload = b"VIDEO frames may naturally contain gps, xmp, or exif byte patterns";
        data.extend_from_slice(&moov);
        data.extend_from_slice(&box_bytes(b"mdat", mdat_payload));

        assert_eq!(detect_file_type(&data), Some("mp4"));
        assert!(!extract_metadata(&data, "mp4").metadata_found.is_empty());

        let cleaned = remove_metadata(&data, "mp4").unwrap();
        assert_eq!(cleaned.len(), data.len());
        assert!(cleaned.windows(b"free".len()).any(|w| w == b"free"));
        assert!(!cleaned
            .windows(b"GPS_SECRET".len())
            .any(|w| w == b"GPS_SECRET"));
        assert!(cleaned
            .windows(mdat_payload.len())
            .any(|w| w == mdat_payload));
        assert!(extract_metadata(&cleaned, "mp4").metadata_found.is_empty());
    }

    #[test]
    fn removes_mp4_content_credentials_uuid_box() {
        let mut data = ftyp(b"mp42");
        let uuid = box_bytes(
            b"uuid",
            b"\x00\x01c2pa.claim Content Credentials manifest https://example.invalid",
        );
        data.extend_from_slice(&uuid);
        data.extend_from_slice(&box_bytes(b"mdat", b"VIDEO"));

        let metadata = extract_metadata(&data, "mp4");
        assert_eq!(metadata.metadata_found.len(), 1);
        assert_eq!(metadata.metadata_found[0].category, "Content Credentials");

        let cleaned = remove_metadata(&data, "mp4").unwrap();
        assert_eq!(cleaned.len(), data.len());
        assert!(cleaned.windows(b"free".len()).any(|w| w == b"free"));
        assert!(!cleaned.windows(b"c2pa".len()).any(|w| w == b"c2pa"));
        assert!(extract_metadata(&cleaned, "mp4").metadata_found.is_empty());
    }

    #[test]
    fn removes_avif_content_credentials_jumb_box() {
        let mut data = ftyp_with_compatible(b"avif", &[*b"avif", *b"mif1"]);
        let jumb = box_bytes(b"jumb", b"Content Credentials manifest");
        data.extend_from_slice(&jumb);
        data.extend_from_slice(&box_bytes(b"mdat", b"IMAGE"));

        assert_eq!(detect_file_type(&data), Some("avif"));
        let metadata = extract_metadata(&data, "avif");
        assert_eq!(metadata.metadata_found.len(), 1);
        assert_eq!(metadata.metadata_found[0].category, "Content Credentials");

        let cleaned = remove_metadata(&data, "avif").unwrap();
        assert_eq!(cleaned.len(), data.len());
        assert!(cleaned.windows(b"free".len()).any(|w| w == b"free"));
        assert!(!cleaned
            .windows(b"Content Credentials".len())
            .any(|w| { w == b"Content Credentials" }));
        assert!(extract_metadata(&cleaned, "avif").metadata_found.is_empty());
    }

    #[test]
    fn leaves_mp4_media_payload_with_metadata_words_unchanged() {
        let mut data = ftyp(b"mp42");
        let mdat_payload =
            b"compressed media payload with coincidental gps xmp exif byte sequences";
        data.extend_from_slice(&box_bytes(b"mdat", mdat_payload));

        assert!(extract_metadata(&data, "mp4").metadata_found.is_empty());

        let cleaned = remove_metadata(&data, "mp4").unwrap();
        assert_eq!(cleaned, data);
    }

    #[test]
    fn zeroes_video_creation_and_modification_timestamps() {
        let mut data = ftyp(b"mp42");
        let mvhd = box_bytes(b"mvhd", &timestamp_payload_v0(0x01020304, 0x05060708));
        let tkhd = box_bytes(b"tkhd", &timestamp_payload_v0(0x11121314, 0x15161718));
        let mdhd = box_bytes(b"mdhd", &timestamp_payload_v0(0x21222324, 0x25262728));
        let mdia = box_bytes(b"mdia", &mdhd);
        let mut trak_payload = Vec::new();
        trak_payload.extend_from_slice(&tkhd);
        trak_payload.extend_from_slice(&mdia);
        let trak = box_bytes(b"trak", &trak_payload);
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&mvhd);
        moov_payload.extend_from_slice(&trak);
        data.extend_from_slice(&box_bytes(b"moov", &moov_payload));
        data.extend_from_slice(&box_bytes(b"mdat", b"VIDEO"));

        let metadata = extract_metadata(&data, "mp4");
        assert_eq!(metadata.metadata_found.len(), 3);

        let cleaned = remove_metadata(&data, "mp4").unwrap();
        let moov = find_top_level_box(&cleaned, *b"moov").unwrap();
        let mvhd = find_child_box(&cleaned, &moov, *b"mvhd").unwrap().unwrap();
        let trak = find_child_box(&cleaned, &moov, *b"trak").unwrap().unwrap();
        let tkhd = find_child_box(&cleaned, &trak, *b"tkhd").unwrap().unwrap();
        let mdia = find_child_box(&cleaned, &trak, *b"mdia").unwrap().unwrap();
        let mdhd = find_child_box(&cleaned, &mdia, *b"mdhd").unwrap().unwrap();

        assert_timestamp_zeroed(&cleaned, &mvhd);
        assert_timestamp_zeroed(&cleaned, &tkhd);
        assert_timestamp_zeroed(&cleaned, &mdhd);
        assert!(extract_metadata(&cleaned, "mp4").metadata_found.is_empty());
    }

    #[test]
    fn removes_mov_user_data_box_in_place() {
        let mut data = ftyp(b"qt  ");
        let udta = box_bytes(b"udta", b"com.apple.quicktime.location.ISO6709+37-122/");
        let moov = box_bytes(b"moov", &udta);
        let mdat_payload = b"MOV media frames may also contain gps, xmp, or exif by chance";
        data.extend_from_slice(&moov);
        data.extend_from_slice(&box_bytes(b"mdat", mdat_payload));

        assert_eq!(detect_file_type(&data), Some("mov"));
        assert!(!extract_metadata(&data, "mov").metadata_found.is_empty());

        let cleaned = remove_metadata(&data, "mov").unwrap();
        assert_eq!(cleaned.len(), data.len());
        assert!(cleaned.windows(b"free".len()).any(|w| w == b"free"));
        assert!(!cleaned.windows(b"location".len()).any(|w| w == b"location"));
        assert!(cleaned
            .windows(mdat_payload.len())
            .any(|w| w == mdat_payload));
        assert!(extract_metadata(&cleaned, "mov").metadata_found.is_empty());
    }

    #[test]
    fn neutralizes_heif_exif_item_and_zeroes_extent() {
        let mut data = ftyp(b"heic");
        let mut infe_payload = Vec::new();
        infe_payload.extend_from_slice(&[2, 0, 0, 0]);
        infe_payload.extend_from_slice(&1u16.to_be_bytes());
        infe_payload.extend_from_slice(&0u16.to_be_bytes());
        infe_payload.extend_from_slice(b"Exif");
        infe_payload.extend_from_slice(b"Exif\0\0");
        let infe = box_bytes(b"infe", &infe_payload);

        let mut iinf_payload = Vec::new();
        iinf_payload.extend_from_slice(&[0, 0, 0, 0]);
        iinf_payload.extend_from_slice(&1u16.to_be_bytes());
        iinf_payload.extend_from_slice(&infe);
        let iinf = box_bytes(b"iinf", &iinf_payload);

        let mut iloc_payload = Vec::new();
        iloc_payload.extend_from_slice(&[0, 0, 0, 0]);
        iloc_payload.push(0x44);
        iloc_payload.push(0x40);
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u32.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        let mdat_payload = b"Exif\0\0GPS_SECRET";
        let iloc_len = 8 + iloc_payload.len() + 8;
        let meta_len = 8 + 4 + iinf.len() + iloc_len;
        let mdat_offset = data.len() + meta_len + 8;
        iloc_payload.extend_from_slice(&(mdat_offset as u32).to_be_bytes());
        iloc_payload.extend_from_slice(&(mdat_payload.len() as u32).to_be_bytes());
        let iloc = box_bytes(b"iloc", &iloc_payload);

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&iinf);
        meta_payload.extend_from_slice(&iloc);
        let meta = box_bytes(b"meta", &meta_payload);
        data.extend_from_slice(&meta);
        data.extend_from_slice(&box_bytes(b"mdat", mdat_payload));

        let info = extract_metadata(&data, "heic");
        assert!(!info.metadata_found.is_empty());
        assert_eq!(info.total_metadata_bytes, mdat_payload.len());

        let cleaned = remove_metadata(&data, "heic").unwrap();
        assert!(!cleaned.windows(b"Exif".len()).any(|w| w == b"Exif"));
        assert!(!cleaned
            .windows(b"GPS_SECRET".len())
            .any(|w| w == b"GPS_SECRET"));
        assert!(extract_metadata(&cleaned, "heic").metadata_found.is_empty());
    }

    #[test]
    fn detects_and_zeroes_heif_xmp_mime_item_with_empty_item_name() {
        // Mirrors real Apple ImageIO output: the XMP infe has an EMPTY
        // item_name followed by the content type, so string slots are
        // positional and must not be compacted.
        let mut data = ftyp(b"heic");
        let mut infe_payload = Vec::new();
        infe_payload.extend_from_slice(&[2, 0, 0, 0]);
        infe_payload.extend_from_slice(&1u16.to_be_bytes());
        infe_payload.extend_from_slice(&0u16.to_be_bytes());
        infe_payload.extend_from_slice(b"mime");
        infe_payload.extend_from_slice(b"\0application/rdf+xml\0");
        let infe = box_bytes(b"infe", &infe_payload);

        let mut iinf_payload = Vec::new();
        iinf_payload.extend_from_slice(&[0, 0, 0, 0]);
        iinf_payload.extend_from_slice(&1u16.to_be_bytes());
        iinf_payload.extend_from_slice(&infe);
        let iinf = box_bytes(b"iinf", &iinf_payload);

        let mut iloc_payload = Vec::new();
        iloc_payload.extend_from_slice(&[0, 0, 0, 0]);
        iloc_payload.push(0x44);
        iloc_payload.push(0x40);
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u16.to_be_bytes());
        iloc_payload.extend_from_slice(&0u32.to_be_bytes());
        iloc_payload.extend_from_slice(&1u16.to_be_bytes());
        let mdat_payload = b"<x:xmpmeta>dc:creator Secret Author</x:xmpmeta>";
        let iloc_len = 8 + iloc_payload.len() + 8;
        let meta_len = 8 + 4 + iinf.len() + iloc_len;
        let mdat_offset = data.len() + meta_len + 8;
        iloc_payload.extend_from_slice(&(mdat_offset as u32).to_be_bytes());
        iloc_payload.extend_from_slice(&(mdat_payload.len() as u32).to_be_bytes());
        let iloc = box_bytes(b"iloc", &iloc_payload);

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&iinf);
        meta_payload.extend_from_slice(&iloc);
        data.extend_from_slice(&box_bytes(b"meta", &meta_payload));
        data.extend_from_slice(&box_bytes(b"mdat", mdat_payload));

        let info = extract_metadata(&data, "heic");
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "XMP item"));
        assert_eq!(info.total_metadata_bytes, mdat_payload.len());

        let cleaned = remove_metadata(&data, "heic").unwrap();
        assert!(!cleaned
            .windows(b"Secret Author".len())
            .any(|w| w == b"Secret Author"));
        assert!(extract_metadata(&cleaned, "heic").metadata_found.is_empty());
    }

    #[test]
    fn zeroes_heif_exif_item_stored_in_idat() {
        let data = heif_exif_idat_fixture(1);

        assert!(!extract_metadata(&data, "heic").metadata_found.is_empty());
        let cleaned = remove_metadata(&data, "heic").unwrap();
        assert!(!cleaned.windows(b"Exif".len()).any(|w| w == b"Exif"));
        assert!(!cleaned
            .windows(b"GPS_SECRET".len())
            .any(|w| w == b"GPS_SECRET"));
        assert!(extract_metadata(&cleaned, "heic").metadata_found.is_empty());
    }

    #[test]
    fn keeps_heif_item_marked_when_extent_cannot_be_zeroed() {
        let data = heif_exif_idat_fixture(2);

        assert!(!extract_metadata(&data, "heic").metadata_found.is_empty());
        let cleaned = remove_metadata(&data, "heic").unwrap();
        assert!(cleaned.windows(b"Exif".len()).any(|w| w == b"Exif"));
        assert!(cleaned
            .windows(b"GPS_SECRET".len())
            .any(|w| w == b"GPS_SECRET"));
        assert!(!extract_metadata(&cleaned, "heic").metadata_found.is_empty());
    }

    #[test]
    fn rejects_malformed_box_size() {
        let mut data = ftyp(b"mp42");
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend_from_slice(b"moov");

        assert_eq!(
            validate(&data, "mp4"),
            Err("Invalid ISO-BMFF box size".to_string())
        );
        assert!(remove_metadata(&data, "mp4").is_err());
    }
}
