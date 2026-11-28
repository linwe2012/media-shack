use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

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
    #[serde(default)]
    pub self_updating: bool,
    #[serde(default)]
    pub requires_restart: bool,
    #[serde(default)]
    pub message: String,
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

fn download_asset_to_updates_dir<F>(
    app_data_dir: &Path,
    asset_url: &str,
    asset_name: &str,
    on_progress: &mut F,
) -> Result<(PathBuf, PathBuf, u64, Option<u64>), String>
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

    Ok((updates_dir, dest_path, downloaded, total_bytes))
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
    let (_, dest_path, downloaded, total_bytes) =
        download_asset_to_updates_dir(app_data_dir, asset_url, asset_name, &mut on_progress)?;

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
        self_updating: false,
        requires_restart: false,
        message: if launched {
            "Opened downloaded installer.".to_string()
        } else {
            "Downloaded installer to updates folder.".to_string()
        },
        platform_os: std::env::consts::OS.to_string(),
    })
}

/// Download a release installer and perform in-place replacement + relaunch when running from
/// a packaged desktop installation (strips macOS quarantine xattrs automatically; uses silent NSIS
/// replacement on Windows). Falls back to opening the installer cleanly in dev builds.
pub fn prepare_and_launch_self_update<F>(
    app_data_dir: &Path,
    asset_url: &str,
    asset_name: &str,
    mut on_progress: F,
) -> Result<UpdateDownloadResult, String>
where
    F: FnMut(UpdateDownloadProgress),
{
    let (updates_dir, dest_path, downloaded, total_bytes) =
        download_asset_to_updates_dir(app_data_dir, asset_url, asset_name, &mut on_progress)?;

    #[cfg(target_os = "macos")]
    {
        return prepare_macos_self_update(
            &updates_dir,
            &dest_path,
            downloaded,
            total_bytes,
            &mut on_progress,
        );
    }

    #[cfg(target_os = "windows")]
    {
        return prepare_windows_self_update(
            &updates_dir,
            &dest_path,
            downloaded,
            total_bytes,
            &mut on_progress,
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = updates_dir;
        on_progress(UpdateDownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total_bytes.or(Some(downloaded)),
            percent: Some(100.0),
            status: "ready".to_string(),
        });
        launch_installer(&dest_path)?;
        Ok(UpdateDownloadResult {
            downloaded_path: dest_path.to_string_lossy().to_string(),
            launched: true,
            self_updating: false,
            requires_restart: false,
            message: "Opened update package.".to_string(),
            platform_os: std::env::consts::OS.to_string(),
        })
    }
}

