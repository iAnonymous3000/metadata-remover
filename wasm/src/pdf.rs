use crate::{MetadataEntry, MetadataInfo};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeSet;
use std::io::Cursor;

const METADATA_KEYS: [&str; 10] = [
    "Title",
    "Author",
    "Subject",
    "Keywords",
    "Creator",
    "Producer",
    "CreationDate",
    "ModDate",
    "Trapped",
    "Marked",
];

const SENSITIVE_DICT_KEYS: &[&[u8]] = &[
    b"Metadata",
    b"PieceInfo",
    b"MarkInfo",
    b"LastModified",
    b"AF",
    b"EmbeddedFiles",
    b"SpiderInfo",
];

fn object_to_string(obj: &Object) -> String {
    match obj {
        Object::String(bytes, _) => {
            // Try UTF-16BE first (starts with BOM)
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let chars: Vec<u16> = bytes[2..]
                    .chunks(2)
                    .filter_map(|chunk| {
                        if chunk.len() == 2 {
                            Some(((chunk[0] as u16) << 8) | (chunk[1] as u16))
                        } else {
                            None
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&chars)
            } else {
                // PDFDocEncoding or ASCII
                String::from_utf8_lossy(bytes).to_string()
            }
        }
        Object::Name(name) => String::from_utf8_lossy(name).to_string(),
        Object::Integer(i) => i.to_string(),
        Object::Real(f) => f.to_string(),
        Object::Boolean(b) => b.to_string(),
        Object::Array(arr) => format!("[{} items]", arr.len()),
        Object::Dictionary(_) => "[Dictionary]".to_string(),
        Object::Stream(_) => "[Stream]".to_string(),
        Object::Null => "null".to_string(),
        Object::Reference(r) => format!("Ref({} {})", r.0, r.1),
    }
}

fn object_subtype_is(dict: &Dictionary, subtype: &[u8]) -> bool {
    dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(subtype)
}

fn metadata_stream_len(doc: &Document, metadata_obj: &Object) -> Option<usize> {
    let metadata_ref = metadata_obj.as_reference().ok()?;
    let metadata_obj = doc.get_object(metadata_ref).ok()?;
    let stream = metadata_obj.as_stream().ok()?;
    Some(stream.content.len())
}

fn collect_object_metadata(
    id: ObjectId,
    obj: &Object,
    entries: &mut Vec<MetadataEntry>,
    total_bytes: &mut usize,
) {
    match obj {
        Object::Dictionary(dict) => {
            for key in SENSITIVE_DICT_KEYS {
                if dict.has(key) {
                    entries.push(MetadataEntry {
                        category: "PDF".to_string(),
                        name: format!(
                            "Object {} {} contains /{}",
                            id.0,
                            id.1,
                            String::from_utf8_lossy(key)
                        ),
                        value: "[dictionary entry]".to_string(),
                    });
                }
            }
        }
        Object::Stream(stream) => {
            if stream.dict.type_is(b"Metadata") || object_subtype_is(&stream.dict, b"XML") {
                entries.push(MetadataEntry {
                    category: "XMP".to_string(),
                    name: format!("Metadata stream {} {}", id.0, id.1),
                    value: format!("{} bytes", stream.content.len()),
                });
                *total_bytes += stream.content.len();
            }
            if stream.dict.type_is(b"EmbeddedFile") {
                entries.push(MetadataEntry {
                    category: "Attachment".to_string(),
                    name: format!("Embedded file stream {} {}", id.0, id.1),
                    value: format!("{} bytes", stream.content.len()),
                });
                *total_bytes += stream.content.len();
            }
        }
        _ => {}
    }
}

fn scrub_object(obj: &mut Object) {
    match obj {
        Object::Array(items) => {
            for item in items {
                scrub_object(item);
            }
        }
        Object::Dictionary(dict) => scrub_dictionary(dict),
        Object::Stream(stream) => scrub_dictionary(&mut stream.dict),
        _ => {}
    }
}

fn scrub_dictionary(dict: &mut Dictionary) {
    for key in SENSITIVE_DICT_KEYS {
        dict.remove(key);
    }

    for (_, value) in dict.iter_mut() {
        scrub_object(value);
    }
}

fn should_delete_object(obj: &Object) -> bool {
    match obj {
        Object::Dictionary(dict) => {
            dict.type_is(b"Metadata")
                || dict.type_is(b"Filespec")
                || (dict.type_is(b"Annot") && object_subtype_is(dict, b"FileAttachment"))
        }
        Object::Stream(stream) => {
            stream.dict.type_is(b"Metadata") || stream.dict.type_is(b"EmbeddedFile")
        }
        _ => false,
    }
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0;

    let doc = match Document::load_from(Cursor::new(data)) {
        Ok(doc) => doc,
        Err(_) => {
            return MetadataInfo {
                file_type: "pdf".to_string(),
                metadata_found: entries,
                total_metadata_bytes: 0,
            };
        }
    };

    // Extract from Info dictionary
    if let Ok(info_dict) = doc.trailer.get(b"Info") {
        if let Ok(info_ref) = info_dict.as_reference() {
            if let Ok(info_obj) = doc.get_object(info_ref) {
                if let Ok(dict) = info_obj.as_dict() {
                    for key in &METADATA_KEYS {
                        if let Ok(value) = dict.get(key.as_bytes()) {
                            let value_str = object_to_string(value);
                            if !value_str.is_empty() {
                                entries.push(MetadataEntry {
                                    category: "Info".to_string(),
                                    name: key.to_string(),
                                    value: value_str.clone(),
                                });
                                total_bytes += value_str.len();
                            }
                        }
                    }
                }
            }
        }
    }

    // Check for XMP metadata stream
    if let Ok(catalog) = doc.catalog() {
        if let Ok(metadata_ref) = catalog.get(b"Metadata") {
            if let Some(len) = metadata_stream_len(&doc, metadata_ref) {
                entries.push(MetadataEntry {
                    category: "XMP".to_string(),
                    name: "Catalog XMP Metadata".to_string(),
                    value: format!("{} bytes", len),
                });
                total_bytes += len;
            }
        }
    }

    for (id, obj) in &doc.objects {
        collect_object_metadata(*id, obj, &mut entries, &mut total_bytes);
    }

    // Check for document ID
    if let Ok(id_array) = doc.trailer.get(b"ID") {
        if let Ok(arr) = id_array.as_array() {
            if !arr.is_empty() {
                entries.push(MetadataEntry {
                    category: "ID".to_string(),
                    name: "Document ID".to_string(),
                    value: format!("{} identifiers", arr.len()),
                });
                total_bytes += 32; // Typical ID size
            }
        }
    }

    MetadataInfo {
        file_type: "pdf".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = Document::load_from(Cursor::new(data))
        .map_err(|e| format!("Failed to parse PDF: {}", e))?;

    let mut ids_to_delete = BTreeSet::new();

    if let Ok(info_ref) = doc.trailer.get(b"Info") {
        if let Ok(obj_ref) = info_ref.as_reference() {
            ids_to_delete.insert(obj_ref);
        }
    }
    doc.trailer.remove(b"Info");

    doc.trailer.remove(b"ID");
    doc.trailer.remove(b"Prev");
    doc.trailer.remove(b"XRefStm");
    scrub_dictionary(&mut doc.trailer);

    for (id, obj) in &mut doc.objects {
        scrub_object(obj);
        if should_delete_object(obj) {
            ids_to_delete.insert(*id);
        }
    }

    for id in ids_to_delete {
        doc.delete_object(id);
    }

    doc.prune_objects();
    doc.renumber_objects();

    let mut output = Vec::new();
    doc.save_to(&mut output)
        .map_err(|e| format!("Failed to save PDF: {}", e))?;

    Ok(output)
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    Document::load_from(Cursor::new(data))
        .map(|_| ())
        .map_err(|e| format!("Failed to parse PDF: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream};

    #[test]
    fn test_object_to_string() {
        assert_eq!(object_to_string(&Object::Integer(42)), "42");
        assert_eq!(object_to_string(&Object::Boolean(true)), "true");
    }

    fn doc_with_metadata() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        let catalog_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let catalog_metadata_id = doc.new_object_id();
        let page_metadata_id = doc.new_object_id();
        let info_id = doc.new_object_id();
        let embedded_id = doc.new_object_id();
        let filespec_id = doc.new_object_id();

        doc.objects.insert(
            catalog_metadata_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "Metadata",
                    "Subtype" => "XML",
                },
                b"SECRET_CATALOG_XMP".to_vec(),
            )),
        );
        doc.objects.insert(
            page_metadata_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "Metadata",
                    "Subtype" => "XML",
                },
                b"SECRET_PAGE_XMP".to_vec(),
            )),
        );
        doc.objects.insert(
            embedded_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "EmbeddedFile",
                },
                b"SECRET_ATTACHMENT".to_vec(),
            )),
        );
        doc.objects.insert(
            filespec_id,
            dictionary! {
                "Type" => "Filespec",
                "F" => Object::string_literal("private.txt"),
                "EF" => dictionary! {
                    "F" => embedded_id,
                },
            }
            .into(),
        );
        doc.objects.insert(
            info_id,
            dictionary! {
                "Author" => Object::string_literal("Alice"),
                "Creator" => Object::string_literal("Secret App"),
            }
            .into(),
        );
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Metadata" => page_metadata_id,
                "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }
            .into(),
        );
        doc.objects.insert(
            catalog_id,
            dictionary! {
                "Type" => "Catalog",
                "Pages" => pages_id,
                "Metadata" => catalog_metadata_id,
                "Names" => dictionary! {
                    "EmbeddedFiles" => dictionary! {
                        "Names" => vec![Object::string_literal("private.txt"), filespec_id.into()],
                    },
                },
            }
            .into(),
        );
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set("Info", info_id);
        doc.trailer.set(
            "ID",
            vec![
                Object::string_literal("SECRET_ID_ONE"),
                Object::string_literal("SECRET_ID_TWO"),
            ],
        );

        let mut input = Vec::new();
        doc.save_to(&mut input).unwrap();
        input
    }

    #[test]
    fn test_extract_metadata_finds_nested_pdf_metadata() {
        let input = doc_with_metadata();
        let info = extract_metadata(&input);

        assert!(info.metadata_found.iter().any(|entry| entry
            .value
            .contains("SECRET_CATALOG_XMP".len().to_string().as_str())
            || entry.name.contains("Metadata stream")));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Info" && entry.name == "Author"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Attachment"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "ID"));
    }

    #[test]
    fn test_remove_metadata_drops_xmp_info_ids_and_attachment_bytes() {
        let input = doc_with_metadata();
        let cleaned = remove_metadata(&input).unwrap();
        let cleaned_text = String::from_utf8_lossy(&cleaned);

        for secret in [
            "SECRET_CATALOG_XMP",
            "SECRET_PAGE_XMP",
            "SECRET_ATTACHMENT",
            "SECRET_ID_ONE",
            "Secret App",
            "Alice",
        ] {
            assert!(
                !cleaned_text.contains(secret),
                "cleaned PDF still contains {secret}"
            );
        }

        let cleaned_info = extract_metadata(&cleaned);
        assert!(cleaned_info.metadata_found.is_empty());
    }
}
