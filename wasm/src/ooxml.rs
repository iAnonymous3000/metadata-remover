use crate::{MetadataEntry, MetadataInfo};
use flate2::read::DeflateDecoder;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const LOCAL_FILE_HEADER: u32 = 0x0403_4b50;
const CENTRAL_FILE_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL_DIRECTORY: u32 = 0x0605_4b50;
const ZIP_TIME: u16 = 0;
const ZIP_DATE: u16 = 0x0021;
const ZIP64_SENTINEL_16: u16 = 0xffff;
const ZIP64_SENTINEL_32: u32 = 0xffff_ffff;
const MAX_ENTRIES: usize = 4096;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OoxmlKind {
    Docx,
    Xlsx,
    Pptx,
}

impl OoxmlKind {
    fn as_str(self) -> &'static str {
        match self {
            OoxmlKind::Docx => "docx",
            OoxmlKind::Xlsx => "xlsx",
            OoxmlKind::Pptx => "pptx",
        }
    }
}

#[derive(Clone, Debug)]
struct ZipEntry {
    name: String,
    flags: u16,
    compression: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    data_start: usize,
    data_end: usize,
    external_attrs: u32,
}

struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
    by_name: BTreeMap<String, usize>,
}

impl<'a> ZipArchive<'a> {
    fn has_entry(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    fn entry(&self, name: &str) -> Option<&ZipEntry> {
        self.by_name.get(name).map(|index| &self.entries[*index])
    }

    fn compressed_data(&self, entry: &ZipEntry) -> &'a [u8] {
        &self.data[entry.data_start..entry.data_end]
    }

    fn read_entry(&self, name: &str, max_size: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(entry) = self.entry(name) else {
            return Ok(None);
        };

        if entry.uncompressed_size as usize > max_size {
            return Err(format!("{} is too large to inspect safely", entry.name));
        }

        let compressed = self.compressed_data(entry);
        let bytes = match entry.compression {
            0 => compressed.to_vec(),
            8 => {
                let mut decoder = DeflateDecoder::new(compressed);
                let mut out = Vec::with_capacity(entry.uncompressed_size as usize);
                decoder
                    .by_ref()
                    .take((max_size as u64) + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| format!("Failed to decompress {}", entry.name))?;
                if out.len() > max_size {
                    return Err(format!("{} is too large to inspect safely", entry.name));
                }
                out
            }
            _ => return Err(format!("Unsupported ZIP compression in {}", entry.name)),
        };

        if bytes.len() != entry.uncompressed_size as usize {
            return Err(format!("ZIP size mismatch in {}", entry.name));
        }

        Ok(Some(bytes))
    }

    fn read_xml(&self, name: &str) -> Result<Option<String>, String> {
        let Some(bytes) = self.read_entry(name, MAX_XML_BYTES)? else {
            return Ok(None);
        };

        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8 XML", name))
    }
}

pub fn detect_file_type(data: &[u8]) -> Option<&'static str> {
    let archive = parse_archive(data).ok()?;
    Some(detect_kind(&archive)?.as_str())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let archive = match parse_archive(data) {
        Ok(archive) => archive,
        Err(_) => {
            return MetadataInfo {
                file_type: "ooxml".to_string(),
                metadata_found: vec![],
                total_metadata_bytes: 0,
            };
        }
    };

    let Some(kind) = detect_kind(&archive) else {
        return MetadataInfo {
            file_type: "ooxml".to_string(),
            metadata_found: vec![],
            total_metadata_bytes: 0,
        };
    };

    let mut entries = Vec::new();
    let mut total_bytes = 0;

    collect_document_properties(&archive, &mut entries, &mut total_bytes);

    match kind {
        OoxmlKind::Docx => collect_docx_review_metadata(&archive, &mut entries, &mut total_bytes),
        OoxmlKind::Xlsx => collect_xlsx_review_metadata(&archive, &mut entries, &mut total_bytes),
        OoxmlKind::Pptx => collect_pptx_review_metadata(&archive, &mut entries, &mut total_bytes),
    }

    MetadataInfo {
        file_type: kind.as_str().to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    let archive = parse_archive(data)?;
    detect_kind(&archive)
        .map(|_| ())
        .ok_or_else(|| "Not a supported Office Open XML file".to_string())
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let archive = parse_archive(data)?;
    let kind =
        detect_kind(&archive).ok_or_else(|| "Not a supported Office Open XML file".to_string())?;

    let mut removed = BTreeSet::new();
    let mut changed = BTreeMap::new();

    for entry in &archive.entries {
        if is_document_property_part(&entry.name) || is_removed_part_for_kind(&entry.name, kind) {
            removed.insert(entry.name.clone());
        }
    }

    if let Some(xml) = archive.read_xml("[Content_Types].xml")? {
        let cleaned = clean_content_types(&xml, kind);
        if cleaned != xml {
            changed.insert("[Content_Types].xml".to_string(), cleaned.into_bytes());
        }
    }

    for name in relationship_part_names(&archive) {
        let Some(xml) = archive.read_xml(&name)? else {
            continue;
        };
        let cleaned = clean_relationships(&xml, kind, &name);
        if cleaned != xml {
            changed.insert(name, cleaned.into_bytes());
        }
    }

    if kind == OoxmlKind::Docx {
        for name in docx_editable_part_names(&archive) {
            let Some(xml) = archive.read_xml(&name)? else {
                continue;
            };
            let cleaned = clean_word_content(&xml);
            if cleaned != xml {
                changed.insert(name, cleaned.into_bytes());
            }
        }
    }

    build_zip(&archive, &removed, &changed)
}

