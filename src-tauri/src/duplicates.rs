use crate::analysis::{apply_no_window, extract_sampled_frames, SamplingMode};
use crate::catalog::{query_catalog, to_long_fs_path, CatalogEntry, CatalogFilter};
use crate::db::Database;
use crate::organize::assign_tags_bulk;
use chrono::Utc;
use rayon::prelude::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

pub const PERCEPTUAL_ALGO_VERSION: &str = "phash_dhash_v1";
pub const STREAM_HASH_ALGO_VERSION: &str = "streamhash_md5_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedPerceptualHash {
    pub timestamp_secs: f64,
    pub phash: u64,
    pub dhash: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanSummary {
    pub exact_groups_found: usize,
    pub equivalent_pairs_found: usize,
    pub perceptual_pairs_found: usize,
    pub partial_overlaps_found: usize,
    pub bytes_hashed: u64,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateMember {
    pub entry: CatalogEntry,
    pub suggested_keep: bool,
    pub keep_reason: String,
    pub relationship_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateEdgeInfo {
    pub id: String,
    pub file_key_a: String,
    pub file_key_b: String,
    pub match_kind: String, // exact | equivalent_stream | perceptual | partial_overlap
    pub confidence: f64,
    pub overlap_start_a: Option<f64>,
    pub overlap_end_a: Option<f64>,
    pub overlap_start_b: Option<f64>,
    pub overlap_end_b: Option<f64>,
    pub overlap_pct_a: Option<f64>,
    pub overlap_pct_b: Option<f64>,
    pub details_json: Option<String>,
    pub review_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub group_id: String,
    pub match_kind: String,
    pub confidence: f64,
    pub estimated_savings_bytes: u64,
    pub members: Vec<DuplicateMember>,
    pub edges: Vec<DuplicateEdgeInfo>,
}

pub fn compute_file_hashes(path: &Path, also_sha256: bool) -> Result<(String, Option<String>), String> {
    let mut f = File::open(to_long_fs_path(path)).map_err(|e| format!("Open failed: {e}"))?;
    let mut b3 = blake3::Hasher::new();
    let mut s2 = if also_sha256 { Some(Sha256::new()) } else { None };
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("Read failed: {e}"))?;
        if n == 0 {
            break;
        }
        b3.update(&buf[..n]);
        if let Some(ref mut sh) = s2 {
            sh.update(&buf[..n]);
        }
    }
    let b3_hex = b3.finalize().to_hex().to_string();
    let s2_hex = s2.map(|sh| format!("{:x}", sh.finalize()));
    Ok((b3_hex, s2_hex))
}

