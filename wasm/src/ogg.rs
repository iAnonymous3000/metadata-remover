use crate::{flac, MetadataInfo};

const OGG_MAGIC: &[u8; 4] = b"OggS";
const VORBIS_ID_PREFIX: &[u8; 7] = b"\x01vorbis";
const VORBIS_COMMENT_PREFIX: &[u8; 7] = b"\x03vorbis";
const OPUS_ID_PREFIX: &[u8; 8] = b"OpusHead";
const OPUS_TAGS_PREFIX: &[u8; 8] = b"OpusTags";
const MAX_HEADER_PACKET_BYTES: usize = 16 * 1024 * 1024;

// OGG page CRC: polynomial 0x04C11DB7, no reflection, zero init, no final xor.
const OGG_CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OggCodec {
    Vorbis,
    Opus,
}

impl OggCodec {
    fn header_packets_after_id(self) -> usize {
        match self {
            // Comment header, then the setup header.
            OggCodec::Vorbis => 2,
            // OpusTags only.
            OggCodec::Opus => 1,
        }
    }

    fn minimal_comment_packet(self) -> Vec<u8> {
        let mut packet = Vec::new();
        match self {
            OggCodec::Vorbis => {
                packet.extend_from_slice(VORBIS_COMMENT_PREFIX);
                packet.extend_from_slice(&0u32.to_le_bytes());
                packet.extend_from_slice(&0u32.to_le_bytes());
                packet.push(1); // framing bit
            }
            OggCodec::Opus => {
                packet.extend_from_slice(OPUS_TAGS_PREFIX);
                packet.extend_from_slice(&0u32.to_le_bytes());
                packet.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        packet
    }

    fn comment_body(self, packet: &[u8]) -> Option<&[u8]> {
        match self {
            OggCodec::Vorbis => packet.strip_prefix(VORBIS_COMMENT_PREFIX),
            OggCodec::Opus => packet.strip_prefix(OPUS_TAGS_PREFIX),
        }
    }
}

struct OggPage<'a> {
    header_type: u8,
    serial: u32,
    segment_table: &'a [u8],
    payload: &'a [u8],
    raw: &'a [u8],
}

impl OggPage<'_> {
    fn is_bos(&self) -> bool {
        self.header_type & 0x02 != 0
    }

    fn is_continued(&self) -> bool {
        self.header_type & 0x01 != 0
    }

    fn ends_mid_packet(&self) -> bool {
        self.segment_table.last() == Some(&255)
    }
}

struct OggLayout<'a> {
    codec: OggCodec,
    pages: Vec<OggPage<'a>>,
    header_packets: Vec<Vec<u8>>,
    first_audio_page: usize,
}

pub fn looks_like_ogg(data: &[u8]) -> bool {
    data.len() >= 5 && &data[0..4] == OGG_MAGIC && data[4] == 0
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    parse_layout(data).map(|_| ())
}

pub fn extract_metadata(data: &[u8]) -> MetadataInfo {
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;

    if let Ok(layout) = parse_layout(data) {
        let comment_packet = &layout.header_packets[0];
        let minimal = layout.codec.minimal_comment_packet();
        if comment_packet != &minimal {
            total_bytes += comment_packet.len();
            if let Some(body) = layout.codec.comment_body(comment_packet) {
                flac::collect_vorbis_comments(body, &mut entries);
            }
        }
    }

    MetadataInfo {
        file_type: "ogg".to_string(),
        metadata_found: entries,
        total_metadata_bytes: total_bytes,
    }
}

