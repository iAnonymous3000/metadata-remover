//! Pipeline tests over real-encoder fixture files.
//!
//! The committed fixtures in tests/fixtures/real-device were produced by real
//! encoders (Apple ImageIO for JPEG/HEIC, Apple QuickTime/AVFoundation for
//! MOV, ffmpeg/libx264 for MP4, ffmpeg/LAME for MP3), so they carry the box
//! layouts, IFD structures, and tag framing that device files have — the
//! places where purely hand-built fixtures historically missed bugs.
//!
//! Set METADATA_REMOVER_FIXTURE_DIR to a local folder of personal files
//! (never committed) to run the same pipeline checks against them.

use metadata_remover::{
    detect_file_type, extract_metadata_info, remove_metadata_bytes, validate_file_bytes,
};
use std::path::{Path, PathBuf};

const LIMITED_VERIFICATION_CATEGORY: &str = "Limited verification";

/// name, expected detected type, marker strings that must be present in the
/// original bytes and absent from the cleaned bytes.
const FIXTURES: &[(&str, &str, &[&str])] = &[
    (
        "apple-imageio-gps.jpg",
        "jpeg",
        &["iPhone 15 Pro", "FIXTURESERIAL01", "Fixture Author"],
    ),
    (
        "apple-imageio-gps.heic",
        "heic",
        &["iPhone 15 Pro", "FIXTURESERIAL01", "Fixture Author"],
    ),
    (
        "apple-quicktime-location.mov",
        "mov",
        &["+37.3316", "com.apple.quicktime.location"],
    ),
    (
        "ffmpeg-location.mp4",
        "mp4",
        &["Fixture Secret Video", "Lavf"],
    ),
    (
        "ffmpeg-lame-id3.mp3",
        "mp3",
        &[
            "Fixture Secret Title",
            "Fixture Secret Artist",
            "Fixture secret comment",
        ],
    ),
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real-device")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn run_pipeline(data: &[u8], label: &str) -> Vec<u8> {
    let file_type = detect_file_type(data);
    assert_ne!(file_type, "unknown", "{label}: type not detected");
    validate_file_bytes(data).unwrap_or_else(|error| panic!("{label}: validate failed: {error}"));

    let cleaned = remove_metadata_bytes(data)
        .unwrap_or_else(|error| panic!("{label}: clean failed: {error}"));
    assert_eq!(
        detect_file_type(&cleaned),
        file_type,
        "{label}: cleaned file changed type"
    );
    validate_file_bytes(&cleaned)
        .unwrap_or_else(|error| panic!("{label}: cleaned file failed validation: {error}"));
    cleaned
}

fn assert_rescan_clean(cleaned: &[u8], label: &str) {
    let rescan = extract_metadata_info(cleaned);
    let remaining: Vec<String> = rescan
        .metadata_found
        .iter()
        .filter(|entry| entry.category != LIMITED_VERIFICATION_CATEGORY)
        .map(|entry| format!("{}: {} = {}", entry.category, entry.name, entry.value))
        .collect();
    assert!(
        remaining.is_empty(),
        "{label}: metadata survived cleaning: {remaining:?}"
    );
}

#[test]
fn real_encoder_fixtures_are_detected_cleaned_and_verified() {
    for (name, expected_type, markers) in FIXTURES {
        let path = fixture_dir().join(name);
        let data = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("missing fixture {}: {error}", path.display()));

        assert_eq!(&detect_file_type(&data), expected_type, "{name}");
        let before = extract_metadata_info(&data);
        assert!(
            !before.metadata_found.is_empty(),
            "{name}: fixture carries no detectable metadata; regenerate it"
        );
        for marker in *markers {
            assert!(
                contains_bytes(&data, marker.as_bytes()),
                "{name}: marker {marker:?} not present in original; regenerate the fixture"
            );
        }

        let cleaned = run_pipeline(&data, name);
        assert_rescan_clean(&cleaned, name);
        for marker in *markers {
            assert!(
                !contains_bytes(&cleaned, marker.as_bytes()),
                "{name}: marker {marker:?} survived cleaning"
            );
        }
    }
}

// Runs every file dropped into the committed fixture folder (even ones not in
// the FIXTURES table) plus an optional local corpus through the pipeline.
#[test]
fn all_fixture_directory_files_survive_the_pipeline() {
    let mut dirs = vec![fixture_dir()];
    if let Ok(local) = std::env::var("METADATA_REMOVER_FIXTURE_DIR") {
        dirs.push(PathBuf::from(local));
    }

    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => panic!("cannot read fixture dir {}: {error}", dir.display()),
        };
        for entry in entries {
            let path = entry.expect("fixture dir entry").path();
            if !path.is_file() || is_non_fixture_file(&path) {
                continue;
            }
            let label = path.display().to_string();
            let data = std::fs::read(&path).expect("read fixture");
            if detect_file_type(&data) == "unknown" {
                continue;
            }
            run_pipeline(&data, &label);
        }
    }
}

fn is_non_fixture_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(name) if name.starts_with('.') || name.ends_with(".txt") || name.ends_with(".md")
    )
}
