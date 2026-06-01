use crate::{MetadataEntry, MetadataInfo};

const MAX_SVG_BYTES: usize = 32 * 1024 * 1024;
const EDITOR_PREFIXES: [&str; 8] = [
    "inkscape:",
    "sodipodi:",
    "adobe:",
    "sketch:",
    "serif:",
    "figma:",
    "dc:",
    "cc:",
];
const EDITOR_ATTRS: [&str; 4] = [
    "xmlns:inkscape",
    "xmlns:sodipodi",
    "xmlns:adobe",
    "xmlns:sketch",
];
const METADATA_ELEMENTS: [&str; 3] = ["metadata", "title", "desc"];
const ACTIVE_ELEMENTS: [&str; 2] = ["script", "foreignObject"];
const RAW_TEXT_ELEMENTS: [&str; 3] = ["script", "style", "foreignObject"];
const URL_ATTRS: [&str; 6] = ["href", "src", "data", "poster", "action", "formaction"];
const SMIL_REFERENCE_ATTRS: [&str; 4] = ["to", "from", "values", "by"];
const LIMITED_VERIFICATION_CATEGORY: &str = "Limited verification";

pub fn looks_like_svg(data: &[u8]) -> bool {
    let Ok(text) = svg_text(data) else {
        return false;
    };
    find_svg_start(text).is_some()
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    if data.len() > MAX_SVG_BYTES {
        return Err("SVG file is too large to inspect safely".to_string());
    }
    let text = svg_text(data)?;
    find_svg_start(text)
        .map(|_| ())
        .ok_or_else(|| "Not a valid SVG file".to_string())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;

    let Ok(text) = svg_text(data) else {
        return MetadataInfo {
            file_type: "svg".to_string(),
            metadata_found: entries,
            total_metadata_bytes: total_bytes,
        };
    };

    let metadata_ranges = element_ranges(text, "metadata");
    if !metadata_ranges.is_empty() {
        total_bytes += metadata_ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum::<usize>();
        entries.push(MetadataEntry {
            category: "SVG metadata".to_string(),
            name: "metadata elements".to_string(),
            value: format!(
                "{} present; removed during SVG cleaning",
                metadata_ranges.len()
            ),
        });
    }

    let descriptive_ranges = element_ranges_any(text, &["title", "desc"]);
    if !descriptive_ranges.is_empty() {
        total_bytes += descriptive_ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum::<usize>();
        entries.push(MetadataEntry {
            category: "SVG metadata".to_string(),
            name: "title/description elements".to_string(),
            value: format!(
                "{} present; removed during SVG cleaning",
                descriptive_ranges.len()
            ),
        });
    }

    let comment_count = count_xml_comments(text);
    if comment_count > 0 {
        total_bytes += comment_byte_total(text);
        entries.push(MetadataEntry {
            category: "SVG metadata".to_string(),
            name: "XML comments".to_string(),
            value: format!("{comment_count} present; removed during SVG cleaning"),
        });
    }

    let processing_count = count_processing_instructions(text);
    if processing_count > 0 {
        entries.push(MetadataEntry {
            category: "SVG metadata".to_string(),
            name: "Processing instructions".to_string(),
            value: format!("{processing_count} present; removed during SVG cleaning"),
        });
    }

    let active_ranges = element_ranges_any(text, &ACTIVE_ELEMENTS);
    if !active_ranges.is_empty() {
        total_bytes += active_ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum::<usize>();
        entries.push(MetadataEntry {
            category: "SVG active content".to_string(),
            name: "script/foreignObject elements".to_string(),
            value: format!(
                "{} present; removed during SVG cleaning",
                active_ranges.len()
            ),
        });
    }

    let external_style_ranges = external_style_element_ranges(text);
    if !external_style_ranges.is_empty() {
        total_bytes += external_style_ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum::<usize>();
        entries.push(MetadataEntry {
            category: "SVG external references".to_string(),
            name: "external style references".to_string(),
            value: format!(
                "{} present; removed during SVG cleaning",
                external_style_ranges.len()
            ),
        });
    }

    let removable_attr_count = count_removable_attrs(text);
    if removable_attr_count > 0 {
        entries.push(MetadataEntry {
            category: "SVG metadata".to_string(),
            name: "removable attributes".to_string(),
            value: format!("{removable_attr_count} present; removed during SVG cleaning"),
        });
    }

    let embedded_data_uri_count = count_embedded_data_uri_references(text);
    if embedded_data_uri_count > 0 {
        entries.push(MetadataEntry {
            category: LIMITED_VERIFICATION_CATEGORY.to_string(),
            name: "Embedded data URI content".to_string(),
            value: format!(
                "{embedded_data_uri_count} data URI reference(s) preserved; embedded raster metadata is not decoded"
            ),
        });
    }

    MetadataInfo {
        file_type: "svg".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    validate(data)?;
    let text = svg_text(data)?;
    let without_comments = remove_xml_comments(text);
    let without_processing = remove_processing_instructions(&without_comments);
    let without_metadata = remove_elements_any(&without_processing, &METADATA_ELEMENTS);
    let without_active = remove_elements_any(&without_metadata, &ACTIVE_ELEMENTS);
    let without_active_closings = remove_closing_elements_any(&without_active, &ACTIVE_ELEMENTS);
    let without_external_styles = remove_external_style_elements(&without_active_closings);
    let without_attrs = strip_removable_attrs(&without_external_styles);
    Ok(without_attrs.into_bytes())
}

fn svg_text(data: &[u8]) -> Result<&str, String> {
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    std::str::from_utf8(data).map_err(|_| "SVG must be UTF-8 XML".to_string())
}

fn find_svg_start(text: &str) -> Option<usize> {
    let mut search = 0;
    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if text[start..].starts_with("<!--") {
            search = text[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)?;
            continue;
        }
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!')) {
            search = find_tag_end(text, start)?;
            continue;
        }
        let Some((name, _attrs_start, _attrs_end, _end, closing, _self_closing)) =
            parse_tag(text, start)
        else {
            search = start + 1;
            continue;
        };
        if !closing && local_name(name).eq_ignore_ascii_case("svg") {
            return Some(start);
        }
        search = start + 1;
    }
    None
}

