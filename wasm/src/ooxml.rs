use crate::{MetadataEntry, MetadataInfo};
use flate2::{read::DeflateDecoder, write::DeflateEncoder, Compression};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

const LOCAL_FILE_HEADER: u32 = 0x0403_4b50;
const CENTRAL_FILE_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL_DIRECTORY: u32 = 0x0605_4b50;
const ZIP_TIME: u16 = 0;
const ZIP_DATE: u16 = 0x0021;
const ZIP64_SENTINEL_16: u16 = 0xffff;
const ZIP64_SENTINEL_32: u32 = 0xffff_ffff;
const MAX_ENTRIES: usize = 4096;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_EMBEDDED_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 100;
const EPUB_IDENTIFIER_REPLACEMENT: &str = "urn:uuid:00000000-0000-0000-0000-000000000000";
const EPUB_MODIFIED_REPLACEMENT: &str = "1970-01-01T00:00:00Z";
const EPUB_PACKAGE_MEDIA_TYPE: &str = "application/oebps-package+xml";
const EPUB_MIMETYPE: &str = "application/epub+zip";
const LIMITED_VERIFICATION_CATEGORY: &str = "Limited verification";
const EPUB_REMOVABLE_DC_ELEMENTS: &[&str] = &[
    "creator",
    "contributor",
    "publisher",
    "date",
    "subject",
    "description",
    "rights",
    "source",
    "coverage",
    "relation",
    "format",
    "type",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OoxmlKind {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
    Epub,
}

impl OoxmlKind {
    fn as_str(self) -> &'static str {
        match self {
            OoxmlKind::Docx => "docx",
            OoxmlKind::Xlsx => "xlsx",
            OoxmlKind::Pptx => "pptx",
            OoxmlKind::Odt => "odt",
            OoxmlKind::Ods => "ods",
            OoxmlKind::Odp => "odp",
            OoxmlKind::Epub => "epub",
        }
    }

    fn is_odf(self) -> bool {
        matches!(self, OoxmlKind::Odt | OoxmlKind::Ods | OoxmlKind::Odp)
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
        if crc32fast::hash(&bytes) != entry.crc32 {
            return Err(format!("ZIP CRC mismatch in {}", entry.name));
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

    fn validate_entry(&self, entry: &ZipEntry) -> Result<(), String> {
        let compressed = self.compressed_data(entry);
        match entry.compression {
            0 => {
                if compressed.len() != entry.uncompressed_size as usize {
                    return Err(format!("ZIP size mismatch in {}", entry.name));
                }
                if crc32fast::hash(compressed) != entry.crc32 {
                    return Err(format!("ZIP CRC mismatch in {}", entry.name));
                }
            }
            8 => {
                let mut decoder = DeflateDecoder::new(compressed);
                let mut hasher = crc32fast::Hasher::new();
                let mut buffer = [0u8; 8192];
                let mut total = 0usize;

                loop {
                    let read = decoder
                        .read(&mut buffer)
                        .map_err(|_| format!("Failed to decompress {}", entry.name))?;
                    if read == 0 {
                        break;
                    }
                    total = total
                        .checked_add(read)
                        .ok_or_else(|| format!("{} is too large to inspect safely", entry.name))?;
                    if total > entry.uncompressed_size as usize {
                        return Err(format!("ZIP size mismatch in {}", entry.name));
                    }
                    hasher.update(&buffer[..read]);
                }

                if total != entry.uncompressed_size as usize {
                    return Err(format!("ZIP size mismatch in {}", entry.name));
                }
                if decoder.total_in() != compressed.len() as u64 {
                    return Err(format!("ZIP compressed data mismatch in {}", entry.name));
                }
                if hasher.finalize() != entry.crc32 {
                    return Err(format!("ZIP CRC mismatch in {}", entry.name));
                }
            }
            _ => return Err(format!("Unsupported ZIP compression in {}", entry.name)),
        }

        Ok(())
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
        OoxmlKind::Odt | OoxmlKind::Ods | OoxmlKind::Odp => {
            collect_odf_metadata(&archive, &mut entries, &mut total_bytes)
        }
        OoxmlKind::Epub => collect_epub_metadata(&archive, &mut entries, &mut total_bytes),
    }
    collect_embedded_image_metadata(&archive, &mut entries, &mut total_bytes);

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
        .ok_or_else(|| "Not a supported ZIP document file".to_string())?;
    validate_archive_entries(&archive)
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let archive = parse_archive(data)?;
    let kind =
        detect_kind(&archive).ok_or_else(|| "Not a supported ZIP document file".to_string())?;
    validate_archive_entries(&archive)?;

    let mut removed = BTreeSet::new();
    let mut changed = BTreeMap::new();

    if kind.is_odf() {
        if archive.has_entry("meta.xml") {
            removed.insert("meta.xml".to_string());
        }

        if let Some(xml) = archive.read_xml("META-INF/manifest.xml")? {
            let cleaned = clean_odf_manifest(&xml);
            if cleaned != xml {
                changed.insert("META-INF/manifest.xml".to_string(), cleaned.into_bytes());
            }
        }

        for name in ["content.xml", "settings.xml"] {
            let Some(xml) = archive.read_xml(name)? else {
                continue;
            };
            let cleaned = clean_odf_content(&xml);
            if cleaned != xml {
                changed.insert(name.to_string(), cleaned.into_bytes());
            }
        }

        return build_zip(&archive, &removed, &changed);
    }

    if kind == OoxmlKind::Epub {
        for entry in &archive.entries {
            if is_epub_removed_part(&entry.name) {
                removed.insert(entry.name.clone());
            }
        }

        let package_path = epub_package_path(&archive)
            .ok_or_else(|| "EPUB missing package document".to_string())?;
        let Some(xml) = archive.read_xml(&package_path)? else {
            return Err("EPUB missing package document".to_string());
        };
        let cleaned = clean_epub_package(&xml);
        if cleaned != xml {
            changed.insert(package_path, cleaned.into_bytes());
        }

        return build_zip(&archive, &removed, &changed);
    }

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

fn validate_archive_entries(archive: &ZipArchive<'_>) -> Result<(), String> {
    for entry in &archive.entries {
        archive.validate_entry(entry)?;
    }
    Ok(())
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
    if archive.has_entry("[Content_Types].xml") && archive.has_entry("word/document.xml") {
        Some(OoxmlKind::Docx)
    } else if archive.has_entry("[Content_Types].xml") && archive.has_entry("xl/workbook.xml") {
        Some(OoxmlKind::Xlsx)
    } else if archive.has_entry("[Content_Types].xml") && archive.has_entry("ppt/presentation.xml")
    {
        Some(OoxmlKind::Pptx)
    } else if detect_epub_kind(archive).is_some() {
        Some(OoxmlKind::Epub)
    } else {
        detect_odf_kind(archive)
    }
}

fn detect_odf_kind(archive: &ZipArchive<'_>) -> Option<OoxmlKind> {
    let mimetype = archive.read_entry("mimetype", 256).ok()??;
    match std::str::from_utf8(&mimetype).ok()?.trim() {
        "application/vnd.oasis.opendocument.text" => Some(OoxmlKind::Odt),
        "application/vnd.oasis.opendocument.spreadsheet" => Some(OoxmlKind::Ods),
        "application/vnd.oasis.opendocument.presentation" => Some(OoxmlKind::Odp),
        _ => None,
    }
}

fn detect_epub_kind(archive: &ZipArchive<'_>) -> Option<OoxmlKind> {
    let mimetype = archive.read_entry("mimetype", 256).ok()??;
    if std::str::from_utf8(&mimetype).ok()?.trim() != EPUB_MIMETYPE {
        return None;
    }

    epub_package_path(archive).map(|_| OoxmlKind::Epub)
}

fn epub_package_path(archive: &ZipArchive<'_>) -> Option<String> {
    let container = archive.read_xml("META-INF/container.xml").ok()??;
    let mut search_pos = 0;
    while let Some(tag) = find_next_xml_tag(&container, search_pos) {
        search_pos = tag.end;
        if tag.closing || tag.local != "rootfile" {
            continue;
        }

        let attrs = parse_xml_attrs(&container, tag.attrs_start, tag.attrs_end);
        let media_type = attr_value(&attrs, "media-type").unwrap_or_default();
        if !media_type.is_empty() && media_type != EPUB_PACKAGE_MEDIA_TYPE {
            continue;
        }

        let full_path = normalize_zip_entry_path(&attr_value(&attrs, "full-path")?)?;
        if archive.has_entry(&full_path) {
            return Some(full_path);
        }
    }

    None
}

fn normalize_zip_entry_path(path: &str) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let normalized = normalized.trim_start_matches('/').to_string();
    validate_entry_name(&normalized).ok()?;
    Some(normalized)
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
                value: "Present; removed during XLSX cleaning".to_string(),
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
        OoxmlKind::Xlsx => is_xlsx_removed_part(name),
        OoxmlKind::Pptx => is_pptx_removed_part(name),
        OoxmlKind::Odt | OoxmlKind::Ods | OoxmlKind::Odp => name == "meta.xml",
        OoxmlKind::Epub => is_epub_removed_part(name),
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
    is_xlsx_removed_part(name)
}

fn is_xlsx_removed_part(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    (name.starts_with("xl/comments") && name.ends_with(".xml"))
        || (name.starts_with("xl/threadedcomments/") && name.ends_with(".xml"))
        || (name.starts_with("xl/revisions/") && name.ends_with(".xml"))
        || (name.starts_with("xl/persons/") && name.ends_with(".xml"))
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

fn collect_odf_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    if let Some(entry) = archive.entry("meta.xml") {
        *total_bytes += entry.uncompressed_size as usize;
        entries.push(MetadataEntry {
            category: "Document properties".to_string(),
            name: "ODF metadata".to_string(),
            value: format!("{} bytes", entry.uncompressed_size),
        });

        if let Ok(Some(xml)) = archive.read_xml("meta.xml") {
            for tag in [
                "title",
                "subject",
                "description",
                "creator",
                "initial-creator",
                "creation-date",
                "date",
                "generator",
                "keyword",
                "editing-duration",
                "editing-cycles",
            ] {
                for value in extract_text_values(&xml, tag) {
                    entries.push(MetadataEntry {
                        category: "Document properties".to_string(),
                        name: tag.to_string(),
                        value,
                    });
                }
            }
        }
    }

    for name in ["content.xml", "settings.xml"] {
        let Ok(Some(xml)) = archive.read_xml(name) else {
            continue;
        };
        let annotations = count_start_tags(&xml, &["annotation"]);
        let authors = collect_attr_values(&xml, "creator");
        if annotations > 0 {
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: format!("Annotations in {name}"),
                value: format!("{annotations} present; removed during ODF cleaning"),
            });
        }
        if !authors.is_empty() {
            entries.push(MetadataEntry {
                category: "Review data".to_string(),
                name: format!("Review authors in {name}"),
                value: authors.into_iter().collect::<Vec<_>>().join(", "),
            });
        }
    }
}

