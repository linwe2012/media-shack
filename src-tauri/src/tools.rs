use crate::db::Database;
use crate::models::{SmokeTestResult, ToolCapabilities, ToolStatus, VideoCodec};
use chrono::Utc;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
const FFMPEG_BIN: &str = "ffmpeg.exe";
#[cfg(not(target_os = "windows"))]
const FFMPEG_BIN: &str = "ffmpeg";

#[cfg(target_os = "windows")]
const FFPROBE_BIN: &str = "ffprobe.exe";
#[cfg(not(target_os = "windows"))]
const FFPROBE_BIN: &str = "ffprobe";

pub fn discover_and_probe(
    db: &Database,
    configured_dir: Option<&str>,
    explicit_ffmpeg: Option<&str>,
    explicit_ffprobe: Option<&str>,
    force_recheck: bool,
) -> ToolStatus {
    let pair = find_tool_pair(configured_dir, explicit_ffmpeg, explicit_ffprobe);
    let (ffmpeg_path, ffprobe_path) = match pair {
        Ok(p) => p,
        Err(err) => {
            return ToolStatus {
                discovered: false,
                capabilities: None,
                error: Some(err),
                repair_hint: Some(repair_hint_text()),
            };
        }
    };

    let identity_key = match compute_pair_identity(&ffmpeg_path, &ffprobe_path) {
        Ok(k) => k,
        Err(e) => {
            return ToolStatus {
                discovered: false,
                capabilities: None,
                error: Some(format!("Cannot inspect tool binaries: {e}")),
                repair_hint: Some(repair_hint_text()),
            };
        }
    };

    if !force_recheck {
        if let Ok(Some(cached)) = db.get_cached_tools(&identity_key) {
            return ToolStatus {
                discovered: true,
                capabilities: Some(cached),
                error: None,
                repair_hint: None,
            };
        }
    }

    match run_capability_check(&ffmpeg_path, &ffprobe_path, &identity_key) {
        Ok(caps) => {
            let _ = db.save_cached_tools(&caps);
            ToolStatus {
                discovered: true,
                capabilities: Some(caps),
                error: None,
                repair_hint: None,
            }
        }
        Err(err) => ToolStatus {
            discovered: false,
            capabilities: None,
            error: Some(err),
            repair_hint: Some(repair_hint_text()),
        },
    }
}