fn element_ranges(text: &str, target_local: &str) -> Vec<std::ops::Range<usize>> {
    element_ranges_any(text, &[target_local])
}

fn element_ranges_any(text: &str, target_locals: &[&str]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut search = 0;

    while let Some(start) = find_next_element(text, search, target_locals) {
        let Some((_, _, _, open_end, _, self_closing)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };

        let end = if self_closing {
            open_end
        } else {
            let Some((name, _, _, _, _, _)) = parse_tag(text, start) else {
                search = start + 1;
                continue;
            };
            let local = local_name(name);
            if matches_local_name(local, &RAW_TEXT_ELEMENTS) {
                find_raw_text_end(text, open_end, local).unwrap_or(open_end)
            } else {
                find_matching_end(text, open_end, local).unwrap_or(open_end)
            }
        };
        ranges.push(start..end);
        search = end;
    }

    ranges
}

fn remove_elements_any(text: &str, target_locals: &[&str]) -> String {
    let ranges = element_ranges_any(text, target_locals);
    remove_ranges(text, &ranges)
}

fn remove_closing_elements_any(text: &str, target_locals: &[&str]) -> String {
    let mut ranges = Vec::new();
    let mut search = 0usize;

    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if text[start..].starts_with("<!--") {
            search = text[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)
                .unwrap_or(start + 4);
            continue;
        }
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!')) {
            search = find_tag_end(text, start).unwrap_or(start + 1);
            continue;
        }
        let Some((name, _, _, end, closing, _)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        if closing && matches_local_name(name, target_locals) {
            ranges.push(start..end);
        }
        search = end;
    }

    remove_ranges(text, &ranges)
}

fn remove_ranges(text: &str, ranges: &[std::ops::Range<usize>]) -> String {
    if ranges.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut copy_pos = 0;
    for range in ranges.iter().cloned() {
        out.push_str(&text[copy_pos..range.start]);
        copy_pos = range.end;
    }
    out.push_str(&text[copy_pos..]);
    out
}

