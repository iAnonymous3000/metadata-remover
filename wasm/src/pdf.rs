use crate::{MetadataEntry, MetadataInfo};
use lopdf::{Document, Object, Dictionary};
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
            if let Ok(metadata_ref) = metadata_ref.as_reference() {
                if let Ok(metadata_obj) = doc.get_object(metadata_ref) {
                    if let Ok(stream) = metadata_obj.as_stream() {
                        entries.push(MetadataEntry {
                            category: "XMP".to_string(),
                            name: "XMP Metadata".to_string(),
                            value: format!("{} bytes", stream.content.len()),
                        });
                        total_bytes += stream.content.len();
                    }
                }
            }
        }
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

    // Remove Info dictionary
    if let Ok(info_ref) = doc.trailer.get(b"Info") {
        if let Ok(obj_ref) = info_ref.as_reference() {
            // Clear the info dictionary content but keep minimal valid structure
            if let Ok(info_obj) = doc.get_object_mut(obj_ref) {
                *info_obj = Object::Dictionary(Dictionary::new());
            }
        }
    }

    // Remove XMP metadata from catalog
    if let Ok(catalog_ref) = doc.trailer.get(b"Root").and_then(|r| r.as_reference()) {
        if let Ok(catalog_obj) = doc.get_object_mut(catalog_ref) {
            if let Ok(catalog_dict) = catalog_obj.as_dict_mut() {
                catalog_dict.remove(b"Metadata");

                // Remove PieceInfo (application-specific data)
                catalog_dict.remove(b"PieceInfo");

                // Remove MarkInfo (tagged PDF info)
                catalog_dict.remove(b"MarkInfo");
            }
        }
    }

    // Remove document IDs (can be used for tracking)
    doc.trailer.remove(b"ID");

    // Write the modified document
    let mut output = Vec::new();
    doc.save_to(&mut output)
        .map_err(|e| format!("Failed to save PDF: {}", e))?;

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_to_string() {
        assert_eq!(object_to_string(&Object::Integer(42)), "42");
        assert_eq!(object_to_string(&Object::Boolean(true)), "true");
    }
}
