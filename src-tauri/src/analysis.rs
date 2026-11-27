use crate::catalog::to_long_fs_path;
use crate::db::Database;
use chrono::Utc;
use image::{DynamicImage, GenericImageView};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

pub const FRAME_CACHE_ALGO_VERSION: &str = "frames_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SamplingMode {
    FixedInterval { interval_secs: f64 },
    Percentage { count: usize },
    SceneChange { threshold: f64, max_frames: usize },
    Keyframes { max_frames: usize },
    Adaptive { initial_count: usize, dense_count: usize },
}

impl Default for SamplingMode {
    fn default() -> Self {
        Self::Percentage { count: 8 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledFrame {
    pub index: usize,
    pub timestamp_secs: f64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackFrozenReport {
    pub file_key: String,
    pub sampled_frames: usize,
    pub black_frame_timestamps: Vec<f64>,
    pub frozen_intervals: Vec<(f64, f64)>,
    pub black_ratio: f64,
    pub is_mostly_black: bool,
    pub has_frozen_segments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub cache_dir: String,
    pub total_bytes: u64,
    pub file_count: usize,
    pub budget_bytes: u64,
}

pub fn analysis_cache_dir(db: &Database) -> PathBuf {
    let dir = db.app_data_dir.join("analysis_cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn get_cache_stats(db: &Database) -> Result<CacheStats, String> {
    let dir = analysis_cache_dir(db);
    let budget_bytes: u64 = db
        .get_setting("analysis_cache_budget_bytes")?
        .unwrap_or(2 * 1024 * 1024 * 1024);
    let mut total_bytes = 0u64;
    let mut file_count = 0usize;
    walk_cache_size(&dir, &mut total_bytes, &mut file_count);
    Ok(CacheStats {
        cache_dir: dir.to_string_lossy().to_string(),
        total_bytes,
        file_count,
        budget_bytes,
    })
}

fn walk_cache_size(dir: &Path, total: &mut u64, count: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_cache_size(&p, total, count);
        } else if let Ok(m) = e.metadata() {
            *total += m.len();
            *count += 1;
        }
    }
}

pub fn evict_cache_to_budget(db: &Database, max_bytes: u64) -> Result<u64, String> {
    let dir = analysis_cache_dir(db);
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    collect_cache_files(&dir, &mut files);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max_bytes {
        return Ok(0);
    }
    files.sort_by_key(|f| f.2);
    let mut freed = 0u64;
    for (path, sz, _) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(sz);
            freed += sz;
        }
    }
    Ok(freed)
}

pub fn clear_analysis_cache(db: &Database) -> Result<u64, String> {
    let dir = analysis_cache_dir(db);
    let stats = get_cache_stats(db)?;
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
    }
    let conn = db.lock_conn()?;
    conn.execute("UPDATE media_files SET thumbnail_path = NULL", [])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM analysis_results WHERE analysis_type = 'sampled_frames'", [])
        .map_err(|e| e.to_string())?;
    Ok(stats.total_bytes)
}

fn collect_cache_files(dir: &Path, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_cache_files(&p, out);
        } else if let Ok(m) = e.metadata() {
            let t = m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((p, m.len(), t));
        }
    }
}

fn sampling_algo_key(mode: &SamplingMode) -> String {
    match mode {
        SamplingMode::FixedInterval { interval_secs } => {
            format!("{FRAME_CACHE_ALGO_VERSION}:fixed_{:.1}", interval_secs)
        }
        SamplingMode::Percentage { count } => format!("{FRAME_CACHE_ALGO_VERSION}:pct_{count}"),
        SamplingMode::SceneChange { threshold, max_frames } => {
            format!("{FRAME_CACHE_ALGO_VERSION}:scene_{:.2}_{max_frames}", threshold)
        }
        SamplingMode::Keyframes { max_frames } => format!("{FRAME_CACHE_ALGO_VERSION}:kf_{max_frames}"),
        SamplingMode::Adaptive { initial_count, dense_count } => {
            format!("{FRAME_CACHE_ALGO_VERSION}:adapt_{initial_count}_{dense_count}")
        }
    }
}