fn find_next_element(text: &str, from: usize, target_locals: &[&str]) -> Option<usize> {
    let mut search = from;
    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if text[start..].starts_with("<!--") {
            search = text[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)?;
            continue;
        }
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!')) {
            search = find_tag_end(text, start)?;
            continue;
        }
        let Some((name, _, _, end, closing, _)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        if !closing && matches_local_name(name, target_locals) {
            return Some(start);
        }
        search = end;
    }
    None
}

fn external_style_element_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    element_ranges(text, "style")
        .into_iter()
        .filter(|range| {
            let Some((_, _, _, open_end, _, _)) = parse_tag(text, range.start) else {
                return false;
            };
            contains_external_reference(&text[open_end..range.end])
        })
        .collect()
}

fn remove_external_style_elements(text: &str) -> String {
    remove_ranges(text, &external_style_element_ranges(text))
}

fn find_matching_end(text: &str, from: usize, target_local: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut search = from;

    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if text[start..].starts_with("<!--") {
            search = text[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)?;
            continue;
        }
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!')) {
            search = find_tag_end(text, start)?;
            continue;
        }
        let Some((name, _, _, end, closing, self_closing)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        if local_name(name).eq_ignore_ascii_case(target_local) {
            if closing {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(end);
                }
            } else if !self_closing {
                depth += 1;
            }
        }
        search = end;
    }

    None
}

fn find_raw_text_end(text: &str, from: usize, target_local: &str) -> Option<usize> {
    let mut search = from;
    while let Some(relative) = text[search..].find("</") {
        let close_start = search + relative;
        let Some((name, _, _, end, closing, _)) = parse_tag(text, close_start) else {
            search = close_start + 2;
            continue;
        };
        if closing && local_name(name).eq_ignore_ascii_case(target_local) {
            return Some(end);
        }
        search = end;
    }

    None
}

fn count_xml_comments(text: &str) -> usize {
    let mut count = 0usize;
    let mut search = 0usize;
    while let Some(start) = text[search..].find("<!--") {
        let start = search + start;
        let Some(end) = text[start + 4..].find("-->") else {
            break;
        };
        count += 1;
        search = start + 4 + end + 3;
    }
    count
}

fn comment_byte_total(text: &str) -> usize {
    let mut total = 0usize;
    let mut search = 0usize;
    while let Some(start) = text[search..].find("<!--") {
        let start = search + start;
        let Some(end) = text[start + 4..].find("-->") else {
            break;
        };
        let end = start + 4 + end + 3;
        total += end - start;
        search = end;
    }
    total
}

fn remove_xml_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copy_pos = 0usize;
    let mut search = 0usize;
    while let Some(start) = text[search..].find("<!--") {
        let start = search + start;
        let Some(end) = text[start + 4..].find("-->") else {
            break;
        };
        let end = start + 4 + end + 3;
        out.push_str(&text[copy_pos..start]);
        copy_pos = end;
        search = end;
    }
    out.push_str(&text[copy_pos..]);
    out
}

fn count_processing_instructions(text: &str) -> usize {
    let mut count = 0usize;
    let mut search = 0usize;
    while let Some(start) = text[search..].find("<?") {
        let start = search + start;
        let Some(end) = text[start + 2..].find("?>") else {
            break;
        };
        let instruction = &text[start + 2..start + 2 + end];
        if !is_xml_declaration(instruction) {
            count += 1;
        }
        search = start + 2 + end + 2;
    }
    count
}

fn remove_processing_instructions(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copy_pos = 0usize;
    let mut search = 0usize;
    while let Some(start) = text[search..].find("<?") {
        let start = search + start;
        let Some(end) = text[start + 2..].find("?>") else {
            break;
        };
        let end = start + 2 + end + 2;
        let instruction = &text[start + 2..end - 2];
        if !is_xml_declaration(instruction) {
            out.push_str(&text[copy_pos..start]);
            copy_pos = end;
        }
        search = end;
    }
    out.push_str(&text[copy_pos..]);
    out
}

fn count_removable_attrs(text: &str) -> usize {
    let mut count = 0usize;
    let mut search = 0usize;
    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!' | b'/')) {
            search = find_tag_end(text, start).unwrap_or(start + 1);
            continue;
        }
        let Some((_, attrs_start, attrs_end, end, _, _)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        count += parse_attrs(text, attrs_start, attrs_end)
            .into_iter()
            .filter(is_removable_attr)
            .count();
        search = end;
    }
    count
}

