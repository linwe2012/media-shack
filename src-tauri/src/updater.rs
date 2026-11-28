use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const GITHUB_REPO_API_LATEST: &str =
    "https://api.github.com/repos/linwe2012/media-shack/releases/latest";
pub const GITHUB_RELEASES_PAGE: &str =
    "https://github.com/linwe2012/media-shack/releases";
const ALLOWED_DOWNLOAD_PREFIX: &str =
    "https://github.com/linwe2012/media-shack/releases/download/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub tag_name: String,
    pub update_available: bool,
    pub release_name: String,
    pub release_notes: String,
    pub html_url: String,
    pub published_at: Option<String>,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: Option<u64>,
    pub platform_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<f64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDownloadResult {
    pub downloaded_path: String,
    pub launched: bool,
    pub platform_os: String,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Parse a semver string like `"v0.3.0"` or `"0.3.0-rc.1"` into `(major, minor, patch, is_prerelease)`.
pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64, bool)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let without_build = trimmed.split('+').next()?.trim();
    let (core, is_pre) = match without_build.split_once('-') {
        Some((c, _)) => (c, true),
        None => (without_build, false),
    };
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, is_pre))
}

/// Return true when `latest` represents a strictly newer version than `current`.
pub fn is_newer_version(latest: &str, current: &str) -> bool {
    let Some((l_maj, l_min, l_pat, l_pre)) = parse_semver(latest) else {
        return false;
    };
    let Some((c_maj, c_min, c_pat, c_pre)) = parse_semver(current) else {
        return false;
    };
    match (l_maj, l_min, l_pat).cmp(&(c_maj, c_min, c_pat)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // Stable release (is_pre == false) is newer than a prerelease of the same tuple.
        std::cmp::Ordering::Equal => !l_pre && c_pre,
    }
}

pub fn current_platform_label() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macOS Apple Silicon (.dmg)"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macOS Intel (.dmg)"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "Windows x64 (.exe)"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        "Unsupported auto-installer platform"
    }
}

fn asset_suffix_candidates() -> &'static [&'static str] {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        &["aarch64.dmg", "arm64.dmg"]
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        &["x64.dmg", "x86_64.dmg"]
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        &["x64-setup.exe", ".exe"]
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        &[]
    }
}

fn select_matching_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    for suffix in asset_suffix_candidates() {
        if let Some(found) = assets
            .iter()
            .find(|a| a.name.to_ascii_lowercase().ends_with(suffix))
        {
            return Some(found);
        }
    }
    None
}

/// Query GitHub Releases for the latest published release and compare against `CARGO_PKG_VERSION`.
pub fn check_latest_release() -> Result<UpdateCheckResult, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let user_agent = format!("media-shack/{current}");
    let resp = ureq::get(GITHUB_REPO_API_LATEST)
        .set("User-Agent", &user_agent)
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("Failed to reach GitHub Releases: {e}"))?;

    let rel: GhRelease = resp
        .into_json()
        .map_err(|e| format!("Invalid response from GitHub Releases API: {e}"))?;

    if rel.draft || rel.prerelease {
        return Ok(UpdateCheckResult {
            current_version: current.clone(),
            latest_version: current,
            tag_name: rel.tag_name,
            update_available: false,
            release_name: rel.name.unwrap_or_default(),
            release_notes: rel.body.unwrap_or_default(),
            html_url: rel
                .html_url
                .unwrap_or_else(|| GITHUB_RELEASES_PAGE.to_string()),
            published_at: rel.published_at,
            asset_name: None,
            asset_url: None,
            asset_size: None,
            platform_label: current_platform_label().to_string(),
        });
    }

    let latest_version = rel.tag_name.trim_start_matches(['v', 'V']).to_string();
    let update_available = is_newer_version(&rel.tag_name, &current);
    let matched = select_matching_asset(&rel.assets);

    Ok(UpdateCheckResult {
        current_version: current,
        latest_version,
        tag_name: rel.tag_name.clone(),
        update_available,
        release_name: rel
            .name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(rel.tag_name),
        release_notes: rel.body.unwrap_or_default(),
        html_url: rel
            .html_url
            .unwrap_or_else(|| GITHUB_RELEASES_PAGE.to_string()),
        published_at: rel.published_at,
        asset_name: matched.map(|a| a.name.clone()),
        asset_url: matched.map(|a| a.browser_download_url.clone()),
        asset_size: matched.map(|a| a.size),
        platform_label: current_platform_label().to_string(),
    })
}

