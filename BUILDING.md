# Building and Verifying the WASM Bundle

Metadata Remover builds its browser parser as WebAssembly. The deploy workflow checks that this WASM bundle is deterministic inside the pinned CI toolchain, then publishes the SHA-256 of the built `metadata_remover_bg.wasm` in the GitHub Actions job summary.

## What Is Pinned

The deploy workflow builds on `ubuntu-latest` with these pinned tools:

| Tool | Version |
|------|---------|
| Rust | 1.94.1 |
| wasm target | wasm32-unknown-unknown |
| wasm-pack | 0.14.0 |
| Binaryen / wasm-opt | 129 |
| Node.js | 22 |

Rust is pinned for local and CI use by `rust-toolchain.toml`. Binaryen is installed explicitly in CI from the `version_129` release archive and verified by SHA-256 before `wasm-pack` runs.

## Determinism Check

On every push to `main`, the deploy workflow:

1. Installs the pinned Rust toolchain, `wasm-pack`, and Binaryen.
2. Runs `npm run build`.
3. Saves the first `web/wasm/metadata_remover_bg.wasm`.
4. Deletes the generated WASM outputs.
5. Runs `npm run build` again.
6. Fails if the two WASM files are not byte-identical.
7. Writes the CI-built WASM SHA-256 to the job summary.

This is a build-twice determinism check, not a committed-hash drift gate. It should fail when the same pinned CI toolchain cannot reproduce its own WASM bytes.

## Scope of the Claim

The reproducibility claim is intentionally narrow:

> The WASM bytes are reproducible inside the pinned CI toolchain: GitHub Actions `ubuntu-latest` on x86_64 Linux with Rust 1.94.1, wasm-pack 0.14.0, and Binaryen 129.

This does not claim that every operating system or CPU architecture will produce the same SHA-256. For example, a local macOS arm64 rebuild may produce a different hash from the CI Linux build. That difference is not evidence of tampering by itself.

## Compare the Deployed WASM to CI

After the first successful deploy for the commit you want to check:

1. Open the matching `Build and Deploy` workflow run for the commit.
2. In the build job summary, copy the SHA-256 printed for `metadata_remover_bg.wasm`.
3. Download the deployed WASM:

```bash
curl -fsSL \
  https://ianonymous3000.github.io/metadata-remover/wasm/metadata_remover_bg.wasm \
  -o metadata_remover_bg.wasm
sha256sum metadata_remover_bg.wasm
```

On macOS, use `shasum -a 256 metadata_remover_bg.wasm` if `sha256sum` is not installed.

The downloaded hash should match the hash printed by the successful workflow run that deployed the site.

## Rebuild Locally

For development, install Rust through `rustup`, install `wasm-pack`, and run:

```bash
npm test
npm run dist
```

`npm test` builds `web/wasm` before running the JavaScript tests, because those tests exercise the real WASM parser. `npm run dist` rebuilds the WASM and copies the static site into `dist/`.

Local rebuilds are useful for development and sanity checks. Use the GitHub Actions build summary for the authoritative deployed SHA-256.