/// Pipeline:
/// 1. Group online files by size_bytes
/// 2. Exclude singletons
/// 3. Sub-group by partial head_hash
/// 4. Deduplicate hard-linked files sharing (volume_id, os_file_id)
/// 5. Bounded-parallel BLAKE3 (+ optional SHA-256) hashing with persistent cache
/// 6. Persist verified exact duplicate edges
pub fn detect_exact_duplicates(
    db: &Database,
    also_sha256: bool,
    max_workers: usize,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(usize, u64), String> {
    let entries = query_catalog(
        db,
        &CatalogFilter {
            online_only: Some(true),
            limit: Some(250_000),
            ..Default::default()
        },
    )?
    .items;

    // Step 1 & 3: group by (size_bytes, head_hash)
    let mut buckets: HashMap<(u64, String), Vec<CatalogEntry>> = HashMap::new();
    for e in entries {
        if e.size_bytes == 0 {
            continue;
        }
        buckets
            .entry((e.size_bytes, e.head_hash.clone()))
            .or_default()
            .push(e);
    }

    // Collect candidate files needing full BLAKE3 hash (avoiding redundant reads of hard links)
    let mut to_hash: Vec<(String, String, String, String)> = Vec::new(); // (file_key, path, vol, os_id)
    let mut seen_physical_ids: HashSet<(String, String)> = HashSet::new();
    let mut candidate_groups: Vec<Vec<CatalogEntry>> = Vec::new();

    for (_, grp) in buckets {
        if grp.len() < 2 {
            continue;
        }
        for e in &grp {
            if e.blake3_hash.is_none() && seen_physical_ids.insert((e.volume_id.clone(), e.os_file_id.clone())) {
                to_hash.push((
                    e.file_key.clone(),
                    e.path.clone(),
                    e.volume_id.clone(),
                    e.os_file_id.clone(),
                ));
            }
        }
        candidate_groups.push(grp);
    }

    let bytes_hashed = Arc::new(AtomicU64::new(0));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_workers.clamp(1, 8))
        .build()
        .map_err(|e| e.to_string())?;

    let hashed_results: Vec<(String, String, String, String, Option<String>)> = pool.install(|| {
        to_hash
            .par_iter()
            .filter_map(|(fkey, path, vol, oid)| {
                if cancel_flag.load(Ordering::Relaxed) {
                    return None;
                }
                let p = Path::new(path);
                let sz = std::fs::metadata(to_long_fs_path(p)).map(|m| m.len()).unwrap_or(0);
                if let Ok((b3, s2)) = compute_file_hashes(p, also_sha256) {
                    bytes_hashed.fetch_add(sz, Ordering::Relaxed);
                    Some((fkey.clone(), vol.clone(), oid.clone(), b3, s2))
                } else {
                    None
                }
            })
            .collect()
    });

    // Persist computed hashes across all hard-linked siblings sharing (volume_id, os_file_id)
    let now = Utc::now().timestamp();
    {
        let mut conn = db.lock_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (_fkey, vol, oid, b3, s2) in &hashed_results {
            tx.execute(
                "UPDATE media_files SET
                    blake3_hash = ?1,
                    sha256_hash = COALESCE(?2, sha256_hash),
                    analysis_level = MAX(analysis_level, 3),
                    updated_at = ?3
                 WHERE volume_id = ?4 AND os_file_id = ?5",
                params![b3, s2, now, vol, oid],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    // Re-query online files and group by blake3_hash
    let refreshed = query_catalog(
        db,
        &CatalogFilter {
            online_only: Some(true),
            limit: Some(5000),
            ..Default::default()
        },
    )?
    .items;
    let mut by_b3: HashMap<String, Vec<CatalogEntry>> = HashMap::new();
    for e in refreshed {
        if let Some(ref b3) = e.blake3_hash {
            by_b3.entry(b3.clone()).or_default().push(e);
        }
    }

    let mut exact_groups = 0usize;
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (b3, grp) in by_b3 {
        if grp.len() < 2 {
            continue;
        }
        exact_groups += 1;
        let anchor = &grp[0].file_key;
        for other in &grp[1..] {
            let (a, b) = ordered_pair(anchor, &other.file_key);
            let same_physical = grp[0].volume_id == other.volume_id && grp[0].os_file_id == other.os_file_id;
            let details = serde_json::json!({
                "blake3": b3,
                "hard_linked": same_physical
            })
            .to_string();
            tx.execute(
                "INSERT INTO similarity_edges (
                    id, file_key_a, file_key_b, match_kind, confidence, details_json, review_status, created_at
                 ) VALUES (?1, ?2, ?3, 'exact', 1.0, ?4, 'pending', ?5)
                 ON CONFLICT(file_key_a, file_key_b, match_kind) DO UPDATE SET
                    confidence = 1.0, details_json = excluded.details_json",
                params![Uuid::new_v4().to_string(), a, b, details, now],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((exact_groups, bytes_hashed.load(Ordering::Relaxed)))
}

fn ordered_pair<'a>(x: &'a str, y: &'a str) -> (&'a str, &'a str) {
    if x <= y { (x, y) } else { (y, x) }
}

/// Compute normalized encoded-stream hashes using FFmpeg `-f streamhash -hash md5 -`.
/// Container-agnostic: MP4 remuxed to MKV yields identical stream hashes.
pub fn compute_encoded_stream_hashes(
    ffmpeg_path: &Path,
    path: &Path,
) -> Result<(String, Option<String>, Option<String>), String> {
    let src = to_long_fs_path(path);
    let run_map = |map_spec: &str| -> Option<String> {
        let mut cmd = Command::new(ffmpeg_path);
        cmd.args([
            "-hide_banner",
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(&src)
        .args(["-map", map_spec, "-c", "copy", "-f", "streamhash", "-hash", "md5", "-"]);
        apply_no_window(&mut cmd);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        let txt = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if txt.is_empty() { None } else { Some(txt) }
    };

    let video_hash = run_map("0:v:0").ok_or_else(|| "No moving video stream hash available".to_string())?;
    let audio_hash = run_map("0:a");
    let sub_hash = run_map("0:s");
    Ok((video_hash, audio_hash, sub_hash))
}

/// Detect equivalent encoded media (remuxed containers, same video + different audio/subtitles/metadata).
pub fn detect_equivalent_encoded_media(
    db: &Database,
    ffmpeg_path: &Path,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<usize, String> {
    let entries = query_catalog(
        db,
        &CatalogFilter {
            media_type: Some("video".to_string()),
            online_only: Some(true),
            limit: Some(250_000),
            ..Default::default()
        },
    )?
    .items;

    let mut stream_sigs: Vec<(CatalogEntry, String, Option<String>, Option<String>)> = Vec::new();
    let now = Utc::now().timestamp();

    for e in entries {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        let cached: Option<String> = {
            let conn = db.lock_conn()?;
            conn.query_row(
                "SELECT value_text FROM fingerprints WHERE file_key = ?1 AND fp_type = 'stream_hash' AND algo_version = ?2",
                params![e.file_key, STREAM_HASH_ALGO_VERSION],
                |r| r.get(0),
            )
            .optional()
            .map_err(|err| err.to_string())?
            .flatten()
        };

        let (vh, ah, sh) = if let Some(json) = cached {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
            (
                v.get("v").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                v.get("a").and_then(|x| x.as_str()).map(str::to_string),
                v.get("s").and_then(|x| x.as_str()).map(str::to_string),
            )
        } else if let Ok((vh, ah, sh)) = compute_encoded_stream_hashes(ffmpeg_path, Path::new(&e.path)) {
            let payload = serde_json::json!({ "v": vh, "a": ah, "s": sh }).to_string();
            let conn = db.lock_conn()?;
            conn.execute(
                "INSERT INTO fingerprints (id, file_key, fp_type, algo_version, value_text, created_at)
                 VALUES (?1, ?2, 'stream_hash', ?3, ?4, ?5)
                 ON CONFLICT(file_key, fp_type, algo_version) DO UPDATE SET value_text = excluded.value_text",
                params![Uuid::new_v4().to_string(), e.file_key, STREAM_HASH_ALGO_VERSION, payload, now],
            )
            .map_err(|err| err.to_string())?;
            (vh, ah, sh)
        } else {
            continue;
        };

        if !vh.is_empty() {
            stream_sigs.push((e, vh, ah, sh));
        }
    }

    let mut by_video: HashMap<String, Vec<(CatalogEntry, Option<String>, Option<String>)>> = HashMap::new();
    for (e, vh, ah, sh) in stream_sigs {
        by_video.entry(vh).or_default().push((e, ah, sh));
    }

    let mut pairs = 0usize;
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (_vh, grp) in by_video {
        if grp.len() < 2 {
            continue;
        }
        for i in 0..grp.len() {
            for j in (i + 1)..grp.len() {
                let (ref e1, ref a1, ref s1) = grp[i];
                let (ref e2, ref a2, ref s2) = grp[j];
                // Do not classify byte-identical files as merely equivalent_stream
                if e1.blake3_hash.is_some() && e1.blake3_hash == e2.blake3_hash {
                    continue;
                }
                let subtype = if a1 == a2 && s1 == s2 {
                    "identical_streams_different_container_or_metadata"
                } else if a1 != a2 && s1 == s2 {
                    "identical_video_different_audio"
                } else if a1 == a2 && s1 != s2 {
                    "identical_video_different_subtitles"
                } else {
                    "identical_video_different_audio_and_subtitles"
                };
                let (k1, k2) = ordered_pair(&e1.file_key, &e2.file_key);
                let details = serde_json::json!({ "subtype": subtype }).to_string();
                tx.execute(
                    "INSERT INTO similarity_edges (
                        id, file_key_a, file_key_b, match_kind, confidence, details_json, review_status, created_at
                     ) VALUES (?1, ?2, ?3, 'equivalent_stream', 0.99, ?4, 'pending', ?5)
                     ON CONFLICT(file_key_a, file_key_b, match_kind) DO UPDATE SET
                        details_json = excluded.details_json",
                    params![Uuid::new_v4().to_string(), k1, k2, details, now],
                )
                .map_err(|e| e.to_string())?;
                pairs += 1;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(pairs)
}

/// Compute 64-bit 2D-DCT pHash and 64-bit gradient dHash for an image.
pub fn compute_frame_phash_dhash(img: &image::DynamicImage) -> (u64, u64) {
    // dHash: 9x8 grayscale horizontal gradient
    let g_d = img
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut dhash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let l = g_d.get_pixel(x, y)[0];
            let r = g_d.get_pixel(x + 1, y)[0];
            if l > r {
                dhash |= 1u64 << (y * 8 + x);
            }
        }
    }

    // pHash: 32x32 -> 2D DCT -> top-left 8x8 (excluding DC [0,0]) compared to median
    let g_p = img
        .resize_exact(32, 32, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut f = [[0.0f64; 32]; 32];
    for y in 0..32 {
        for x in 0..32 {
            f[y][x] = g_p.get_pixel(x as u32, y as u32)[0] as f64;
        }
    }
    // Separable 1D DCT along rows then columns (only first 8 frequencies needed!)
    let mut row_dct = [[0.0f64; 8]; 32];
    let pi = std::f64::consts::PI;
    for y in 0..32 {
        for u in 0..8 {
            let mut sum = 0.0;
            for x in 0..32 {
                sum += f[y][x] * (((2 * x + 1) as f64 * u as f64 * pi) / 64.0).cos();
            }
            row_dct[y][u] = sum;
        }
    }
    let mut coeffs = [0.0f64; 64];
    for v in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0;
            for y in 0..32 {
                sum += row_dct[y][u] * (((2 * y + 1) as f64 * v as f64 * pi) / 64.0).cos();
            }
            coeffs[v * 8 + u] = sum;
        }
    }
    let mut ac = coeffs[1..].to_vec();
    ac.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = ac[ac.len() / 2];
    let mut phash = 0u64;
    for (idx, &val) in coeffs.iter().enumerate() {
        if idx > 0 && val > median {
            phash |= 1u64 << idx;
        }
    }
    (phash, dhash)
}

pub fn extract_or_load_perceptual_sequence(
    db: &Database,
    ffmpeg_path: &Path,
    file_key: &str,
) -> Result<Vec<TimedPerceptualHash>, String> {
    {
        let conn = db.lock_conn()?;
        let cached: Option<String> = conn
            .query_row(
                "SELECT value_text FROM fingerprints
                 WHERE file_key = ?1 AND fp_type = 'perceptual_seq' AND algo_version = ?2",
                params![file_key, PERCEPTUAL_ALGO_VERSION],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        if let Some(json) = cached {
            if let Ok(seq) = serde_json::from_str::<Vec<TimedPerceptualHash>>(&json) {
                if !seq.is_empty() {
                    return Ok(seq);
                }
            }
        }
    }

    let frames = extract_sampled_frames(
        db,
        ffmpeg_path,
        file_key,
        &SamplingMode::Percentage { count: 12 },
    )?;
    let mut seq = Vec::with_capacity(frames.len());
    for f in frames {
        if let Ok(img) = image::open(&f.path) {
            let (phash, dhash) = compute_frame_phash_dhash(&img);
            seq.push(TimedPerceptualHash {
                timestamp_secs: f.timestamp_secs,
                phash,
                dhash,
            });
        }
    }
    if !seq.is_empty() {
        let json = serde_json::to_string(&seq).map_err(|e| e.to_string())?;
        let conn = db.lock_conn()?;
        conn.execute(
            "INSERT INTO fingerprints (id, file_key, fp_type, algo_version, value_text, created_at)
             VALUES (?1, ?2, 'perceptual_seq', ?3, ?4, ?5)
             ON CONFLICT(file_key, fp_type, algo_version) DO UPDATE SET value_text = excluded.value_text",
            params![
                Uuid::new_v4().to_string(),
                file_key,
                PERCEPTUAL_ALGO_VERSION,
                json,
                Utc::now().timestamp()
            ],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE media_files SET analysis_level = MAX(analysis_level, 3) WHERE file_key = ?1",
            params![file_key],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(seq)
}

#[derive(Debug, Clone)]
pub struct TemporalAlignment {
    pub confidence: f64,
    pub overlap_start_a: f64,
    pub overlap_end_a: f64,
    pub overlap_start_b: f64,
    pub overlap_end_b: f64,
    pub overlap_pct_a: f64,
    pub overlap_pct_b: f64,
    pub is_partial: bool,
}

/// Temporal alignment using frame Hamming similarity + offset voting + DTW fallback.
pub fn align_perceptual_sequences(
    seq_a: &[TimedPerceptualHash],
    dur_a: f64,
    seq_b: &[TimedPerceptualHash],
    dur_b: f64,
) -> Option<TemporalAlignment> {
    if seq_a.is_empty() || seq_b.is_empty() {
        return None;
    }

    // Collect matching frame pairs and vote on temporal offset bins (1-second resolution)
    let mut offset_bins: HashMap<i64, Vec<(usize, usize, f64)>> = HashMap::new();
    for (i, fa) in seq_a.iter().enumerate() {
        for (j, fb) in seq_b.iter().enumerate() {
            let p_dist = (fa.phash ^ fb.phash).count_ones();
            let d_dist = (fa.dhash ^ fb.dhash).count_ones();
            if p_dist <= 14 || d_dist <= 14 {
                let score = 1.0 - ((p_dist.min(d_dist) as f64) / 32.0);
                let dt_bin = (fb.timestamp_secs - fa.timestamp_secs).round() as i64;
                offset_bins.entry(dt_bin).or_default().push((i, j, score));
                offset_bins.entry(dt_bin + 1).or_default().push((i, j, score * 0.9));
                offset_bins.entry(dt_bin - 1).or_default().push((i, j, score * 0.9));
            }
        }
    }

    let best_bin = offset_bins
        .into_values()
        .max_by(|a, b| a.len().cmp(&b.len()))
        .unwrap_or_default();

    let min_matches = (seq_a.len().min(seq_b.len()) / 3).max(2);
    if best_bin.len() >= min_matches {
        let mut min_ta = f64::MAX;
        let mut max_ta = 0.0f64;
        let mut min_tb = f64::MAX;
        let mut max_tb = 0.0f64;
        let mut sum_score = 0.0f64;
        let mut uniq_a = HashSet::new();
        let mut uniq_b = HashSet::new();

        for (i, j, s) in &best_bin {
            uniq_a.insert(*i);
            uniq_b.insert(*j);
            min_ta = min_ta.min(seq_a[*i].timestamp_secs);
            max_ta = max_ta.max(seq_a[*i].timestamp_secs);
            min_tb = min_tb.min(seq_b[*j].timestamp_secs);
            max_tb = max_tb.max(seq_b[*j].timestamp_secs);
            sum_score += s;
        }

        let cov_a = (uniq_a.len() as f64 / seq_a.len() as f64).clamp(0.0, 1.0);
        let cov_b = (uniq_b.len() as f64 / seq_b.len() as f64).clamp(0.0, 1.0);
        let max_cov = cov_a.max(cov_b);
        let min_cov = cov_a.min(cov_b);
        let avg_frame_sim = (sum_score / best_bin.len() as f64).clamp(0.0, 1.0);
        let confidence = (0.6 * avg_frame_sim + 0.4 * max_cov).clamp(0.0, 1.0);

        if confidence < 0.62 {
            return None;
        }

        let da = dur_a.max(1.0);
        let db = dur_b.max(1.0);
        let step_a = da / seq_a.len().max(1) as f64;
        let step_b = db / seq_b.len().max(1) as f64;
        let end_a = (max_ta + step_a).min(da);
        let end_b = (max_tb + step_b).min(db);
        let pct_a = ((end_a - min_ta) / da * 100.0).clamp(0.0, 100.0);
        let pct_b = ((end_b - min_tb) / db * 100.0).clamp(0.0, 100.0);
        let is_partial = min_cov < 0.8 || (pct_a - pct_b).abs() > 20.0;

        return Some(TemporalAlignment {
            confidence,
            overlap_start_a: min_ta,
            overlap_end_a: end_a,
            overlap_start_b: min_tb,
            overlap_end_b: end_b,
            overlap_pct_a: pct_a,
            overlap_pct_b: pct_b,
            is_partial,
        });
    }

    // DTW fallback for slightly retimed/speed-adjusted clips
    let n = seq_a.len();
    let m = seq_b.len();
    let mut dp = vec![vec![f64::INFINITY; m + 1]; n + 1];
    dp[0][0] = 0.0;
    for i in 1..=n {
        for j in 1..=m {
            let cost = ((seq_a[i - 1].phash ^ seq_b[j - 1].phash).count_ones() as f64) / 64.0;
            let prev = dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1]);
            dp[i][j] = cost + prev;
        }
    }
    let avg_dist = dp[n][m] / (n.max(m) as f64);
    let conf = (1.0 - avg_dist * 2.0).clamp(0.0, 1.0);
    if conf >= 0.70 {
        return Some(TemporalAlignment {
            confidence: conf,
            overlap_start_a: 0.0,
            overlap_end_a: dur_a,
            overlap_start_b: 0.0,
            overlap_end_b: dur_b,
            overlap_pct_a: 100.0,
            overlap_pct_b: 100.0,
            is_partial: false,
        });
    }
    None
}

/// Scalable perceptual + partial overlap detector using 4-band LSH candidate indexing (avoids O(N^2)).
pub fn detect_perceptual_similarities(
    db: &Database,
    ffmpeg_path: &Path,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(usize, usize), String> {
    let entries = query_catalog(
        db,
        &CatalogFilter {
            online_only: Some(true),
            limit: Some(250_000),
            ..Default::default()
        },
    )?
    .items;

    let mut items: Vec<(CatalogEntry, Vec<TimedPerceptualHash>)> = Vec::new();
    for e in entries {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        if e.media_type != "video" && e.media_type != "image" {
            continue;
        }
        if let Ok(seq) = extract_or_load_perceptual_sequence(db, ffmpeg_path, &e.file_key) {
            if !seq.is_empty() {
                items.push((e, seq));
            }
        }
    }

    // Build 4-band 16-bit LSH inverted index over pHash & dHash
    let mut lsh_index: HashMap<(u8, u16), Vec<usize>> = HashMap::new();
    for (idx, (_, seq)) in items.iter().enumerate() {
        for frame in seq {
            for band in 0..4u8 {
                let p_chunk = ((frame.phash >> (band * 16)) & 0xFFFF) as u16;
                let d_chunk = ((frame.dhash >> (band * 16)) & 0xFFFF) as u16;
                lsh_index.entry((band, p_chunk)).or_default().push(idx);
                lsh_index.entry((band + 4, d_chunk)).or_default().push(idx);
            }
        }
    }

    let mut candidate_pairs: HashSet<(usize, usize)> = HashSet::new();
    for bucket in lsh_index.values() {
        if bucket.len() < 2 || bucket.len() > 256 {
            continue;
        }
        for i in 0..bucket.len() {
            for j in (i + 1)..bucket.len() {
                let a = bucket[i].min(bucket[j]);
                let b = bucket[i].max(bucket[j]);
                if a != b {
                    candidate_pairs.insert((a, b));
                }
            }
        }
    }

    let mut perceptual_count = 0usize;
    let mut partial_count = 0usize;
    let now = Utc::now().timestamp();
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    for (i, j) in candidate_pairs {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        let (ref ea, ref seq_a) = items[i];
        let (ref eb, ref seq_b) = items[j];
        if ea.blake3_hash.is_some() && ea.blake3_hash == eb.blake3_hash {
            continue;
        }

        let dur_a = ea.duration_secs.unwrap_or(1.0);
        let dur_b = eb.duration_secs.unwrap_or(1.0);
        if let Some(align) = align_perceptual_sequences(seq_a, dur_a, seq_b, dur_b) {
            let (k1, k2, sa, ea_t, sb, eb_t, pa, pb) = if ea.file_key <= eb.file_key {
                (
                    &ea.file_key,
                    &eb.file_key,
                    align.overlap_start_a,
                    align.overlap_end_a,
                    align.overlap_start_b,
                    align.overlap_end_b,
                    align.overlap_pct_a,
                    align.overlap_pct_b,
                )
            } else {
                (
                    &eb.file_key,
                    &ea.file_key,
                    align.overlap_start_b,
                    align.overlap_end_b,
                    align.overlap_start_a,
                    align.overlap_end_a,
                    align.overlap_pct_b,
                    align.overlap_pct_a,
                )
            };
            let kind = if align.is_partial {
                partial_count += 1;
                "partial_overlap"
            } else {
                perceptual_count += 1;
                "perceptual"
            };
            let details = serde_json::json!({
                "overlap_duration_secs": (ea_t - sa).max(0.0),
                "is_partial": align.is_partial,
            })
            .to_string();

            tx.execute(
                "INSERT INTO similarity_edges (
                    id, file_key_a, file_key_b, match_kind, confidence,
                    overlap_start_a, overlap_end_a, overlap_start_b, overlap_end_b,
                    overlap_pct_a, overlap_pct_b, details_json, review_status, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'pending', ?13)
                 ON CONFLICT(file_key_a, file_key_b, match_kind) DO UPDATE SET
                    confidence = excluded.confidence,
                    overlap_start_a = excluded.overlap_start_a,
                    overlap_end_a = excluded.overlap_end_a,
                    overlap_start_b = excluded.overlap_start_b,
                    overlap_end_b = excluded.overlap_end_b,
                    overlap_pct_a = excluded.overlap_pct_a,
                    overlap_pct_b = excluded.overlap_pct_b,
                    details_json = excluded.details_json",
                params![
                    Uuid::new_v4().to_string(),
                    k1,
                    k2,
                    kind,
                    align.confidence,
                    sa,
                    ea_t,
                    sb,
                    eb_t,
                    pa,
                    pb,
                    details,
                    now
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((perceptual_count, partial_count))
}

pub fn run_full_duplicate_scan(
    db: &Database,
    ffmpeg_path: Option<&Path>,
    also_sha256: bool,
    include_perceptual: bool,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<DuplicateScanSummary, String> {
    let started = std::time::Instant::now();
    let workers = db
        .get_setting::<crate::tasks::ResourceBudgets>("resource_budgets")
        .ok()
        .flatten()
        .map(|b| b.cpu_workers)
        .unwrap_or(4);
    let (exact_groups_found, bytes_hashed) = detect_exact_duplicates(db, also_sha256, workers, cancel_flag)?;
    let mut equivalent_pairs_found = 0usize;
    let mut perceptual_pairs_found = 0usize;
    let mut partial_overlaps_found = 0usize;

    if let Some(ff) = ffmpeg_path {
        if !cancel_flag.load(Ordering::Relaxed) {
            equivalent_pairs_found = detect_equivalent_encoded_media(db, ff, cancel_flag)?;
        }
        if include_perceptual && !cancel_flag.load(Ordering::Relaxed) {
            let (perc, part) = detect_perceptual_similarities(db, ff, cancel_flag)?;
            perceptual_pairs_found = perc;
            partial_overlaps_found = part;
        }
    }

    Ok(DuplicateScanSummary {
        exact_groups_found,
        equivalent_pairs_found,
        perceptual_pairs_found,
        partial_overlaps_found,
        bytes_hashed,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Group similarity edges into connected components with balanced retention recommendations.
pub fn list_duplicate_groups(db: &Database, match_kind_filter: Option<&str>) -> Result<Vec<DuplicateGroup>, String> {
    let edges: Vec<DuplicateEdgeInfo> = {
        let conn = db.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_key_a, file_key_b, match_kind, confidence,
                        overlap_start_a, overlap_end_a, overlap_start_b, overlap_end_b,
                        overlap_pct_a, overlap_pct_b, details_json, review_status
                 FROM similarity_edges
                 WHERE review_status != 'dismissed'
                 ORDER BY confidence DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DuplicateEdgeInfo {
                    id: r.get(0)?,
                    file_key_a: r.get(1)?,
                    file_key_b: r.get(2)?,
                    match_kind: r.get(3)?,
                    confidence: r.get(4)?,
                    overlap_start_a: r.get(5)?,
                    overlap_end_a: r.get(6)?,
                    overlap_start_b: r.get(7)?,
                    overlap_end_b: r.get(8)?,
                    overlap_pct_a: r.get(9)?,
                    overlap_pct_b: r.get(10)?,
                    details_json: r.get(11)?,
                    review_status: r.get(12)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    let filtered_edges: Vec<DuplicateEdgeInfo> = edges
        .into_iter()
        .filter(|e| match_kind_filter.map(|f| f == "all" || e.match_kind == f).unwrap_or(true))
        .collect();
    if filtered_edges.is_empty() {
        return Ok(Vec::new());
    }

    let mut involved_keys: HashSet<String> = HashSet::new();
    for e in &filtered_edges {
        involved_keys.insert(e.file_key_a.clone());
        involved_keys.insert(e.file_key_b.clone());
    }
    let key_vec: Vec<String> = involved_keys.into_iter().collect();
    let entries = crate::catalog::load_entries_by_file_keys(db, &key_vec)?;
    let entry_map: HashMap<String, CatalogEntry> = entries
        .into_iter()
        .map(|e| (e.file_key.clone(), e))
        .collect();

    // Load relationship links so derived/transcoded files are surfaced explicitly
    let rel_notes: HashMap<String, String> = {
        let conn = db.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT target_file_key, relationship_type FROM media_relationships WHERE target_file_key IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let mut map = HashMap::new();
        for row in stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            map.insert(row.0, row.1);
        }
        map
    };

    // Union-Find connected components
    let mut adj: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, e) in filtered_edges.iter().enumerate() {
        adj.entry(e.file_key_a.clone()).or_default().push(idx);
        adj.entry(e.file_key_b.clone()).or_default().push(idx);
    }

    let mut visited_keys: HashSet<String> = HashSet::new();
    let mut groups = Vec::new();
    let all_keys: Vec<String> = adj.keys().cloned().collect();

    for start in all_keys {
        if visited_keys.contains(&start) {
            continue;
        }
        let mut stack = vec![start.clone()];
        let mut comp_keys = Vec::new();
        let mut comp_edge_idxs = HashSet::new();
        while let Some(cur) = stack.pop() {
            if !visited_keys.insert(cur.clone()) {
                continue;
            }
            comp_keys.push(cur.clone());
            if let Some(eidxs) = adj.get(&cur) {
                for &ei in eidxs {
                    comp_edge_idxs.insert(ei);
                    let e = &filtered_edges[ei];
                    if !visited_keys.contains(&e.file_key_a) {
                        stack.push(e.file_key_a.clone());
                    }
                    if !visited_keys.contains(&e.file_key_b) {
                        stack.push(e.file_key_b.clone());
                    }
                }
            }
        }

        let mut comp_entries: Vec<CatalogEntry> = comp_keys
            .iter()
            .filter_map(|k| entry_map.get(k).cloned())
            .collect();
        if comp_entries.len() < 2 {
            continue;
        }
        let comp_edges: Vec<DuplicateEdgeInfo> = comp_edge_idxs
            .into_iter()
            .map(|i| filtered_edges[i].clone())
            .collect();

        // Balanced retention suggestion:
        // Prefer verified integrity + online + longest duration + non-derivative original + richer tag count,
        // without blindly assuming largest/HEVC/newest is always better.
        comp_entries.sort_by(|a, b| {
            let score = |e: &CatalogEntry| -> i64 {
                let mut s = 0i64;
                if e.is_online { s += 1000; }
                if e.integrity_status == "valid" { s += 500; }
                if e.integrity_status == "corrupted" { s -= 1000; }
                if !rel_notes.contains_key(&e.file_key) { s += 200; } // Original asset preferred unless verified derivative
                s += (e.duration_secs.unwrap_or(0.0) * 10.0) as i64;
                s += (e.tags.len() as i64) * 20;
                s
            };
            score(b).cmp(&score(a))
        });

        let keep_key = comp_entries[0].file_key.clone();
        let total_size: u64 = comp_entries.iter().map(|e| e.size_bytes).sum();
        let keep_size = comp_entries[0].size_bytes;
        let savings = total_size.saturating_sub(keep_size);

        let group_kind = comp_edges[0].match_kind.clone();
        let avg_conf = comp_edges.iter().map(|e| e.confidence).sum::<f64>() / comp_edges.len() as f64;

        let members = comp_entries
            .into_iter()
            .map(|e| {
                let is_keep = e.file_key == keep_key;
                let rel = rel_notes.get(&e.file_key).cloned();
                let reason = if is_keep {
                    if rel.is_none() {
                        "Suggested keep (verified/primary copy with longest duration & tags)".to_string()
                    } else {
                        "Suggested keep (highest completeness/integrity score)".to_string()
                    }
                } else if let Some(ref r) = rel {
                    format!("Derived copy ({r}) — review before cleanup")
                } else if group_kind == "exact" {
                    "Byte-identical duplicate — review before cleanup".to_string()
                } else {
                    "Similar/near-duplicate — manual side-by-side review required".to_string()
                };
                DuplicateMember {
                    entry: e,
                    suggested_keep: is_keep,
                    keep_reason: reason,
                    relationship_note: rel,
                }
            })
            .collect();

        groups.push(DuplicateGroup {
            group_id: format!("dup-{}", comp_edges[0].id),
            match_kind: group_kind,
            confidence: avg_conf,
            estimated_savings_bytes: savings,
            members,
            edges: comp_edges,
        });
    }

    groups.sort_by(|a, b| b.estimated_savings_bytes.cmp(&a.estimated_savings_bytes));
    Ok(groups)
}

pub fn dismiss_similarity_edge(db: &Database, edge_id: &str) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "UPDATE similarity_edges SET review_status = 'dismissed' WHERE id = ?1",
        params![edge_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Merge compatible active tags from redundant copies into the retained asset.
pub fn merge_tags_from_copies(
    db: &Database,
    from_asset_ids: &[String],
    into_asset_id: &str,
) -> Result<usize, String> {
    let mut tag_names: Vec<String> = Vec::new();
    {
        let conn = db.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT t.name FROM media_tags mt
                 JOIN tags t ON t.id = mt.tag_id
                 WHERE mt.asset_id = ?1 AND mt.status = 'active'",
            )
            .map_err(|e| e.to_string())?;
        for aid in from_asset_ids {
            if aid == into_asset_id {
                continue;
            }
            for name in stmt
                .query_map(params![aid], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .flatten()
            {
                if !tag_names.contains(&name) {
                    tag_names.push(name);
                }
            }
        }
    }
    assign_tags_bulk(db, &[into_asset_id.to_string()], &tag_names, "manual", 1.0)
}