/// Download a release installer asset strictly from the Media Shack GitHub Releases URL
/// into `app_data_dir/updates/` and optionally open/launch it.
pub fn download_release_asset<F>(
    app_data_dir: &Path,
    asset_url: &str,
    asset_name: &str,
    auto_launch: bool,
    mut on_progress: F,
) -> Result<UpdateDownloadResult, String>
where
    F: FnMut(UpdateDownloadProgress),
{
    if !asset_url.starts_with(ALLOWED_DOWNLOAD_PREFIX) {
        return Err("Refusing update download from untrusted URL.".to_string());
    }

    let safe_name = Path::new(asset_name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && !s.contains(".."))
        .ok_or_else(|| "Invalid update asset filename.".to_string())?;

    let updates_dir = app_data_dir.join("updates");
    std::fs::create_dir_all(&updates_dir)
        .map_err(|e| format!("Failed creating updates folder: {e}"))?;

    let dest_path = updates_dir.join(safe_name);
    let part_path = updates_dir.join(format!("{safe_name}.part"));

    let user_agent = format!("media-shack/{}", env!("CARGO_PKG_VERSION"));
    let resp = ureq::get(asset_url)
        .set("User-Agent", &user_agent)
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("Failed downloading update asset: {e}"))?;

    let total_bytes = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());

    on_progress(UpdateDownloadProgress {
        downloaded_bytes: 0,
        total_bytes,
        percent: total_bytes.map(|_| 0.0),
        status: "downloading".to_string(),
    });

    let mut reader = resp.into_reader();
    let mut out = File::create(&part_path)
        .map_err(|e| format!("Failed creating temporary update file: {e}"))?;

    let mut buf = [0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now();

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Error reading update stream: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("Error writing update file: {e}"))?;
        downloaded += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(200) {
            let pct = total_bytes.and_then(|t| {
                if t > 0 {
                    Some((downloaded as f64 / t as f64) * 100.0)
                } else {
                    None
                }
            });
            on_progress(UpdateDownloadProgress {
                downloaded_bytes: downloaded,
                total_bytes,
                percent: pct,
                status: "downloading".to_string(),
            });
            last_emit = Instant::now();
        }
    }

    out.flush()
        .map_err(|e| format!("Failed flushing downloaded update: {e}"))?;
    drop(out);

    if downloaded == 0 {
        let _ = std::fs::remove_file(&part_path);
        return Err("Downloaded update file was empty.".to_string());
    }

    if dest_path.exists() {
        let _ = std::fs::remove_file(&dest_path);
    }
    std::fs::rename(&part_path, &dest_path)
        .map_err(|e| format!("Failed finalizing downloaded update: {e}"))?;

    on_progress(UpdateDownloadProgress {
        downloaded_bytes: downloaded,
        total_bytes: total_bytes.or(Some(downloaded)),
        percent: Some(100.0),
        status: "ready".to_string(),
    });

    let launched = if auto_launch {
        launch_installer(&dest_path)?;
        true
    } else {
        false
    };

    Ok(UpdateDownloadResult {
        downloaded_path: dest_path.to_string_lossy().to_string(),
        launched,
        platform_os: std::env::consts::OS.to_string(),
    })
}

fn launch_installer(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open installer image: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(path)
            .spawn()
            .map_err(|e| format!("Failed to launch installer: {e}"))?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open installer: {e}"))?;
        return Ok(());
    }
}

/// Open an HTTPS URL (restricted to github.com/linwe2012/media-shack) in the default browser.
pub fn open_github_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://github.com/linwe2012/media-shack") {
        return Err("Refusing to open untrusted external URL.".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {e}"))?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {e}"))?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parsing_and_comparison() {
        assert_eq!(parse_semver("v0.2.0"), Some((0, 2, 0, false)));
        assert_eq!(parse_semver("0.3.1-rc.1"), Some((0, 3, 1, true)));
        assert!(is_newer_version("v0.3.0", "0.2.0"));
        assert!(is_newer_version("v0.10.0", "0.2.9"));
        assert!(is_newer_version("v1.0.0", "0.99.99"));
        assert!(!is_newer_version("v0.2.0", "0.2.0"));
        assert!(!is_newer_version("v0.1.9", "0.2.0"));
        assert!(is_newer_version("v0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer_version("v0.3.0-rc.1", "0.3.0"));
    }

    #[test]
    fn rejects_untrusted_download_url() {
        let tmp = std::env::temp_dir().join("ms-updater-test");
        let err = download_release_asset(
            &tmp,
            "https://evil.example.com/payload.exe",
            "payload.exe",
            false,
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("untrusted URL"));
    }

    #[test]
    fn rejects_untrusted_external_url() {
        let err = open_github_url("https://evil.example.com").unwrap_err();
        assert!(err.contains("untrusted"));
    }
}