pub fn remove_metadata(data: &[u8]) -> Result<Vec<u8>, String> {
    let layout = parse_layout(data)?;

    let minimal = layout.codec.minimal_comment_packet();
    if layout.header_packets[0] == minimal {
        return Ok(data.to_vec());
    }

    let mut replacement_packets: Vec<&[u8]> = vec![&minimal];
    for packet in &layout.header_packets[1..] {
        replacement_packets.push(packet);
    }

    let first_page = &layout.pages[0];
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(first_page.raw);
    let next_sequence = write_header_pages(&mut out, first_page.serial, 1, &replacement_packets)?;

    for (index, page) in layout.pages[layout.first_audio_page..].iter().enumerate() {
        let sequence = next_sequence
            .checked_add(index as u32)
            .ok_or_else(|| "OGG file contains too many pages".to_string())?;
        out.extend_from_slice(&resequenced_page(page.raw, sequence));
    }

    Ok(out)
}

fn parse_layout(data: &[u8]) -> Result<OggLayout<'_>, String> {
    let pages = parse_pages(data)?;
    let first = &pages[0];
    if !first.is_bos() || first.is_continued() {
        return Err("OGG file does not start with a stream header page".to_string());
    }
    if first.ends_mid_packet() || first.segment_table.iter().filter(|l| **l < 255).count() != 1 {
        return Err("OGG identification header page is malformed".to_string());
    }
    for page in &pages[1..] {
        if page.serial != first.serial {
            return Err("Multiplexed OGG streams are not supported".to_string());
        }
        if page.is_bos() {
            return Err("Chained OGG streams are not supported".to_string());
        }
    }

    let codec = if first.payload.starts_with(VORBIS_ID_PREFIX) {
        OggCodec::Vorbis
    } else if first.payload.starts_with(OPUS_ID_PREFIX) {
        OggCodec::Opus
    } else {
        return Err("OGG stream codec is not supported".to_string());
    };

    let (header_packets, first_audio_page) =
        collect_header_packets(&pages, codec.header_packets_after_id())?;
    if first_audio_page >= pages.len() {
        return Err("OGG missing audio packets".to_string());
    }

    let comment_packet = &header_packets[0];
    if codec.comment_body(comment_packet).is_none() {
        return Err("OGG comment header is malformed".to_string());
    }
    if codec == OggCodec::Vorbis && !header_packets[1].starts_with(b"\x05vorbis") {
        return Err("OGG Vorbis setup header is malformed".to_string());
    }

    Ok(OggLayout {
        codec,
        pages,
        header_packets,
        first_audio_page,
    })
}

fn parse_pages(data: &[u8]) -> Result<Vec<OggPage<'_>>, String> {
    let mut pages = Vec::new();
    let mut offset = 0usize;

    while offset < data.len() {
        let header = data
            .get(offset..offset + 27)
            .ok_or_else(|| "Truncated OGG page".to_string())?;
        if &header[0..4] != OGG_MAGIC {
            return Err("Invalid OGG page header".to_string());
        }
        if header[4] != 0 {
            return Err("Unsupported OGG page version".to_string());
        }
        let segment_count = header[26] as usize;
        let table_start = offset + 27;
        let table_end = table_start
            .checked_add(segment_count)
            .ok_or_else(|| "Truncated OGG page".to_string())?;
        let segment_table = data
            .get(table_start..table_end)
            .ok_or_else(|| "Truncated OGG page".to_string())?;
        let payload_len: usize = segment_table.iter().map(|l| *l as usize).sum();
        let payload_end = table_end
            .checked_add(payload_len)
            .ok_or_else(|| "Truncated OGG page".to_string())?;
        let payload = data
            .get(table_end..payload_end)
            .ok_or_else(|| "Truncated OGG page".to_string())?;
        let raw = &data[offset..payload_end];

        let declared_crc = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
        if ogg_page_crc(raw) != declared_crc {
            return Err("OGG page checksum mismatch".to_string());
        }

        pages.push(OggPage {
            header_type: header[5],
            serial: u32::from_le_bytes([header[14], header[15], header[16], header[17]]),
            segment_table,
            payload,
            raw,
        });
        offset = payload_end;
    }

    if pages.is_empty() {
        return Err("Not a valid OGG file".to_string());
    }
    Ok(pages)
}

