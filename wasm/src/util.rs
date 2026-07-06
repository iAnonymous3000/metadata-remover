//! Small helpers shared by multiple format parsers.

/// Case-insensitive ASCII substring search over raw bytes.
pub fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// Strips an XML namespace prefix, returning the local element/attribute name.
pub fn local_name(name: &str) -> &str {
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

/// Decodes the predefined XML entities plus numeric character references.
/// Unknown named entities are passed through unchanged.
pub fn decode_xml_entities(value: &str) -> String {
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

/// Truncates a display value to `max_chars` characters, appending "..." when cut.
pub fn truncate_for_display(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_ascii_ci_matches_case_insensitively_and_rejects_empty_needle() {
        assert!(contains_ascii_ci(b"has C2PA marker", b"c2pa"));
        assert!(!contains_ascii_ci(b"nothing here", b"c2pa"));
        assert!(!contains_ascii_ci(b"short", b""));
        assert!(!contains_ascii_ci(b"a", b"longer"));
    }

    #[test]
    fn local_name_strips_namespace_prefix() {
        assert_eq!(local_name("dc:creator"), "creator");
        assert_eq!(local_name("creator"), "creator");
    }

    #[test]
    fn decode_xml_entities_handles_named_numeric_and_unknown() {
        assert_eq!(decode_xml_entities("a&amp;b&lt;c&#65;&#x42;"), "a&b<cAB");
        assert_eq!(decode_xml_entities("&unknown;&"), "&unknown;&");
    }

    #[test]
    fn truncate_for_display_appends_ellipsis_only_when_cut() {
        assert_eq!(truncate_for_display("short", 10), "short");
        assert_eq!(truncate_for_display("longvalue", 4), "long...");
    }
}
