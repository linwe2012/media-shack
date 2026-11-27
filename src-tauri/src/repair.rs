use crate::analysis::apply_no_window;
use crate::catalog::{register_derived_file, to_long_fs_path};
use crate::db::Database;
use crate::planner::probe_file;
use crate::verify::run_full_decode_check;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mp4AtomInfo {
    pub box_type: String,
    pub offset: u64,
    pub size: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairDiagnosis {
    pub file_key: Option<String>,
    pub path: String,
    pub size_bytes: u64,
    pub ffprobe_readable: bool,
    pub ffprobe_error: Option<String>,
    pub container_integrity: String, // healthy | missing_moov | truncated_mdat | decode_errors | unreadable
    pub missing_moov_atom: bool,
    pub truncated_mdat: bool,
    pub decoder_errors: usize,
    pub audio_valid: bool,
    pub video_valid: bool,
    pub atoms: Vec<Mp4AtomInfo>,
    pub recommended_strategy: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairResult {
    pub source_path: String,
    pub output_path: String,
    pub strategy_used: String,
    pub verified_decoded: bool,
    pub partial_recovery: bool,
    pub output_bytes: u64,
    pub duration_secs: Option<f64>,
    pub derived_file_key: Option<String>,
    pub logs: Vec<String>,
}

/// Inspect ISO-BMFF / MP4 / MOV top-level atoms (`ftyp`, `moov`, `mdat`, etc.) without decoding.
pub fn inspect_mp4_atoms(path: &Path) -> Result<Vec<Mp4AtomInfo>, String> {
    let mut f = File::open(to_long_fs_path(path)).map_err(|e| format!("Cannot open file: {e}"))?;
    let total_len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut offset = 0u64;
    let mut atoms = Vec::new();
    let mut hdr = [0u8; 8];

    while offset + 8 <= total_len && atoms.len() < 128 {
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let box_type: String = hdr[4..8]
            .iter()
            .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })
            .collect();

        let size = if size32 == 1 {
            let mut ext = [0u8; 8];
            if offset + 16 <= total_len && f.read_exact(&mut ext).is_ok() {
                u64::from_be_bytes(ext)
            } else {
                0
            }
        } else if size32 == 0 {
            total_len.saturating_sub(offset)
        } else {
            size32
        };

        if size < 8 {
            atoms.push(Mp4AtomInfo {
                box_type,
                offset,
                size,
                truncated: true,
            });
            break;
        }
        let truncated = offset.saturating_add(size) > total_len;
        atoms.push(Mp4AtomInfo {
            box_type,
            offset,
            size,
            truncated,
        });
        if truncated {
            break;
        }
        offset = offset.saturating_add(size);
    }
    Ok(atoms)
}