fn parse_archive(data: &[u8]) -> Result<ZipArchive<'_>, String> {
    let eocd_offset = find_eocd(data).ok_or_else(|| "Not a valid ZIP archive".to_string())?;

    if read_u16_le(data, eocd_offset + 4)? != 0 || read_u16_le(data, eocd_offset + 6)? != 0 {
        return Err("Multi-disk ZIP archives are not supported".to_string());
    }

    let disk_entries = read_u16_le(data, eocd_offset + 8)?;
    let total_entries = read_u16_le(data, eocd_offset + 10)?;
    let central_size = read_u32_le(data, eocd_offset + 12)?;
    let central_offset = read_u32_le(data, eocd_offset + 16)?;

    if disk_entries != total_entries {
        return Err("Multi-disk ZIP archives are not supported".to_string());
    }
    if total_entries == ZIP64_SENTINEL_16
        || central_size == ZIP64_SENTINEL_32
        || central_offset == ZIP64_SENTINEL_32
    {
        return Err("ZIP64 Office files are not supported".to_string());
    }

    let total_entries = total_entries as usize;
    if total_entries > MAX_ENTRIES {
        return Err("Office file contains too many ZIP entries".to_string());
    }

    let central_offset = central_offset as usize;
    let central_size = central_size as usize;
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or_else(|| "Invalid ZIP central directory".to_string())?;
    if central_end > data.len() || central_end > eocd_offset {
        return Err("Invalid ZIP central directory".to_string());
    }

    let mut entries = Vec::with_capacity(total_entries);
    let mut by_name = BTreeMap::new();
    let mut offset = central_offset;
    let mut total_uncompressed = 0u64;

    for _ in 0..total_entries {
        if read_u32_le(data, offset)? != CENTRAL_FILE_HEADER {
            return Err("Invalid ZIP central directory entry".to_string());
        }

        let flags = read_u16_le(data, offset + 8)?;
        let compression = read_u16_le(data, offset + 10)?;
        let crc32 = read_u32_le(data, offset + 16)?;
        let compressed_size = read_u32_le(data, offset + 20)?;
        let uncompressed_size = read_u32_le(data, offset + 24)?;
        let name_len = read_u16_le(data, offset + 28)? as usize;
        let extra_len = read_u16_le(data, offset + 30)? as usize;
        let comment_len = read_u16_le(data, offset + 32)? as usize;
        let disk_start = read_u16_le(data, offset + 34)?;
        let external_attrs = read_u32_le(data, offset + 38)?;
        let local_header_offset = read_u32_le(data, offset + 42)?;

        if flags & 0x0001 != 0 {
            return Err("Encrypted Office files are not supported".to_string());
        }
        if disk_start != 0 {
            return Err("Multi-disk ZIP archives are not supported".to_string());
        }
        if compression != 0 && compression != 8 {
            return Err("Office file uses unsupported ZIP compression".to_string());
        }
        if compressed_size == ZIP64_SENTINEL_32
            || uncompressed_size == ZIP64_SENTINEL_32
            || local_header_offset == ZIP64_SENTINEL_32
        {
            return Err("ZIP64 Office files are not supported".to_string());
        }

        let name_start = offset + 46;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or_else(|| "Invalid ZIP entry name".to_string())?;
        let entry_end = name_end
            .checked_add(extra_len)
            .and_then(|end| end.checked_add(comment_len))
            .ok_or_else(|| "Invalid ZIP central directory entry".to_string())?;
        if entry_end > central_end {
            return Err("Invalid ZIP central directory entry".to_string());
        }

        let name = std::str::from_utf8(&data[name_start..name_end])
            .map_err(|_| "ZIP entry names must be UTF-8".to_string())?
            .to_string();
        validate_entry_name(&name)?;

        let local_offset = local_header_offset as usize;
        if read_u32_le(data, local_offset)? != LOCAL_FILE_HEADER {
            return Err(format!("Invalid ZIP local header for {}", name));
        }

        let local_name_len = read_u16_le(data, local_offset + 26)? as usize;
        let local_extra_len = read_u16_le(data, local_offset + 28)? as usize;
        let local_name_start = local_offset + 30;
        let local_name_end = local_name_start
            .checked_add(local_name_len)
            .ok_or_else(|| format!("Invalid ZIP local header for {}", name))?;
        let data_start = local_name_end
            .checked_add(local_extra_len)
            .ok_or_else(|| format!("Invalid ZIP local header for {}", name))?;
        let data_end = data_start
            .checked_add(compressed_size as usize)
            .ok_or_else(|| format!("Invalid ZIP compressed size for {}", name))?;
        if local_name_end > data.len() || data_end > data.len() {
            return Err(format!("Invalid ZIP compressed size for {}", name));
        }
        if data[local_name_start..local_name_end] != data[name_start..name_end] {
            return Err(format!("ZIP local header name mismatch for {}", name));
        }

        total_uncompressed = total_uncompressed
            .checked_add(uncompressed_size as u64)
            .ok_or_else(|| "Office file is too large to inspect safely".to_string())?;
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err("Office file is too large to inspect safely".to_string());
        }

        let entry = ZipEntry {
            name: name.clone(),
            flags,
            compression,
            crc32,
            compressed_size,
            uncompressed_size,
            data_start,
            data_end,
            external_attrs,
        };
        if by_name.insert(name.clone(), entries.len()).is_some() {
            return Err(format!("Duplicate ZIP entry {}", name));
        }
        entries.push(entry);
        offset = entry_end;
    }

    if offset != central_end {
        return Err("Invalid ZIP central directory size".to_string());
    }

    if total_uncompressed > (data.len() as u64).saturating_mul(MAX_COMPRESSION_RATIO)
        && total_uncompressed > 64 * 1024 * 1024
    {
        return Err("Office file has an unsafe compression ratio".to_string());
    }

    Ok(ZipArchive {
        data,
        entries,
        by_name,
    })
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }

    let search_start = data.len().saturating_sub(22 + u16::MAX as usize);
    for offset in (search_start..=data.len() - 22).rev() {
        if data.get(offset..offset + 4) == Some(&END_OF_CENTRAL_DIRECTORY.to_le_bytes()) {
            let comment_len = read_u16_le(data, offset + 20).ok()? as usize;
            if offset + 22 + comment_len == data.len() {
                return Some(offset);
            }
        }
    }

    None
}

