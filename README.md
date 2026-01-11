# Metadata Remover

Privacy-focused metadata removal tool that runs entirely in your browser using WebAssembly. **No server uploads** - files never leave your machine.

## Features

- **JPEG** - Removes EXIF (camera, GPS, timestamps), XMP, IPTC, ICC profiles, comments
- **PNG** - Removes tEXt, zTXt, iTXt, eXIf, tIME, iCCP, and other ancillary chunks
- **PDF** - Removes author, creator, dates, XMP metadata, document IDs

## How It Works

1. Drop files into the browser
2. View detected metadata before cleaning
3. Click "Remove All Metadata" to process
4. Download cleaned files

All processing happens client-side via WebAssembly (~290KB). Files exist only in browser memory and are never transmitted anywhere.

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
cd wasm && wasm-pack build --target web --release && cd ..

# Serve locally
npx serve web -p 3000
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
│   │   └── pdf.rs      # PDF metadata handling
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
- **Streaming-friendly** architecture (processes files sequentially)
- Optimized WASM binary (~290KB with `opt-level=z`, LTO, panic=abort)

## Supported Metadata Types

| Format | Removed |
|--------|---------|
| JPEG | EXIF, XMP, IPTC, ICC Profile, Adobe, Ducky, Comments |
| PNG | tEXt, zTXt, iTXt, eXIf, tIME, iCCP, sRGB, gAMA, cHRM, pHYs |
| PDF | Info dictionary, XMP metadata stream, Document ID |

## Privacy

- 100% client-side processing
- No analytics, tracking, or network requests
- Works offline after initial load
- Open source (audit the code yourself)

## License

MIT