// Collects `count` complete packets starting at page 1. Both the Vorbis and
// Opus specs require the header packets to finish at a page boundary before
// audio begins, which keeps the rebuild surgical.
fn collect_header_packets(
    pages: &[OggPage<'_>],
    count: usize,
) -> Result<(Vec<Vec<u8>>, usize), String> {
    let mut packets = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut total_bytes = 0usize;

    for (page_index, page) in pages.iter().enumerate().skip(1) {
        if page.is_continued() == current.is_empty() {
            return Err("OGG header packets are not page-aligned".to_string());
        }

        let mut payload_offset = 0usize;
        for lacing in page.segment_table {
            let lacing = *lacing as usize;
            total_bytes = total_bytes.saturating_add(lacing);
            if total_bytes > MAX_HEADER_PACKET_BYTES {
                return Err("OGG header packets are too large".to_string());
            }
            current.extend_from_slice(&page.payload[payload_offset..payload_offset + lacing]);
            payload_offset += lacing;
            if lacing < 255 {
                packets.push(std::mem::take(&mut current));
                if packets.len() == count {
                    if payload_offset != page.payload.len() {
                        return Err("OGG audio packets share a header page".to_string());
                    }
                    return Ok((packets, page_index + 1));
                }
            }
        }
    }

    Err("OGG is missing its header packets".to_string())
}

// Lays packets out onto new pages (granule 0, sequential numbering starting at
// `first_sequence`) and returns the next free sequence number.
fn write_header_pages(
    out: &mut Vec<u8>,
    serial: u32,
    first_sequence: u32,
    packets: &[&[u8]],
) -> Result<u32, String> {
    let mut lacings: Vec<u8> = Vec::new();
    let mut payload: Vec<u8> = Vec::new();
    for packet in packets {
        for chunk in packet.chunks(255) {
            lacings.push(chunk.len() as u8);
            payload.extend_from_slice(chunk);
        }
        if packet.len() % 255 == 0 {
            // A packet whose length is a multiple of 255 (including empty)
            // is terminated by an explicit zero lacing value.
            lacings.push(0);
        }
    }

    let mut sequence = first_sequence;
    let mut lacing_offset = 0usize;
    let mut payload_offset = 0usize;
    let mut continued = false;

    while lacing_offset < lacings.len() {
        let page_lacings = &lacings[lacing_offset..(lacing_offset + 255).min(lacings.len())];
        let page_payload_len: usize = page_lacings.iter().map(|l| *l as usize).sum();
        let page_payload = &payload[payload_offset..payload_offset + page_payload_len];

        let mut page = Vec::with_capacity(27 + page_lacings.len() + page_payload.len());
        page.extend_from_slice(OGG_MAGIC);
        page.push(0);
        page.push(if continued { 0x01 } else { 0x00 });
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(page_lacings.len() as u8);
        page.extend_from_slice(page_lacings);
        page.extend_from_slice(page_payload);
        let crc = ogg_page_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&page);

        continued = page_lacings.last() == Some(&255);
        lacing_offset += page_lacings.len();
        payload_offset += page_payload_len;
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| "OGG file contains too many pages".to_string())?;
    }

    Ok(sequence)
}