fn count_embedded_data_uri_references(text: &str) -> usize {
    let mut count = 0usize;
    let mut search = 0usize;
    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!' | b'/')) {
            search = find_tag_end(text, start).unwrap_or(start + 1);
            continue;
        }
        let Some((_, attrs_start, attrs_end, end, _, _)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        count += parse_attrs(text, attrs_start, attrs_end)
            .into_iter()
            .filter(is_embedded_data_uri_attr)
            .count();
        search = end;
    }
    count
}

fn strip_removable_attrs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copy_pos = 0usize;
    let mut search = 0usize;

    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        if matches!(text.as_bytes().get(start + 1), Some(b'?' | b'!' | b'/')) {
            search = find_tag_end(text, start).unwrap_or(start + 1);
            continue;
        }
        let Some((_, attrs_start, attrs_end, end, _, self_closing)) = parse_tag(text, start) else {
            search = start + 1;
            continue;
        };
        let attrs = parse_attrs(text, attrs_start, attrs_end);
        if attrs.iter().all(|attr| !is_removable_attr(attr)) {
            search = end;
            continue;
        }

        out.push_str(&text[copy_pos..start]);
        out.push_str(&text[start..attrs_start]);
        for attr in attrs {
            if !is_removable_attr(&attr) {
                out.push_str(&text[attr.start..attr.end]);
            }
        }
        if self_closing {
            out.push_str("/>");
        } else {
            out.push('>');
        }
        copy_pos = end;
        search = end;
    }

    out.push_str(&text[copy_pos..]);
    out
}

fn is_removable_attr(attr: &Attr) -> bool {
    is_editor_attr(&attr.name)
        || is_event_handler_attr(&attr.name)
        || is_external_reference_attr(attr)
        || contains_css_url_reference(&attr.value)
}

fn is_embedded_data_uri_attr(attr: &Attr) -> bool {
    let local = local_name(&attr.name);
    let is_url_attr = URL_ATTRS
        .iter()
        .any(|name| local.eq_ignore_ascii_case(name));

    (is_url_attr && is_data_uri_value(&attr.value))
        || (is_css_reference_attr(local) && contains_data_uri_reference(&attr.value))
}

fn is_css_reference_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "style"
            | "fill"
            | "stroke"
            | "filter"
            | "clip-path"
            | "mask"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "cursor"
    )
}

fn is_editor_attr(name: &str) -> bool {
    let name = name.trim();
    EDITOR_ATTRS.contains(&name)
        || EDITOR_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn is_event_handler_attr(name: &str) -> bool {
    let local = local_name(name).to_ascii_lowercase();
    local.len() > 2 && local.starts_with("on")
}

fn is_external_reference_attr(attr: &Attr) -> bool {
    let local = local_name(&attr.name);
    if URL_ATTRS
        .iter()
        .any(|name| local.eq_ignore_ascii_case(name))
    {
        return is_external_reference_value(&attr.value);
    }

    SMIL_REFERENCE_ATTRS
        .iter()
        .any(|name| local.eq_ignore_ascii_case(name))
        && contains_remote_or_dangerous_reference(&attr.value)
}

fn contains_external_reference(value: &str) -> bool {
    contains_css_url_reference(value)
}

fn contains_css_url_reference(value: &str) -> bool {
    let xml_decoded = decode_xml_entities(value);
    let decoded = decode_css_escapes(value);
    let xml_and_css_decoded = decode_css_escapes(&xml_decoded);
    contains_css_url_reference_plain(value)
        || contains_css_url_reference_plain(&xml_decoded)
        || contains_css_url_reference_plain(&decoded)
        || contains_css_url_reference_plain(&xml_and_css_decoded)
}

fn contains_data_uri_reference(value: &str) -> bool {
    let xml_decoded = decode_xml_entities(value);
    let decoded = decode_css_escapes(value);
    let xml_and_css_decoded = decode_css_escapes(&xml_decoded);
    contains_data_uri_reference_plain(value)
        || contains_data_uri_reference_plain(&xml_decoded)
        || contains_data_uri_reference_plain(&decoded)
        || contains_data_uri_reference_plain(&xml_and_css_decoded)
}

fn contains_css_url_reference_plain(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(relative) = lower[search..].find("url(") {
        let start = search + relative + 4;
        let Some(end) = lower[start..].find(')') else {
            return true;
        };
        if is_external_reference_value(&value[start..start + end]) {
            return true;
        }
        search = start + end + 1;
    }
    lower.contains("@import")
}

fn contains_data_uri_reference_plain(value: &str) -> bool {
    if is_data_uri_value(value) {
        return true;
    }

    let lower = value.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(relative) = lower[search..].find("url(") {
        let start = search + relative + 4;
        let Some(end) = lower[start..].find(')') else {
            return lower[start..].contains("data:");
        };
        if is_data_uri_value(&value[start..start + end]) {
            return true;
        }
        search = start + end + 1;
    }

    false
}

fn decode_css_escapes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let mut hex = String::new();
        while hex.len() < 6 {
            let Some(next) = chars.peek().copied() else {
                break;
            };
            if !next.is_ascii_hexdigit() {
                break;
            }
            hex.push(next);
            chars.next();
        }

        if !hex.is_empty() {
            if let Ok(code) = u32::from_str_radix(&hex, 16) {
                if let Some(decoded) = char::from_u32(code) {
                    out.push(decoded);
                }
            }
            if chars.peek().is_some_and(|next| next.is_ascii_whitespace()) {
                chars.next();
            }
        } else if let Some(escaped) = chars.next() {
            out.push(escaped);
        }
    }

    out
}

