use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

mod bmff;
mod gif;
mod jpeg;
mod mp3;
mod ooxml;
mod pdf;
mod png;
mod svg;
mod tiff;
mod webp;

// Magic bytes for file type detection
const GIF87A_MAGIC: [u8; 6] = [0x47, 0x49, 0x46, 0x38, 0x37, 0x61];
const GIF89A_MAGIC: [u8; 6] = [0x47, 0x49, 0x46, 0x38, 0x39, 0x61];
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];
const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const PDF_MAGIC: [u8; 4] = [0x25, 0x50, 0x44, 0x46]; // %PDF
const RIFF_MAGIC: [u8; 4] = [0x52, 0x49, 0x46, 0x46];
const TIFF_LE_MAGIC: [u8; 4] = [0x49, 0x49, 0x2A, 0x00];
const TIFF_BE_MAGIC: [u8; 4] = [0x4D, 0x4D, 0x00, 0x2A];
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

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    pub file_type: String,
    pub metadata: MetadataInfo,
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
    } else if data.len() >= TIFF_LE_MAGIC.len()
        && (data[..TIFF_LE_MAGIC.len()] == TIFF_LE_MAGIC
            || data[..TIFF_BE_MAGIC.len()] == TIFF_BE_MAGIC)
    {
        "tiff".into()
    } else if data.len() >= GIF87A_MAGIC.len()
        && (data[..GIF87A_MAGIC.len()] == GIF87A_MAGIC
            || data[..GIF89A_MAGIC.len()] == GIF89A_MAGIC)
    {
        "gif".into()
    } else if let Some(file_type) = bmff::detect_file_type(data) {
        file_type.to_string()
    } else if mp3::looks_like_mp3(data) {
        "mp3".into()
    } else if data.len() >= OLE_MAGIC.len() && data[..OLE_MAGIC.len()] == OLE_MAGIC {
        "office-legacy".into()
    } else if let Some(file_type) = ooxml::detect_file_type(data) {
        file_type.to_string()
    } else if svg::looks_like_svg(data) {
        "svg".into()
    } else {
        "unknown".into()
    }
}

fn extract_detected_metadata(data: &[u8], file_type: &str) -> MetadataInfo {
    match file_type {
        "jpeg" => jpeg::extract_metadata(data),
        "png" => png::extract_metadata(data),
        "pdf" => pdf::extract_metadata(data),
        "webp" => webp::extract_metadata(data),
        "tiff" => tiff::extract_metadata(data),
        "gif" => gif::extract_metadata(data),
        "avif" | "heic" | "heif" | "mp4" | "mov" => bmff::extract_metadata(data, file_type),
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "epub" => ooxml::extract_metadata(data),
        "mp3" => mp3::extract_metadata(data),
        "svg" => svg::extract_metadata(data),
        _ => MetadataInfo {
            file_type: file_type.to_string(),
            metadata_found: vec![],
            total_metadata_bytes: 0,
        },
    }
}

pub fn extract_metadata_info(data: &[u8]) -> MetadataInfo {
    let file_type = detect_file_type(data);
    extract_detected_metadata(data, &file_type)
}

#[wasm_bindgen]
pub fn extract_metadata(data: &[u8]) -> JsValue {
    let result = extract_metadata_info(data);
    serde_wasm_bindgen::to_value(&result).unwrap_or(JsValue::NULL)
}

fn validate_detected_file(data: &[u8], file_type: &str) -> Result<(), String> {
    match file_type {
        "jpeg" => jpeg::validate(data),
        "png" => png::validate(data),
        "pdf" => pdf::validate(data),
        "webp" => webp::validate(data),
        "tiff" => tiff::validate(data),
        "gif" => gif::validate(data),
        "avif" | "heic" | "heif" | "mp4" | "mov" => bmff::validate(data, file_type),
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "epub" => ooxml::validate(data),
        "mp3" => mp3::validate(data),
        "svg" => svg::validate(data),
        "office-legacy" => Err(legacy_office_error()),
        _ => Err("Unsupported file type".to_string()),
    }
}

pub fn validate_file_bytes(data: &[u8]) -> Result<(), String> {
    let file_type = detect_file_type(data);
    validate_detected_file(data, &file_type)
}

#[wasm_bindgen]
pub fn validate_file(data: &[u8]) -> Result<(), JsValue> {
    validate_file_bytes(data).map_err(|error| JsValue::from_str(&error))
}

fn remove_detected_metadata(data: &[u8], file_type: &str) -> Result<Vec<u8>, String> {
    match file_type {
        "jpeg" => jpeg::remove_metadata(data),
        "png" => png::remove_metadata(data),
        "pdf" => pdf::remove_metadata(data),
        "webp" => webp::remove_metadata(data),
        "tiff" => tiff::remove_metadata(data),
        "gif" => gif::remove_metadata(data),
        "avif" | "heic" | "heif" | "mp4" | "mov" => bmff::remove_metadata(data, file_type),
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "epub" => ooxml::remove_metadata(data),
        "mp3" => mp3::remove_metadata(data),
        "svg" => svg::remove_metadata(data),
        "office-legacy" => Err(legacy_office_error()),
        _ => Err("Unsupported file type".to_string()),
    }
}