fn repair_hint_text() -> String {
    if cfg!(target_os = "macos") {
        "Install FFmpeg via Homebrew (`brew install ffmpeg`) or click Locate FFmpeg tools to select ffmpeg and ffprobe.".to_string()
    } else if cfg!(target_os = "windows") {
        "Install FFmpeg (e.g. `winget install Gyan.FFmpeg` or BtbN release) and click Locate FFmpeg tools to select ffmpeg.exe and ffprobe.exe.".to_string()
    } else {
        "Install ffmpeg and ffprobe or click Locate FFmpeg tools.".to_string()
    }
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return (meta.permissions().mode() & 0o111) != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn pair_in_dir(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let ff = dir.join(FFMPEG_BIN);
    let fp = dir.join(FFPROBE_BIN);
    if is_executable_file(&ff) && is_executable_file(&fp) {
        Some((
            ff.canonicalize().unwrap_or(ff),
            fp.canonicalize().unwrap_or(fp),
        ))
    } else {
        None
    }
}

pub fn find_tool_pair(
    configured_dir: Option<&str>,
    explicit_ffmpeg: Option<&str>,
    explicit_ffprobe: Option<&str>,
) -> Result<(PathBuf, PathBuf), String> {
    // 0. Explicit persisted absolute paths (both valid, or one explicit + sibling in same directory)
    match (explicit_ffmpeg, explicit_ffprobe) {
        (Some(ff_s), Some(fp_s)) => {
            let ff = PathBuf::from(ff_s);
            let fp = PathBuf::from(fp_s);
            if ff.is_absolute() && fp.is_absolute() && is_executable_file(&ff) && is_executable_file(&fp) {
                return Ok((ff.canonicalize().unwrap_or(ff), fp.canonicalize().unwrap_or(fp)));
            }
        }
        (Some(ff_s), None) => {
            let ff = PathBuf::from(ff_s);
            if ff.is_absolute() && is_executable_file(&ff) {
                if let Some(dir) = ff.parent() {
                    let fp = dir.join(FFPROBE_BIN);
                    if is_executable_file(&fp) {
                        return Ok((ff.canonicalize().unwrap_or(ff), fp.canonicalize().unwrap_or(fp)));
                    }
                }
            }
        }
        (None, Some(fp_s)) => {
            let fp = PathBuf::from(fp_s);
            if fp.is_absolute() && is_executable_file(&fp) {
                if let Some(dir) = fp.parent() {
                    let ff = dir.join(FFMPEG_BIN);
                    if is_executable_file(&ff) {
                        return Ok((ff.canonicalize().unwrap_or(ff), fp.canonicalize().unwrap_or(fp)));
                    }
                }
            }
        }
        (None, None) => {}
    }

    // 1. Configured tools directory
    if let Some(dir_s) = configured_dir {
        let dir = PathBuf::from(dir_s);
        if dir.is_absolute() {
            if let Some(pair) = pair_in_dir(&dir) {
                return Ok(pair);
            }
        }
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.canonicalize().ok());

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let mut push_trusted = |p: PathBuf| {
        if !p.is_absolute() {
            return;
        }
        if let Ok(canon) = p.canonicalize() {
            if let Some(ref c) = cwd {
                if &canon == c {
                    return;
                }
            }
            if seen.insert(canon.clone()) {
                search_dirs.push(canon);
            }
        }
    };

    // 2. Installed locations (including both Homebrew prefixes on macOS and standard Windows locations)
    #[cfg(target_os = "macos")]
    {
        push_trusted(PathBuf::from("/opt/homebrew/bin"));
        push_trusted(PathBuf::from("/usr/local/bin"));
        push_trusted(PathBuf::from("/opt/local/bin"));
    }

    #[cfg(target_os = "windows")]
    {
        push_trusted(PathBuf::from(r"C:\ffmpeg\bin"));
        if let Ok(pf) = std::env::var("ProgramFiles") {
            push_trusted(PathBuf::from(&pf).join("ffmpeg").join("bin"));
        }
        if let Ok(la) = std::env::var("LOCALAPPDATA") {
            push_trusted(PathBuf::from(&la).join("Programs").join("ffmpeg").join("bin"));
            push_trusted(
                PathBuf::from(&la)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }
        if let Ok(up) = std::env::var("USERPROFILE") {
            push_trusted(PathBuf::from(&up).join("scoop").join("shims"));
        }
        push_trusted(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        push_trusted(PathBuf::from("/usr/local/bin"));
        push_trusted(PathBuf::from("/usr/bin"));
    }

    // 3. Trusted PATH directories (skip empty / relative / CWD)
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() || dir == Path::new(".") || !dir.is_absolute() {
                continue;
            }
            push_trusted(dir);
        }
    }

    for dir in search_dirs {
        if let Some(pair) = pair_in_dir(&dir) {
            return Ok(pair);
        }
    }

    Err("Could not find ffmpeg and ffprobe in configured tools folder, standard install locations, or trusted PATH.".to_string())
}

fn compute_pair_identity(ffmpeg: &Path, ffprobe: &Path) -> Result<String, String> {
    let ff_meta = std::fs::metadata(ffmpeg).map_err(|e| e.to_string())?;
    let fp_meta = std::fs::metadata(ffprobe).map_err(|e| e.to_string())?;
    let ff_mt = ff_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let fp_mt = fp_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!(
        "{}:{}:{}|{}:{}:{}",
        ffmpeg.display(),
        ff_meta.len(),
        ff_mt,
        ffprobe.display(),
        fp_meta.len(),
        fp_mt
    ))
}

fn run_cmd_capture(bin: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to run {}: {e}", bin.display()))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
    if combined.trim().is_empty() {
        combined = String::from_utf8_lossy(&out.stderr).to_string();
    }
    if !out.status.success() && combined.trim().is_empty() {
        return Err(format!("{} exited with {:?}", bin.display(), out.status.code()));
    }
    Ok(combined)
}

