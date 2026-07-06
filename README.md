# Metadata Remover

[![Build and Deploy](https://github.com/iAnonymous3000/metadata-remover/actions/workflows/deploy.yml/badge.svg)](https://github.com/iAnonymous3000/metadata-remover/actions/workflows/deploy.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Privacy-focused metadata removal that runs entirely in your browser with WebAssembly. Files are processed locally and are never uploaded.

Live demo: https://ianonymous3000.github.io/metadata-remover/

## Supported Formats

- **JPEG** - removes APP/JFIF, EXIF except minimal orientation, XMP, IPTC, Content Credentials/JUMBF, comments, and trailing appended data
- **PNG** - removes text, EXIF, Content Credentials/JUMBF chunks, timestamps, physical-resolution data, and unknown ancillary chunks
- **WebP** - removes EXIF, XMP, Content Credentials/JUMBF chunks, and unknown non-visual RIFF chunks
- **AVIF** - removes identifiable EXIF, XMP, and Content Credentials metadata from ISO-BMFF image containers
- **GIF** - removes comments, XMP blocks, and unknown application extensions
- **HEIC/HEIF** - removes identifiable EXIF, XMP, and Content Credentials metadata from ISO-BMFF image containers
- **TIFF** - removes EXIF, GPS, XMP, IPTC, camera, software, author, timestamp, physical-resolution, and copyright tags from classic TIFF IFDs
- **SVG** - removes metadata elements, title/description text, XML comments, processing instructions, active content, event handlers, external references, and common editor namespace attributes
- **MP4/MOV** - removes QuickTime/MP4 user-data, Content Credentials boxes, metadata boxes, and recording timestamps while preserving media tracks
- **MP3** - removes ID3v1/ID3v2, APEv2, and Lyrics3 tags, including title/artist/album/comment frames, private frames, and embedded artwork, while preserving MPEG audio frames
- **PDF** - removes info dictionaries, XMP metadata streams, document IDs, app-specific metadata, and embedded file attachments
- **DOCX** - removes package properties, comments, review authors, revision IDs, and tracked-change metadata
- **PPTX** - removes package properties, comments, comment authors, threaded comments, and modern author parts
- **XLSX** - removes package properties, comments, threaded comments, person records, and revision trails
- **ODT/ODS/ODP** - removes ODF package metadata and annotations
- **EPUB** - removes OPF author, publisher, subject, description, identifiers, calibre metadata, metadata links, and reading-position sidecars while preserving book content

Cleaned files are validated as the same file type, re-scanned before download, and flagged if removable metadata remains or verification is limited by preserved embedded content. Browser-renderable images can also prepare a local visual verification preview after cleaning when you open the file details.

## Use

1. Drop files into the browser.
2. Review detected metadata.
3. Click "Remove All Metadata".
4. Download cleaned files individually or as a ZIP.

All processing happens client-side in a browser worker. The app has no analytics and sends no file data to a server.

## Development

Prerequisites:

- Node.js 22 or newer for the JavaScript checks and static server commands.
- The Rust toolchain in `rust-toolchain.toml`, including `wasm32-unknown-unknown`, `rustfmt`, and `clippy`.
- `wasm-pack` 0.14.x and Binaryen `wasm-opt` for release WASM builds. CI currently pins wasm-pack 0.14.0 and Binaryen 129.
- `cargo-audit` if you want to run the same Rust advisory check as CI.

```bash
npm run build
npm test
npm run dist
npm run serve
```

For a full local release check, also run:

```bash
(cd wasm && cargo fmt --check)
(cd wasm && cargo test --locked)
(cd wasm && cargo clippy --locked --all-targets -- -D warnings)
(cd wasm && cargo audit --deny warnings)
npm test
npm run dist
```

The GitHub Pages deployment cannot apply Cloudflare Pages or Netlify `_headers` rules. The hosted demo uses HTTPS from GitHub Pages, an in-document CSP for same-origin app resources, and a no-referrer meta policy. Header-only protections such as HSTS, `frame-ancestors`, COOP/COEP/CORP, `X-Content-Type-Options`, and Permissions-Policy require a host or proxy that can set HTTP response headers.

## Limitations

- The browser UI rejects individual files over 100 MB and applies a device-memory-aware batch budget. Download or clear finished files before adding more.
- JPEG orientation and color-transform/profile segments are preserved to avoid sideways images and color shifts.
- BigTIFF is not supported; save or export as classic TIFF first.
- AVIF, HEIC/HEIF, and MP4/MOV cleaning target known ISO-BMFF metadata structures this parser can identify.
- EPUB cleaning preserves required package fields by normalizing identifiers and modified timestamps to neutral values.
- MP3 support removes ID3v1/ID3v2, APEv2, and Lyrics3 tags. FLAC, OGG/Opus, and WAV are not supported yet.
- Embedded images inside Office, ODF, and EPUB packages are scanned and flagged when supported image metadata is detected, but they are not recursively cleaned and keep the result in review.
- Visual verification is limited to browser-renderable JPEG, PNG, WebP, AVIF, GIF, and cleaned SVG previews, and runs after cleaning when local memory headroom allows.
- SVG active content and external references are removed, which can change interactive or externally styled SVGs.
- SVG embedded raster images are not decoded; SVG data URIs are preserved and keep the result in review.
- Unknown/private TIFF tags and uncommon or `ftyp`-less video variants may be preserved or rejected.
- Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are not supported; save them as `.docx`, `.xlsx`, or `.pptx` first.
- This tool removes structural metadata; it does not detect steganographic content embedded in pixels or document bodies.

## License

MIT