fn detect_kind(archive: &ZipArchive<'_>) -> Option<OoxmlKind> {
    if !archive.has_entry("[Content_Types].xml") {
        return None;
    }

    if archive.has_entry("word/document.xml") {
        Some(OoxmlKind::Docx)
    } else if archive.has_entry("xl/workbook.xml") {
        Some(OoxmlKind::Xlsx)
    } else if archive.has_entry("ppt/presentation.xml") {
        Some(OoxmlKind::Pptx)
    } else {
        None
    }
}

fn validate_entry_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.split('/').any(|part| part == "..")
    {
        return Err(format!("Unsafe ZIP entry name {}", name));
    }
    Ok(())
}

fn collect_document_properties(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    for (name, label, tags) in [
        (
            "docProps/core.xml",
            "Core properties",
            &[
                "title",
                "subject",
                "creator",
                "keywords",
                "description",
                "lastModifiedBy",
                "created",
                "modified",
                "revision",
                "category",
            ][..],
        ),
        (
            "docProps/app.xml",
            "App properties",
            &["Application", "Company", "Manager", "Template", "TotalTime"][..],
        ),
    ] {
        let Some(entry) = archive.entry(name) else {
            continue;
        };
        *total_bytes += entry.uncompressed_size as usize;
        entries.push(MetadataEntry {
            category: "Document properties".to_string(),
            name: label.to_string(),
            value: format!("{} bytes", entry.uncompressed_size),
        });

        if let Ok(Some(xml)) = archive.read_xml(name) {
            for tag in tags {
                for value in extract_text_values(&xml, tag) {
                    if !value.is_empty() {
                        entries.push(MetadataEntry {
                            category: "Document properties".to_string(),
                            name: (*tag).to_string(),
                            value,
                        });
                    }
                }
            }
        }
    }

    let Some(entry) = archive.entry("docProps/custom.xml") else {
        return;
    };
    *total_bytes += entry.uncompressed_size as usize;
    entries.push(MetadataEntry {
        category: "Document properties".to_string(),
        name: "Custom properties".to_string(),
        value: format!("{} bytes", entry.uncompressed_size),
    });

    if let Ok(Some(xml)) = archive.read_xml("docProps/custom.xml") {
        for property in collect_custom_property_names(&xml) {
            entries.push(MetadataEntry {
                category: "Document properties".to_string(),
                name: "Custom property".to_string(),
                value: property,
            });
        }
    }
}

fn collect_docx_review_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    let mut authors = BTreeSet::new();

    for name in docx_editable_part_names(archive) {
        let Ok(Some(xml)) = archive.read_xml(&name) else {
            continue;
        };

        let insertions = count_start_tags(&xml, &["ins", "moveTo"]);
        let deletions = count_start_tags(&xml, &["del", "moveFrom"]);
        let revision_ids = count_attrs_with_local_prefix(&xml, "rsid");

        for author in collect_attr_values(&xml, "author") {
            authors.insert(author);
        }

        if insertions > 0 {
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: format!("Tracked insertions in {}", name),
                value: format!("{insertions} present; removed during DOCX cleaning"),
            });
        }
        if deletions > 0 {
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: format!("Tracked deletions in {}", name),
                value: format!("{deletions} present; removed during DOCX cleaning"),
            });
        }
        if revision_ids > 0 {
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: format!("Revision IDs in {}", name),
                value: format!("{revision_ids} present; removed during DOCX cleaning"),
            });
        }
    }

    for name in docx_comment_part_names(archive) {
        let Some(entry) = archive.entry(&name) else {
            continue;
        };
        *total_bytes += entry.uncompressed_size as usize;
        entries.push(MetadataEntry {
            category: "Review data".to_string(),
            name: format!("Comments in {}", name),
            value: "Present; removed during DOCX cleaning".to_string(),
        });

        if let Ok(Some(xml)) = archive.read_xml(&name) {
            for author in collect_attr_values(&xml, "author") {
                authors.insert(author);
            }
        }
    }

    if !authors.is_empty() {
        entries.push(MetadataEntry {
            category: "Review data".to_string(),
            name: "Review authors".to_string(),
            value: authors.into_iter().collect::<Vec<_>>().join(", "),
        });
    }
}

fn collect_xlsx_review_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    for entry in &archive.entries {
        if is_xlsx_review_part(&entry.name) {
            *total_bytes += entry.uncompressed_size as usize;
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: entry.name.clone(),
                value: "Present, NOT removed by this version".to_string(),
            });
        }
    }
}

fn collect_pptx_review_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    for entry in &archive.entries {
        if is_pptx_review_part(&entry.name) {
            *total_bytes += entry.uncompressed_size as usize;
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: entry.name.clone(),
                value: "Present; removed during PPTX cleaning".to_string(),
            });
        }
    }
}

fn relationship_part_names(archive: &ZipArchive<'_>) -> Vec<String> {
    archive
        .entries
        .iter()
        .filter(|entry| entry.name.ends_with(".rels"))
        .map(|entry| entry.name.clone())
        .collect()
}

fn docx_editable_part_names(archive: &ZipArchive<'_>) -> Vec<String> {
    archive
        .entries
        .iter()
        .filter(|entry| is_docx_editable_part(&entry.name))
        .map(|entry| entry.name.clone())
        .collect()
}

