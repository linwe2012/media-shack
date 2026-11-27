# FFmpeg Queue

A local desktop batch video encoder and remuxer for **macOS** and **Windows**, built with **Tauri 2**, **Rust**, and a lightweight static **HTML/CSS/JS** interface.

Drop a folder of videos, choose **Archive → Balanced** (`HEVC libx265` or `AV1 libsvtav1`, or `Remux` stream copy), inspect tracks and output paths, and run an unattended overnight queue with durable crash recovery and source-safe cleanup controls.

## Prerequisites (External FFmpeg Tools)

FFmpeg Queue uses separately installed `ffmpeg` and `ffprobe` binaries:

- **macOS:**
  ```sh
  brew install ffmpeg
  ```
- **Windows:**
  ```powershell
  winget install Gyan.FFmpeg
  ```
  Or place `ffmpeg.exe` and `ffprobe.exe` in `C:\ffmpeg\bin` (or choose them via **Locate FFmpeg tools** inside the app).

---

## Installing Prebuilt Releases

### Installing on macOS (Unsigned Build)

Because the GitHub Actions macOS builds are not signed with an Apple Developer ID or notarized, macOS Gatekeeper marks downloaded `.dmg` / `.app` bundles as quarantined and may display **"FFmpeg Queue is damaged and can't be opened"** or **"Apple could not verify FFmpeg Queue"**.

1. Open the downloaded `.dmg` and drag **FFmpeg Queue.app** into `/Applications`.
2. Open **Terminal** and clear the Gatekeeper quarantine attribute:
   ```sh
   xattr -cr "/Applications/FFmpeg Queue.app"
   ```
3. If your macOS version also requires a local ad-hoc signature, run:
   ```sh
   codesign --force --deep --sign - "/Applications/FFmpeg Queue.app"
   ```
4. Launch **FFmpeg Queue** from `/Applications` (or, if prompted in **System Settings → Privacy & Security**, click **Open Anyway**).

### Installing on Windows

1. Download and run the NSIS installer (`FFmpeg Queue_*_x64-setup.exe`).
2. Because the installer is unsigned, Windows Defender SmartScreen may show *"Windows protected your PC"*. Click **More info → Run anyway** if you trust the release source.

---

## Building from Source & CI

- Local clean-machine build instructions: [BUILD.md](BUILD.md)
- Release checklist & measured metrics: [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md)
- Automated GitHub Actions workflow (macOS `arm64`, macOS `x86_64`, Windows `x64`): [.github/workflows/build.yml](.github/workflows/build.yml)

> **Note:** If this directory is placed inside a larger monorepo, move `.github/workflows/build.yml` to the repository root and set `defaults.run.working-directory` (or `working-directory`) accordingly.