pub fn remove_metadata_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    let file_type = detect_file_type(data);
    validate_detected_file(data, &file_type)?;
    let cleaned = remove_detected_metadata(data, &file_type)?;
    validate_cleaned_file(&cleaned, &file_type)?;
    Ok(cleaned)
}

#[wasm_bindgen]
pub fn analyze_file(data: &[u8]) -> Result<JsValue, JsValue> {
    let file_type = detect_file_type(data);
    if let Err(error) = validate_detected_file(data, &file_type) {
        return Err(wasm_file_error(&file_type, &error));
    }

    let metadata = extract_detected_metadata(data, &file_type);
    serialize_js(&AnalyzeResult {
        file_type,
        metadata,
    })
}

#[wasm_bindgen]
pub fn process_file(data: &[u8]) -> Result<JsValue, JsValue> {
    let file_type = detect_file_type(data);
    if let Err(error) = validate_detected_file(data, &file_type) {
        return Err(wasm_file_error(&file_type, &error));
    }

    let cleaned = remove_detected_metadata(data, &file_type)
        .map_err(|error| wasm_file_error(&file_type, &error))?;
    validate_cleaned_file(&cleaned, &file_type)
        .map_err(|error| wasm_file_error(&file_type, &error))?;
    let verification = extract_detected_metadata(&cleaned, &file_type);

    let result = js_sys::Object::new();
    let cleaned_array = uint8_array_from_bytes(&cleaned);
    let verification_value = serialize_js(&verification)?;
    js_sys::Reflect::set(
        &result,
        &JsValue::from_str("cleaned"),
        cleaned_array.as_ref(),
    )?;
    js_sys::Reflect::set(
        &result,
        &JsValue::from_str("verification"),
        &verification_value,
    )?;
    Ok(result.into())
}

#[wasm_bindgen]
pub fn remove_metadata(data: &[u8]) -> Result<js_sys::Uint8Array, JsValue> {
    remove_metadata_bytes(data)
        .map(|bytes| uint8_array_from_bytes(&bytes))
        .map_err(|error| JsValue::from_str(&error))
}

fn uint8_array_from_bytes(bytes: &[u8]) -> js_sys::Uint8Array {
    let arr = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    arr.copy_from(bytes);
    arr
}

fn validate_cleaned_file(cleaned: &[u8], original_file_type: &str) -> Result<(), String> {
    let cleaned_file_type = detect_file_type(cleaned);
    if cleaned_file_type != original_file_type {
        return Err(format!(
            "Cleaned file type changed from {original_file_type} to {cleaned_file_type}"
        ));
    }

    validate_detected_file(cleaned, original_file_type)
        .map_err(|error| format!("Cleaned file failed validation: {error}"))
}

fn legacy_office_error() -> String {
    "Legacy binary Office files (.doc, .xls, .ppt) are not supported. Save the file as .docx, .xlsx, or .pptx first.".to_string()
}

fn serialize_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|error| JsValue::from_str(&error.to_string()))
}

fn wasm_file_error(file_type: &str, error: &str) -> JsValue {
    let result = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &result,
        &JsValue::from_str("fileType"),
        &JsValue::from_str(file_type),
    );
    let _ = js_sys::Reflect::set(
        &result,
        &JsValue::from_str("error"),
        &JsValue::from_str(error),
    );
    result.into()
}

/// Returns WASM module version for cache busting
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_cleaner_validates_cleaned_output() {
        let data = [
            0xff, 0xd8, 0xff, 0xfe, 0x00, 0x08, b's', b'e', b'c', b'r', b'e', b't', 0xff, 0xd9,
        ];

        let before = extract_metadata_info(&data);
        assert_eq!(before.file_type, "jpeg");
        assert!(!before.metadata_found.is_empty());

        let cleaned = remove_metadata_bytes(&data).unwrap();
        assert_eq!(detect_file_type(&cleaned), "jpeg");
        validate_file_bytes(&cleaned).unwrap();
        assert!(extract_metadata_info(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn top_level_cleaner_rejects_invalid_input_before_cleaning() {
        let data = [
            0xff, 0xd8, 0xff, 0xfe, 0x00, 0x08, b's', b'e', b'c', b'r', b'e', b't',
        ];

        assert_eq!(detect_file_type(&data), "jpeg");
        assert_eq!(
            remove_metadata_bytes(&data),
            Err("JPEG missing EOI marker".to_string())
        );
    }
}