fn docx_comment_part_names(archive: &ZipArchive<'_>) -> Vec<String> {
    archive
        .entries
        .iter()
        .filter(|entry| is_docx_removed_part(&entry.name))
        .map(|entry| entry.name.clone())
        .collect()
}

fn is_document_property_part(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("docprops/")
}

fn is_removed_part_for_kind(name: &str, kind: OoxmlKind) -> bool {
    match kind {
        OoxmlKind::Docx => is_docx_removed_part(name),
        OoxmlKind::Xlsx => false,
        OoxmlKind::Pptx => is_pptx_removed_part(name),
    }
}

fn is_docx_editable_part(name: &str) -> bool {
    name == "word/document.xml"
        || name == "word/settings.xml"
        || name == "word/footnotes.xml"
        || name == "word/endnotes.xml"
        || (name.starts_with("word/header") && name.ends_with(".xml"))
        || (name.starts_with("word/footer") && name.ends_with(".xml"))
}

fn is_docx_removed_part(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    (name.starts_with("word/comments") && name.ends_with(".xml"))
        || name == "word/people.xml"
        || name == "word/commentauthors.xml"
}

fn is_xlsx_review_part(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    (name.starts_with("xl/comments") && name.ends_with(".xml"))
        || (name.starts_with("xl/threadedcomments/") && name.ends_with(".xml"))
        || (name.starts_with("xl/revisions/") && name.ends_with(".xml"))
        || name == "xl/persons/person.xml"
}

fn is_pptx_review_part(name: &str) -> bool {
    is_pptx_removed_part(name)
}

fn is_pptx_removed_part(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    (name.starts_with("ppt/comments/") && name.ends_with(".xml"))
        || name == "ppt/commentauthors.xml"
        || (name.starts_with("ppt/threadedcomments/") && name.ends_with(".xml"))
        || name == "ppt/authors.xml"
}

fn clean_content_types(xml: &str, kind: OoxmlKind) -> String {
    remove_xml_elements_where(xml, &["Override"], |attrs| {
        let part_name = attr_value(attrs, "PartName")
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_ascii_lowercase();

        part_name.starts_with("docprops/") || is_removed_part_for_kind(&part_name, kind)
    })
}

fn clean_relationships(xml: &str, kind: OoxmlKind, relationship_part_name: &str) -> String {
    remove_xml_elements_where(xml, &["Relationship"], |attrs| {
        if attr_value(attrs, "TargetMode")
            .map(|value| value.eq_ignore_ascii_case("External"))
            .unwrap_or(false)
        {
            return false;
        }

        let target = attr_value(attrs, "Target")
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default()
            .replace('\\', "/");
        let Some(target_part) = relationship_target_part_name(relationship_part_name, &target)
        else {
            return false;
        };

        is_document_property_part(&target_part) || is_removed_part_for_kind(&target_part, kind)
    })
}

fn relationship_target_part_name(relationship_part_name: &str, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if !target.starts_with('/') {
        let base = relationship_source_base(relationship_part_name)?;
        parts.extend(
            base.split('/')
                .filter(|part| !part.is_empty())
                .map(str::to_string),
        );
    }

    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part.to_string()),
        }
    }

    Some(parts.join("/").to_ascii_lowercase())
}

fn relationship_source_base(relationship_part_name: &str) -> Option<&str> {
    if relationship_part_name == "_rels/.rels" {
        return Some("");
    }

    relationship_part_name
        .rfind("/_rels/")
        .map(|rels_offset| &relationship_part_name[..rels_offset])
}

fn clean_word_content(xml: &str) -> String {
    let without_deleted = remove_xml_elements(
        xml,
        &[
            "del",
            "moveFrom",
            "commentRangeStart",
            "commentRangeEnd",
            "commentReference",
            "rsids",
            "trackRevisions",
        ],
    );
    let flattened = unwrap_xml_elements(&without_deleted, &["ins", "moveTo"]);
    strip_attrs_by_local_name(&flattened, &["author", "date", "initials"], &["rsid"])
}

fn build_zip(
    archive: &ZipArchive<'_>,
    removed: &BTreeSet<String>,
    changed: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(archive.data.len());
    let mut central = Vec::new();
    let mut seen = BTreeSet::new();

    for entry in &archive.entries {
        if removed.contains(&entry.name) {
            continue;
        }

        seen.insert(entry.name.clone());
        let local_offset = to_u32(out.len(), "ZIP output is too large")?;
        if let Some(data) = changed.get(&entry.name) {
            let crc32 = crc32fast::hash(data);
            let size = to_u32(data.len(), "ZIP entry is too large")?;
            write_local_header(&mut out, &entry.name, 0x0800, 0, crc32, size, size)?;
            out.extend_from_slice(data);
            central.push(CentralRecord {
                name: entry.name.clone(),
                flags: 0x0800,
                compression: 0,
                crc32,
                compressed_size: size,
                uncompressed_size: size,
                local_offset,
                external_attrs: entry.external_attrs,
            });
        } else {
            let flags = entry.flags & !0x0008;
            write_local_header(
                &mut out,
                &entry.name,
                flags,
                entry.compression,
                entry.crc32,
                entry.compressed_size,
                entry.uncompressed_size,
            )?;
            out.extend_from_slice(archive.compressed_data(entry));
            central.push(CentralRecord {
                name: entry.name.clone(),
                flags,
                compression: entry.compression,
                crc32: entry.crc32,
                compressed_size: entry.compressed_size,
                uncompressed_size: entry.uncompressed_size,
                local_offset,
                external_attrs: entry.external_attrs,
            });
        }
    }

    for (name, data) in changed {
        if seen.contains(name) || removed.contains(name) {
            continue;
        }
        let local_offset = to_u32(out.len(), "ZIP output is too large")?;
        let crc32 = crc32fast::hash(data);
        let size = to_u32(data.len(), "ZIP entry is too large")?;
        write_local_header(&mut out, name, 0x0800, 0, crc32, size, size)?;
        out.extend_from_slice(data);
        central.push(CentralRecord {
            name: name.clone(),
            flags: 0x0800,
            compression: 0,
            crc32,
            compressed_size: size,
            uncompressed_size: size,
            local_offset,
            external_attrs: 0,
        });
    }

    let central_offset = to_u32(out.len(), "ZIP output is too large")?;
    for record in &central {
        write_central_header(&mut out, record)?;
    }
    let central_size = to_u32(
        out.len() - central_offset as usize,
        "ZIP central directory is too large",
    )?;
    let entry_count = u16::try_from(central.len())
        .map_err(|_| "ZIP output contains too many entries".to_string())?;

    write_u32_le(&mut out, END_OF_CENTRAL_DIRECTORY);
    write_u16_le(&mut out, 0);
    write_u16_le(&mut out, 0);
    write_u16_le(&mut out, entry_count);
    write_u16_le(&mut out, entry_count);
    write_u32_le(&mut out, central_size);
    write_u32_le(&mut out, central_offset);
    write_u16_le(&mut out, 0);

    Ok(out)
}

