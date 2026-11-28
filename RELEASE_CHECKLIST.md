# Media Shack — Release Checklist & Platform Test Results

## Measured Build & Footprint Metrics (macOS 15.6 arm64)

- **Toolchain:** Rust `1.93.0`, `cargo-tauri 2.12.6`, `Cargo.lock` committed
- **Clean release build (`cargo tauri build`):** `54.09s` Rust compile, `74.24s` total including `.app` and `.dmg` bundling
- **Incremental release build:** `26.25s`–`29.78s` Rust compile
- **macOS `.app` bundle:** `11.88 MiB` (`src-tauri/target/release/bundle/macos/Media Shack.app`)
- **macOS `.dmg` installer:** `4.43 MiB` (v0.1.0 baseline: `src-tauri/target/release/bundle/dmg/Media Shack_0.1.0_aarch64.dmg`; current release: `0.3.3`)
- **Window Chrome:** Cross-platform Chrome-style overlay titlebar via `tauri-plugin-decorum` `1.1` (macOS `Overlay` traffic lights + fullscreen inset sync; Windows frameless Snap Layout controls)
- **In-App Updates:** Checks GitHub Releases (`linwe2012/media-shack`), downloads matching platform `.dmg` / `.exe` installer with progress, and opens/reveals installer
- **Workspace Navigation:** Removed global title-bar `Library & Tools` menu and global `Queue` view; Library Catalog, Catalog Duplicates, Batch Rename/AI, Catalog People, Integrity & PAR2, and Tasks & Budgets now live as sub-tabs inside `Organize`, and each Encode tab displays its own Queue inside the tab with real-time per-video compression ratio, current output size, and ETA
- **Filter & Smart Selection Rules:** Unified filter + selection action bar, multi-select codec rules, smart rule pills, clickable `≥` / `<` threshold toggles, and batch `Deselect` / `+ Add` / `Select N` actions
- **External media tools tested:** `ffmpeg 9.1` / `ffprobe 9.1` (`x265 4.3`, `svt-av1 4.3.0`; `56.5 MiB` Homebrew Cellar directory), previously also verified on `ffmpeg 8.0.1` (`481 KiB` + `281 KiB` binaries; `53 MiB` Cellar)
- **Windows NSIS installer / idle GUI memory:** Pending native Windows 11 x64 build & interactive GUI measurement

## Automated Test Suite Results

Executed `cargo test` in `src-tauri/` against **FFmpeg 9.1** (`7 passed; 0 failed` in `3.45s` after final edit; also passed earlier on FFmpeg 8.0.1 in `4.96s`):
- `unit_planner_reserved_names_and_stem_preservation` — **Passed** (Windows reserved names `CON`, stem dot preservation, case-insensitive collision numbering)
- `integration_tool_discovery_skips_cwd_and_probes_capabilities` — **Passed** (macOS `/opt/homebrew/bin` discovery, CWD exclusion, `libx265` + `libsvtav1` + `mp4` smoke tests, SQLite capability cache)
- `integration_all_eight_archive_combos_plus_remux_and_special_fixtures` — **Passed** (All 8 Archive Best/Balanced × HEVC/AV1 × MKV/MP4 combinations + Remux, HDR10 gating, multi-video blocking/selection, ASS subtitle MP4 conflict & MKV resolution, 10-bit SDR preservation, rotated `-noautorotate` orientation, VFR passthrough, silent video, Unicode nested path, damaged faststart & unreadable fixtures, full A/V decode verification)
- `integration_sample_preview_and_live_preview` — **Passed** (15-second sample encode, measured bytes/elapsed, stale marking on recipe change, JPEG comparison frame decode, MP4 `-f tee` bounded packet-copy live preview branch)
- `integration_multi_queue_profile_deletion_and_recovery_transitions` — **Passed** (Multiple queues referencing shared profile, profile deletion preserving queues, nested scan, `Publishing` intent recovery, stale attempt cleanup -> `Interrupted`, `CleanupIntent` crash reconciliation -> `Uncertain`)
- `integration_source_cleanup_contract_and_safety_cases` — **Passed** (Default keep, missing/denied consent survival, cross-queue shared-source hold, output tampering detection, missing full decode rejection, swapped source inode rejection, verified `NSFileManager` recoverable Trash move, atomic no-replace `renameatx_np(RENAME_EXCL)`)
- `integration_min_savings_reserve_crossing_and_failure_then_success` — **Passed** (`BelowSavingsTarget` threshold miss keeping original & discarding staged output, mid-queue encode failure followed by valid job completing, Morning Summary accuracy, disk-reserve check path)

## Platform Coverage Matrix (`TEST_PLAN.md`)

| Target | Build & Bundle | Backend/FFmpeg Integration | Interactive GUI / Multi-Display / Overnight |
| --- | --- | --- | --- |
| macOS 15.6 `arm64` | **Passed** (`.app` + `.dmg`) | **Passed** (7/7 tests with FFmpeg 8.0.1 & FFmpeg 9.1) | **Untested** (requires manual hands-on UI run per `TEST_PLAN.md`) |
| macOS `x86_64` | **Untested** (pending Intel runner) | **Untested** | **Untested** |
| Windows 11 `x64` | **Passed** (`cargo build`, `media-shack.exe`) | **Passed** (12/12 tests: 7/7 `integration_tests` + 5/5 `platform_tests` with FFmpeg 9.0.2 & ExifTool 13.64; `PERF_1000`: 2367ms scan / 15ms search) | **Pending live GUI verification** |

## Release Sign-Off Gates

- [ ] Run interactive `TEST_PLAN.md` walkthrough on packaged macOS `arm64` `.dmg` (Retina, Light/Dark, keyboard nav, Activity Monitor preview worker kill, overnight mixed batch)
- [ ] Build and verify macOS `x86_64` native runner + code sign & notarize
- [ ] Build and verify Windows 11 `x64` native runner (`cargo tauri build`, NSIS installer, 100%/150%/200% DPI, Job Object & Recycle Bin checks, Authenticode signing)
- [x] Verify `Cargo.lock` and `rust-toolchain.toml` committed
- [ ] Keep automatic source cleanup disabled on any filesystem/volume type lacking verified recoverable Trash tests
