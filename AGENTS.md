# Media Shack — Agent & Contributor Guidelines

## Architecture & Platform Targets
- **Stack:** Tauri 2 + Rust backend (`src-tauri/`) with a static HTML/CSS/JS frontend (`ui/`, no Node bundler).
- **Supported Targets:** macOS (`aarch64-apple-darwin`, `x86_64-apple-darwin`) and Windows (`x86_64-pc-windows-msvc`).
- **Window Chrome:** Cross-platform Chrome-style overlay titlebar via `tauri-plugin-decorum` (`titleBarStyle: Overlay` + `hiddenTitle: true` on macOS; frameless Snap Layout controls in `#window-controls-slot` on Windows).
- **In-App Updates:** `src-tauri/src/updater.rs` queries GitHub Releases (`linwe2012/media-shack`), compares semver strictly, skips drafts and prereleases, and downloads the matching `.dmg` / `-setup.exe` installer into `app_data_dir/updates/`.

## Cross-Platform Rust & Compiler Warning Rules
- Keep builds warning-free on **both** macOS and Windows:
  - Gate platform-specific imports right next to their usage with `#[cfg(unix)]`, `#[cfg(target_os = "macos")]`, or `#[cfg(target_os = "windows")]` instead of unconditional top-level `use`.
  - Use `#[cfg_attr(not(unix), allow(unused_mut))]` (or narrow scopes) when a binding is only mutated on one OS.
  - Ensure every `#[cfg(...)]` early-return block has complete fallback returns (avoiding `E0317`), and guard generic OS fallbacks with `#[cfg(not(any(target_os = "windows", target_os = "macos")))]` so `unreachable_code` never fires.
  - Remove dead/leftover imports rather than suppressing warnings globally.

## Linting & Git Pre-Commit Hook
- **Hook location:** `.githooks/pre-commit`
- **Enable locally:** `git config core.hooksPath .githooks`
- **Checks run on every commit:**
  - `node --check` on `ui/app.js`, `ui/platform.js`, and `stress/mock-tauri.js`
  - JSON syntax validation on `src-tauri/tauri.conf.json`, `src-tauri/tauri.bundle.json`, and `src-tauri/capabilities/default.json`
  - `cargo check --all-targets` and `cargo clippy --all-targets -- -D unused-imports -D unreachable-code -D unused-mut` (when `cargo` is installed locally; otherwise enforced in GitHub Actions)

## Versioning, Tagging & GitHub Releases
- **Use SemVer conservatively:**
  - **Patch (`0.3.1`, `0.3.2`):** bug fixes, compiler warning cleanups, CI/small UI tweaks.
  - **Minor (`0.4.0`, `0.5.0`):** new user-facing features.
  - **Major (`1.0.0`):** milestone or breaking changes.
- Keep versions synchronized across `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` (`media-shack`), `src-tauri/tauri.conf.json`, and `RELEASE_CHECKLIST.md` when releasing.
- **Tagging & deleting bad releases:**
  - Tags (`v*`) trigger `.github/workflows/build.yml` to build `.dmg` / `.exe` installers and publish a GitHub Release.
  - If a tag/release needs to be pulled immediately before users upgrade, remove it with `gh release delete vX.Y.Z --yes --cleanup-tag && git tag -d vX.Y.Z`.
  - Once a version has been live, prefer cutting a new **patch** version (e.g. `v0.3.1`) rather than re-tagging, because the in-app updater checks `is_newer_version(latest, current)` strictly.
  - Mark experimental/test builds as GitHub **prereleases** (or `-rc.1` tags) so the in-app updater ignores them.
- **GitHub Actions workflow edits:** Pushing changes to `.github/workflows/*.yml` requires a token with `workflow` scope (`gh auth refresh -s workflow`) or SSH push access.
