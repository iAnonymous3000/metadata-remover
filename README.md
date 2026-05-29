# Metadata Remover

Privacy-focused metadata removal tool that runs entirely in your browser using WebAssembly. **No server uploads** - files never leave your machine.

## Features

- **JPEG** - Removes JFIF/APP metadata, EXIF (camera, GPS, timestamps), XMP, IPTC, comments, and trailing appended data while preserving orientation and color rendering
- **PNG** - Removes text, EXIF, timestamps, physical-resolution metadata, and unknown ancillary chunks while preserving transparency and color rendering
- **WebP** - Removes EXIF, XMP, and unknown non-visual RIFF chunks while preserving image, alpha, animation, and color-profile chunks
- **GIF** - Removes comments, XMP blocks, and unknown application extensions while preserving image frames, transparency controls, plain-text blocks, and animation loops
- **PDF** - Removes info dictionaries, XMP metadata streams, document IDs, app-specific metadata, and embedded file attachments
- **Verification** - Re-scans cleaned output before download and flags any remaining removable metadata

## How It Works

1. Drop files into the browser
2. View detected metadata before cleaning
3. Click "Remove All Metadata" to process
4. Download individual cleaned files or a ZIP containing the full cleaned batch

All processing happens client-side via WebAssembly in a browser worker. Files exist only in browser memory and are never transmitted anywhere.

## Deploy Your Own

### GitHub Pages (Automatic)

1. Fork this repository
2. Go to Settings > Pages > Source: "GitHub Actions"
3. Push to `main` branch
4. Site deploys to `https://username.github.io/metadata-remover`

### Local Development

```bash
# Prerequisites: Rust, wasm-pack, Node.js

# Build WASM
npm run build

# Serve locally
npm run serve
```

## Project Structure

```
metadata-remover/
├── .github/workflows/   # GitHub Actions for auto-deploy
├── wasm/                # Rust WASM crate
│   ├── src/
│   │   ├── lib.rs      # Entry point, file detection
│   │   ├── jpeg.rs     # JPEG metadata handling
│   │   ├── png.rs      # PNG metadata handling
│   │   ├── pdf.rs      # PDF metadata handling
│   │   ├── webp.rs     # WebP metadata handling
│   │   └── gif.rs      # GIF metadata handling
│   └── Cargo.toml
├── web/                 # Static frontend
│   ├── index.html
│   ├── css/style.css
│   └── js/app.js
└── package.json
```

## Technical Details

- **Rust** compiled to WASM for native-speed binary parsing
- **Zero dependencies** frontend (vanilla JS)
- Worker-based processing keeps parsing and cleaning off the main UI thread
- Per-file size limit prevents accidental browser memory exhaustion
- CSP limits scripts, workers, network fetches, forms, and embedded objects to the local app origin
- Optimized WASM binary (~340KB with `opt-level=z`, LTO, panic=abort)

## Supported Metadata Types

| Format | Removed |
|--------|---------|
| JPEG | APP/JFIF segments, EXIF except minimal orientation, XMP, IPTC, Ducky, Comments, trailing appended data |
| PNG | tEXt, zTXt, iTXt, eXIf, tIME, pHYs, unknown ancillary chunks |
| WebP | EXIF chunks, XMP chunks, unknown non-visual RIFF chunks |
| GIF | Comment extensions, XMP application extensions, unknown application extensions |
| PDF | Info dictionary, XMP metadata streams, Document ID, PieceInfo, MarkInfo, embedded file attachments |

## Privacy

- 100% client-side processing
- No analytics, tracking, or network requests
- Works offline after initial load through a service worker cache
- Installable as a standalone web app where browser PWA support is available
- Open source (audit the code yourself)

## Limitations

- JPEG orientation and color-transform/profile segments are preserved to avoid sideways images and color shifts.
- This tool removes structural metadata; it does not detect steganographic content embedded in pixels or document bodies.

## License

MIT
