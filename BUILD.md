# Building Media Shack

Media Shack is built with **Tauri 2**, **Rust**, and a static **HTML/CSS/JavaScript** frontend (`ui/`), using the Cargo Tauri CLI with no Node.js toolchain required.

## Pinned Toolchain

- **Rust:** `1.93.0` (pinned in [rust-toolchain.toml](rust-toolchain.toml))
- **Tauri CLI:** `cargo-tauri 2.12.6`
- **Dependencies:** pinned in `src-tauri/Cargo.lock`

## macOS Clean-Machine Build Recipe (arm64 and x86_64)

1. Install Xcode Command Line Tools:
   ```sh
   xcode-select --install
   ```
2. Install Rust via `rustup` (automatically selects `1.93.0` from `rust-toolchain.toml`):
   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
3. Install the pinned Cargo Tauri CLI:
   ```sh
   cargo install tauri-cli --version 2.12.6 --locked
   ```
4. Install FFmpeg and ffprobe separately (with `libx265` and `libsvtav1`):
   ```sh
   brew install ffmpeg
   ```
   On first launch, Media Shack searches configured tools directories, `/opt/homebrew/bin`, `/usr/local/bin`, `/opt/local/bin`, and trusted `PATH` entries (skipping the current working directory). You can also click **Locate FFmpeg tools** in the app.
5. Run in development or build the native bundle (`.app` and `.dmg`) from the project root:
   ```sh
   cargo tauri dev
   cargo tauri build
   ```
   For public macOS distribution, sign with Developer ID Application and notarize via `xcrun notarytool`.

## Windows 11 x64 Clean-Machine Build Recipe

1. Install **Microsoft C++ Build Tools** (Desktop development with C++ workload) and ensure **Microsoft Edge WebView2 Runtime** is installed (preinstalled on Windows 11).
2. Install Rust via `rustup-init.exe` (uses `1.93.0` from `rust-toolchain.toml`).
3. Install the pinned Cargo Tauri CLI:
   ```powershell
   cargo install tauri-cli --version 2.12.6 --locked
   ```
4. Install FFmpeg and ffprobe (e.g. `winget install Gyan.FFmpeg` or place `ffmpeg.exe` and `ffprobe.exe` in `C:\ffmpeg\bin` or `%LOCALAPPDATA%\Programs\ffmpeg\bin`).
5. Run from the project root:
   ```powershell
   cargo tauri dev
   cargo tauri build
   ```
   This builds the native binary and NSIS installer (`src-tauri/target/release/bundle/nsis/`). For public Windows releases, Authenticode-sign both the app executable and NSIS installer.

## Running Automated Tests

```sh
cd src-tauri
cargo test
```

## Measured Build & Runtime Metrics

- Measured metrics and platform verification status are tracked in [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md).