struct CentralRecord {
    name: String,
    flags: u16,
    compression: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    local_offset: u32,
    external_attrs: u32,
}

fn write_local_header(
    out: &mut Vec<u8>,
    name: &str,
    flags: u16,
    compression: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
) -> Result<(), String> {
    let name_len =
        u16::try_from(name.len()).map_err(|_| "ZIP entry name is too long".to_string())?;
    write_u32_le(out, LOCAL_FILE_HEADER);
    write_u16_le(out, 20);
    write_u16_le(out, flags);
    write_u16_le(out, compression);
    write_u16_le(out, ZIP_TIME);
    write_u16_le(out, ZIP_DATE);
    write_u32_le(out, crc32);
    write_u32_le(out, compressed_size);
    write_u32_le(out, uncompressed_size);
    write_u16_le(out, name_len);
    write_u16_le(out, 0);
    out.extend_from_slice(name.as_bytes());
    Ok(())
}

fn write_central_header(out: &mut Vec<u8>, record: &CentralRecord) -> Result<(), String> {
    let name_len =
        u16::try_from(record.name.len()).map_err(|_| "ZIP entry name is too long".to_string())?;
    write_u32_le(out, CENTRAL_FILE_HEADER);
    write_u16_le(out, 20);
    write_u16_le(out, 20);
    write_u16_le(out, record.flags);
    write_u16_le(out, record.compression);
    write_u16_le(out, ZIP_TIME);
    write_u16_le(out, ZIP_DATE);
    write_u32_le(out, record.crc32);
    write_u32_le(out, record.compressed_size);
    write_u32_le(out, record.uncompressed_size);
    write_u16_le(out, name_len);
    write_u16_le(out, 0);
    write_u16_le(out, 0);
    write_u16_le(out, 0);
    write_u16_le(out, 0);
    write_u32_le(out, record.external_attrs);
    write_u32_le(out, record.local_offset);
    out.extend_from_slice(record.name.as_bytes());
    Ok(())
}

fn extract_text_values(xml: &str, local_name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing || tag.self_closing || tag.local != local_name {
            continue;
        }

        let Some((content_end, end_tag_end)) = find_matching_element_bounds(xml, &tag) else {
            continue;
        };
        search_pos = end_tag_end;
        let text = decode_xml_entities(&strip_xml_tags(&xml[tag.end..content_end]))
            .trim()
            .to_string();
        if !text.is_empty() {
            values.push(truncate_for_display(&text, 180));
        }
    }

    values
}

fn collect_custom_property_names(xml: &str) -> Vec<String> {
    let mut properties = Vec::new();
    let mut search_pos = 0;
    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing || tag.local != "property" {
            continue;
        }
        let attrs = parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end);
        if let Some(name) = attr_value(&attrs, "name") {
            properties.push(truncate_for_display(&name, 180));
        }
    }
    properties
}

fn collect_attr_values(xml: &str, attr_local_name: &str) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing {
            continue;
        }
        for attr in parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end) {
            if local_name(&attr.name) == attr_local_name && !attr.value.trim().is_empty() {
                values.insert(truncate_for_display(attr.value.trim(), 180));
            }
        }
    }

    values
}

fn count_start_tags(xml: &str, local_names: &[&str]) -> usize {
    let mut count = 0;
    let mut search_pos = 0;
    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if !tag.closing && local_names.contains(&tag.local) {
            count += 1;
        }
    }
    count
}

fn count_attrs_with_local_prefix(xml: &str, prefix: &str) -> usize {
    let mut count = 0;
    let mut search_pos = 0;
    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing {
            continue;
        }
        count += parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end)
            .into_iter()
            .filter(|attr| local_name(&attr.name).starts_with(prefix))
            .count();
    }
    count
}

fn remove_xml_elements(xml: &str, local_names: &[&str]) -> String {
    remove_xml_elements_where(xml, local_names, |_| true)
}

fn remove_xml_elements_where<F>(xml: &str, local_names: &[&str], should_remove: F) -> String
where
    F: Fn(&[XmlAttr]) -> bool,
{
    let mut out = String::with_capacity(xml.len());
    let mut copy_pos = 0;
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing || !local_names.contains(&tag.local) {
            continue;
        }

        let attrs = parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end);
        if !should_remove(&attrs) {
            continue;
        }

        let remove_end = if tag.self_closing {
            tag.end
        } else {
            find_matching_element_bounds(xml, &tag)
                .map(|(_, end_tag_end)| end_tag_end)
                .unwrap_or(tag.end)
        };

        out.push_str(&xml[copy_pos..tag.start]);
        copy_pos = remove_end;
        search_pos = remove_end;
    }

    out.push_str(&xml[copy_pos..]);
    out
}