fn clean_odf_manifest(xml: &str) -> String {
    remove_xml_elements_where(xml, &["file-entry"], |attrs| {
        attr_value(attrs, "full-path")
            .map(|value| value == "meta.xml")
            .unwrap_or(false)
    })
}

fn clean_odf_content(xml: &str) -> String {
    let without_annotations = remove_xml_elements(xml, &["annotation", "annotation-end"]);
    strip_attrs_by_local_name(&without_annotations, &["creator", "date", "initials"], &[])
}

fn collect_epub_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    let Some(package_path) = epub_package_path(archive) else {
        return;
    };
    let Some(package_entry) = archive.entry(&package_path) else {
        return;
    };
    let Ok(Some(xml)) = archive.read_xml(&package_path) else {
        return;
    };

    let mut package_entries = removable_epub_metadata_entries(&xml);
    if !package_entries.is_empty() {
        *total_bytes += package_entry.uncompressed_size as usize;
        entries.push(MetadataEntry {
            category: "EPUB metadata".to_string(),
            name: "Package metadata".to_string(),
            value: format!(
                "{} removable entries in {}",
                package_entries.len(),
                package_path
            ),
        });
        entries.append(&mut package_entries);
    }

    for entry in &archive.entries {
        if is_epub_removed_part(&entry.name) {
            *total_bytes += entry.uncompressed_size as usize;
            entries.push(MetadataEntry {
                category: "EPUB metadata".to_string(),
                name: entry.name.clone(),
                value: "Present; removed during EPUB cleaning".to_string(),
            });
        }
    }
}