#[cfg(target_os = "macos")]
fn find_macos_app_bundle_from_exe(exe_path: &Path) -> Option<PathBuf> {
    let path_str = exe_path.to_string_lossy();
    if path_str.contains("/target/debug/") || path_str.contains("/target/release/") {
        return None;
    }
    for anc in exe_path.ancestors() {
        if anc.extension().and_then(|e| e.to_str()) == Some("app")
            && anc.join("Contents/MacOS").is_dir()
        {
            return Some(anc.to_path_buf());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
struct DmgMountGuard {
    mount_point: PathBuf,
}

#[cfg(target_os = "macos")]
impl Drop for DmgMountGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .args(["detach", "-quiet"])
            .arg(&self.mount_point)
            .output()
            .or_else(|_| {
                std::process::Command::new("hdiutil")
                    .args(["detach", "-force", "-quiet"])
                    .arg(&self.mount_point)
                    .output()
            });
        let _ = std::fs::remove_dir(&self.mount_point);
    }
}

#[cfg(target_os = "macos")]
fn prepare_macos_self_update<F>(
    updates_dir: &Path,
    dmg_path: &Path,
    downloaded: u64,
    total_bytes: Option<u64>,
    on_progress: &mut F,
) -> Result<UpdateDownloadResult, String>
where
    F: FnMut(UpdateDownloadProgress),
{
    let Ok(cur_exe) = std::env::current_exe() else {
        on_progress(UpdateDownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total_bytes.or(Some(downloaded)),
            percent: Some(100.0),
            status: "ready".to_string(),
        });
        launch_installer(dmg_path)?;
        return Ok(UpdateDownloadResult {
            downloaded_path: dmg_path.to_string_lossy().to_string(),
            launched: true,
            self_updating: false,
            requires_restart: false,
            message: "Could not locate running app executable; opened DMG instead.".to_string(),
            platform_os: std::env::consts::OS.to_string(),
        });
    };

    let Some(target_app) = find_macos_app_bundle_from_exe(&cur_exe) else {
        on_progress(UpdateDownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total_bytes.or(Some(downloaded)),
            percent: Some(100.0),
            status: "ready".to_string(),
        });
        launch_installer(dmg_path)?;
        return Ok(UpdateDownloadResult {
            downloaded_path: dmg_path.to_string_lossy().to_string(),
            launched: true,
            self_updating: false,
            requires_restart: false,
            message: "Running outside packaged .app bundle; opened DMG instead.".to_string(),
            platform_os: std::env::consts::OS.to_string(),
        });
    };

    on_progress(UpdateDownloadProgress {
        downloaded_bytes: downloaded,
        total_bytes: total_bytes.or(Some(downloaded)),
        percent: Some(100.0),
        status: "preparing".to_string(),
    });

    let uid = Uuid::new_v4().to_string();
    let mount_point = updates_dir.join(format!("mnt-{uid}"));
    std::fs::create_dir_all(&mount_point)
        .map_err(|e| format!("Failed creating DMG mount directory: {e}"))?;

    let attach_out = std::process::Command::new("hdiutil")
        .arg("attach")
        .arg(dmg_path)
        .args(["-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount_point)
        .output()
        .map_err(|e| format!("Failed launching hdiutil: {e}"))?;
    if !attach_out.status.success() {
        let _ = std::fs::remove_dir(&mount_point);
        return Err(format!(
            "Failed mounting update DMG: {}",
            String::from_utf8_lossy(&attach_out.stderr).trim()
        ));
    }
    let _mount_guard = DmgMountGuard {
        mount_point: mount_point.clone(),
    };

    let target_name = target_app
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("Media Shack.app"));
    let preferred_src_app = mount_point.join(&target_name);
    let src_app = if preferred_src_app.is_dir() {
        preferred_src_app
    } else {
        let found = std::fs::read_dir(&mount_point)
            .map_err(|e| format!("Failed inspecting mounted DMG: {e}"))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir() && p.extension().and_then(|s| s.to_str()) == Some("app"));
        found.ok_or_else(|| "Mounted DMG does not contain a .app bundle.".to_string())?
    };

    let staged_dir = updates_dir.join("staged-app");
    if staged_dir.exists() {
        let _ = std::fs::remove_dir_all(&staged_dir);
    }
    std::fs::create_dir_all(&staged_dir)
        .map_err(|e| format!("Failed creating staging folder: {e}"))?;
    let staged_app = staged_dir.join(&target_name);

    let ditto_out = std::process::Command::new("ditto")
        .arg(&src_app)
        .arg(&staged_app)
        .output()
        .map_err(|e| format!("Failed running ditto: {e}"))?;
    if !ditto_out.status.success() {
        return Err(format!(
            "Failed extracting updated app bundle: {}",
            String::from_utf8_lossy(&ditto_out.stderr).trim()
        ));
    }

    // Strip any quarantine / extended attributes on staged bundle before swapping.
    let _ = std::process::Command::new("xattr")
        .args(["-cr"])
        .arg(&staged_app)
        .output();

    drop(_mount_guard);

    on_progress(UpdateDownloadProgress {
        downloaded_bytes: downloaded,
        total_bytes: total_bytes.or(Some(downloaded)),
        percent: Some(100.0),
        status: "installing".to_string(),
    });

    let backup_app = updates_dir.join(format!("backup-{uid}.app"));
    let log_file = updates_dir.join("self-update.log");
    let script_path = updates_dir.join(format!("apply-update-{uid}.sh"));
    let admin_script_path = updates_dir.join(format!("admin-swap-{uid}.sh"));

    let q_staged = shell_quote(&staged_app.to_string_lossy());
    let q_target = shell_quote(&target_app.to_string_lossy());
    let q_backup = shell_quote(&backup_app.to_string_lossy());
    let q_script = shell_quote(&script_path.to_string_lossy());
    let q_admin_script = shell_quote(&admin_script_path.to_string_lossy());
    let q_dmg = shell_quote(&dmg_path.to_string_lossy());
    let q_target_parent = shell_quote(
        &target_app
            .parent()
            .unwrap_or_else(|| Path::new("/Applications"))
            .to_string_lossy(),
    );

    let admin_script_body = format!(
        r#"#!/bin/sh
set -u
STAGED={q_staged}
TARGET={q_target}
BACKUP={q_backup}
rm -rf "$BACKUP" 2>/dev/null || true
if [ -e "$TARGET" ]; then
  mv "$TARGET" "$BACKUP" || exit 1
fi
if ! mv "$STAGED" "$TARGET" 2>/dev/null; then
  ditto "$STAGED" "$TARGET" || {{
    rm -rf "$TARGET" 2>/dev/null || true
    if [ -e "$BACKUP" ]; then
      mv "$BACKUP" "$TARGET" 2>/dev/null || true
    fi
    exit 1
  }}
  rm -rf "$STAGED" 2>/dev/null || true
fi
xattr -cr "$TARGET" >/dev/null 2>&1 || true
rm -rf "$BACKUP" 2>/dev/null || true
"#
    );
    std::fs::write(&admin_script_path, admin_script_body)
        .map_err(|e| format!("Failed writing admin updater helper script: {e}"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&admin_script_path, std::fs::Permissions::from_mode(0o700));
    }

    let pid = std::process::id();
    let script_body = format!(
        r#"#!/bin/sh
set -u
OLD_PID={pid}
STAGED={q_staged}
TARGET={q_target}
TARGET_DIR={q_target_parent}
BACKUP={q_backup}
SELF_SCRIPT={q_script}
ADMIN_SCRIPT={q_admin_script}
DMG_FILE={q_dmg}

i=0
while kill -0 "$OLD_PID" 2>/dev/null && [ "$i" -lt 120 ]; do
  sleep 0.25
  i=$((i + 1))
done

swap_direct() {{
  rm -rf "$BACKUP" 2>/dev/null || true
  if [ -e "$TARGET" ]; then
    mv "$TARGET" "$BACKUP" || return 1
  fi
  if ! mv "$STAGED" "$TARGET" 2>/dev/null; then
    ditto "$STAGED" "$TARGET" || {{
      rm -rf "$TARGET" 2>/dev/null || true
      if [ -e "$BACKUP" ]; then
        mv "$BACKUP" "$TARGET" 2>/dev/null || true
      fi
      return 1
    }}
    rm -rf "$STAGED" 2>/dev/null || true
  fi
  xattr -cr "$TARGET" >/dev/null 2>&1 || true
  rm -rf "$BACKUP" 2>/dev/null || true
  return 0
}}

if [ -w "$TARGET_DIR" ] && ( [ ! -e "$TARGET" ] || [ -w "$TARGET" ] ); then
  if ! swap_direct; then
    exit 1
  fi
else
  /usr/bin/osascript -e 'on run argv' -e 'do shell script (item 1 of argv) with administrator privileges' -e 'end run' "$ADMIN_SCRIPT" || exit 1
fi

rm -f "$DMG_FILE" "$SELF_SCRIPT" "$ADMIN_SCRIPT" 2>/dev/null || true
open -n "$TARGET" >/dev/null 2>&1 || open "$TARGET" >/dev/null 2>&1 || true
"#
    );

    std::fs::write(&script_path, script_body)
        .map_err(|e| format!("Failed writing updater helper script: {e}"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700));
    }

    let log_out = File::create(&log_file).ok();
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg(&script_path)
        .stdin(std::process::Stdio::null());
    if let Some(lf) = log_out {
        if let Ok(lf_err) = lf.try_clone() {
            cmd.stdout(std::process::Stdio::from(lf))
                .stderr(std::process::Stdio::from(lf_err));
        }
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("Failed starting updater helper: {e}"))?;

    Ok(UpdateDownloadResult {
        downloaded_path: dmg_path.to_string_lossy().to_string(),
        launched: true,
        self_updating: true,
        requires_restart: true,
        message: "Replacing app bundle, stripping quarantine attributes, and relaunching…"
            .to_string(),
        platform_os: std::env::consts::OS.to_string(),
    })
}