fn resequenced_page(raw: &[u8], sequence: u32) -> Vec<u8> {
    let mut page = raw.to_vec();
    page[18..22].copy_from_slice(&sequence.to_le_bytes());
    page[22..26].copy_from_slice(&0u32.to_le_bytes());
    let crc = ogg_page_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn ogg_page_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for (index, byte) in page.iter().enumerate() {
        // The CRC field itself (bytes 22..26) is treated as zero.
        let byte = if (22..26).contains(&index) { 0 } else { *byte };
        crc = (crc << 8) ^ OGG_CRC_TABLE[(((crc >> 24) as u8) ^ byte) as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(
        header_type: u8,
        granule: u64,
        serial: u32,
        sequence: u32,
        packets: &[&[u8]],
        trailing_continues: bool,
    ) -> Vec<u8> {
        let mut lacings: Vec<u8> = Vec::new();
        let mut payload: Vec<u8> = Vec::new();
        for (index, packet) in packets.iter().enumerate() {
            for chunk in packet.chunks(255) {
                lacings.push(chunk.len() as u8);
                payload.extend_from_slice(chunk);
            }
            let is_last = index == packets.len() - 1;
            if packet.len() % 255 == 0 && !(is_last && trailing_continues) {
                lacings.push(0);
            }
            if is_last && trailing_continues {
                assert_eq!(packet.len() % 255, 0, "continuing packet must end at 255");
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(OGG_MAGIC);
        out.push(0);
        out.push(header_type);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(lacings.len() as u8);
        out.extend_from_slice(&lacings);
        out.extend_from_slice(&payload);
        let crc = ogg_page_crc(&out);
        out[22..26].copy_from_slice(&crc.to_le_bytes());
        out
    }

    fn opus_id_packet() -> Vec<u8> {
        let mut packet = OPUS_ID_PREFIX.to_vec();
        packet.extend_from_slice(&[1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0]);
        packet
    }

    fn opus_tags_packet(comments: &[&str]) -> Vec<u8> {
        let vendor = b"Lavf99 test vendor";
        let mut packet = OPUS_TAGS_PREFIX.to_vec();
        packet.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        packet.extend_from_slice(vendor);
        packet.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for comment in comments {
            packet.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            packet.extend_from_slice(comment.as_bytes());
        }
        packet
    }

    fn opus_file(comments: &[&str]) -> Vec<u8> {
        let mut data = page(0x02, 0, 7, 0, &[&opus_id_packet()], false);
        data.extend_from_slice(&page(0x00, 0, 7, 1, &[&opus_tags_packet(comments)], false));
        data.extend_from_slice(&page(0x04, 960, 7, 2, &[b"AUDIOPACKET"], false));
        data
    }

    fn vorbis_file(comments: &[&str]) -> Vec<u8> {
        let mut id = VORBIS_ID_PREFIX.to_vec();
        id.extend_from_slice(&[0; 23]);
        let vendor = b"Xiph test vendor";
        let mut comment = VORBIS_COMMENT_PREFIX.to_vec();
        comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        comment.extend_from_slice(vendor);
        comment.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for entry in comments {
            comment.extend_from_slice(&(entry.len() as u32).to_le_bytes());
            comment.extend_from_slice(entry.as_bytes());
        }
        comment.push(1);
        let mut setup = b"\x05vorbis".to_vec();
        setup.extend_from_slice(b"SETUPDATA");

        let mut data = page(0x02, 0, 3, 0, &[&id], false);
        data.extend_from_slice(&page(0x00, 0, 3, 1, &[&comment, &setup], false));
        data.extend_from_slice(&page(0x04, 1024, 3, 2, &[b"VORBISAUDIO"], false));
        data
    }

    #[test]
    fn removes_opus_tags_and_preserves_audio() {
        let input = opus_file(&["TITLE=Secret Song", "ARTIST=Secret Artist"]);

        assert!(looks_like_ogg(&input));
        validate(&input).unwrap();
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "TITLE" && entry.value == "Secret Song"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "Encoder (vendor)"));

        let cleaned = remove_metadata(&input).unwrap();
        validate(&cleaned).unwrap();
        for secret in [&b"Secret"[..], b"Lavf99"] {
            assert!(
                !cleaned.windows(secret.len()).any(|w| w == secret),
                "cleaned OGG still contains {}",
                String::from_utf8_lossy(secret)
            );
        }
        assert!(cleaned
            .windows(b"AUDIOPACKET".len())
            .any(|w| w == b"AUDIOPACKET"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn removes_vorbis_comments_and_keeps_setup_header() {
        let input = vorbis_file(&["TITLE=Secret Song"]);

        validate(&input).unwrap();
        assert!(!extract_metadata(&input).metadata_found.is_empty());

        let cleaned = remove_metadata(&input).unwrap();
        validate(&cleaned).unwrap();
        assert!(!cleaned.windows(6).any(|w| w == b"Secret"));
        assert!(cleaned
            .windows(b"SETUPDATA".len())
            .any(|w| w == b"SETUPDATA"));
        assert!(cleaned
            .windows(b"VORBISAUDIO".len())
            .any(|w| w == b"VORBISAUDIO"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn labels_vorbis_comment_artwork_and_cleans_multipage_comments() {
        let artwork = format!("METADATA_BLOCK_PICTURE={}", "A".repeat(600));
        let tags = opus_tags_packet(&[&artwork, "TITLE=Secret Song"]);
        // Split the tags packet across two pages (510 = 2 * 255 lacing values).
        let (head, tail) = tags.split_at(510);
        let mut input = page(0x02, 0, 7, 0, &[&opus_id_packet()], false);
        input.extend_from_slice(&page(0x00, 0, 7, 1, &[head], true));
        input.extend_from_slice(&page(0x01, 0, 7, 2, &[tail], false));
        input.extend_from_slice(&page(0x04, 960, 7, 3, &[b"AUDIOPACKET"], false));

        validate(&input).unwrap();
        let info = extract_metadata(&input);
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.category == "Embedded artwork"));
        assert!(info
            .metadata_found
            .iter()
            .any(|entry| entry.name == "TITLE"));

        let cleaned = remove_metadata(&input).unwrap();
        validate(&cleaned).unwrap();
        assert!(!cleaned.windows(6).any(|w| w == b"Secret"));
        assert!(!cleaned.windows(10).any(|w| w == b"AAAAAAAAAA"));
        assert!(cleaned
            .windows(b"AUDIOPACKET".len())
            .any(|w| w == b"AUDIOPACKET"));
        assert!(extract_metadata(&cleaned).metadata_found.is_empty());
    }

    #[test]
    fn already_minimal_ogg_is_left_unchanged() {
        let mut minimal_tags = OPUS_TAGS_PREFIX.to_vec();
        minimal_tags.extend_from_slice(&0u32.to_le_bytes());
        minimal_tags.extend_from_slice(&0u32.to_le_bytes());
        let mut input = page(0x02, 0, 7, 0, &[&opus_id_packet()], false);
        input.extend_from_slice(&page(0x00, 0, 7, 1, &[&minimal_tags], false));
        input.extend_from_slice(&page(0x04, 960, 7, 2, &[b"AUDIOPACKET"], false));

        assert!(extract_metadata(&input).metadata_found.is_empty());
        assert_eq!(remove_metadata(&input).unwrap(), input);
    }

    #[test]
    fn rejects_corrupt_multiplexed_and_unsupported_streams() {
        let mut corrupted = opus_file(&["TITLE=Secret"]);
        let len = corrupted.len();
        corrupted[len - 1] ^= 0xff;
        assert_eq!(
            validate(&corrupted),
            Err("OGG page checksum mismatch".to_string())
        );

        let mut multiplexed = page(0x02, 0, 7, 0, &[&opus_id_packet()], false);
        multiplexed.extend_from_slice(&page(0x00, 0, 8, 1, &[&opus_tags_packet(&[])], false));
        assert_eq!(
            validate(&multiplexed),
            Err("Multiplexed OGG streams are not supported".to_string())
        );

        let theora = page(0x02, 0, 7, 0, &[b"\x80theora----------"], false);
        assert_eq!(
            validate(&theora),
            Err("OGG stream codec is not supported".to_string())
        );

        let headers_only = page(0x02, 0, 7, 0, &[&opus_id_packet()], false);
        assert!(validate(&headers_only).is_err());
    }
}