/// Shared adaptive frame extraction used by thumbnails, perceptual hashing, OCR, semantic search, and black/frozen checks.
/// Cached by content identity (`file_key` + `sampling_algo_key`), surviving renames and tag edits without colliding across modes.
pub fn extract_sampled_frames(
    db: &Database,
    ffmpeg_path: &Path,
    file_key: &str,
    mode: &SamplingMode,
) -> Result<Vec<SampledFrame>, String> {
    let sampling_key = serde_json::to_string(mode).unwrap_or_else(|_| "default".to_string());
    let algo_key = sampling_algo_key(mode);
    {
        let conn = db.lock_conn()?;
        let cached: Option<String> = conn
            .query_row(
                "SELECT result_json FROM analysis_results
                 WHERE file_key = ?1 AND analysis_type = 'sampled_frames' AND algo_version = ?2",
                params![file_key, algo_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(json) = cached {
            if let Ok(frames) = serde_json::from_str::<Vec<SampledFrame>>(&json) {
                if !frames.is_empty() && frames.iter().all(|f| Path::new(&f.path).exists()) {
                    return Ok(frames);
                }
            }
        }
    }

    let (file_path, media_type, duration_secs, vr_projection, width, height): (
        String,
        String,
        Option<f64>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    ) = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT fl.path, ma.media_type, mf.duration_secs, mf.vr_projection, mf.width, mf.height
             FROM media_files mf
             JOIN media_assets ma ON ma.asset_id = mf.asset_id
             JOIN file_locations fl ON fl.file_key = mf.file_key AND fl.is_current = 1
             WHERE mf.file_key = ?1
             LIMIT 1",
            params![file_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .map_err(|e| format!("Media file not found in catalog: {e}"))?
    };

    let src = to_long_fs_path(Path::new(&file_path));
    if !src.exists() {
        return Err(format!("Source file is offline or missing: {file_path}"));
    }

    let cache_subdir = analysis_cache_dir(db).join(file_key);
    let _ = std::fs::create_dir_all(&cache_subdir);

    // Handle static images directly without FFmpeg video decoding
    if media_type == "image" {
        let out_png = cache_subdir.join("frame_0000.png");
        let img = image::open(&src).map_err(|e| format!("Failed to open image: {e}"))?;
        let thumb = img.thumbnail(640, 640);
        thumb
            .save(&out_png)
            .map_err(|e| format!("Failed to save cached frame: {e}"))?;
        let frames = vec![SampledFrame {
            index: 0,
            timestamp_secs: 0.0,
            path: out_png.to_string_lossy().to_string(),
        }];
        save_sampled_frames_cache(db, file_key, &algo_key, &sampling_key, &frames)?;
        return Ok(frames);
    }

    if media_type != "video" {
        return Ok(Vec::new());
    }

    let dur = duration_secs.unwrap_or(10.0).max(0.2);
    let timestamps = compute_timestamps(dur, mode);
    let is_tb = height.unwrap_or(0) >= width.unwrap_or(1) && vr_projection.is_some();
    let vr_crop = match vr_projection.as_deref() {
        Some("VR180") if is_tb => Some("crop=iw:ih/2:0:0,"),
        Some("VR180") => Some("crop=iw/2:ih:0:0,"),
        Some("VR360") => Some("v360=e:rectilinear:h_fov=100:v_fov=75:w=640:h=480,"),
        _ => None,
    };

    let mut frames = Vec::new();
    match mode {
        SamplingMode::SceneChange { threshold, max_frames } => {
            let pattern = cache_subdir.join("scene_%04d.png");
            let vf = format!(
                "{}select='gt(scene,{threshold:.3})',scale='min(640,iw)':-2",
                vr_crop.unwrap_or("")
            );
            let mut cmd = Command::new(ffmpeg_path);
            cmd.args([
                "-hide_banner",
                "-y",
                "-protocol_whitelist",
                "file,pipe",
                "-i",
            ])
            .arg(&src)
            .args([
                "-vf",
                &vf,
                "-vsync",
                "vfr",
                "-frames:v",
                &max_frames.to_string(),
            ])
            .arg(&pattern);
            apply_no_window(&mut cmd);
            let _ = cmd.output();
            if let Ok(entries) = std::fs::read_dir(&cache_subdir) {
                let mut found: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.starts_with("scene_") && s.ends_with(".png"))
                            .unwrap_or(false)
                    })
                    .collect();
                found.sort();
                let n = found.len().max(1);
                for (i, p) in found.into_iter().enumerate() {
                    let t = dur * ((i as f64 + 0.5) / n as f64);
                    frames.push(SampledFrame {
                        index: i,
                        timestamp_secs: t,
                        path: p.to_string_lossy().to_string(),
                    });
                }
            }
            if frames.is_empty() {
                // Fallback to percentage sampling when a video has uniform scenes
                return extract_sampled_frames(
                    db,
                    ffmpeg_path,
                    file_key,
                    &SamplingMode::Percentage {
                        count: (*max_frames).min(8),
                    },
                );
            }
        }
        SamplingMode::Keyframes { max_frames } => {
            let pattern = cache_subdir.join("kf_%04d.png");
            let vf = format!(
                "{}select='eq(pict_type,I)',scale='min(640,iw)':-2",
                vr_crop.unwrap_or("")
            );
            let mut cmd = Command::new(ffmpeg_path);
            cmd.args([
                "-hide_banner",
                "-y",
                "-skip_frame",
                "nokey",
                "-protocol_whitelist",
                "file,pipe",
                "-i",
            ])
            .arg(&src)
            .args([
                "-vf",
                &vf,
                "-vsync",
                "vfr",
                "-frames:v",
                &max_frames.to_string(),
            ])
            .arg(&pattern);
            apply_no_window(&mut cmd);
            let _ = cmd.output();
            if let Ok(entries) = std::fs::read_dir(&cache_subdir) {
                let mut found: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.starts_with("kf_") && s.ends_with(".png"))
                            .unwrap_or(false)
                    })
                    .collect();
                found.sort();
                let n = found.len().max(1);
                for (i, p) in found.into_iter().enumerate() {
                    let t = dur * ((i as f64 + 0.5) / n as f64);
                    frames.push(SampledFrame {
                        index: i,
                        timestamp_secs: t,
                        path: p.to_string_lossy().to_string(),
                    });
                }
            }
        }
        _ => {
            for (idx, ts) in timestamps.iter().enumerate() {
                let out_png = cache_subdir.join(format!("f_{}_{idx:04}.png", (ts * 100.0).round() as u64));
                if !out_png.exists() {
                    let vf = format!("{}scale='min(640,iw)':-2", vr_crop.unwrap_or(""));
                    let mut cmd = Command::new(ffmpeg_path);
                    cmd.args([
                        "-hide_banner",
                        "-y",
                        "-protocol_whitelist",
                        "file,pipe",
                        "-ss",
                        &format!("{ts:.3}"),
                        "-i",
                    ])
                    .arg(&src)
                    .args(["-frames:v", "1", "-vf", &vf])
                    .arg(&out_png);
                    apply_no_window(&mut cmd);
                    let _ = cmd.output();
                }
                if out_png.exists() {
                    frames.push(SampledFrame {
                        index: idx,
                        timestamp_secs: *ts,
                        path: out_png.to_string_lossy().to_string(),
                    });
                }
            }
        }
    }

    if let Some(first) = frames.first() {
        let thumb_path = cache_subdir.join("thumb.jpg");
        if !thumb_path.exists() {
            if let Ok(img) = image::open(&first.path) {
                let _ = img.thumbnail(320, 180).save(&thumb_path);
            }
        }
        if thumb_path.exists() {
            let conn = db.lock_conn()?;
            conn.execute(
                "UPDATE media_files SET thumbnail_path = ?1, analysis_level = MAX(analysis_level, 2) WHERE file_key = ?2",
                params![thumb_path.to_string_lossy().to_string(), file_key],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // Adaptive second-pass refinement when initial frames show high motion/variance
    if let SamplingMode::Adaptive { initial_count: _, dense_count } = mode {
        if frames.len() >= 2 && *dense_count > frames.len() {
            let mut high_diff = false;
            let mut prev: Option<Vec<u8>> = None;
            for f in &frames {
                if let Ok(im) = image::open(&f.path) {
                    let g = im.resize_exact(16, 16, image::imageops::FilterType::Triangle).to_luma8().into_raw();
                    if let Some(ref p) = prev {
                        let sad: f64 = g.iter().zip(p.iter()).map(|(&a, &b)| (a as i32 - b as i32).abs() as f64).sum::<f64>() / 256.0;
                        if sad > 22.0 {
                            high_diff = true;
                            break;
                        }
                    }
                    prev = Some(g);
                }
            }
            if high_diff {
                return extract_sampled_frames(
                    db,
                    ffmpeg_path,
                    file_key,
                    &SamplingMode::Percentage { count: *dense_count },
                );
            }
        }
    }

    save_sampled_frames_cache(db, file_key, &algo_key, &sampling_key, &frames)?;
    Ok(frames)
}

fn save_sampled_frames_cache(
    db: &Database,
    file_key: &str,
    algo_key: &str,
    sampling_key: &str,
    frames: &[SampledFrame],
) -> Result<(), String> {
    let json = serde_json::to_string(frames).map_err(|e| e.to_string())?;
    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO analysis_results (
            id, file_key, analysis_type, algo_version, model_version, sampling_config, result_json, created_at
         ) VALUES (?1, ?2, 'sampled_frames', ?3, NULL, ?4, ?5, ?6)
         ON CONFLICT(file_key, analysis_type, algo_version) DO UPDATE SET
            sampling_config = excluded.sampling_config,
            result_json = excluded.result_json,
            created_at = excluded.created_at",
        params![
            Uuid::new_v4().to_string(),
            file_key,
            algo_key,
            sampling_key,
            json,
            Utc::now().timestamp()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn compute_timestamps(dur: f64, mode: &SamplingMode) -> Vec<f64> {
    match mode {
        SamplingMode::FixedInterval { interval_secs } => {
            let step = interval_secs.max(0.5);
            let mut out = Vec::new();
            let mut t = 0.0;
            while t < dur && out.len() < 120 {
                out.push(t);
                t += step;
            }
            if out.is_empty() {
                out.push(0.0);
            }
            out
        }
        SamplingMode::Percentage { count } => {
            let n = (*count).max(1).min(64);
            (0..n)
                .map(|i| {
                    let frac = (i as f64 + 0.5) / (n as f64);
                    (dur * frac).clamp(0.0, (dur - 0.05).max(0.0))
                })
                .collect()
        }
        SamplingMode::Adaptive { initial_count, .. } => {
            let n = (*initial_count).max(2).min(64);
            (0..n)
                .map(|i| {
                    let frac = (i as f64 + 0.5) / (n as f64);
                    (dur * frac).clamp(0.0, (dur - 0.05).max(0.0))
                })
                .collect()
        }
        _ => vec![0.0],
    }
}

/// Detect black frames and frozen segments using shared sampled frames.
pub fn detect_black_and_frozen_frames(
    db: &Database,
    ffmpeg_path: &Path,
    file_key: &str,
) -> Result<BlackFrozenReport, String> {
    let frames = extract_sampled_frames(
        db,
        ffmpeg_path,
        file_key,
        &SamplingMode::Percentage { count: 10 },
    )?;
    let mut black_ts = Vec::new();
    let mut frozen_intervals = Vec::new();
    let mut prev_luma: Option<(f64, Vec<u8>)> = None;

    for f in &frames {
        let Ok(img) = image::open(&f.path) else {
            continue;
        };
        let small = img.resize_exact(32, 32, image::imageops::FilterType::Triangle).to_luma8();
        let bytes = small.into_raw();
        let mean = bytes.iter().map(|&b| b as f64).sum::<f64>() / (bytes.len().max(1) as f64);
        if mean < 18.0 {
            black_ts.push(f.timestamp_secs);
        }
        if let Some((pt, ref pbytes)) = prev_luma {
            let sad: f64 = bytes
                .iter()
                .zip(pbytes.iter())
                .map(|(&a, &b)| (a as i32 - b as i32).abs() as f64)
                .sum::<f64>()
                / (bytes.len().max(1) as f64);
            if sad < 1.5 {
                frozen_intervals.push((pt, f.timestamp_secs));
            }
        }
        prev_luma = Some((f.timestamp_secs, bytes));
    }

    let black_ratio = if frames.is_empty() {
        0.0
    } else {
        black_ts.len() as f64 / frames.len() as f64
    };
    let report = BlackFrozenReport {
        file_key: file_key.to_string(),
        sampled_frames: frames.len(),
        is_mostly_black: black_ratio >= 0.75,
        has_frozen_segments: !frozen_intervals.is_empty(),
        black_frame_timestamps: black_ts,
        frozen_intervals,
        black_ratio,
    };

    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO analysis_results (
            id, file_key, analysis_type, algo_version, result_json, created_at
         ) VALUES (?1, ?2, 'black_frozen', 'bf_v1', ?3, ?4)
         ON CONFLICT(file_key, analysis_type, algo_version) DO UPDATE SET
            result_json = excluded.result_json, created_at = excluded.created_at",
        params![
            Uuid::new_v4().to_string(),
            file_key,
            serde_json::to_string(&report).unwrap_or_default(),
            Utc::now().timestamp()
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(report)
}

pub fn apply_no_window(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// Utility to get image dimensions cleanly
pub fn image_dimensions(img: &DynamicImage) -> (u32, u32) {
    img.dimensions()
}
