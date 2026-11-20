# Arc for Tezbar

Arc's Raycast extension ported to a standalone Rust process for Tezbar 0.3.0-beta.12 and newer. The port keeps all 16 command IDs and the macOS platform declaration. It reads Arc history and downloads on demand and uses Arc's macOS scripting interface for Spaces, Favorites, Pinned Tabs and window or tab actions. See [the port report](PORT-REPORT.md) for the implementation and the remaining desktop checks.

Build and package an Apple Silicon development artifact locally on a Mac.

```sh
CARGO_BUILD_JOBS=1 cargo build --release --locked
python3 scripts/package.py --target aarch64-apple-darwin --binary target/release/tezbar-arc
```

Run synthetic checks on macOS or Windows. Append `.exe` to the binary on Windows.

```sh
CARGO_BUILD_JOBS=1 cargo test --locked --features protocol-fixtures
CARGO_BUILD_JOBS=1 cargo build --locked --features protocol-fixtures
python3 scripts/protocol-smoke.py target/debug/tezbar-arc
node tests/jxa-identity.cjs
```

The fixture feature requires an explicit `--fixtures` directory and the `arc-fixture` protocol context. The macOS development packaging path checks that fixture support was omitted. On Windows, build with `--release --locked --features protocol-fixtures` and select the `x86_64-pc-windows-msvc` packager target to generate a development probe. Use `python3 scripts/package.py --source` to regenerate the portable source archive and its adjacent SHA-256 and file inventory.

The [original source](https://github.com/raycast/extensions/tree/3627d9b15ba4506ca8c8ac9d93c4585c5d2ec7ae/extensions/arc/) and [original license](LICENSE) are preserved. [UPSTREAM.json](UPSTREAM.json) records the Store identity, immutable commit, tree and blob hashes.
