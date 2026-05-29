use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

mod gif;
mod jpeg;
mod pdf;
mod png;
mod webp;

// Magic bytes for file type detection
const GIF87A_MAGIC: [u8; 6] = [0x47, 0x49, 0x46, 0x38, 0x37, 0x61];
const GIF89A_MAGIC: [u8; 6] = [0x47, 0x49, 0x46, 0x38, 0x39, 0x61];
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const PDF_MAGIC: [u8; 4] = [0x25, 0x50, 0x44, 0x46]; // %PDF
const RIFF_MAGIC: [u8; 4] = [0x52, 0x49, 0x46, 0x46];
const WEBP_MAGIC: [u8; 4] = [0x57, 0x45, 0x42, 0x50];

#[derive(Serialize, Deserialize, Debug)]
pub struct MetadataInfo {
    pub file_type: String,
    pub metadata_found: Vec<MetadataEntry>,
    pub total_metadata_bytes: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MetadataEntry {
    pub category: String,
    pub name: String,
    pub value: String,
}

#[wasm_bindgen]
pub fn detect_file_type(data: &[u8]) -> String {
    if data.len() >= JPEG_MAGIC.len() && data[..JPEG_MAGIC.len()] == JPEG_MAGIC {
        "jpeg".into()
    } else if data.len() >= PNG_MAGIC.len() && data[..PNG_MAGIC.len()] == PNG_MAGIC {
        "png".into()
    } else if data.len() >= PDF_MAGIC.len() && data[..PDF_MAGIC.len()] == PDF_MAGIC {
        "pdf".into()
    } else if data.len() >= 12
        && data[..RIFF_MAGIC.len()] == RIFF_MAGIC
        && data[8..12] == WEBP_MAGIC
    {
        "webp".into()
    } else if data.len() >= GIF87A_MAGIC.len()
        && (data[..GIF87A_MAGIC.len()] == GIF87A_MAGIC
            || data[..GIF89A_MAGIC.len()] == GIF89A_MAGIC)
    {
        "gif".into()
    } else {
        "unknown".into()
    }
}

#[wasm_bindgen]
pub fn extract_metadata(data: &[u8]) -> JsValue {
    let file_type = detect_file_type(data);

    let result = match file_type.as_str() {
        "jpeg" => jpeg::extract_metadata(data),
        "png" => png::extract_metadata(data),
        "pdf" => pdf::extract_metadata(data),
        "webp" => webp::extract_metadata(data),
        "gif" => gif::extract_metadata(data),
        _ => MetadataInfo {
            file_type: "unknown".to_string(),
            metadata_found: vec![],
            total_metadata_bytes: 0,
        },
    };

    serde_wasm_bindgen::to_value(&result).unwrap_or(JsValue::NULL)
}

#[wasm_bindgen]
pub fn validate_file(data: &[u8]) -> Result<(), JsValue> {
    let file_type = detect_file_type(data);

    let result = match file_type.as_str() {
        "jpeg" => jpeg::validate(data),
        "png" => png::validate(data),
        "pdf" => pdf::validate(data),
        "webp" => webp::validate(data),
        "gif" => gif::validate(data),
        _ => Err("Unsupported file type".to_string()),
    };

    result.map_err(|error| JsValue::from_str(&error))
}

#[wasm_bindgen]
pub fn remove_metadata(data: &[u8]) -> Result<js_sys::Uint8Array, JsValue> {
    let file_type = detect_file_type(data);

    let cleaned = match file_type.as_str() {
        "jpeg" => jpeg::remove_metadata(data),
        "png" => png::remove_metadata(data),
        "pdf" => pdf::remove_metadata(data),
        "webp" => webp::remove_metadata(data),
        "gif" => gif::remove_metadata(data),
        _ => Err("Unsupported file type".to_string()),
    };

    match cleaned {
        Ok(bytes) => {
            let arr = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
            arr.copy_from(&bytes);
            Ok(arr)
        }
        Err(e) => Err(JsValue::from_str(&e)),
    }
}

/// Returns WASM module version for cache busting
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}