fn collect_embedded_image_metadata(
    archive: &ZipArchive<'_>,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    let mut embedded_image_count = 0usize;
    let mut scanned_image_count = 0usize;

    for entry in &archive.entries {
        let Some(image_type) = embedded_image_type(&entry.name) else {
            continue;
        };
        embedded_image_count += 1;
        let Ok(Some(data)) = archive.read_entry(&entry.name, MAX_EMBEDDED_IMAGE_BYTES) else {
            continue;
        };
        scanned_image_count += 1;

        let info = match image_type {
            "jpeg" => crate::jpeg::extract_metadata(&data),
            "png" => crate::png::extract_metadata(&data),
            "webp" => crate::webp::extract_metadata(&data),
            "gif" => crate::gif::extract_metadata(&data),
            "tiff" => crate::tiff::extract_metadata(&data),
            "svg" => crate::svg::extract_metadata(&data),
            _ => continue,
        };
        if info.metadata_found.is_empty() {
            continue;
        }

        let embedded_count = info.metadata_found.len();
        *total_bytes += info.total_metadata_bytes;
        entries.push(MetadataEntry {
            category: "Embedded image metadata".to_string(),
            name: entry.name.clone(),
            value: format!("{embedded_count} metadata entries inside packaged image"),
        });
        for embedded in info.metadata_found {
            entries.push(MetadataEntry {
                category: format!("Embedded image metadata: {}", embedded.category),
                name: format!("{}: {}", entry.name, embedded.name),
                value: embedded.value,
            });
        }
    }

    if embedded_image_count > 0 {
        entries.push(MetadataEntry {
            category: LIMITED_VERIFICATION_CATEGORY.to_string(),
            name: "Embedded package images".to_string(),
            value: embedded_image_limited_verification_value(
                embedded_image_count,
                scanned_image_count,
            ),
        });
    }
}

fn embedded_image_limited_verification_value(total: usize, scanned: usize) -> String {
    let unscanned = total.saturating_sub(scanned);
    if unscanned == 0 {
        return format!(
            "{total} packaged image(s) preserved; {scanned} scanned for supported metadata; not recursively cleaned"
        );
    }

    format!(
        "{total} packaged image(s) preserved; {scanned} scanned for supported metadata; {unscanned} not scanned due to archive read or size limits; not recursively cleaned"
    )
}

fn embedded_image_type(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("jpeg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        "tif" | "tiff" => Some("tiff"),
        "svg" => Some("svg"),
        _ => None,
    }
}