fn unwrap_xml_elements(xml: &str, local_names: &[&str]) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut copy_pos = 0;
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if local_names.contains(&tag.local) {
            out.push_str(&xml[copy_pos..tag.start]);
            copy_pos = tag.end;
        }
    }

    out.push_str(&xml[copy_pos..]);
    out
}

fn strip_attrs_by_local_name(xml: &str, exact: &[&str], prefixes: &[&str]) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut copy_pos = 0;
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing {
            continue;
        }

        let attrs = parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end);
        if attrs.is_empty() {
            continue;
        }

        let mut kept = Vec::new();
        let mut changed = false;
        for attr in attrs {
            let local = local_name(&attr.name);
            if exact.contains(&local) || prefixes.iter().any(|prefix| local.starts_with(prefix)) {
                changed = true;
            } else {
                kept.push(attr);
            }
        }

        if !changed {
            continue;
        }

        out.push_str(&xml[copy_pos..tag.start]);
        out.push_str(&xml[tag.start..tag.attrs_start]);
        for attr in kept {
            out.push_str(&xml[attr.start..attr.end]);
        }
        if tag.self_closing {
            out.push_str("/>");
        } else {
            out.push('>');
        }
        copy_pos = tag.end;
    }

    out.push_str(&xml[copy_pos..]);
    out
}

fn strip_xml_tags(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut copy_pos = 0;
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        out.push_str(&xml[copy_pos..tag.start]);
        copy_pos = tag.end;
    }

    out.push_str(&xml[copy_pos..]);
    out
}

#[derive(Debug)]
struct XmlTag<'a> {
    start: usize,
    end: usize,
    attrs_start: usize,
    attrs_end: usize,
    local: &'a str,
    closing: bool,
    self_closing: bool,
}

#[derive(Debug)]
struct XmlAttr {
    name: String,
    value: String,
    start: usize,
    end: usize,
}

fn find_next_xml_tag(xml: &str, from: usize) -> Option<XmlTag<'_>> {
    let mut search = from;
    while search < xml.len() {
        let relative = xml[search..].find('<')?;
        let start = search + relative;
        if xml[start..].starts_with("<!--") {
            search = xml[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)?;
            continue;
        }
        if xml[start..].starts_with("<![CDATA[") {
            search = xml[start + 9..]
                .find("]]>")
                .map(|end| start + 9 + end + 3)?;
            continue;
        }
        if matches!(xml.as_bytes().get(start + 1), Some(b'!' | b'?')) {
            search = find_tag_end(xml, start)?;
            continue;
        }
        return parse_xml_tag(xml, start).or_else(|| {
            search = start + 1;
            find_next_xml_tag(xml, search)
        });
    }
    None
}

fn parse_xml_tag(xml: &str, start: usize) -> Option<XmlTag<'_>> {
    let end = find_tag_end(xml, start)?;
    let bytes = xml.as_bytes();
    let mut pos = start + 1;
    let closing = bytes.get(pos) == Some(&b'/');
    if closing {
        pos += 1;
    }

    while matches!(bytes.get(pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        pos += 1;
    }

    let name_start = pos;
    while let Some(byte) = bytes.get(pos) {
        if matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b'/' | b'>') {
            break;
        }
        pos += 1;
    }
    if pos == name_start {
        return None;
    }

    let name = &xml[name_start..pos];
    let mut attrs_end = end - 1;
    while attrs_end > pos && matches!(bytes[attrs_end - 1], b' ' | b'\n' | b'\r' | b'\t') {
        attrs_end -= 1;
    }
    let self_closing = !closing && attrs_end > pos && bytes[attrs_end - 1] == b'/';
    if self_closing {
        attrs_end -= 1;
    }

    Some(XmlTag {
        start,
        end,
        attrs_start: pos,
        attrs_end,
        local: local_name(name),
        closing,
        self_closing,
    })
}

fn find_tag_end(xml: &str, start: usize) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut quote = None;
    let mut pos = start + 1;
    while let Some(byte) = bytes.get(pos) {
        match (quote, byte) {
            (Some(q), current) if q == *current => quote = None,
            (None, b'\'' | b'"') => quote = Some(*byte),
            (None, b'>') => return Some(pos + 1),
            _ => {}
        }
        pos += 1;
    }
    None
}

fn parse_xml_attrs(xml: &str, from: usize, to: usize) -> Vec<XmlAttr> {
    let bytes = xml.as_bytes();
    let mut attrs = Vec::new();
    let mut pos = from;

    while pos < to {
        let attr_start = pos;
        while pos < to && matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
        }
        if pos >= to {
            break;
        }

        let name_start = pos;
        while pos < to && !matches!(bytes[pos], b'=' | b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
        }
        let name_end = pos;

        while pos < to && matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
        }
        if pos >= to || bytes[pos] != b'=' {
            continue;
        }
        pos += 1;
        while pos < to && matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
        }
        if pos >= to {
            break;
        }

        let quote = bytes[pos];
        if quote != b'\'' && quote != b'"' {
            continue;
        }
        pos += 1;
        let value_start = pos;
        while pos < to && bytes[pos] != quote {
            pos += 1;
        }
        if pos >= to {
            break;
        }
        let value_end = pos;
        pos += 1;

        attrs.push(XmlAttr {
            name: xml[name_start..name_end].to_string(),
            value: decode_xml_entities(&xml[value_start..value_end]),
            start: attr_start,
            end: pos,
        });
    }

    attrs
}

fn find_matching_element_bounds(xml: &str, open: &XmlTag<'_>) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut search_pos = open.end;
    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.local != open.local {
            continue;
        }
        if tag.closing {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some((tag.start, tag.end));
            }
        } else if !tag.self_closing {
            depth += 1;
        }
    }
    None
}

