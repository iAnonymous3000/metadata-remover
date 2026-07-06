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

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decodes standard base64 (padding optional, ASCII whitespace ignored).
/// Returns None on any character outside the standard alphabet.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }

    Some(out)
}

/// Encodes bytes as standard padded base64.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32);
        out.push(BASE64_ALPHABET[(value >> 18) as usize & 0x3f] as char);
        out.push(BASE64_ALPHABET[(value >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(value >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[value as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
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

    #[test]
    fn base64_round_trips_and_rejects_invalid_input() {
        for data in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"binary\x00\xff\x7f",
        ] {
            let encoded = base64_encode(data);
            assert_eq!(base64_decode(&encoded).unwrap(), data);
        }
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert!(base64_decode("not*valid").is_none());
    }
}
