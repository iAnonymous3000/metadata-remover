#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = metadata_remover::detect_file_type(data);
    let _ = metadata_remover::extract_metadata_info(data);
    let _ = metadata_remover::validate_file_bytes(data);
    let _ = metadata_remover::remove_metadata_bytes(data);
});
