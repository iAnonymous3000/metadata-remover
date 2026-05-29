# Contributing

Thanks for helping improve Metadata Remover.

## Local Setup

Prerequisites:

- Rust with the `wasm32-unknown-unknown` target
- `wasm-pack`
- Node.js

Useful commands:

```bash
npm run build
npm run serve
```

## Checks

Run these before opening a pull request:

```bash
cd wasm
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo audit --deny warnings
cd ..
npm test
npm run dist
```

## Parser Changes

This project parses untrusted binary files in the browser. Parser changes should include regression tests for malformed input, trailing data, preserved visual data, and cleaned-output verification.

## Pull Requests

Keep pull requests focused. Describe what changed, why it changed, and how you verified it.