fn attr_value(attrs: &[XmlAttr], local: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attr| local_name(&attr.name) == local)
        .map(|attr| attr.value.clone())
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

fn decode_xml_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let Some(end) = after.find(';') else {
            out.push('&');
            rest = after;
            continue;
        };

        let entity = &after[..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") => {
                if let Ok(code) = u32::from_str_radix(&entity[2..], 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ if entity.starts_with('#') => {
                if let Ok(code) = entity[1..].parse::<u32>() {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
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

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| "Unexpected end of ZIP data".to_string())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "Unexpected end of ZIP data".to_string())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn write_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn to_u32(value: usize, error: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, data) in entries {
            let local_offset = out.len() as u32;
            let crc32 = crc32fast::hash(data);
            let size = data.len() as u32;
            write_local_header(&mut out, name, 0x0800, 0, crc32, size, size).unwrap();
            out.extend_from_slice(data);
            central.push(CentralRecord {
                name: (*name).to_string(),
                flags: 0x0800,
                compression: 0,
                crc32,
                compressed_size: size,
                uncompressed_size: size,
                local_offset,
                external_attrs: 0,
            });
        }

        let central_offset = out.len() as u32;
        for record in &central {
            write_central_header(&mut out, record).unwrap();
        }
        let central_size = out.len() as u32 - central_offset;
        let count = central.len() as u16;
        write_u32_le(&mut out, END_OF_CENTRAL_DIRECTORY);
        write_u16_le(&mut out, 0);
        write_u16_le(&mut out, 0);
        write_u16_le(&mut out, count);
        write_u16_le(&mut out, count);
        write_u32_le(&mut out, central_size);
        write_u32_le(&mut out, central_offset);
        write_u16_le(&mut out, 0);
        out
    }

    fn dirty_docx() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>
</Types>"#;
        let root_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>
</Relationships>"#;
        let document_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>
</Relationships>"#;
        let core = br#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Jane Author</dc:creator>
  <cp:lastModifiedBy>Bob Reviewer</cp:lastModifiedBy>
</cp:coreProperties>"#;
        let app = br#"<?xml version="1.0" encoding="UTF-8"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Company>Secret Company</Company>
</Properties>"#;
        let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:rsidR="001">
  <w:body>
    <w:p w:rsidR="002">
      <w:ins w:id="1" w:author="Jane Author" w:date="2026-05-29T10:00:00Z"><w:r><w:t>Keep me</w:t></w:r></w:ins>
      <w:del w:id="2" w:author="Bob Reviewer" w:date="2026-05-29T10:01:00Z"><w:r><w:delText>Delete me</w:delText></w:r></w:del>
      <w:commentRangeStart w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:commentRangeEnd w:id="0"/>
    </w:p>
  </w:body>
</w:document>"#;
        let comments = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:id="0" w:author="Bob Reviewer" w:date="2026-05-29T10:02:00Z"><w:p><w:r><w:t>Secret comment text</w:t></w:r></w:p></w:comment>
</w:comments>"#;

        stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("docProps/core.xml", core),
            ("docProps/app.xml", app),
            ("word/document.xml", document),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/comments.xml", comments),
        ])
    }

    fn dirty_pptx() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
  <Override PartName="/ppt/comments/comment1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.comments+xml"/>
  <Override PartName="/ppt/threadedComments/threadedComment1.xml" ContentType="application/vnd.ms-powerpoint.threadedcomments+xml"/>
  <Override PartName="/ppt/commentAuthors.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.commentAuthors+xml"/>
  <Override PartName="/ppt/authors.xml" ContentType="application/vnd.ms-powerpoint.authors+xml"/>
</Types>"#;
        let root_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
</Relationships>"#;
        let presentation_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/commentAuthors" Target="commentAuthors.xml"/>
  <Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2018/10/relationships/authors" Target="authors.xml"/>
</Relationships>"#;
        let slide_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments/comment1.xml"/>
  <Relationship Id="rId5" Type="http://schemas.microsoft.com/office/2017/10/relationships/threadedComments" Target="../threadedComments/threadedComment1.xml"/>
  <Relationship Id="rId6" Type="http://schemas.example.test/relationships/commentsExtended" Target="../notes/commentary.xml"/>
</Relationships>"#;
        let core = br#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Presenter Person</dc:creator>
</cp:coreProperties>"#;
        let presentation = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#;
        let slide = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:txBody><a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:r><a:t>Slide text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
        let comments = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:cmLst xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cm authorId="0" text="Secret slide comment"/></p:cmLst>"#;
        let threaded_comments = br#"<?xml version="1.0" encoding="UTF-8"?>
<p188:threadedComment xmlns:p188="http://schemas.microsoft.com/office/powerpoint/2018/8/main">Thread secret</p188:threadedComment>"#;
        let comment_authors = br#"<?xml version="1.0" encoding="UTF-8"?>
<p:cmAuthorLst xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cmAuthor name="Bob Reviewer"/></p:cmAuthorLst>"#;
        let authors = br#"<?xml version="1.0" encoding="UTF-8"?>
<p188:authorLst xmlns:p188="http://schemas.microsoft.com/office/powerpoint/2018/8/main"><p188:author name="Modern Author"/></p188:authorLst>"#;
        let retained_comment_adjacent_part = br#"<?xml version="1.0" encoding="UTF-8"?>