fn contains_remote_or_dangerous_reference(value: &str) -> bool {
    value
        .split(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | ';'))
        .any(is_remote_or_dangerous_reference_value)
}

fn is_remote_or_dangerous_reference_value(value: &str) -> bool {
    let value = value.trim().trim_matches(|c| matches!(c, '"' | '\''));
    if value.is_empty() || value.starts_with('#') || value.starts_with("data:") {
        return false;
    }

    let lower = value.to_ascii_lowercase();
    lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with("ftp:")
        || lower.starts_with("file:")
        || lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower
            .find(':')
            .is_some_and(|colon| colon < lower.find('/').unwrap_or(usize::MAX))
}

fn is_external_reference_value(value: &str) -> bool {
    let value = value.trim().trim_matches(|c| matches!(c, '"' | '\''));
    if value.is_empty() || value.starts_with('#') || value.starts_with("data:") {
        return false;
    }

    let lower = value.to_ascii_lowercase();
    lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with("ftp:")
        || lower.starts_with("file:")
        || lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower
            .find(':')
            .is_some_and(|colon| colon < lower.find('/').unwrap_or(usize::MAX))
        || !value.contains(':')
}

fn is_data_uri_value(value: &str) -> bool {
    value
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\''))
        .to_ascii_lowercase()
        .starts_with("data:")
}

#[derive(Debug)]
struct Attr {
    name: String,
    value: String,
    start: usize,
    end: usize,
}

fn parse_attrs(text: &str, from: usize, to: usize) -> Vec<Attr> {
    let bytes = text.as_bytes();
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
        if name_start == name_end {
            pos += 1;
            continue;
        }
        while pos < to && matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
        }
        if pos >= to || bytes[pos] != b'=' {
            attrs.push(Attr {
                name: text[name_start..name_end].to_string(),
                value: String::new(),
                start: attr_start,
                end: pos,
            });
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
        let (value_start, value_end);
        if quote == b'\'' || quote == b'"' {
            pos += 1;
            value_start = pos;
            while pos < to && bytes[pos] != quote {
                pos += 1;
            }
            if pos >= to {
                break;
            }
            value_end = pos;
            pos += 1;
        } else {
            value_start = pos;
            while pos < to && !matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
                pos += 1;
            }
            value_end = pos;
        }
        attrs.push(Attr {
            name: text[name_start..name_end].to_string(),
            value: decode_xml_entities(&text[value_start..value_end]),
            start: attr_start,
            end: pos,
        });
    }
    attrs
}

