# Pneuma for VS Code

Build, upload and monitor microcontrollers from VS Code with the Pneuma CLI.

## Install

1. Build the CLI: `cargo install --path crates/pneuma-cli --locked` (or set `pneuma.path` to `target/release/pneuma`).
2. `npx @vscode/vsce package` in this folder, then `code --install-extension pneuma-0.1.0.vsix`.

## Use

Open a folder with `pneuma.lua` (or run **Pneuma: New Project**). The status bar shows the environment, the port and Build / Upload / Monitor buttons.

- `Ctrl+Alt+B` build, `Ctrl+Alt+U` upload, `Ctrl+Alt+M` serial monitor.
- Compiler errors appear in the Problems panel and point at your files in `src/` and `libs/`.
- **Pneuma: Add Library** downloads a library from the Arduino registry into `libs/`.
- Tasks: `{ "type": "pneuma", "command": "build" }` works in `tasks.json`.

Uploading while the monitor terminal is open is fine: Pneuma releases the port and the monitor reconnects.