fn run_capability_check(
    ffmpeg: &Path,
    ffprobe: &Path,
    identity_key: &str,
) -> Result<ToolCapabilities, String> {
    let ff_ver_raw = run_cmd_capture(ffmpeg, &["-version"])?;
    let fp_ver_raw = run_cmd_capture(ffprobe, &["-version"])?;
    let ffmpeg_version = ff_ver_raw
        .lines()
        .next()
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let ffprobe_version = fp_ver_raw
        .lines()
        .next()
        .unwrap_or("unknown")
        .trim()
        .to_string();

    let encoders_txt = run_cmd_capture(ffmpeg, &["-hide_banner", "-encoders"])?;
    let muxers_txt = run_cmd_capture(ffmpeg, &["-hide_banner", "-muxers"])?;

    let has_libx265_listed = encoders_txt.contains("libx265");
    let has_libsvtav1_listed = encoders_txt.contains("libsvtav1");
    let has_aac = encoders_txt.lines().any(|l| l.contains(" aac "));
    let has_mov_text = encoders_txt.lines().any(|l| l.contains(" mov_text "));
    let has_matroska = muxers_txt.lines().any(|l| l.contains(" matroska "));
    let has_mp4 = muxers_txt.lines().any(|l| l.contains(" mp4 "));

    // Encoder help checks
    let x265_help_ok = has_libx265_listed
        && run_cmd_capture(ffmpeg, &["-hide_banner", "-h", "encoder=libx265"])
            .map(|s| s.contains("crf") || s.contains("preset"))
            .unwrap_or(false);
    let svtav1_help_ok = has_libsvtav1_listed
        && run_cmd_capture(ffmpeg, &["-hide_banner", "-h", "encoder=libsvtav1"])
            .map(|s| s.contains("crf") || s.contains("preset"))
            .unwrap_or(false);

    let mut smoke_tests = Vec::new();
    let tmp_dir = std::env::temp_dir().join(format!("ffq-smoke-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&tmp_dir);

    let mut x265_smoke_ok = false;
    if x265_help_ok && has_matroska {
        let out_file = tmp_dir.join("smoke_hevc.mkv");
        let res = run_smoke_encode(
            ffmpeg,
            &[
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:d=0.1:r=10",
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "28",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "matroska",
                &out_file.to_string_lossy(),
            ],
            &out_file,
        );
        x265_smoke_ok = res.passed;
        smoke_tests.push(SmokeTestResult {
            name: "libx265 + matroska".to_string(),
            passed: res.passed,
            details: res.details,
        });
    } else {
        smoke_tests.push(SmokeTestResult {
            name: "libx265 + matroska".to_string(),
            passed: false,
            details: "libx265 or matroska muxer unavailable".to_string(),
        });
    }

    let mut svtav1_smoke_ok = false;
    if svtav1_help_ok && has_matroska {
        let out_file = tmp_dir.join("smoke_av1.mkv");
        let res = run_smoke_encode(
            ffmpeg,
            &[
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:d=0.1:r=10",
                "-c:v",
                "libsvtav1",
                "-preset",
                "8",
                "-crf",
                "35",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "matroska",
                &out_file.to_string_lossy(),
            ],
            &out_file,
        );
        svtav1_smoke_ok = res.passed;
        smoke_tests.push(SmokeTestResult {
            name: "libsvtav1 + matroska".to_string(),
            passed: res.passed,
            details: res.details,
        });
    } else {
        smoke_tests.push(SmokeTestResult {
            name: "libsvtav1 + matroska".to_string(),
            passed: false,
            details: "libsvtav1 or matroska muxer unavailable".to_string(),
        });
    }

    if has_mp4 {
        let out_file = tmp_dir.join("smoke_remux.mp4");
        let res = run_smoke_encode(
            ffmpeg,
            &[
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.1",
                "-c:a",
                if has_aac { "aac" } else { "copy" },
                "-f",
                "mp4",
                &out_file.to_string_lossy(),
            ],
            &out_file,
        );
        smoke_tests.push(SmokeTestResult {
            name: "mp4 muxer".to_string(),
            passed: res.passed,
            details: res.details,
        });
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);

    let mut available_codecs = Vec::new();
    if x265_smoke_ok {
        available_codecs.push(VideoCodec::Hevc);
    }
    if svtav1_smoke_ok {
        available_codecs.push(VideoCodec::Av1);
    }

    Ok(ToolCapabilities {
        ffmpeg_path: ffmpeg.to_string_lossy().to_string(),
        ffprobe_path: ffprobe.to_string_lossy().to_string(),
        ffmpeg_version,
        ffprobe_version,
        has_libx265: x265_smoke_ok,
        has_libsvtav1: svtav1_smoke_ok,
        has_aac,
        has_matroska,
        has_mp4,
        has_mov_text,
        available_codecs,
        smoke_tests,
        checked_at: Utc::now().timestamp(),
        identity_key: identity_key.to_string(),
    })
}

struct SmokeOutcome {
    passed: bool,
    details: String,
}

fn run_smoke_encode(ffmpeg: &Path, args: &[&str], out_file: &Path) -> SmokeOutcome {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {
            let size = std::fs::metadata(out_file).map(|m| m.len()).unwrap_or(0);
            if size > 0 {
                SmokeOutcome {
                    passed: true,
                    details: format!("OK ({size} bytes)"),
                }
            } else {
                SmokeOutcome {
                    passed: false,
                    details: "Produced 0-byte output".to_string(),
                }
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: String = err.lines().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
            SmokeOutcome {
                passed: false,
                details: format!("Exit {:?}: {}", o.status.code(), tail),
            }
        }
        Err(e) => SmokeOutcome {
            passed: false,
            details: format!("Spawn error: {e}"),
        },
    }
}