fn removable_epub_metadata_entries(xml: &str) -> Vec<MetadataEntry> {
    let mut entries = Vec::new();
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing {
            continue;
        }

        let attrs = parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end);
        if EPUB_REMOVABLE_DC_ELEMENTS.contains(&tag.local) {
            entries.push(MetadataEntry {
                category: "EPUB metadata".to_string(),
                name: tag.local.to_string(),
                value: epub_element_text(xml, &tag).unwrap_or_else(|| "Present".to_string()),
            });
        } else if tag.local == "identifier" {
            if let Some(value) = epub_element_text(xml, &tag) {
                if value != EPUB_IDENTIFIER_REPLACEMENT {
                    entries.push(MetadataEntry {
                        category: "EPUB metadata".to_string(),
                        name: "identifier".to_string(),
                        value,
                    });
                }
            }
        } else if tag.local == "meta" {
            let property = attr_value(&attrs, "property")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if property == "dcterms:modified" {
                if let Some(value) = epub_element_text(xml, &tag) {
                    if value != EPUB_MODIFIED_REPLACEMENT {
                        entries.push(MetadataEntry {
                            category: "EPUB metadata".to_string(),
                            name: "dcterms:modified".to_string(),
                            value,
                        });
                    }
                }
            } else if is_removable_epub_meta_tag(&attrs) {
                entries.push(MetadataEntry {
                    category: "EPUB metadata".to_string(),
                    name: epub_meta_name(&attrs),
                    value: epub_element_text(xml, &tag)
                        .or_else(|| attr_value(&attrs, "content"))
                        .unwrap_or_else(|| "Present".to_string()),
                });
            }
        } else if tag.local == "link" {
            entries.push(MetadataEntry {
                category: "EPUB metadata".to_string(),
                name: epub_link_name(&attrs),
                value: attr_value(&attrs, "href").unwrap_or_else(|| "Present".to_string()),
            });
        }
    }

    entries
}

fn epub_element_text(xml: &str, tag: &XmlTag<'_>) -> Option<String> {
    if tag.self_closing {
        return None;
    }
    let (content_end, _) = find_matching_element_bounds(xml, tag)?;
    let text = decode_xml_entities(&strip_xml_tags(&xml[tag.end..content_end]))
        .trim()
        .to_string();
    (!text.is_empty()).then_some(truncate_for_display(&text, 180))
}

fn epub_meta_name(attrs: &[XmlAttr]) -> String {
    attr_value(attrs, "property")
        .or_else(|| attr_value(attrs, "name"))
        .map(|value| format!("meta {}", truncate_for_display(&value, 80)))
        .unwrap_or_else(|| "meta".to_string())
}

fn epub_link_name(attrs: &[XmlAttr]) -> String {
    attr_value(attrs, "rel")
        .map(|value| format!("link {}", truncate_for_display(&value, 80)))
        .unwrap_or_else(|| "link".to_string())
}

fn is_epub_removed_part(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "meta-inf/metadata.xml"
            | "metadata.xml"
            | "meta-inf/calibre_bookmarks.txt"
            | "calibre_bookmarks.txt"
    )
}

fn is_removable_epub_meta_tag(attrs: &[XmlAttr]) -> bool {
    let property = attr_value(attrs, "property")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let name = attr_value(attrs, "name")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if property == "dcterms:modified"
        || property.starts_with("rendition:")
        || property.starts_with("media:")
        || name == "cover"
    {
        return false;
    }

    true
}

