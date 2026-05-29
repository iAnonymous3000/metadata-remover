# Changelog

All notable changes to Metadata Remover are documented here.

## Unreleased

- Added WebP and GIF metadata cleaning.
- Preserved APNG animation chunks, JPEG fill bytes, orientation, and color rendering data needed for correct display.
- Added GPS coordinate decoding for JPEG EXIF metadata.
- Added validation during analysis with friendly errors for invalid or corrupt files.
- Improved worker recovery after fatal parser failures.
- Added privacy/trust copy for local Rust and WebAssembly processing.
- Added browser hardening, CI checks, Dependabot, and community documentation.