fn parse_tag(text: &str, start: usize) -> Option<(&str, usize, usize, usize, bool, bool)> {
    let end = find_tag_end(text, start)?;
    let bytes = text.as_bytes();
    let mut pos = start + 1;
    let closing = bytes.get(pos) == Some(&b'/');
    if closing {
        pos += 1;
    }
    while matches!(bytes.get(pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        pos += 1;
    }
    let name_start = pos;
    while pos < end && !matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t' | b'/' | b'>') {
        pos += 1;
    }
    if name_start == pos {
        return None;
    }
    let name = &text[name_start..pos];
    let mut attrs_end = end - 1;
    let self_closing = text[..end].trim_end().ends_with("/>");
    if self_closing {
        attrs_end = text[..end].trim_end().len() - 2;
    }
    Some((name, pos, attrs_end, end, closing, self_closing))
}

fn find_tag_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = start + 1;
    let mut quote = None;
    while pos < bytes.len() {
        match (bytes[pos], quote) {
            (b'\'' | b'"', None) => quote = Some(bytes[pos]),
            (value, Some(current)) if value == current => quote = None,
            (b'>', None) => return Some(pos + 1),
            _ => {}
        }
        pos += 1;
    }
    None
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

fn matches_local_name(name: &str, targets: &[&str]) -> bool {
    targets
        .iter()
        .any(|target| local_name(name).eq_ignore_ascii_case(target))
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

fn is_xml_declaration(instruction: &str) -> bool {
    let instruction = instruction.trim_start();
    instruction == "xml" || instruction.starts_with("xml ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_clean_svg_without_metadata_unchanged() {
        let svg =
            br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"/></svg>"#;

        validate(svg).unwrap();
        assert!(extract_metadata(svg).metadata_found.is_empty());
        assert_eq!(remove_metadata(svg).unwrap(), svg);
    }

    #[test]
    fn removes_svg_metadata_comments_and_editor_attrs() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="x" inkscape:version="1"><!-- secret --><metadata><rdf:RDF><dc:creator>Alice</dc:creator></rdf:RDF></metadata><rect width="10" height="10"/></svg>"#;

        assert!(looks_like_svg(svg));
        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "metadata elements"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "XML comments"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();
        assert!(!cleaned_text.contains("Alice"));
        assert!(!cleaned_text.contains("secret"));
        assert!(!cleaned_text.contains("inkscape"));
        assert!(cleaned_text.contains("<rect"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_active_content_and_external_references() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" onload="track()"><title>Alice draft</title><desc>Private description</desc><style>@import url("https://tracker.example/style.css"); .a { fill: red; }</style><defs><linearGradient id="g"/></defs><image href="https://tracker.example/pixel.png" xlink:href="javascript:alert(1)" width="10" height="10"/><use href="#g"/><rect onclick="secret()" filter="url(https://tracker.example/filter.svg#x)" fill="url(#g)" width="10" height="10"/><script>alert("secret")</script><foreignObject><body>secret</body></foreignObject></svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "title/description elements"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "script/foreignObject elements"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "external style references"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "removable attributes"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        for removed in [
            "Alice draft",
            "Private description",
            "tracker.example",
            "javascript:",
            "onclick",
            "onload",
            "<script",
            "<foreignObject",
        ] {
            assert!(
                !cleaned_text.contains(removed),
                "cleaned SVG still contains {removed}"
            );
        }

        assert!(cleaned_text.contains(r##"href="#g""##));
        assert!(cleaned_text.contains(r##"fill="url(#g)""##));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_unquoted_active_attributes_and_urls() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" onload=track()><image href=https://tracker.example/pixel.png xlink:href=javascript:alert(1) width=10 height=10/><rect style=filter:url(https://tracker.example/filter.svg#x) onclick=secret() width=10 height=10/></svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "removable attributes"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        for removed in [
            "onload",
            "onclick",
            "tracker.example",
            "javascript:",
            "style=",
        ] {
            assert!(
                !cleaned_text.contains(removed),
                "cleaned SVG still contains {removed}"
            );
        }
        assert!(cleaned_text.contains("width=10"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_css_escaped_external_references() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg"><style>.a{fill:u\72l(https\3a//tracker.example/style.svg#x)}</style><rect style="filter:u\72l(https\3a//tracker.example/filter.svg#x);fill:red" width="10" height="10"/></svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "external style references"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "removable attributes"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        assert!(!cleaned_text.contains("tracker.example"));
        assert!(!cleaned_text.contains(r"u\72l"));
        assert!(!cleaned_text.contains("style="));
        assert!(cleaned_text.contains("<rect"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_xml_entity_encoded_external_style_references() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg"><style>.a{fill:url(&#x68;ttps&#x3a;//tracker.example/style.svg#x)}</style><rect width="10" height="10"/></svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "external style references"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        assert!(!cleaned_text.contains("tracker.example"));
        assert!(!cleaned_text.contains("<style"));
        assert!(cleaned_text.contains("<rect"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_script_raw_text_with_lt_body() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>if(a<b){steal()}</script><rect/></svg>"#;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "script/foreignObject elements"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        assert!(!cleaned_text.contains("if(a"));
        assert!(!cleaned_text.contains("steal"));
        assert!(!cleaned_text.contains("</script>"));
        assert!(cleaned_text.contains("<rect/>"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_script_payload_with_premature_closing_pattern() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg"><script>const payload = "</script><image href=https://tracker.example/pixel.png onload=steal()></script>";</script><rect width="10"/></svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "script/foreignObject elements"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "removable attributes"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        for removed in ["<script", "</script", "tracker.example", "onload", "steal"] {
            assert!(
                !cleaned_text.contains(removed),
                "cleaned SVG still contains {removed}"
            );
        }
        assert!(cleaned_text.contains("<rect"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_namespaced_raw_active_and_style_elements() {
        let svg = br##"<svg:svg xmlns:svg="http://www.w3.org/2000/svg"><svg:style>.a{fill:url(https://tracker.example/style.svg#x)}</svg:style><svg:script>if(a<b){steal()}</svg:script><svg:rect width="10" height="10"/></svg:svg>"##;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "script/foreignObject elements"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "external style references"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        assert!(!cleaned_text.contains("tracker.example"));
        assert!(!cleaned_text.contains("steal"));
        assert!(!cleaned_text.contains("svg:script"));
        assert!(!cleaned_text.contains("svg:style"));
        assert!(cleaned_text.contains("<svg:rect"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_svg_smil_external_animation_targets() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect><animate attributeName="xlink:href" to="https://evil/x"/></rect></svg>"#;

        let info = extract_metadata(svg);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "removable attributes"));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();

        assert!(!cleaned_text.contains("https://evil/x"));
        assert!(cleaned_text.contains("<animate"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn flags_svg_data_uri_content_as_limited_verification() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><image href="data:image/jpeg;base64,/9j/4AAQSkZJRg==" width="10" height="10"/><rect fill="url(data:image/png;base64,AAA=)"/></svg>"#;

        let info = extract_metadata(svg);
        assert!(info.metadata_found.iter().any(|entry| {
            entry.category == LIMITED_VERIFICATION_CATEGORY
                && entry.name == "Embedded data URI content"
        }));

        let cleaned = remove_metadata(svg).unwrap();
        let cleaned_text = std::str::from_utf8(&cleaned).unwrap();
        assert!(cleaned_text.contains("data:image/jpeg"));
        assert!(cleaned_text.contains("data:image/png"));
        assert!(extract_metadata(&cleaned)
            .metadata_found
            .iter()
            .any(|entry| {
                entry.category == LIMITED_VERIFICATION_CATEGORY
                    && entry.name == "Embedded data URI content"
            }));
    }

    #[test]
    fn ignores_svg_data_uri_text_in_non_reference_attrs() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect data-note="url(data:image/png;base64,AAA=)" width="10" height="10"/></svg>"#;

        assert!(extract_metadata(svg).metadata_found.is_empty());
        assert!(extract_metadata(&remove_metadata(svg).unwrap())
            .metadata_found
            .is_empty());
    }

    #[test]
    fn rejects_non_utf8_svg_bytes() {
        let invalid = [0xff, 0xfe, b'<', b's', b'v', b'g', b'>'];

        assert_eq!(validate(&invalid), Err("SVG must be UTF-8 XML".to_string()));
        assert!(!looks_like_svg(&invalid));
        assert!(remove_metadata(&invalid).is_err());
    }
}