fn clean_epub_package(xml: &str) -> String {
    let without_optional_dc = remove_xml_elements(xml, EPUB_REMOVABLE_DC_ELEMENTS);
    let without_optional_meta =
        remove_xml_elements_where(&without_optional_dc, &["meta"], |attrs| {
            is_removable_epub_meta_tag(attrs)
        });
    let without_links = remove_xml_elements(&without_optional_meta, &["link"]);
    let normalized_identifier = replace_xml_element_text(&without_links, &["identifier"], |_| {
        EPUB_IDENTIFIER_REPLACEMENT.to_string()
    });

    replace_xml_element_text(&normalized_identifier, &["meta"], |attrs| {
        let property = attr_value(attrs, "property")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if property == "dcterms:modified" {
            EPUB_MODIFIED_REPLACEMENT.to_string()
        } else {
            String::new()
        }
    })
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
            let (compression, payload) = encode_changed_zip_data(data)?;
            let crc32 = crc32fast::hash(data);
            let compressed_size = to_u32(payload.len(), "ZIP entry is too large")?;
            let uncompressed_size = to_u32(data.len(), "ZIP entry is too large")?;
            write_local_header(
                &mut out,
                &entry.name,
                0x0800,
                compression,
                crc32,
                compressed_size,
                uncompressed_size,
            )?;
            out.extend_from_slice(&payload);
            central.push(CentralRecord {
                name: entry.name.clone(),
                flags: 0x0800,
                compression,
                crc32,
                compressed_size,
                uncompressed_size,
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
        let (compression, payload) = encode_changed_zip_data(data)?;
        let compressed_size = to_u32(payload.len(), "ZIP entry is too large")?;
        let uncompressed_size = to_u32(data.len(), "ZIP entry is too large")?;
        write_local_header(
            &mut out,
            name,
            0x0800,
            compression,
            crc32,
            compressed_size,
            uncompressed_size,
        )?;
        out.extend_from_slice(&payload);
        central.push(CentralRecord {
            name: name.clone(),
            flags: 0x0800,
            compression,
            crc32,
            compressed_size,
            uncompressed_size,
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

fn encode_changed_zip_data(data: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|_| "Failed to compress ZIP entry".to_string())?;
    let compressed = encoder
        .finish()
        .map_err(|_| "Failed to compress ZIP entry".to_string())?;

    if compressed.len() < data.len() {
        Ok((8, compressed))
    } else {
        Ok((0, data.to_vec()))
    }
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

fn replace_xml_element_text<F>(xml: &str, local_names: &[&str], replacement_for: F) -> String
where
    F: Fn(&[XmlAttr]) -> String,
{
    let mut out = String::with_capacity(xml.len());
    let mut copy_pos = 0;
    let mut search_pos = 0;

    while let Some(tag) = find_next_xml_tag(xml, search_pos) {
        search_pos = tag.end;
        if tag.closing || tag.self_closing || !local_names.contains(&tag.local) {
            continue;
        }

        let attrs = parse_xml_attrs(xml, tag.attrs_start, tag.attrs_end);
        let replacement = replacement_for(&attrs);
        if replacement.is_empty() {
            continue;
        }

        let Some((content_end, end_tag_end)) = find_matching_element_bounds(xml, &tag) else {
            continue;
        };

        out.push_str(&xml[copy_pos..tag.end]);
        out.push_str(&replacement);
        copy_pos = content_end;
        search_pos = end_tag_end;
    }

    out.push_str(&xml[copy_pos..]);
    out
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
        if let Some(tag) = parse_xml_tag(xml, start) {
            return Some(tag);
        } else {
            search = start + 1;
        }
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

    fn stored_zip_with_declared_uncompressed_size(
        entries: &[(&str, &[u8], Option<u32>)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, data, declared_uncompressed_size) in entries {
            let local_offset = out.len() as u32;
            let crc32 = crc32fast::hash(data);
            let compressed_size = data.len() as u32;
            let uncompressed_size = declared_uncompressed_size.unwrap_or(compressed_size);
            write_local_header(
                &mut out,
                name,
                0x0800,
                0,
                crc32,
                compressed_size,
                uncompressed_size,
            )
            .unwrap();
            out.extend_from_slice(data);
            central.push(CentralRecord {
                name: (*name).to_string(),
                flags: 0x0800,
                compression: 0,
                crc32,
                compressed_size,
                uncompressed_size,
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

    fn deflated_zip(entries: &[(&str, &[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, data, extra_compressed_tail) in entries {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(data).unwrap();
            let mut payload = encoder.finish().unwrap();
            payload.extend_from_slice(extra_compressed_tail);

            let local_offset = out.len() as u32;
            let crc32 = crc32fast::hash(data);
            let compressed_size = payload.len() as u32;
            let uncompressed_size = data.len() as u32;
            write_local_header(
                &mut out,
                name,
                0x0800,
                8,
                crc32,
                compressed_size,
                uncompressed_size,
            )
            .unwrap();
            out.extend_from_slice(&payload);
            central.push(CentralRecord {
                name: (*name).to_string(),
                flags: 0x0800,
                compression: 8,
                crc32,
                compressed_size,
                uncompressed_size,
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

    fn sample_jpeg_with_xmp() -> Vec<u8> {
        let payload = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta><dc:creator>Hidden Cover Author</dc:creator></x:xmpmeta>";
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        let length = (payload.len() + 2) as u16;
        jpeg.extend_from_slice(&length.to_be_bytes());
        jpeg.extend_from_slice(payload);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    fn sample_clean_jpeg() -> Vec<u8> {
        vec![0xff, 0xd8, 0xff, 0xd9]
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
        let archive = parse_archive(&cleaned).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        for removed_name in ["docProps/core.xml", "docProps/app.xml", "word/comments.xml"] {
            assert!(
                archive.entry(removed_name).is_none(),
                "cleaned DOCX still contains ZIP entry {removed_name}"
            );
            assert!(
                !cleaned_text.contains(removed_name),
                "cleaned DOCX still references {removed_name}"
            );
        }

        let document_xml = archive
            .read_xml("word/document.xml")
            .unwrap()
            .expect("document XML");
        let content_types = archive
            .read_xml("[Content_Types].xml")
            .unwrap()
            .expect("content types");
        let root_rels = archive.read_xml("_rels/.rels").unwrap().expect("root rels");
        let document_rels = archive
            .read_xml("word/_rels/document.xml.rels")
            .unwrap()
            .expect("document rels");

        for secret in [
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
                !document_xml.contains(secret)
                    && !content_types.contains(secret)
                    && !root_rels.contains(secret)
                    && !document_rels.contains(secret),
                "cleaned DOCX still contains {secret}"
            );
        }

        for xml in [&content_types, &root_rels, &document_rels] {
            for removed_reference in ["docProps", "comments.xml"] {
                assert!(
                    !xml.contains(removed_reference),
                    "cleaned DOCX XML still contains {removed_reference}: {xml}"
                );
            }
        }

        assert!(document_xml.contains("Keep me"));
        validate(&cleaned).unwrap();

        let cleaned_info = extract_metadata(&cleaned);
        assert!(
            cleaned_info.metadata_found.is_empty(),
            "remaining metadata: {:?}",
            cleaned_info.metadata_found
        );
    }

    #[test]
    fn test_embedded_docx_image_metadata_is_flagged_after_cleaning() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>"#;
        let image = sample_jpeg_with_xmp();
        let input = stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document),
            ("word/media/image1.jpg", image.as_slice()),
        ]);

        assert_eq!(detect_file_type(&input), Some("docx"));
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category.starts_with("Embedded image metadata")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("word/media/image1.jpg: XMP Data")));

        let cleaned = remove_metadata(&input).unwrap();
        let verification = extract_metadata(&cleaned);
        assert!(verification
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("word/media/image1.jpg")));
    }

    #[test]
    fn test_clean_embedded_docx_image_gets_limited_verification_note() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>"#;
        let image = sample_clean_jpeg();
        let input = stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document),
            ("word/media/image1.jpg", image.as_slice()),
        ]);

        let info = extract_metadata(&input);
        let note = info
            .metadata_found
            .iter()
            .find(|entry| {
                entry.category == LIMITED_VERIFICATION_CATEGORY
                    && entry.name == "Embedded package images"
            })
            .expect("limited verification note");
        assert!(note.value.contains("1 scanned for supported metadata"));
        assert!(!info
            .metadata_found
            .iter()
            .any(|entry| entry.category.starts_with("Embedded image metadata")));

        let cleaned = remove_metadata(&input).unwrap();
        let verification = extract_metadata(&cleaned);
        assert!(verification.metadata_found.iter().any(|entry| {
            entry.category == LIMITED_VERIFICATION_CATEGORY
                && entry.name == "Embedded package images"
        }));
    }

    #[test]
    fn test_embedded_docx_image_note_reports_unscanned_images() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>"#;
        let image = sample_clean_jpeg();
        let too_large = (MAX_EMBEDDED_IMAGE_BYTES as u32) + 1;
        let input = stored_zip_with_declared_uncompressed_size(&[
            ("[Content_Types].xml", content_types, None),
            ("_rels/.rels", rels, None),
            ("word/document.xml", document, None),
            ("word/media/image1.jpg", image.as_slice(), Some(too_large)),
        ]);

        let info = extract_metadata(&input);
        let note = info
            .metadata_found
            .iter()
            .find(|entry| {
                entry.category == LIMITED_VERIFICATION_CATEGORY
                    && entry.name == "Embedded package images"
            })
            .expect("limited verification note");

        assert!(note.value.contains("1 packaged image(s) preserved"));
        assert!(note.value.contains("0 scanned for supported metadata"));
        assert!(note
            .value
            .contains("1 not scanned due to archive read or size limits"));
        assert!(!note.value.contains("embedded image bytes are scanned"));
        assert!(!info
            .metadata_found
            .iter()
            .any(|entry| entry.category.starts_with("Embedded image metadata")));
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
    fn test_xlsx_comments_revisions_and_person_parts_are_removed() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/comments1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml"/><Override PartName="/xl/threadedComments/threadedComment1.xml" ContentType="application/vnd.ms-excel.threadedcomments+xml"/><Override PartName="/xl/persons/person.xml" ContentType="application/vnd.ms-excel.person+xml"/><Override PartName="/xl/persons/person2.xml" ContentType="application/vnd.ms-excel.person+xml"/><Override PartName="/xl/revisions/revisionLog1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.revisionLog+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
        let workbook_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId4" Type="http://schemas.microsoft.com/office/2017/10/relationships/person" Target="persons/person.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/revisionLog" Target="revisions/revisionLog1.xml"/><Relationship Id="rId7" Type="http://schemas.microsoft.com/office/2017/10/relationships/person" Target="persons/person2.xml"/></Relationships>"#;
        let sheet_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/><Relationship Id="rId6" Type="http://schemas.microsoft.com/office/2017/10/relationships/threadedComment" Target="../threadedComments/threadedComment1.xml"/></Relationships>"#;
        let core = br#"<cp:coreProperties xmlns:cp="x" xmlns:dc="y"><dc:creator>Alice</dc:creator></cp:coreProperties>"#;
        let workbook =
            br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#;
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#;
        let comments = br#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>Alice</author></authors></comments>"#;
        let threaded_comments = br#"<ThreadedComments><threadedComment personId="{11111111-1111-1111-1111-111111111111}" text="Thread secret"/></ThreadedComments>"#;
        let persons = br#"<personList><person displayName="Bob Reviewer" userId="bob@example.test"/></personList>"#;
        let extra_persons = br#"<personList><person displayName="Extra Reviewer" userId="extra@example.test"/></personList>"#;
        let revisions = br#"<revisionLog><rcc ua="Carol Reviewer"/></revisionLog>"#;
        let input = stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("docProps/core.xml", core),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ("xl/comments1.xml", comments),
            (
                "xl/threadedComments/threadedComment1.xml",
                threaded_comments,
            ),
            ("xl/persons/person.xml", persons),
            ("xl/persons/person2.xml", extra_persons),
            ("xl/revisions/revisionLog1.xml", revisions),
        ]);

        assert_eq!(detect_file_type(&input), Some("xlsx"));
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("removed during XLSX cleaning")));

        let cleaned = remove_metadata(&input).unwrap();
        let archive = parse_archive(&cleaned).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        for removed_name in [
            "docProps/core.xml",
            "xl/comments1.xml",
            "xl/threadedComments/threadedComment1.xml",
            "xl/persons/person.xml",
            "xl/persons/person2.xml",
            "xl/revisions/revisionLog1.xml",
        ] {
            assert!(
                archive.entry(removed_name).is_none(),
                "cleaned XLSX still contains ZIP entry {removed_name}"
            );
            assert!(
                !cleaned_text.contains(removed_name),
                "cleaned XLSX still references {removed_name}"
            );
        }

        for secret in [
            "Alice",
            "Thread secret",
            "Bob Reviewer",
            "bob@example.test",
            "Extra Reviewer",
            "extra@example.test",
            "Carol Reviewer",
        ] {
            assert!(
                !cleaned_text.contains(secret),
                "cleaned XLSX still contains {secret}"
            );
        }

        let cleaned_content_types = archive
            .read_xml("[Content_Types].xml")
            .unwrap()
            .expect("content types");
        let cleaned_workbook_rels = archive
            .read_xml("xl/_rels/workbook.xml.rels")
            .unwrap()
            .expect("workbook rels");
        let cleaned_sheet_rels = archive
            .read_xml("xl/worksheets/_rels/sheet1.xml.rels")
            .unwrap()
            .expect("sheet rels");

        for xml in [
            &cleaned_content_types,
            &cleaned_workbook_rels,
            &cleaned_sheet_rels,
        ] {
            for removed_reference in [
                "comments1.xml",
                "threadedComments",
                "persons/",
                "revisions/revisionLog1.xml",
                "docProps",
            ] {
                assert!(
                    !xml.contains(removed_reference),
                    "cleaned XLSX XML still contains {removed_reference}: {xml}"
                );
            }
        }

        assert_eq!(archive.entry("[Content_Types].xml").unwrap().compression, 8);

        let cleaned_info = extract_metadata(&cleaned);
        assert!(
            cleaned_info.metadata_found.is_empty(),
            "remaining metadata: {:?}",
            cleaned_info.metadata_found
        );
    }

    #[test]
    fn test_odf_meta_and_annotations_are_removed() {
        let mimetype = b"application/vnd.oasis.opendocument.text";
        let manifest = br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
        let meta = br#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"><office:meta><dc:creator>Alice Author</dc:creator><meta:initial-creator>Bob Reviewer</meta:initial-creator><meta:generator>SecretOffice</meta:generator></office:meta></office:document-meta>"#;
        let content = br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:body><text:p xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">Keep me<office:annotation><dc:creator>Alice Author</dc:creator><dc:date>2026-05-31</dc:date><text:p>Secret note</text:p></office:annotation></text:p></office:body></office:document-content>"#;
        let input = stored_zip(&[
            ("mimetype", mimetype),
            ("META-INF/manifest.xml", manifest),
            ("meta.xml", meta),
            ("content.xml", content),
        ]);

        assert_eq!(detect_file_type(&input), Some("odt"));
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("Alice Author")));

        let cleaned = remove_metadata(&input).unwrap();
        let archive = parse_archive(&cleaned).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);
        let content_xml = archive
            .read_xml("content.xml")
            .unwrap()
            .expect("content XML");
        let manifest_xml = archive
            .read_xml("META-INF/manifest.xml")
            .unwrap()
            .expect("manifest XML");
        assert!(archive.entry("meta.xml").is_none());
        assert!(!cleaned_text.contains("Alice Author"));
        assert!(!cleaned_text.contains("Bob Reviewer"));
        assert!(!cleaned_text.contains("SecretOffice"));
        assert!(!content_xml.contains("Secret note"));
        assert!(!manifest_xml.contains("meta.xml"));
        assert!(content_xml.contains("Keep me"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_clean_odf_spreadsheet_has_no_metadata() {
        let mimetype = b"application/vnd.oasis.opendocument.spreadsheet";
        let manifest = br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
        let content = br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"><office:body><office:spreadsheet/></office:body></office:document-content>"#;
        let input = stored_zip(&[
            ("mimetype", mimetype),
            ("META-INF/manifest.xml", manifest),
            ("content.xml", content),
        ]);

        assert_eq!(detect_file_type(&input), Some("ods"));
        validate(&input).unwrap();
        assert!(extract_metadata(&input).metadata_found.is_empty());

        let cleaned = remove_metadata(&input).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_epub_package_metadata_identifiers_and_calibre_data_are_removed() {
        let mimetype = b"application/epub+zip";
        let container = br#"<?xml version="1.0" encoding="UTF-8"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles>
    <rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
        let opf = br##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="bookid" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">urn:uuid:secret-device-id</dc:identifier>
    <dc:title>Keep Title</dc:title>
    <dc:language>en</dc:language>
    <dc:creator id="creator">Secret Author</dc:creator>
    <meta refines="#creator" property="file-as">Author, Secret</meta>
    <dc:publisher>Secret Publisher</dc:publisher>
    <dc:subject>Private Subject</dc:subject>
    <meta property="dcterms:modified">2026-05-31T12:34:56Z</meta>
    <meta name="calibre:timestamp" content="2026-05-31T12:34:56Z"/>
    <meta name="cover" content="cover-image"/>
    <link rel="record" href="https://example.invalid/profile"/>
  </metadata>
  <manifest><item id="cover-image" href="cover.jpg" media-type="image/jpeg"/></manifest>
  <spine/>
</package>"##;
        let input = stored_zip(&[
            ("mimetype", mimetype),
            ("META-INF/container.xml", container),
            ("OPS/content.opf", opf),
            ("META-INF/calibre_bookmarks.txt", b"secret reading position"),
            ("OPS/chapter.xhtml", b"<html><body>Book text</body></html>"),
        ]);

        assert_eq!(detect_file_type(&input), Some("epub"));
        let info = extract_metadata(&input);
        assert_eq!(info.file_type, "epub");
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.value.contains("Secret Author")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "identifier"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("calibre")));

        let cleaned = remove_metadata(&input).unwrap();
        let archive = parse_archive(&cleaned).unwrap();
        let opf = archive
            .read_xml("OPS/content.opf")
            .unwrap()
            .expect("OPF package");
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        assert!(archive.entry("META-INF/calibre_bookmarks.txt").is_none());
        for secret in [
            "secret-device-id",
            "Secret Author",
            "Author, Secret",
            "Secret Publisher",
            "Private Subject",
            "calibre:timestamp",
            "https://example.invalid/profile",
            "secret reading position",
        ] {
            assert!(
                !cleaned_text.contains(secret),
                "cleaned EPUB still contains {secret}"
            );
        }
        assert!(opf.contains("Keep Title"));
        assert!(opf.contains("cover-image"));
        assert!(opf.contains(EPUB_IDENTIFIER_REPLACEMENT));
        assert!(opf.contains(EPUB_MODIFIED_REPLACEMENT));
        validate(&cleaned).unwrap();
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn test_epub_cover_image_metadata_is_flagged_after_package_cleaning() {
        let mimetype = b"application/epub+zip";
        let container = br#"<?xml version="1.0" encoding="UTF-8"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles>
    <rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
        let opf = br##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="bookid" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">urn:uuid:secret-device-id</dc:identifier>
    <dc:title>Keep Title</dc:title>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2026-05-31T12:34:56Z</meta>
  </metadata>
  <manifest><item id="cover-image" href="images/cover.jpg" media-type="image/jpeg"/></manifest>
  <spine/>
</package>"##;
        let cover = sample_jpeg_with_xmp();
        let input = stored_zip(&[
            ("mimetype", mimetype),
            ("META-INF/container.xml", container),
            ("OPS/content.opf", opf),
            ("OPS/images/cover.jpg", cover.as_slice()),
        ]);

        let cleaned = remove_metadata(&input).unwrap();
        let verification = extract_metadata(&cleaned);
        assert!(verification
            .metadata_found
            .iter()
            .any(|entry| entry.name.contains("OPS/images/cover.jpg: XMP Data")));
        assert!(!String::from_utf8_lossy(&cleaned).contains("secret-device-id"));
    }

    #[test]
    fn test_rejects_unsafe_zip_paths() {
        let input = stored_zip(&[("../word/document.xml", b"bad")]);
        assert!(parse_archive(&input).is_err());
    }

    #[test]
    fn test_read_entry_rejects_crc_mismatch() {
        let mut input = stored_zip(&[("word/document.xml", b"Keep me")]);
        let secret_offset = input
            .windows(b"Keep me".len())
            .position(|window| window == b"Keep me")
            .expect("stored test payload should be present");
        input[secret_offset] = b'L';

        let archive = parse_archive(&input).unwrap();
        assert_eq!(
            archive.read_entry("word/document.xml", MAX_XML_BYTES),
            Err("ZIP CRC mismatch in word/document.xml".to_string())
        );
    }

    #[test]
    fn test_validate_rejects_crc_mismatch_in_uninspected_entry() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>"#;
        let mut input = stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("word/document.xml", document),
            ("word/media/image1.bin", b"embedded image bytes"),
        ]);
        let data_offset = input
            .windows(b"embedded image bytes".len())
            .position(|window| window == b"embedded image bytes")
            .expect("stored test payload should be present");
        input[data_offset] = b'E';

        assert_eq!(detect_file_type(&input), Some("docx"));
        assert_eq!(
            validate(&input),
            Err("ZIP CRC mismatch in word/media/image1.bin".to_string())
        );
        assert_eq!(
            remove_metadata(&input),
            Err("ZIP CRC mismatch in word/media/image1.bin".to_string())
        );
    }

    #[test]
    fn test_validate_rejects_deflated_entry_with_hidden_tail() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>"#;
        let input = deflated_zip(&[
            ("[Content_Types].xml", content_types, b""),
            ("word/document.xml", document, b"HIDDEN_TAIL"),
        ]);

        assert_eq!(detect_file_type(&input), Some("docx"));
        assert_eq!(
            validate(&input),
            Err("ZIP compressed data mismatch in word/document.xml".to_string())
        );
        assert_eq!(
            remove_metadata(&input),
            Err("ZIP compressed data mismatch in word/document.xml".to_string())
        );
    }

    #[test]
    fn test_find_next_xml_tag_skips_many_malformed_candidates_iteratively() {
        let mut xml = "<>".repeat(200_000);
        xml.push_str("<Types><Override/></Types>");

        let tag = find_next_xml_tag(&xml, 0).expect("valid tag after malformed candidates");
        assert_eq!(tag.local, "Types");
        assert_eq!(&xml[tag.start..tag.end], "<Types>");
    }
}
