# Contributing

Thanks for helping. Pneuma is a Rust workspace:

- `crates/pneuma-core` — config, build backends, ports, filters
- `crates/pneuma-cli` — the `pneuma` binary, terminal UI and monitor
- `vscode-pneuma` — VS Code extension (plain JS, no build step)

## Build and test

```bash
./pneuma.sh test                       # cargo test (core + cli)
cd vscode-pneuma && node --test test   # extension tests
```

The native-backend tests compile for real when a toolchain exists (`pneuma setup`) and skip otherwise.

## Before a pull request

- Add a test for behavior you change; note any hardware-only check in the pull request.
- Native builds must stay byte-identical to arduino-cli's `.hex` (compare with `PNEUMA_BACKEND=arduino-cli`).
- Keep `cargo test` green and `cargo build --release` warning-free.

By contributing you agree your work is licensed under Apache-2.0.
