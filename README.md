# Media Shack

An all-in-one local desktop media management workstation for **macOS** and **Windows**, built with **Tauri 2**, **Rust**, and a lightweight static **HTML/CSS/JS** interface.

Each tab has a mode selector (**Encode** or **Repair**):

- **Encode Queue Mode:** Drop videos or folders, choose **Archive → Balanced** (`HEVC libx265` or `AV1 libsvtav1`, or `Remux` stream copy), inspect tracks and output paths, preview 15-second clips, and run unattended overnight queues with crash recovery and source-safe Trash cleanup.
- **MP4 Video Repair Mode (`video-recover`):** Diagnose damaged MP4/MOV/MPEG-TS files (`ftyp`, `moov`, `moof`, `mdat`, truncated/corrupt offsets, zero-filled/repetitive regions), analyze optional healthy reference videos (SPS/PPS/VPS, AAC `AudioSpecificConfig`, Quest 2/VR `st3d`/`sv3d` projection tags), run staged lossless container + custom binary H.264/HEVC NAL bitstream recovery, verify SHA-256 non-modification of originals, evaluate every candidate via full FFmpeg decode + contact-sheet thumbnails, and export JSON/Text reports.

---

## Bundled Dependencies & CLI Usage

Prebuilt release installers bundle static `ffmpeg` and `ffprobe` binaries directly alongside Media Shack so users do not need a separate install. When developing from source, Media Shack automatically discovers local tools (`brew install ffmpeg` on macOS, `winget install Gyan.FFmpeg` / BtbN GPL builds on Windows), and optionally leverages `untrunc` if present on `PATH`.

### Standalone CLI (`video-recover`)

```sh
video-recover diagnose broken.mp4 [--json] [--report diag.json]
video-recover recover broken.mp4 --reference healthy.mp4 --output recovered.mp4
video-recover recover broken.mp4 --video-only --deep --timeout-secs 300
video-recover recover broken.mp4 --reference-dir ./references --deep --json
video-recover batch ./damaged --output-dir ./recovered --reference healthy.mp4
```

---

## Installing Prebuilt Releases

### Installing on macOS (Unsigned Build)

Because the GitHub Actions macOS builds are not signed with an Apple Developer ID or notarized, macOS Gatekeeper marks downloaded `.dmg` / `.app` bundles as quarantined and may display **"Media Shack is damaged and can't be opened"** or **"Apple could not verify Media Shack"**.

1. Open the downloaded `.dmg` and drag **Media Shack.app** into `/Applications`.
2. Open **Terminal** and clear the Gatekeeper quarantine attribute:
   ```sh
   xattr -cr "/Applications/Media Shack.app"
   ```
3. If your macOS version also requires a local ad-hoc signature, run:
   ```sh
   codesign --force --deep --sign - "/Applications/Media Shack.app"
   ```
4. Launch **Media Shack** from `/Applications` (or, if prompted in **System Settings → Privacy & Security**, click **Open Anyway**).

### Installing on Windows

1. Download and run the NSIS installer (`Media Shack_*_x64-setup.exe`).
2. Because the installer is unsigned, Windows Defender SmartScreen may show *"Windows protected your PC"*. Click **More info → Run anyway** if you trust the release source.

---

## Building from Source & CI

- Local clean-machine build instructions: [BUILD.md](BUILD.md)
- Release checklist & measured metrics: [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md)
- Automated GitHub Actions workflow (macOS `arm64`, macOS `x86_64`, Windows `x64`): [.github/workflows/build.yml](.github/workflows/build.yml)