<note>Comment-adjacent content should stay</note>"#;

        stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("docProps/core.xml", core),
            ("ppt/presentation.xml", presentation),
            ("ppt/_rels/presentation.xml.rels", presentation_rels),
            ("ppt/slides/slide1.xml", slide),
            ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
            ("ppt/comments/comment1.xml", comments),
            (
                "ppt/threadedComments/threadedComment1.xml",
                threaded_comments,
            ),
            ("ppt/commentAuthors.xml", comment_authors),
            ("ppt/authors.xml", authors),
            ("ppt/notes/commentary.xml", retained_comment_adjacent_part),
        ])
    }

    #[test]
    fn test_detects_docx_and_extracts_properties_and_review_data() {
        let input = dirty_docx();

        assert_eq!(detect_file_type(&input), Some("docx"));

        let info = extract_metadata(&input);
        assert_eq!(info.file_type, "docx");
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("Jane Author")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("Bob Reviewer")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("Tracked insertions")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("Comments")));
    }

    #[test]
    fn test_remove_metadata_strips_docx_properties_comments_and_tracked_changes() {
        let input = dirty_docx();
        let cleaned = remove_metadata(&input).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        for secret in [
            "docProps/core.xml",
            "docProps/app.xml",
            "word/comments.xml",
            "Jane Author",
            "Bob Reviewer",
            "Secret Company",
            "Secret comment text",
            "Delete me",
            "w:ins",
            "w:del",
            "w:rsid",
            "w:author",
        ] {
            assert!(
                !cleaned_text.contains(secret),
                "cleaned DOCX still contains {secret}"
            );
        }

        assert!(cleaned_text.contains("Keep me"));
        validate(&cleaned).unwrap();

        let cleaned_info = extract_metadata(&cleaned);
        assert!(
            cleaned_info.metadata_found.is_empty(),
            "remaining metadata: {:?}",
            cleaned_info.metadata_found
        );
    }

    #[test]
    fn test_remove_metadata_strips_pptx_properties_comments_and_author_parts() {
        let input = dirty_pptx();

        assert_eq!(detect_file_type(&input), Some("pptx"));
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ppt/comments/comment1.xml"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "ppt/authors.xml"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("removed during PPTX cleaning")));

        let cleaned = remove_metadata(&input).unwrap();
        let archive = parse_archive(&cleaned).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        for removed_name in [
            "docProps/core.xml",
            "ppt/comments/comment1.xml",
            "ppt/threadedComments/threadedComment1.xml",
            "ppt/commentAuthors.xml",
            "ppt/authors.xml",
        ] {
            assert!(
                archive.entry(removed_name).is_none(),
                "cleaned PPTX still contains ZIP entry {removed_name}"
            );
            assert!(
                !cleaned_text.contains(removed_name),
                "cleaned PPTX still references {removed_name}"
            );
        }

        for secret in [
            "Presenter Person",
            "Secret slide comment",
            "Thread secret",
            "Bob Reviewer",
            "Modern Author",
        ] {
            assert!(
                !cleaned_text.contains(secret),
                "cleaned PPTX still contains {secret}"
            );
        }

        let content_types = archive
            .read_xml("[Content_Types].xml")
            .unwrap()
            .expect("content types");
        let presentation_rels = archive
            .read_xml("ppt/_rels/presentation.xml.rels")
            .unwrap()
            .expect("presentation rels");
        let slide_rels = archive
            .read_xml("ppt/slides/_rels/slide1.xml.rels")
            .unwrap()
            .expect("slide rels");

        for xml in [&content_types, &presentation_rels, &slide_rels] {
            for removed_reference in [
                "comments/comment1.xml",
                "threadedComments/threadedComment1.xml",
                "commentAuthors.xml",
                "authors.xml",
                "docProps",
            ] {
                assert!(
                    !xml.contains(removed_reference),
                    "cleaned PPTX XML still contains {removed_reference}: {xml}"
                );
            }
        }

        assert!(archive.entry("ppt/notes/commentary.xml").is_some());
        assert!(slide_rels.contains("commentsExtended"));
        assert!(slide_rels.contains("../notes/commentary.xml"));
        assert!(cleaned_text.contains("Slide text"));
        assert!(cleaned_text.contains("Comment-adjacent content should stay"));
        validate(&cleaned).unwrap();

        let cleaned_info = extract_metadata(&cleaned);
        assert!(
            cleaned_info.metadata_found.is_empty(),
            "remaining metadata: {:?}",
            cleaned_info.metadata_found
        );
    }

    #[test]
    fn test_xlsx_comments_are_reported_as_not_removed() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/comments1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
        let sheet_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/></Relationships>"#;
        let core = br#"<cp:coreProperties xmlns:cp="x" xmlns:dc="y"><dc:creator>Alice</dc:creator></cp:coreProperties>"#;
        let workbook =
            br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#;
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#;
        let comments = br#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>Alice</author></authors></comments>"#;
        let input = stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("docProps/core.xml", core),
            ("xl/workbook.xml", workbook),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ("xl/comments1.xml", comments),
        ]);

        assert_eq!(detect_file_type(&input), Some("xlsx"));
        let cleaned = remove_metadata(&input).unwrap();
        let archive = parse_archive(&cleaned).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);
        assert!(!cleaned_text.contains("docProps/core.xml"));
        assert!(cleaned_text.contains("xl/comments1.xml"));
        assert!(archive.entry("xl/comments1.xml").is_some());

        let cleaned_content_types = archive
            .read_xml("[Content_Types].xml")
            .unwrap()
            .expect("content types");
        let cleaned_sheet_rels = archive
            .read_xml("xl/worksheets/_rels/sheet1.xml.rels")
            .unwrap()
            .expect("sheet rels");
        assert!(cleaned_content_types.contains("xl/comments1.xml"));
        assert!(cleaned_sheet_rels.contains("../comments1.xml"));

        let cleaned_info = extract_metadata(&cleaned);
        assert!(cleaned_info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("NOT removed")));
    }

    #[test]
    fn test_rejects_unsafe_zip_paths() {
        let input = stored_zip(&[("../word/document.xml", b"bad")]);
        assert!(parse_archive(&input).is_err());
    }
}
