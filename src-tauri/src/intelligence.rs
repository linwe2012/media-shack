use crate::analysis::{apply_no_window, extract_sampled_frames, SamplingMode};
use crate::catalog::{query_catalog, to_long_fs_path, CatalogFilter};
use crate::db::Database;
use crate::organize::{assign_tags_bulk, preview_batch_move_or_copy, preview_batch_rename, FileOpPreviewResult, RenameOptions};
use chrono::Utc;
use image::GenericImageView;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

pub const RUST_EMBEDDING_MODEL_VERSION: &str = "rust_visual_concept_v1";
pub const RUST_EMBEDDING_DIMS: usize = 64;
pub const FACE_EMBEDDING_DIMS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrDetectionRecord {
    pub id: String,
    pub file_key: String,
    pub path: String,
    pub raw_text: String,
    pub normalized_text: String,
    pub confidence: f64,
    pub timestamp_secs: f64,
    pub bbox_json: String,
    pub frequency: i32,
    pub classification: String, // watermark | subtitle | title | date | creator | unknown
    pub review_status: String,  // pending | approved | rejected
    pub suggested_creator_tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchHit {
    pub file_key: String,
    pub asset_id: String,
    pub filename: String,
    pub path: String,
    pub score: f64,
    pub best_timestamp_secs: f64,
    pub matching_timestamps: Vec<f64>,
    pub model_version: String,
    pub thumbnail_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceGroupRecord {
    pub id: String,
    pub label: Option<String>,
    pub is_unassigned: bool,
    pub is_excluded: bool,
    pub sample_crop_path: Option<String>,
    pub observation_count: usize,
    pub media_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceObservationRecord {
    pub id: String,
    pub file_key: String,
    pub path: String,
    pub group_id: Option<String>,
    pub timestamp_secs: f64,
    pub bbox_json: String,
    pub crop_path: Option<String>,
    pub confidence: f64,
    pub is_excluded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: String, // ollama | lm_studio | openai_compat
    pub endpoint_url: String,
    pub model_name: String,
    pub api_key: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "ollama".to_string(),
            endpoint_url: "http://127.0.0.1:11434".to_string(),
            model_name: "qwen2.5:7b".to_string(),
            api_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmOrganizationProposal {
    pub summary: String,
    pub suggested_tags: Vec<(String, Vec<String>)>, // (file_key, tags)
    pub rename_preview: Option<FileOpPreviewResult>,
    pub move_preview: Option<FileOpPreviewResult>,
    pub raw_model_output: String,
}

pub fn discover_ml_worker() -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        for _ in 0..5 {
            if let Some(d) = cur {
                dirs.push(d.clone());
                cur = d.parent().map(Path::to_path_buf);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    for base in dirs {
        for rel in ["workers/ml_worker.py", "../workers/ml_worker.py", "../../workers/ml_worker.py"] {
            let p = base.join(rel);
            if p.is_file() {
                return Some(p.canonicalize().unwrap_or(p));
            }
        }
    }
    None
}

fn try_run_ml_worker(action: &str, payload: serde_json::Value) -> Option<serde_json::Value> {
    let worker = discover_ml_worker()?;
    let python = if cfg!(target_os = "windows") { "python" } else { "python3" };
    let mut cmd = Command::new(python);
    cmd.arg(worker).arg(action).arg(payload.to_string());
    apply_no_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

// ============================================================================
// 9. Local Chinese/English OCR & Watermark vs Subtitle Detection
// ============================================================================

pub fn normalize_ocr_text(raw: &str, creator_aliases: &HashMap<String, String>) -> String {
    let cleaned: String = raw
        .trim()
        .replace('\u{3000}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = cleaned.to_ascii_lowercase();
    for (alias, canonical) in creator_aliases {
        if lower == alias.to_ascii_lowercase() || lower.contains(&alias.to_ascii_lowercase()) {
            return canonical.clone();
        }
    }
    cleaned
}

pub fn get_creator_aliases(db: &Database) -> Result<HashMap<String, String>, String> {
    Ok(db
        .get_setting::<HashMap<String, String>>("creator_aliases")?
        .unwrap_or_default())
}

pub fn save_creator_alias(db: &Database, alias: &str, canonical: &str) -> Result<(), String> {
    let mut map = get_creator_aliases(db)?;
    map.insert(alias.trim().to_string(), canonical.trim().to_string());
    db.set_setting("creator_aliases", &map)
}

pub fn run_ocr_and_watermark_analysis(
    db: &Database,
    ffmpeg_path: &Path,
    file_key: &str,
) -> Result<Vec<OcrDetectionRecord>, String> {
    let frames = extract_sampled_frames(
        db,
        ffmpeg_path,
        file_key,
        &SamplingMode::Percentage { count: 8 },
    )?;
    let (file_path, media_type): (String, String) = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT fl.path, ma.media_type FROM media_files mf
             JOIN media_assets ma ON ma.asset_id = mf.asset_id
             JOIN file_locations fl ON fl.file_key = mf.file_key AND fl.is_current = 1
             WHERE mf.file_key = ?1 LIMIT 1",
            params![file_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    };

    let aliases = get_creator_aliases(db)?;
    let mut raw_hits: Vec<(String, f64, serde_json::Value, f64, String)> = Vec::new();

    // 1. Optional PaddleOCR/RapidOCR worker over sampled frames (Chinese + English)
    let frame_paths: Vec<String> = frames.iter().map(|f| f.path.clone()).collect();
    if let Some(res) = try_run_ml_worker("ocr", serde_json::json!({ "frames": frame_paths })) {
        if let Some(arr) = res.get("detections").and_then(|v| v.as_array()) {
            for det in arr {
                let text = det.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                let conf = det.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let frame_idx = det.get("frame_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let bbox = det.get("bbox").cloned().unwrap_or_else(|| serde_json::json!({}));
                let ts = frames.get(frame_idx).map(|f| f.timestamp_secs).unwrap_or(0.0);
                if !text.is_empty() && conf >= 0.45 {
                    raw_hits.push((text.to_string(), conf, bbox, ts, "ocr".to_string()));
                }
            }
        }
    }

    // 2. Fallback: extract embedded text/subtitle lines via FFmpeg if present
    if media_type == "video" {
        if let Some(subs) = extract_embedded_subtitle_samples(ffmpeg_path, Path::new(&file_path)) {
            for (ts, line) in subs {
                raw_hits.push((
                    line,
                    0.92,
                    serde_json::json!({"region": "bottom_subtitle"}),
                    ts,
                    "subtitle".to_string(),
                ));
            }
        }
    }

    // 3. Pure-Rust spatial-temporal persistent watermark / overlay detector across sampled frames
    if frames.len() >= 3 {
        if let Some((region_label, bbox, persistence)) = detect_persistent_corner_overlay(&frames) {
            raw_hits.push((
                format!("[persistent_overlay:{region_label}]"),
                persistence,
                bbox,
                frames[0].timestamp_secs,
                "watermark_region".to_string(),
            ));
        }
    }

    // Aggregate frequency & spatial consistency across frames
    let mut grouped: HashMap<String, (String, f64, f64, serde_json::Value, i32, bool)> = HashMap::new();
    for (raw, conf, bbox, ts, hint) in raw_hits {
        let norm = normalize_ocr_text(&raw, &aliases);
        if norm.is_empty() {
            continue;
        }
        let is_sub_hint = hint == "subtitle";
        let entry = grouped.entry(norm.clone()).or_insert((raw.clone(), conf, ts, bbox, 0, is_sub_hint));
        entry.4 += 1;
        if conf > entry.1 {
            entry.1 = conf;
        }
    }

    let frame_total = frames.len().max(1) as f64;
    let now = Utc::now().timestamp();
    let mut out = Vec::new();
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM ocr_detections WHERE file_key = ?1 AND review_status = 'pending'", params![file_key])
        .map_err(|e| e.to_string())?;

    for (norm, (raw, conf, ts, bbox, freq, is_sub_hint)) in grouped {
        let ratio = (freq as f64) / frame_total;
        let classification = if is_sub_hint || (freq == 1 && frames.len() > 3 && !raw.starts_with("[persistent_overlay")) {
            "subtitle"
        } else if raw.starts_with("[persistent_overlay") || (ratio >= 0.45 && freq >= 2) {
            if norm.starts_with('@') || norm.contains("http") || norm.contains(".com") {
                "watermark"
            } else {
                "watermark"
            }
        } else if norm.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ':' || c == '/') && norm.len() >= 6 {
            "date"
        } else {
            "unknown"
        };

        // Never fabricate creator names when no reliable evidence exists
        let suggested_creator_tag = if classification == "watermark"
            && !raw.starts_with("[persistent_overlay")
            && conf >= 0.65
            && norm.len() >= 2
            && norm.len() <= 40
        {
            Some(format!("creator:{}", norm.trim_start_matches('@')))
        } else {
            None
        };

        let id = Uuid::new_v4().to_string();
        let bbox_s = bbox.to_string();
        tx.execute(
            "INSERT INTO ocr_detections (
                id, file_key, raw_text, normalized_text, confidence, timestamp_secs,
                bbox_json, frequency, classification, review_status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
            params![id, file_key, raw, norm, conf, ts, bbox_s, freq, classification, now],
        )
        .map_err(|e| e.to_string())?;

        out.push(OcrDetectionRecord {
            id,
            file_key: file_key.to_string(),
            path: file_path.clone(),
            raw_text: raw,
            normalized_text: norm,
            confidence: conf,
            timestamp_secs: ts,
            bbox_json: bbox_s,
            frequency: freq,
            classification: classification.to_string(),
            review_status: "pending".to_string(),
            suggested_creator_tag,
        });
    }

    tx.execute(
        "UPDATE media_files SET analysis_level = MAX(analysis_level, 4) WHERE file_key = ?1",
        params![file_key],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(out)
}

fn extract_embedded_subtitle_samples(ffmpeg_path: &Path, path: &Path) -> Option<Vec<(f64, String)>> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-hide_banner", "-v", "error", "-i"])
        .arg(to_long_fs_path(path))
        .args(["-map", "0:s:0", "-f", "srt", "-"]);
    apply_no_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut samples = Vec::new();
    let mut cur_ts = 0.0f64;
    for line in text.lines() {
        let l = line.trim();
        if l.contains("-->") {
            if let Some(start) = l.split("-->").next() {
                cur_ts = parse_srt_ts(start.trim());
            }
        } else if !l.is_empty() && !l.chars().all(|c| c.is_ascii_digit()) {
            samples.push((cur_ts, l.to_string()));
            if samples.len() >= 15 {
                break;
            }
        }
    }
    if samples.is_empty() { None } else { Some(samples) }
}

fn parse_srt_ts(s: &str) -> f64 {
    let clean = s.replace(',', ".");
    let parts: Vec<&str> = clean.split(':').collect();
    if parts.len() == 3 {
        let h: f64 = parts[0].parse().unwrap_or(0.0);
        let m: f64 = parts[1].parse().unwrap_or(0.0);
        let sec: f64 = parts[2].parse().unwrap_or(0.0);
        h * 3600.0 + m * 60.0 + sec
    } else {
        0.0
    }
}

/// Detect high-contrast static overlay regions (persistent logo/text watermark) across corner quadrants.
fn detect_persistent_corner_overlay(frames: &[crate::analysis::SampledFrame]) -> Option<(String, serde_json::Value, f64)> {
    let mut imgs = Vec::new();
    for f in frames.iter().take(6) {
        if let Ok(img) = image::open(&f.path) {
            imgs.push(img.resize_exact(120, 68, image::imageops::FilterType::Triangle).to_luma8());
        }
    }
    if imgs.len() < 3 {
        return None;
    }
    let corners = [
        ("top_left", 0u32, 0u32, 40u32, 18u32),
        ("top_right", 80, 0, 120, 18),
        ("bottom_left", 0, 50, 40, 68),
        ("bottom_right", 80, 50, 120, 68),
    ];

    let mut best: Option<(String, serde_json::Value, f64)> = None;
    for (name, x0, y0, x1, y1) in corners {
        let mut static_edges = 0usize;
        let mut total_px = 0usize;
        for y in y0..y1 {
            for x in x0..x1.saturating_sub(1) {
                total_px += 1;
                let mut min_v = 255u8;
                let mut max_v = 0u8;
                let mut bright_edge = false;
                for im in &imgs {
                    let p = im.get_pixel(x, y)[0];
                    let p2 = im.get_pixel(x + 1, y)[0];
                    min_v = min_v.min(p);
                    max_v = max_v.max(p);
                    if (p as i32 - p2 as i32).abs() > 35 {
                        bright_edge = true;
                    }
                }
                if bright_edge && (max_v - min_v) < 12 {
                    static_edges += 1;
                }
            }
        }
        let ratio = static_edges as f64 / total_px.max(1) as f64;
        if ratio >= 0.04 {
            let conf = (0.55 + ratio * 2.0).min(0.95);
            if best.as_ref().map(|b| conf > b.2).unwrap_or(true) {
                best = Some((
                    name.to_string(),
                    serde_json::json!({ "quadrant": name, "x0": x0, "y0": y0, "x1": x1, "y1": y1 }),
                    conf,
                ));
            }
        }
    }
    best
}

pub fn list_ocr_detections(db: &Database, review_status: Option<&str>) -> Result<Vec<OcrDetectionRecord>, String> {
    let conn = db.lock_conn()?;
    let sql = if review_status.is_some() {
        "SELECT o.id, o.file_key, COALESCE(fl.path, ''), o.raw_text, o.normalized_text,
                o.confidence, o.timestamp_secs, o.bbox_json, o.frequency, o.classification, o.review_status
         FROM ocr_detections o
         LEFT JOIN file_locations fl ON fl.file_key = o.file_key AND fl.is_current = 1
         WHERE o.review_status = ?1
         ORDER BY o.frequency DESC, o.confidence DESC LIMIT 500"
    } else {
        "SELECT o.id, o.file_key, COALESCE(fl.path, ''), o.raw_text, o.normalized_text,
                o.confidence, o.timestamp_secs, o.bbox_json, o.frequency, o.classification, o.review_status
         FROM ocr_detections o
         LEFT JOIN file_locations fl ON fl.file_key = o.file_key AND fl.is_current = 1
         ORDER BY o.frequency DESC, o.confidence DESC LIMIT 500"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mapper = |r: &rusqlite::Row<'_>| -> rusqlite::Result<OcrDetectionRecord> {
        let norm: String = r.get(4)?;
        let cls: String = r.get(9)?;
        let conf: f64 = r.get(5)?;
        let raw: String = r.get(3)?;
        let suggested = if cls == "watermark" && !raw.starts_with("[persistent_overlay") && conf >= 0.65 {
            Some(format!("creator:{}", norm.trim_start_matches('@')))
        } else {
            None
        };
        Ok(OcrDetectionRecord {
            id: r.get(0)?,
            file_key: r.get(1)?,
            path: r.get(2)?,
            raw_text: raw,
            normalized_text: norm,
            confidence: conf,
            timestamp_secs: r.get(6)?,
            bbox_json: r.get(7)?,
            frequency: r.get(8)?,
            classification: cls,
            review_status: r.get(10)?,
            suggested_creator_tag: suggested,
        })
    };
    let rows = if let Some(rs) = review_status {
        stmt.query_map(params![rs], mapper).map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map([], mapper).map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
    };
    rows.map_err(|e| e.to_string())
}

pub fn review_ocr_detections_batch(
    db: &Database,
    detection_ids: &[String],
    approve: bool,
    tag_prefix: Option<&str>,
) -> Result<usize, String> {
    let status = if approve { "approved" } else { "rejected" };
    let mut updated = 0usize;
    let mut tag_ops: Vec<(String, String, f64)> = Vec::new();
    {
        let conn = db.lock_conn()?;
        for id in detection_ids {
            if let Ok((fkey, norm, conf)) = conn.query_row(
                "SELECT file_key, normalized_text, confidence FROM ocr_detections WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)),
            ) {
                conn.execute(
                    "UPDATE ocr_detections SET review_status = ?1 WHERE id = ?2",
                    params![status, id],
                )
                .map_err(|e| e.to_string())?;
                updated += 1;
                if approve && !norm.starts_with('[') {
                    if let Ok(asset_id) = conn.query_row(
                        "SELECT asset_id FROM media_files WHERE file_key = ?1",
                        params![fkey],
                        |r| r.get::<_, String>(0),
                    ) {
                        let tag_name = match tag_prefix {
                            Some(pfx) if !pfx.is_empty() => format!("{pfx}:{}", norm.trim_start_matches('@')),
                            _ => format!("creator:{}", norm.trim_start_matches('@')),
                        };
                        tag_ops.push((asset_id, tag_name, conf));
                    }
                }
            }
        }
    }
    for (aid, tname, conf) in tag_ops {
        let _ = assign_tags_bulk(db, &[aid], &[tname], "ai_suggested", conf);
    }
    Ok(updated)
}

// ============================================================================
// 11. Local Semantic Media Search (Visual Embeddings + Exact Cosine Retrieval)
// ============================================================================

pub fn index_semantic_embeddings(
    db: &Database,
    ffmpeg_path: &Path,
    file_key: &str,
) -> Result<usize, String> {
    let frames = extract_sampled_frames(
        db,
        ffmpeg_path,
        file_key,
        &SamplingMode::Percentage { count: 8 },
    )?;
    if frames.is_empty() {
        return Ok(0);
    }

    // Try optional CLIP/SigLIP ONNX worker first; otherwise use pure-Rust visual concept descriptor
    let frame_paths: Vec<String> = frames.iter().map(|f| f.path.clone()).collect();
    let (model_version, dims, vectors): (String, usize, Vec<(f64, Vec<f32>)>) =
        if let Some(res) = try_run_ml_worker("embed_images", serde_json::json!({ "frames": frame_paths })) {
            let mv = res
                .get("model_version")
                .and_then(|v| v.as_str())
                .unwrap_or("clip_onnx_v1")
                .to_string();
            let arr = res.get("embeddings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut out = Vec::new();
            let mut d = 0usize;
            for (i, item) in arr.into_iter().enumerate() {
                let vec: Vec<f32> = item
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                    .unwrap_or_default();
                if !vec.is_empty() {
                    d = vec.len();
                    let ts = frames.get(i).map(|f| f.timestamp_secs).unwrap_or(0.0);
                    out.push((ts, l2_normalize(vec)));
                }
            }
            if !out.is_empty() {
                (mv, d, out)
            } else {
                compute_rust_frame_embeddings(&frames)
            }
        } else {
            compute_rust_frame_embeddings(&frames)
        };

    let now = Utc::now().timestamp();
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM media_embeddings WHERE file_key = ?1 AND model_version = ?2",
        params![file_key, model_version],
    )
    .map_err(|e| e.to_string())?;

    for (ts, vec) in &vectors {
        let blob = f32_slice_to_bytes(vec);
        tx.execute(
            "INSERT INTO media_embeddings (id, file_key, timestamp_secs, model_version, dimensions, vector_blob, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                Uuid::new_v4().to_string(),
                file_key,
                ts,
                model_version,
                dims as i64,
                blob,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE media_files SET analysis_level = MAX(analysis_level, 4) WHERE file_key = ?1",
        params![file_key],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(vectors.len())
}

fn compute_rust_frame_embeddings(frames: &[crate::analysis::SampledFrame]) -> (String, usize, Vec<(f64, Vec<f32>)>) {
    let mut out = Vec::new();
    for f in frames {
        if let Ok(img) = image::open(&f.path) {
            let v = encode_visual_concept_vector(&img);
            out.push((f.timestamp_secs, v));
        }
    }
    (RUST_EMBEDDING_MODEL_VERSION.to_string(), RUST_EMBEDDING_DIMS, out)
}

/// Pure-Rust 64-dim visual concept vector (color regions, brightness, blue/ocean, white/snow, dark/night, green/nature, screen-text edges).
fn encode_visual_concept_vector(img: &image::DynamicImage) -> Vec<f32> {
    let rgb = img.resize_exact(32, 32, image::imageops::FilterType::Triangle).to_rgb8();
    let mut v = vec![0.0f32; RUST_EMBEDDING_DIMS];
    let mut n = 0.0f32;

    for y in 0..32u32 {
        for x in 0..32u32 {
            let p = rgb.get_pixel(x, y).0;
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let sat = r.max(g).max(b) - r.min(g).min(b);

            // Spatial 4x4 grid color/luma (indices 0..48)
            let gx = (x / 8) as usize;
            let gy = (y / 8) as usize;
            let cell = (gy * 4 + gx) * 3;
            v[cell] += r;
            v[cell + 1] += g;
            v[cell + 2] += b;

            // Semantic visual concept bins (48..64)
            if b > r + 0.10 && b > g + 0.03 { v[48] += 1.0; } // ocean / sky blue
            if luma > 0.78 && sat < 0.18 { v[49] += 1.0; }     // snow / white / bright UI
            if luma < 0.20 { v[50] += 1.0; }                   // night / dark
            if g > r + 0.08 && g > b + 0.05 { v[51] += 1.0; }  // forest / nature green
            if r > g + 0.12 && r > b + 0.12 { v[52] += 1.0; }  // warm / sunset / skin
            if sat < 0.08 && luma > 0.12 && luma < 0.88 { v[53] += 1.0; } // neutral gray / code editor

            if x + 1 < 32 {
                let pr = rgb.get_pixel(x + 1, y).0;
                let edge = ((p[0] as i32 - pr[0] as i32).abs()
                    + (p[1] as i32 - pr[1] as i32).abs()
                    + (p[2] as i32 - pr[2] as i32).abs()) as f32 / 765.0;
                if edge > 0.20 {
                    v[54] += 1.0; // sharp edges (screen recordings, text, UI)
                }
                if y >= 22 && edge > 0.25 && luma > 0.65 {
                    v[55] += 1.0; // bottom subtitle-like bright text region
                }
            }
            n += 1.0;
        }
    }
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    l2_normalize(v)
}

fn encode_query_to_rust_vector(query: &str) -> Vec<f32> {
    let q = query.to_ascii_lowercase();
    let mut v = vec![0.05f32; RUST_EMBEDDING_DIMS];
    if q.contains("ocean") || q.contains("sea") || q.contains("water") || q.contains("sky") || q.contains("海") {
        v[48] += 3.0;
        // Blue tint in lower/upper bands
        for cell in (0..48).step_by(3) {
            v[cell + 2] += 0.8;
        }
    }
    if q.contains("snow") || q.contains("ski") || q.contains("mountain") || q.contains("winter") || q.contains("雪") {
        v[49] += 3.5;
        for cell in (0..48).step_by(3) {
            v[cell] += 0.6;
            v[cell + 1] += 0.6;
            v[cell + 2] += 0.65;
        }
    }
    if q.contains("night") || q.contains("dark") || q.contains("evening") || q.contains("夜") {
        v[50] += 3.5;
    }
    if q.contains("forest") || q.contains("tree") || q.contains("grass") || q.contains("nature") || q.contains("山") {
        v[51] += 3.0;
        for cell in (0..48).step_by(3) {
            v[cell + 1] += 0.7;
        }
    }
    if q.contains("sunset") || q.contains("fire") || q.contains("cat") || q.contains("warm") || q.contains("猫") {
        v[52] += 2.5;
    }
    if q.contains("screen") || q.contains("programming") || q.contains("code") || q.contains("terminal") {
        v[53] += 2.0;
        v[54] += 3.0;
    }
    if q.contains("subtitle") || q.contains("caption") || q.contains("chinese") || q.contains("字幕") {
        v[55] += 3.5;
    }
    l2_normalize(v)
}

pub fn semantic_media_search(
    db: &Database,
    query: &str,
    filter: Option<&CatalogFilter>,
    limit: usize,
) -> Result<Vec<SemanticSearchHit>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    // Determine active embedding model version in DB (prefer CLIP ONNX if indexed, else rust_visual_concept_v1)
    let active_model: Option<(String, usize)> = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT model_version, dimensions FROM media_embeddings
             ORDER BY CASE WHEN model_version != ?1 THEN 0 ELSE 1 END, created_at DESC LIMIT 1",
            params![RUST_EMBEDDING_MODEL_VERSION],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)),
        )
        .optional()
        .map_err(|e| e.to_string())?
    };

    let Some((model_version, dims)) = active_model else {
        return Ok(Vec::new());
    };

    let query_vec = if model_version != RUST_EMBEDDING_MODEL_VERSION {
        if let Some(res) = try_run_ml_worker("embed_text", serde_json::json!({ "query": trimmed })) {
            let vec: Vec<f32> = res
                .get("embedding")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            if vec.len() == dims {
                l2_normalize(vec)
            } else {
                return Err("Text embedding dimensions do not match stored model_version.".to_string());
            }
        } else {
            return Err(format!("Embedding model '{model_version}' requires optional ML worker for text queries."));
        }
    } else {
        encode_query_to_rust_vector(trimmed)
    };

    let allowed_keys: Option<HashSet<String>> = if let Some(f) = filter {
        let p = query_catalog(db, f)?;
        Some(p.items.into_iter().map(|e| e.file_key).collect())
    } else {
        None
    };

    let catalog_map: HashMap<String, crate::catalog::CatalogEntry> = query_catalog(
        db,
        &CatalogFilter {
            limit: Some(250_000),
            ..Default::default()
        },
    )?
    .items
    .into_iter()
    .map(|e| (e.file_key.clone(), e))
    .collect();

    let conn = db.lock_conn()?;
    // Strict model_version & dimensions filter so incompatible embeddings are NEVER combined
    let mut stmt = conn
        .prepare(
            "SELECT file_key, timestamp_secs, vector_blob
             FROM media_embeddings
             WHERE model_version = ?1 AND dimensions = ?2",
        )
        .map_err(|e| e.to_string())?;

    let mut per_file: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    let rows = stmt
        .query_map(params![model_version, dims as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows.flatten() {
        let (fkey, ts, blob) = row;
        if let Some(ref allow) = allowed_keys {
            if !allow.contains(&fkey) {
                continue;
            }
        }
        let vec = bytes_to_f32_slice(&blob);
        if vec.len() != dims {
            continue;
        }
        let mut sim = dot(&query_vec, &vec) as f64;
        // Boost exact metadata/OCR/tag text alignment gently when visual + text agree
        if let Some(entry) = catalog_map.get(&fkey) {
            let ql = trimmed.to_ascii_lowercase();
            let has_text_hit = entry.filename.to_ascii_lowercase().contains(&ql)
                || entry.tags.iter().any(|t| t.name.to_ascii_lowercase().contains(&ql));
            if has_text_hit {
                sim = (sim + 0.25).min(1.0);
            }
        }
        per_file.entry(fkey).or_default().push((sim, ts));
    }

    let mut hits: Vec<SemanticSearchHit> = Vec::new();
    for (fkey, mut ts_scores) in per_file {
        let Some(entry) = catalog_map.get(&fkey) else {
            continue;
        };
        ts_scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let best = ts_scores[0];
        let matching_ts: Vec<f64> = ts_scores
            .iter()
            .filter(|(s, _)| *s >= best.0 * 0.85)
            .take(5)
            .map(|(_, t)| *t)
            .collect();
        hits.push(SemanticSearchHit {
            file_key: fkey,
            asset_id: entry.asset_id.clone(),
            filename: entry.filename.clone(),
            path: entry.path.clone(),
            score: best.0,
            best_timestamp_secs: best.1,
            matching_timestamps: matching_ts,
            model_version: model_version.clone(),
            thumbnail_path: entry.thumbnail_path.clone(),
        });
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit.clamp(1, 200));
    Ok(hits)
}

// ============================================================================
// 10. Optional Local Anonymous Face Grouping (Opt-in + Biometric Purge)
// ============================================================================

pub fn is_face_grouping_enabled(db: &Database) -> Result<bool, String> {
    Ok(db.get_setting::<bool>("face_grouping_enabled")?.unwrap_or(false))
}

pub fn set_face_grouping_enabled(db: &Database, enabled: bool) -> Result<(), String> {
    db.set_setting("face_grouping_enabled", &enabled)
}

pub fn run_face_grouping_for_files(
    db: &Database,
    ffmpeg_path: &Path,
    file_keys: &[String],
) -> Result<usize, String> {
    if !is_face_grouping_enabled(db)? {
        return Err("Face grouping is opt-in and currently disabled in Settings.".to_string());
    }

    let crops_dir = db.app_data_dir.join("analysis_cache").join("face_crops");
    let _ = std::fs::create_dir_all(&crops_dir);
    let mut new_observations = 0usize;
    let now = Utc::now().timestamp();

    for fkey in file_keys {
        let frames = extract_sampled_frames(
            db,
            ffmpeg_path,
            fkey,
            &SamplingMode::Percentage { count: 6 },
        )?;
        let frame_paths: Vec<String> = frames.iter().map(|f| f.path.clone()).collect();

        // Optional ML worker face detector; fallback to local center-region portrait/skin-cluster detector
        let mut detections: Vec<(f64, serde_json::Value, Vec<f32>, f64, Option<String>)> = Vec::new();
        if let Some(res) = try_run_ml_worker("detect_faces", serde_json::json!({ "frames": frame_paths, "crops_dir": crops_dir })) {
            if let Some(arr) = res.get("faces").and_then(|v| v.as_array()) {
                for face in arr {
                    let frame_idx = face.get("frame_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let ts = frames.get(frame_idx).map(|f| f.timestamp_secs).unwrap_or(0.0);
                    let bbox = face.get("bbox").cloned().unwrap_or_else(|| serde_json::json!({}));
                    let conf = face.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.8);
                    let crop = face.get("crop_path").and_then(|v| v.as_str()).map(str::to_string);
                    let emb: Vec<f32> = face
                        .get("embedding")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                        .unwrap_or_default();
                    if !emb.is_empty() && conf >= 0.60 {
                        detections.push((ts, bbox, l2_normalize(emb), conf, crop));
                    }
                }
            }
        } else {
            // Conservative pure-Rust skin-tone + upper-center face candidate detector
            for f in &frames {
                if let Ok(img) = image::open(&f.path) {
                    if let Some((bbox, emb, conf, crop_img)) = detect_rust_face_candidate(&img) {
                        let crop_path = crops_dir.join(format!("face_{}_{}.jpg", fkey, Uuid::new_v4()));
                        let _ = crop_img.save(&crop_path);
                        detections.push((
                            f.timestamp_secs,
                            bbox,
                            emb,
                            conf,
                            Some(crop_path.to_string_lossy().to_string()),
                        ));
                    }
                }
            }
        }

        for (ts, bbox, emb, conf, crop_path) in detections {
            let group_id = assign_face_to_group(db, &emb, conf, crop_path.as_deref(), now)?;
            let conn = db.lock_conn()?;
            conn.execute(
                "INSERT INTO face_observations (
                    id, file_key, group_id, timestamp_secs, bbox_json, crop_path,
                    embedding_blob, confidence, is_excluded, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
                params![
                    Uuid::new_v4().to_string(),
                    fkey,
                    group_id,
                    ts,
                    bbox.to_string(),
                    crop_path,
                    f32_slice_to_bytes(&emb),
                    conf,
                    now
                ],
            )
            .map_err(|e| e.to_string())?;
            new_observations += 1;
        }
    }

    Ok(new_observations)
}

fn detect_rust_face_candidate(
    img: &image::DynamicImage,
) -> Option<(serde_json::Value, Vec<f32>, f64, image::DynamicImage)> {
    let (w, h) = img.dimensions();
    if w < 32 || h < 32 {
        return None;
    }
    let rgb = img.to_rgb8();
    let mut skin_x = 0u64;
    let mut skin_y = 0u64;
    let mut skin_cnt = 0u64;
    let total = (w as u64) * (h as u64);

    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let p = rgb.get_pixel(x, y).0;
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            // Standard RGB skin-chrominance bounds
            if r > 95 && g > 40 && b > 20 && (r - g).abs() > 15 && r > g && r > b {
                skin_x += x as u64;
                skin_y += y as u64;
                skin_cnt += 1;
            }
        }
    }
    let sampled_total = total / 4;
    let ratio = skin_cnt as f64 / sampled_total.max(1) as f64;
    if !(0.03..=0.55).contains(&ratio) {
        return None;
    }
    let cx = (skin_x / skin_cnt.max(1)) as u32;
    let cy = (skin_y / skin_cnt.max(1)) as u32;
    let box_sz = (w.min(h) / 3).max(24);
    let x0 = cx.saturating_sub(box_sz / 2).min(w.saturating_sub(box_sz));
    let y0 = cy.saturating_sub(box_sz / 2).min(h.saturating_sub(box_sz));
    let crop = img.crop_imm(x0, y0, box_sz, box_sz);

    let small = crop
        .resize_exact(8, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let emb: Vec<f32> = small.into_raw().into_iter().map(|b| b as f32 / 255.0).collect();
    let conf = (0.55 + ratio).min(0.85);
    Some((
        serde_json::json!({"x": x0, "y": y0, "w": box_sz, "h": box_sz}),
        l2_normalize(emb),
        conf,
        crop,
    ))
}

fn assign_face_to_group(
    db: &Database,
    emb: &[f32],
    conf: f64,
    crop_path: Option<&str>,
    now: i64,
) -> Result<String, String> {
    let conn = db.lock_conn()?;
    // Conservative threshold: uncertain observations go to Unassigned group
    if conf < 0.65 {
        return ensure_unassigned_face_group(&conn, crop_path, now);
    }

    let mut stmt = conn
        .prepare("SELECT id, centroid_blob FROM face_groups WHERE is_unassigned = 0 AND is_excluded = 0 AND centroid_blob IS NOT NULL")
        .map_err(|e| e.to_string())?;
    let groups: Vec<(String, Vec<u8>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    let mut best_gid: Option<String> = None;
    let mut best_sim = 0.0f32;
    for (gid, blob) in groups {
        let centroid = bytes_to_f32_slice(&blob);
        if centroid.len() == emb.len() {
            let sim = dot(emb, &centroid);
            if sim > best_sim {
                best_sim = sim;
                best_gid = Some(gid);
            }
        }
    }

    if best_sim >= 0.86 {
        if let Some(gid) = best_gid {
            return Ok(gid);
        }
    }

    // Create new anonymous face group
    let gid = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO face_groups (id, label, is_unassigned, is_excluded, sample_crop_path, centroid_blob, created_at, updated_at)
         VALUES (?1, NULL, 0, 0, ?2, ?3, ?4, ?4)",
        params![gid, crop_path, f32_slice_to_bytes(emb), now],
    )
    .map_err(|e| e.to_string())?;
    Ok(gid)
}

fn ensure_unassigned_face_group(
    conn: &rusqlite::Connection,
    crop_path: Option<&str>,
    now: i64,
) -> Result<String, String> {
    if let Ok(gid) = conn.query_row(
        "SELECT id FROM face_groups WHERE is_unassigned = 1 LIMIT 1",
        [],
        |r| r.get::<_, String>(0),
    ) {
        return Ok(gid);
    }
    let gid = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO face_groups (id, label, is_unassigned, is_excluded, sample_crop_path, centroid_blob, created_at, updated_at)
         VALUES (?1, 'Unassigned', 1, 0, ?2, NULL, ?3, ?3)",
        params![gid, crop_path, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(gid)
}

pub fn list_face_groups(db: &Database) -> Result<Vec<FaceGroupRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT fg.id, fg.label, fg.is_unassigned, fg.is_excluded, fg.sample_crop_path,
                    COUNT(fo.id) AS obs_cnt, COUNT(DISTINCT fo.file_key) AS media_cnt
             FROM face_groups fg
             LEFT JOIN face_observations fo ON fo.group_id = fg.id AND fo.is_excluded = 0
             WHERE fg.is_excluded = 0
             GROUP BY fg.id
             ORDER BY fg.is_unassigned ASC, obs_cnt DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(FaceGroupRecord {
                id: r.get(0)?,
                label: r.get(1)?,
                is_unassigned: r.get::<_, i64>(2)? != 0,
                is_excluded: r.get::<_, i64>(3)? != 0,
                sample_crop_path: r.get(4)?,
                observation_count: r.get::<_, i64>(5)?.max(0) as usize,
                media_count: r.get::<_, i64>(6)?.max(0) as usize,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn list_face_observations(db: &Database, group_id: &str) -> Result<Vec<FaceObservationRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT fo.id, fo.file_key, COALESCE(fl.path, ''), fo.group_id, fo.timestamp_secs,
                    fo.bbox_json, fo.crop_path, fo.confidence, fo.is_excluded
             FROM face_observations fo
             LEFT JOIN file_locations fl ON fl.file_key = fo.file_key AND fl.is_current = 1
             WHERE fo.group_id = ?1 AND fo.is_excluded = 0
             ORDER BY fo.timestamp_secs ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![group_id], |r| {
            Ok(FaceObservationRecord {
                id: r.get(0)?,
                file_key: r.get(1)?,
                path: r.get(2)?,
                group_id: r.get(3)?,
                timestamp_secs: r.get(4)?,
                bbox_json: r.get(5)?,
                crop_path: r.get(6)?,
                confidence: r.get(7)?,
                is_excluded: r.get::<_, i64>(8)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn label_face_group(db: &Database, group_id: &str, label: &str) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "UPDATE face_groups SET label = ?1, is_unassigned = 0, updated_at = ?2 WHERE id = ?3",
        params![label.trim(), Utc::now().timestamp(), group_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn merge_face_groups(db: &Database, source_group_id: &str, target_group_id: &str) -> Result<(), String> {
    if source_group_id == target_group_id {
        return Err("Source and target groups must be different.".to_string());
    }
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE face_observations SET group_id = ?1 WHERE group_id = ?2",
        params![target_group_id, source_group_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM face_groups WHERE id = ?1", params![source_group_id])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn split_face_observation(db: &Database, observation_id: &str) -> Result<String, String> {
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let (crop, emb): (Option<String>, Vec<u8>) = tx
        .query_row(
            "SELECT crop_path, embedding_blob FROM face_observations WHERE id = ?1",
            params![observation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("Observation not found: {e}"))?;
    let new_gid = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    tx.execute(
        "INSERT INTO face_groups (id, label, is_unassigned, is_excluded, sample_crop_path, centroid_blob, created_at, updated_at)
         VALUES (?1, NULL, 0, 0, ?2, ?3, ?4, ?4)",
        params![new_gid, crop, emb, now],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE face_observations SET group_id = ?1 WHERE id = ?2",
        params![new_gid, observation_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(new_gid)
}

pub fn exclude_face_observation(db: &Database, observation_id: &str) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "UPDATE face_observations SET is_excluded = 1 WHERE id = ?1",
        params![observation_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Permanently delete all locally stored face crops, embeddings, observations, and groups.
pub fn purge_all_biometric_data(db: &Database) -> Result<usize, String> {
    let crops_dir = db.app_data_dir.join("analysis_cache").join("face_crops");
    if crops_dir.exists() {
        let _ = std::fs::remove_dir_all(&crops_dir);
    }
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let deleted = tx
        .execute("DELETE FROM face_observations", [])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM face_groups", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(deleted)
}

// ============================================================================
// 12. Optional Local Language Model Integration (Ollama / LM Studio / OpenAI-compat)
// ============================================================================

pub fn get_llm_config(db: &Database) -> Result<LlmConfig, String> {
    Ok(db.get_setting::<LlmConfig>("llm_config")?.unwrap_or_default())
}

pub fn save_llm_config(db: &Database, cfg: &LlmConfig) -> Result<(), String> {
    db.set_setting("llm_config", cfg)
}

pub fn generate_llm_organization_proposal(
    db: &Database,
    instruction: &str,
    file_keys: &[String],
) -> Result<LlmOrganizationProposal, String> {
    let cfg = get_llm_config(db)?;
    if !cfg.enabled {
        return Err("Local LLM integration is disabled. Enable it in Settings first.".to_string());
    }
    let selected: Vec<_> = crate::catalog::load_entries_by_file_keys(db, file_keys)?
        .into_iter()
        .take(40)
        .collect();
    if selected.is_empty() {
        return Err("No catalog items selected for organization proposal.".to_string());
    }

    let context_items: Vec<serde_json::Value> = selected
        .iter()
        .map(|e| {
            serde_json::json!({
                "file_key": e.file_key,
                "filename": e.filename,
                "title": e.title,
                "creator": e.creator,
                "codec": e.video_codec,
                "width": e.width,
                "height": e.height,
                "tags": e.tags.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();

    let prompt = format!(
        "You are a local media archival assistant. Respond ONLY with valid JSON matching:\n\
         {{\"summary\": \"...\", \"rename_template\": \"optional template using {{creator}},{{date}},{{title}},{{resolution}},{{index}}.{{extension}}\", \
         \"folder_template\": \"optional e.g. Media/{{creator}}/{{year}}/{{title}}.{{extension}}\", \
         \"suggested_tags\": [{{\"file_key\": \"...\", \"tags\": [\"...\"]}}]}}\n\
         User instruction: {instruction}\n\
         Media items: {}",
        serde_json::to_string(&context_items).unwrap_or_default()
    );

    let raw_text = call_local_llm_endpoint(&cfg, &prompt)?;
    let parsed: serde_json::Value = extract_json_object(&raw_text).unwrap_or_else(|| serde_json::json!({}));

    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("Proposed organization rules from local LLM")
        .to_string();

    let mut suggested_tags = Vec::new();
    if let Some(arr) = parsed.get("suggested_tags").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(fk), Some(tarr)) = (
                item.get("file_key").and_then(|v| v.as_str()),
                item.get("tags").and_then(|v| v.as_array()),
            ) {
                let tags: Vec<String> = tarr
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect();
                if !tags.is_empty() {
                    suggested_tags.push((fk.to_string(), tags));
                }
            }
        }
    }

    // Validate all proposed filesystem rules through the existing preview engine (NEVER execute directly!)
    let rename_preview = parsed
        .get("rename_template")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .and_then(|tmpl| {
            preview_batch_rename(
                db,
                file_keys,
                &RenameOptions {
                    template: tmpl.to_string(),
                    ..Default::default()
                },
            )
            .ok()
        });

    let move_preview = parsed
        .get("folder_template")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .and_then(|tmpl| {
            let base = Path::new(&selected[0].parent_dir);
            preview_batch_move_or_copy(db, file_keys, base, Some(tmpl), false, Some("first")).ok()
        });

    Ok(LlmOrganizationProposal {
        summary,
        suggested_tags,
        rename_preview,
        move_preview,
        raw_model_output: raw_text,
    })
}

fn call_local_llm_endpoint(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let base = cfg.endpoint_url.trim_end_matches('/');
    if cfg.provider == "ollama" {
        let url = format!("{base}/api/generate");
        let resp = ureq::post(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send_json(serde_json::json!({
                "model": cfg.model_name,
                "prompt": prompt,
                "stream": false
            }))
            .map_err(|e| format!("Local Ollama endpoint unreachable ({url}): {e}"))?;
        let val: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        return Ok(val
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string());
    }

    // LM Studio / OpenAI-compatible `/v1/chat/completions`
    let url = if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    };
    let mut req = ureq::post(&url).timeout(std::time::Duration::from_secs(30));
    if let Some(ref key) = cfg.api_key {
        if !key.trim().is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
    }
    let resp = req
        .send_json(serde_json::json!({
            "model": cfg.model_name,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.2
        }))
        .map_err(|e| format!("Local OpenAI-compatible LLM unreachable ({url}): {e}"))?;
    let val: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    Ok(val["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

fn extract_json_object(s: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        return Some(v);
    }
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        serde_json::from_str(&s[start..=end]).ok()
    } else {
        None
    }
}

fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn bytes_to_f32_slice(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