/// Diagnose damaged video: FFprobe readability, MP4 atoms, missing `moov`, truncated `mdat`, and FFmpeg decoder errors.
pub fn diagnose_media_file(
    db: &Database,
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    path: &Path,
    file_key: Option<&str>,
) -> Result<RepairDiagnosis, String> {
    let long_p = to_long_fs_path(path);
    let size_bytes = std::fs::metadata(&long_p)
        .map_err(|e| format!("Cannot stat file: {e}"))?
        .len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let is_isobmff = matches!(ext.as_str(), "mp4" | "m4v" | "mov" | "3gp" | "3g2");
    let atoms = if is_isobmff {
        inspect_mp4_atoms(path).unwrap_or_default()
    } else {
        Vec::new()
    };

    let has_moov = atoms.iter().any(|a| a.box_type == "moov" && !a.truncated);
    let has_mdat = atoms.iter().any(|a| a.box_type == "mdat");
    let truncated_mdat = atoms.iter().any(|a| a.box_type == "mdat" && a.truncated);
    let missing_moov_atom = is_isobmff && !has_moov && (has_mdat || size_bytes > 0);

    let probe_res = probe_file(ffprobe_path, path);
    let (ffprobe_readable, ffprobe_error, video_valid, audio_valid) = match &probe_res {
        Ok(pr) => {
            let v = pr.streams.iter().any(|s| s.codec_type == "video");
            let a = pr.streams.iter().any(|s| s.codec_type == "audio")
                || !pr.streams.iter().any(|s| s.codec_type == "audio");
            (true, None, v, a)
        }
        Err(e) => (false, Some(e.clone()), false, false),
    };

    let mut decoder_errors = 0usize;
    let mut decode_failed = false;
    if ffprobe_readable {
        if let Err(err_msg) = run_full_decode_check(ffmpeg_path, path) {
            decode_failed = true;
            decoder_errors = err_msg.lines().count().max(1);
        }
    }

    let container_integrity = if missing_moov_atom {
        "missing_moov"
    } else if truncated_mdat {
        "truncated_mdat"
    } else if !ffprobe_readable {
        "unreadable"
    } else if decode_failed {
        "decode_errors"
    } else {
        "healthy"
    };

    let recommended_strategy = match container_integrity {
        "missing_moov" => "untrunc_reference_repair",
        "truncated_mdat" | "decode_errors" => "lossless_remux_and_resilient_recover",
        "unreadable" => "untrunc_reference_repair",
        _ => "none_needed",
    };

    let summary = match container_integrity {
        "healthy" => "File probed and decoded cleanly with no structural errors.".to_string(),
        "missing_moov" => "Missing MP4/MOV moov atom (index header). Provide a healthy reference video from the same camera/encoder for Untrunc-style recovery.".to_string(),
        "truncated_mdat" => "MP4 mdat payload is truncated before declared box end; lossless remux / partial packet recovery recommended.".to_string(),
        "decode_errors" => format!("Container probes cleanly, but decoder reported {decoder_errors} stream error(s). Lossless remux / packet salvage recommended."),
        _ => format!("Container unreadable by ffprobe: {}", ffprobe_error.as_deref().unwrap_or("unknown error")),
    };

    if let Some(fk) = file_key {
        let status = if container_integrity == "healthy" { "valid" } else { "corrupted" };
        let now = Utc::now().timestamp();
        let conn = db.lock_conn()?;
        conn.execute(
            "UPDATE media_files SET integrity_status = ?1, updated_at = ?2 WHERE file_key = ?3",
            params![status, now, fk],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO integrity_results (id, file_key, check_type, status, details, checked_at)
             VALUES (?1, ?2, 'repair_diagnosis', ?3, ?4, ?5)",
            params![Uuid::new_v4().to_string(), fk, status, summary, now],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(RepairDiagnosis {
        file_key: file_key.map(str::to_string),
        path: path.to_string_lossy().to_string(),
        size_bytes,
        ffprobe_readable,
        ffprobe_error,
        container_integrity: container_integrity.to_string(),
        missing_moov_atom,
        truncated_mdat,
        decoder_errors,
        audio_valid,
        video_valid,
        atoms,
        recommended_strategy: recommended_strategy.to_string(),
        summary,
    })
}

/// Non-destructive video recovery engine:
/// Never modifies the damaged source. Always writes to a new output file, verifies by actual FFprobe + FFmpeg decoding,
/// and registers `recovered_from` relationships in the shared catalog.
pub fn repair_damaged_video(
    db: &Database,
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    damaged_path: &Path,
    reference_path: Option<&Path>,
    output_dir: Option<&Path>,
    source_file_key: Option<&str>,
) -> Result<RepairResult, String> {
    let long_src = to_long_fs_path(damaged_path);
    if !long_src.exists() {
        return Err(format!("Damaged source does not exist: {}", damaged_path.display()));
    }

    let stem = damaged_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());
    let dest_dir = output_dir
        .map(PathBuf::from)
        .or_else(|| damaged_path.parent().map(PathBuf::from))
        .ok_or_else(|| "Cannot determine output directory for repaired file.".to_string())?;
    std::fs::create_dir_all(to_long_fs_path(&dest_dir)).map_err(|e| e.to_string())?;

    let mut out_path = dest_dir.join(format!("{stem}.recovered.mp4"));
    let mut idx = 2usize;
    while out_path.exists() {
        out_path = dest_dir.join(format!("{stem}.recovered ({idx}).mp4"));
        idx += 1;
    }

    let mut logs: Vec<String> = Vec::new();
    let diag = diagnose_media_file(db, ffmpeg_path, ffprobe_path, damaged_path, source_file_key)?;
    logs.push(format!("Diagnosis: {} ({})", diag.container_integrity, diag.summary));

    let ext = damaged_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "3gp" | "3g2") {
        let rec_opts = crate::recovery::RecoveryOptions {
            input_path: damaged_path.to_path_buf(),
            reference_paths: reference_path.into_iter().map(PathBuf::from).collect(),
            output_path: out_path.clone(),
            video_only: false,
            deep_scan: true,
            timeout_secs: 240,
            generate_contact_sheet: false,
        };
        if let Ok(report) = crate::recovery::recover_file(&rec_opts, ffmpeg_path, ffprobe_path, None) {
            logs.push(format!("recovery engine status: {}", report.honest_summary));
            if report.overall_status != "unrecoverable" && out_path.exists() {
                let strategy = report
                    .selected_strategy
                    .unwrap_or_else(|| "mp4_recovery_engine".to_string());
                return finalize_repair(
                    db,
                    ffprobe_path,
                    damaged_path,
                    &out_path,
                    &strategy,
                    report.overall_status != "recovered_clean",
                    source_file_key,
                    logs,
                );
            }
        }
        let _ = std::fs::remove_file(to_long_fs_path(&out_path));
    }

    // Strategy 1: If readable by ffprobe, try lossless stream-copy remux first
    if diag.ffprobe_readable {
        logs.push("Attempting lossless stream-copy remux (-c copy -fflags +genpts+igndts)...".to_string());
        let mut cmd = Command::new(ffmpeg_path);
        cmd.args([
            "-hide_banner",
            "-y",
            "-err_detect",
            "ignore_err",
            "-fflags",
            "+genpts+igndts",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(&long_src)
        .args(["-map", "0", "-c", "copy", "-movflags", "+faststart"])
        .arg(to_long_fs_path(&out_path));
        apply_no_window(&mut cmd);
        if let Ok(out) = cmd.output() {
            if out.status.success() && verify_repaired_output(ffmpeg_path, ffprobe_path, &out_path, &mut logs) {
                return finalize_repair(
                    db,
                    ffprobe_path,
                    damaged_path,
                    &out_path,
                    "lossless_remux",
                    diag.truncated_mdat || diag.decoder_errors > 0,
                    source_file_key,
                    logs,
                );
            }
        }
        let _ = std::fs::remove_file(to_long_fs_path(&out_path));
    }

    // Strategy 2: Untrunc-style reference-guided MP4 recovery (missing moov / corrupted header)
    if let Some(ref_p) = reference_path {
        if let Some(untrunc_bin) = discover_untrunc() {
            logs.push(format!("Using external untrunc binary: {}", untrunc_bin.display()));
            let mut cmd = Command::new(untrunc_bin);
            cmd.arg(to_long_fs_path(ref_p)).arg(&long_src);
            apply_no_window(&mut cmd);
            if let Ok(out) = cmd.output() {
                logs.push(String::from_utf8_lossy(&out.stdout).to_string());
                // untrunc writes <damaged>_fixed.mp4 next to damaged_path
                let candidate = PathBuf::from(format!("{}_fixed.mp4", damaged_path.display()));
                if candidate.exists() {
                    let _ = std::fs::rename(to_long_fs_path(&candidate), to_long_fs_path(&out_path));
                    if verify_repaired_output(ffmpeg_path, ffprobe_path, &out_path, &mut logs) {
                        return finalize_repair(
                            db,
                            ffprobe_path,
                            damaged_path,
                            &out_path,
                            "untrunc_cli_reference",
                            true,
                            source_file_key,
                            logs,
                        );
                    }
                    let _ = std::fs::remove_file(to_long_fs_path(&out_path));
                }
            }
        }

        // Built-in Rust Annex-B NAL / mdat payload recovery guided by reference ffprobe parameters
        logs.push("Running built-in reference-guided H.264/HEVC mdat Annex-B stream salvage...".to_string());
        if let Ok(true) = recover_mdat_with_reference(ffmpeg_path, ffprobe_path, damaged_path, ref_p, &out_path, &mut logs) {
            if verify_repaired_output(ffmpeg_path, ffprobe_path, &out_path, &mut logs) {
                return finalize_repair(
                    db,
                    ffprobe_path,
                    damaged_path,
                    &out_path,
                    "builtin_reference_mdat_salvage",
                    true,
                    source_file_key,
                    logs,
                );
            }
            let _ = std::fs::remove_file(to_long_fs_path(&out_path));
        }
    }

    // Strategy 3: Frame-level salvage re-encode (if container had severe mid-stream corruption)
    if diag.ffprobe_readable {
        logs.push("Attempting resilient decode-and-salvage fallback...".to_string());
        let mut cmd = Command::new(ffmpeg_path);
        cmd.args([
            "-hide_banner",
            "-y",
            "-err_detect",
            "ignore_err",
            "-fflags",
            "+discardcorrupt+genpts",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(&long_src)
        .args(["-c:v", "libx264", "-crf", "18", "-preset", "fast", "-c:a", "aac", "-movflags", "+faststart"])
        .arg(to_long_fs_path(&out_path));
        apply_no_window(&mut cmd);
        if let Ok(out) = cmd.output() {
            if out.status.success() && verify_repaired_output(ffmpeg_path, ffprobe_path, &out_path, &mut logs) {
                return finalize_repair(
                    db,
                    ffprobe_path,
                    damaged_path,
                    &out_path,
                    "resilient_decode_salvage",
                    true,
                    source_file_key,
                    logs,
                );
            }
        }
        let _ = std::fs::remove_file(to_long_fs_path(&out_path));
    }

    Err(format!(
        "Recovery failed: could not produce a decodable output.\nLogs:\n{}",
        logs.join("\n")
    ))
}

fn discover_untrunc() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["untrunc.exe"]
    } else {
        &["untrunc"]
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for n in names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Extract length-prefixed AVCC/HVCC NAL units from a damaged MP4 `mdat` payload using the reference file's codec/FPS,
/// convert to Annex-B, and remux losslessly via FFmpeg.
fn recover_mdat_with_reference(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    damaged_path: &Path,
    reference_path: &Path,
    out_path: &Path,
    logs: &mut Vec<String>,
) -> Result<bool, String> {
    let ref_probe = probe_file(ffprobe_path, reference_path)?;
    let ref_video = ref_probe
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0)
        .ok_or_else(|| "Reference file has no video stream.".to_string())?;
    let codec = ref_video.codec_name.as_str();
    if codec != "h264" && codec != "hevc" {
        logs.push(format!("Reference codec {codec} not supported by Annex-B mdat salvage."));
        return Ok(false);
    }

    // Extract SPS/PPS/VPS headers from the healthy reference file via ffmpeg bsf
    let tmp_dir = std::env::temp_dir().join(format!("ms_repair_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let raw_ext = if codec == "hevc" { "h265" } else { "h264" };
    let bsf = if codec == "hevc" { "hevc_mp4toannexb" } else { "h264_mp4toannexb" };
    let ref_annexb = tmp_dir.join(format!("ref.{raw_ext}"));

    let mut ref_cmd = Command::new(ffmpeg_path);
    ref_cmd
        .args(["-hide_banner", "-y", "-i"])
        .arg(to_long_fs_path(reference_path))
        .args(["-frames:v", "1", "-c:v", "copy", "-bsf:v", bsf, "-f", raw_ext])
        .arg(&ref_annexb);
    apply_no_window(&mut ref_cmd);
    let _ = ref_cmd.output();

    let header_nals = extract_param_set_nals(&ref_annexb, codec);
    if header_nals.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        logs.push("Could not extract SPS/PPS parameter sets from reference video.".to_string());
        return Ok(false);
    }

    // Scan damaged file for mdat box (or full payload) and parse 4-byte length-prefixed NAL units
    let raw_bytes = std::fs::read(to_long_fs_path(damaged_path)).map_err(|e| e.to_string())?;
    let mdat_start = find_mdat_payload_start(&raw_bytes).unwrap_or(0);
    let payload = &raw_bytes[mdat_start..];

    let dmg_annexb = tmp_dir.join(format!("salvaged.{raw_ext}"));
    let mut out_f = File::create(&dmg_annexb).map_err(|e| e.to_string())?;
    out_f.write_all(&header_nals).map_err(|e| e.to_string())?;

    let mut pos = 0usize;
    let mut nal_count = 0usize;
    while pos + 5 <= payload.len() {
        let len = u32::from_be_bytes([payload[pos], payload[pos + 1], payload[pos + 2], payload[pos + 3]]) as usize;
        if len >= 2 && len <= 2 * 1024 * 1024 && pos + 4 + len <= payload.len() {
            let nal_header = payload[pos + 4];
            let valid_nal = if codec == "h264" {
                let forbidden = (nal_header & 0x80) == 0;
                let ntype = nal_header & 0x1F;
                forbidden && matches!(ntype, 1..=12)
            } else {
                let forbidden = (nal_header & 0x80) == 0;
                let ntype = (nal_header >> 1) & 0x3F;
                forbidden && ntype <= 40
            };
            if valid_nal {
                out_f.write_all(&[0, 0, 0, 1]).map_err(|e| e.to_string())?;
                out_f.write_all(&payload[pos + 4..pos + 4 + len]).map_err(|e| e.to_string())?;
                pos += 4 + len;
                nal_count += 1;
                continue;
            }
        }
        pos += 1;
    }
    drop(out_f);
    logs.push(format!("Extracted {nal_count} candidate NAL units from damaged mdat."));

    if nal_count == 0 {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Ok(false);
    }

    let fps_arg = ref_video
        .r_frame_rate
        .as_deref()
        .or(ref_video.avg_frame_rate.as_deref())
        .unwrap_or("24");
    let mut mux_cmd = Command::new(ffmpeg_path);
    mux_cmd
        .args(["-hide_banner", "-y", "-r", fps_arg, "-f", raw_ext, "-i"])
        .arg(&dmg_annexb)
        .args(["-c:v", "copy", "-movflags", "+faststart"])
        .arg(to_long_fs_path(out_path));
    apply_no_window(&mut mux_cmd);
    let mux_out = mux_cmd.output().map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(mux_out.status.success() && out_path.exists())
}

fn find_mdat_payload_start(bytes: &[u8]) -> Option<usize> {
    for i in 0..bytes.len().saturating_sub(8) {
        if &bytes[i + 4..i + 8] == b"mdat" {
            return Some(i + 8);
        }
    }
    None
}

fn extract_param_set_nals(annexb_path: &Path, codec: &str) -> Vec<u8> {
    let Ok(bytes) = std::fs::read(annexb_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4] == [0, 0, 0, 1] {
            starts.push((i, 4));
            i += 4;
        } else if i + 3 <= bytes.len() && bytes[i..i + 3] == [0, 0, 1] {
            starts.push((i, 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    for idx in 0..starts.len() {
        let (s, prefix_len) = starts[idx];
        let end = starts.get(idx + 1).map(|x| x.0).unwrap_or(bytes.len());
        if s + prefix_len >= end {
            continue;
        }
        let hdr = bytes[s + prefix_len];
        let is_param = if codec == "hevc" {
            let t = (hdr >> 1) & 0x3F;
            matches!(t, 32 | 33 | 34)
        } else {
            let t = hdr & 0x1F;
            matches!(t, 7 | 8)
        };
        if is_param {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&bytes[s + prefix_len..end]);
        }
    }
    out
}

fn verify_repaired_output(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    out_path: &Path,
    logs: &mut Vec<String>,
) -> bool {
    let Ok(meta) = std::fs::metadata(to_long_fs_path(out_path)) else {
        logs.push("Verification failed: repaired output missing.".to_string());
        return false;
    };
    if meta.len() == 0 {
        logs.push("Verification failed: repaired output is 0 bytes.".to_string());
        return false;
    }
    if let Err(e) = probe_file(ffprobe_path, out_path) {
        logs.push(format!("Verification failed (ffprobe): {e}"));
        return false;
    }
    if let Err(e) = run_full_decode_check(ffmpeg_path, out_path) {
        logs.push(format!("Verification failed (full decode): {e}"));
        return false;
    }
    logs.push("Verification passed: output probed and decoded cleanly.".to_string());
    true
}

#[allow(clippy::too_many_arguments)]
fn finalize_repair(
    db: &Database,
    ffprobe_path: &Path,
    damaged_path: &Path,
    out_path: &Path,
    strategy: &str,
    partial_recovery: bool,
    source_file_key: Option<&str>,
    logs: Vec<String>,
) -> Result<RepairResult, String> {
    let output_bytes = std::fs::metadata(to_long_fs_path(out_path))
        .map(|m| m.len())
        .unwrap_or(0);
    let duration_secs = probe_file(ffprobe_path, out_path)
        .ok()
        .and_then(|p| p.format.duration_secs);

    let src_ref = source_file_key
        .map(str::to_string)
        .unwrap_or_else(|| damaged_path.to_string_lossy().to_string());
    let derived_file_key = register_derived_file(
        db,
        &src_ref,
        out_path,
        "recovered_from",
        serde_json::json!({
            "strategy": strategy,
            "partial_recovery": partial_recovery,
        }),
        true,
        Some(ffprobe_path),
    )
    .ok();

    Ok(RepairResult {
        source_path: damaged_path.to_string_lossy().to_string(),
        output_path: out_path.to_string_lossy().to_string(),
        strategy_used: strategy.to_string(),
        verified_decoded: true,
        partial_recovery,
        output_bytes,
        duration_secs,
        derived_file_key,
        logs,
    })
}