#[cfg(target_os = "windows")]
fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn is_packaged_windows_exe(exe_path: &Path) -> bool {
    let lower = exe_path.to_string_lossy().to_ascii_lowercase();
    !(lower.contains("\\target\\debug\\") || lower.contains("\\target\\release\\"))
}

#[cfg(target_os = "windows")]
fn prepare_windows_self_update<F>(
    updates_dir: &Path,
    installer_path: &Path,
    downloaded: u64,
    total_bytes: Option<u64>,
    on_progress: &mut F,
) -> Result<UpdateDownloadResult, String>
where
    F: FnMut(UpdateDownloadProgress),
{
    let cur_exe = match std::env::current_exe() {
        Ok(p) if is_packaged_windows_exe(&p) => p,
        _ => {
            on_progress(UpdateDownloadProgress {
                downloaded_bytes: downloaded,
                total_bytes: total_bytes.or(Some(downloaded)),
                percent: Some(100.0),
                status: "ready".to_string(),
            });
            launch_installer(installer_path)?;
            return Ok(UpdateDownloadResult {
                downloaded_path: installer_path.to_string_lossy().to_string(),
                launched: true,
                self_updating: false,
                requires_restart: false,
                message: "Running outside packaged install; launched installer instead."
                    .to_string(),
                platform_os: std::env::consts::OS.to_string(),
            });
        }
    };

    on_progress(UpdateDownloadProgress {
        downloaded_bytes: downloaded,
        total_bytes: total_bytes.or(Some(downloaded)),
        percent: Some(100.0),
        status: "installing".to_string(),
    });

    let uid = Uuid::new_v4().to_string();
    let script_path = updates_dir.join(format!("apply-update-{uid}.ps1"));
    let pid = std::process::id();
    let ps_installer = ps_single_quote(&installer_path.to_string_lossy());
    let ps_exe = ps_single_quote(&cur_exe.to_string_lossy());
    let ps_script = ps_single_quote(&script_path.to_string_lossy());

    let ps_body = format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$oldPid = {pid}
for ($i = 0; $i -lt 120; $i++) {{
  if (-not (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) {{ break }}
  Start-Sleep -Milliseconds 250
}}
$installer = {ps_installer}
$exePath = {ps_exe}
$selfScript = {ps_script}
$proc = Start-Process -FilePath $installer -ArgumentList '/S' -Wait -PassThru
if (Test-Path $exePath) {{
  Start-Process -FilePath $exePath
}}
Remove-Item -Path $selfScript -Force
"#
    );

    std::fs::write(&script_path, ps_body)
        .map_err(|e| format!("Failed writing Windows updater script: {e}"))?;

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("Failed launching Windows updater helper: {e}"))?;

    Ok(UpdateDownloadResult {
        downloaded_path: installer_path.to_string_lossy().to_string(),
        launched: true,
        self_updating: true,
        requires_restart: true,
        message: "Closing app, running silent installer, and relaunching…".to_string(),
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_bundle_lookup_and_shell_quote() {
        assert_eq!(shell_quote("/Applications/Media Shack.app"), "'/Applications/Media Shack.app'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert!(find_macos_app_bundle_from_exe(Path::new("/repo/target/debug/media-shack")).is_none());
        let tmp = tempfile::TempDir::new().unwrap();
        let macos_dir = tmp.path().join("Media Shack.app/Contents/MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        let exe = macos_dir.join("media-shack");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(
            find_macos_app_bundle_from_exe(&exe),
            Some(tmp.path().join("Media Shack.app"))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_ps_quote_and_dev_guard() {
        assert_eq!(ps_single_quote("C:\\Media'Shack"), "'C:\\Media''Shack'");
        assert!(!is_packaged_windows_exe(Path::new("C:\\repo\\src-tauri\\target\\debug\\media-shack.exe")));
        assert!(is_packaged_windows_exe(Path::new("C:\\Users\\test\\AppData\\Local\\Media Shack\\media-shack.exe")));
    }
}
