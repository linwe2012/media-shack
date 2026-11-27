use crate::models::*;
use crate::planner::{parse_rational, probe_file};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct VerificationReport {
    pub passed: bool,
    pub output_bytes: u64,
    pub full_decode_verified: bool,
    pub summary: String,
    pub details: String,
}

pub fn verify_staged_output(
    ffprobe_path: &Path,
    ffmpeg_path: &Path,
    source_path: &Path,
    source_probe: &ProbeResult,
    staged_path: &Path,
    recipe: &EncodeRecipe,
    plan: &TrackPlan,
    require_full_decode: bool,
) -> VerificationReport {
    let meta = match std::fs::metadata(staged_path) {
        Ok(m) => m,
        Err(e) => {
            return VerificationReport {
                passed: false,
                output_bytes: 0,
                full_decode_verified: false,
                summary: "Staged output missing".to_string(),
                details: format!("Cannot stat {}: {e}", staged_path.display()),
            };
        }
    };
    let output_bytes = meta.len();
    if output_bytes == 0 {
        return VerificationReport {
            passed: false,
            output_bytes: 0,
            full_decode_verified: false,
            summary: "Output file is empty".to_string(),
            details: "Staged output has 0 bytes.".to_string(),
        };
    }

    let out_probe = match probe_file(ffprobe_path, staged_path) {
        Ok(p) => p,
        Err(e) => {
            return VerificationReport {
                passed: false,
                output_bytes,
                full_decode_verified: false,
                summary: "Output probe failed".to_string(),
                details: format!("ffprobe failed on staged file: {e}"),
            };
        }
    };

    let mut mismatches: Vec<String> = Vec::new();
    let expected_tracks: Vec<&TrackDecision> = plan
        .tracks
        .iter()
        .filter(|t| !matches!(t.action, TrackAction::Omit))
        .collect();

    if out_probe.streams.len() != expected_tracks.len() {
        mismatches.push(format!(
            "Stream count mismatch: expected {}, got {}",
            expected_tracks.len(),
            out_probe.streams.len()
        ));
    }

    for (ord, expected) in expected_tracks.iter().enumerate() {
        let Some(actual) = out_probe.streams.get(ord) else {
            continue;
        };
        let src_stream = source_probe
            .streams
            .iter()
            .find(|s| s.index == expected.stream_index);

        match expected.action {
            TrackAction::EncodeVideo => {
                let want_codec = match recipe.codec {
                    VideoCodec::Hevc => "hevc",
                    VideoCodec::Av1 => "av1",
                };
                if actual.codec_name != want_codec {
                    mismatches.push(format!(
                        "Video codec mismatch: expected {want_codec}, got {}",
                        actual.codec_name
                    ));
                }
                if let Some(src) = src_stream {
                    if actual.width != src.width || actual.height != src.height {
                        mismatches.push(format!(
                            "Dimension mismatch: expected {:?}x{:?}, got {:?}x{:?}",
                            src.width, src.height, actual.width, actual.height
                        ));
                    }
                    if let Some(ref target_pf) = plan.target_pix_fmt {
                        if actual.pix_fmt.as_deref() != Some(target_pf.as_str()) {
                            mismatches.push(format!(
                                "Pixel format mismatch: expected {target_pf}, got {:?}",
                                actual.pix_fmt
                            ));
                        }
                    }
                    if plan.preserve_rotation && src.rotation != actual.rotation {
                        mismatches.push(format!(
                            "Rotation mismatch: expected {:?}, got {:?}",
                            src.rotation, actual.rotation
                        ));
                    }
                    check_stream_duration(src, actual, &mut mismatches);
                }
            }
            TrackAction::Copy => {
                if let Some(src) = src_stream {
                    if actual.codec_name != src.codec_name {
                        mismatches.push(format!(
                            "Copied stream #{} codec mismatch: expected {}, got {}",
                            expected.stream_index, src.codec_name, actual.codec_name
                        ));
                    }
                    if actual.disposition.default != src.disposition.default
                        || actual.disposition.forced != src.disposition.forced
                    {
                        mismatches.push(format!(
                            "Disposition mismatch on stream #{}",
                            expected.stream_index
                        ));
                    }
                }
            }
            TrackAction::TranscodeAudioAac => {
                if actual.codec_name != "aac" {
                    mismatches.push(format!(
                        "Audio fallback codec mismatch: expected aac, got {}",
                        actual.codec_name
                    ));
                }
            }
            TrackAction::ConvertSubMovText => {
                if actual.codec_name != "mov_text" {
                    mismatches.push(format!(
                        "Subtitle fallback codec mismatch: expected mov_text, got {}",
                        actual.codec_name
                    ));
                }
            }
            TrackAction::Omit => {}
        }
    }

    if out_probe.chapters.len() != source_probe.chapters.len() {
        mismatches.push(format!(
            "Chapter count mismatch: expected {}, got {}",
            source_probe.chapters.len(),
            out_probe.chapters.len()
        ));
    }

    // Format duration / completeness check
    match (source_probe.format.duration_secs, out_probe.format.duration_secs) {
        (Some(sd), Some(od)) if sd > 0.0 => {
            let tol = compute_duration_tolerance(source_probe, sd);
            if (sd - od).abs() > tol {
                mismatches.push(format!(
                    "Duration mismatch: source {sd:.3}s vs output {od:.3}s (tol {tol:.3}s)"
                ));
            }
        }
        _ => {
            // Missing duration requires a frame-count completeness check against source
            match (
                count_video_frames(ffprobe_path, source_path),
                count_video_frames(ffprobe_path, staged_path),
            ) {
                (Ok(sf), Ok(of)) if sf > 0 => {
                    if sf != of {
                        mismatches.push(format!(
                            "Frame count completeness mismatch (unknown duration): source {sf} frames vs output {of} frames"
                        ));
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    mismatches.push(format!("Completeness frame count failed: {e}"));
                }
                _ => {
                    mismatches.push("Could not verify completeness: missing duration and 0 video frames".to_string());
                }
            }
        }
    }

    if !mismatches.is_empty() {
        return VerificationReport {
            passed: false,
            output_bytes,
            full_decode_verified: false,
            summary: "Output verification failed".to_string(),
            details: mismatches.join("\n"),
        };
    }

    let mut full_decode_ok = false;
    if require_full_decode {
        match run_full_decode_check(ffmpeg_path, staged_path) {
            Ok(()) => full_decode_ok = true,
            Err(e) => {
                return VerificationReport {
                    passed: false,
                    output_bytes,
                    full_decode_verified: false,
                    summary: "Full A/V decode check failed".to_string(),
                    details: e,
                };
            }
        }
    }

    VerificationReport {
        passed: true,
        output_bytes,
        full_decode_verified: full_decode_ok,
        summary: if full_decode_ok {
            "Verified structure, timing, and full A/V decode".to_string()
        } else {
            "Verified structure, streams, and timing".to_string()
        },
        details: format!("{} bytes verified cleanly", output_bytes),
    }
}

fn compute_duration_tolerance(source_probe: &ProbeResult, duration_secs: f64) -> f64 {
    let mut tb_max = 0.002f64;
    let mut frame_dur = 0.04f64;
    if let Some(vs) = source_probe
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0)
    {
        if let Some(tb) = vs.time_base.as_deref().and_then(parse_rational) {
            tb_max = tb_max.max(tb);
        }
        if let Some(fps) = vs
            .avg_frame_rate
            .as_deref()
            .or(vs.r_frame_rate.as_deref())
            .and_then(parse_rational)
        {
            if fps > 0.0 {
                frame_dur = 1.0 / fps;
            }
        }
    }
    // Permit codec delay (up to ~6 frames) + time base rounding + 0.5s floor, or 1% for multi-hour files
    let base_tol = 0.5f64.max(6.0 * frame_dur + 4.0 * tb_max);
    base_tol.max(duration_secs * 0.005)
}

fn check_stream_duration(src: &StreamInfo, actual: &StreamInfo, mismatches: &mut Vec<String>) {
    if let (Some(sd), Some(od)) = (src.duration_secs, actual.duration_secs) {
        if sd > 0.0 {
            let tol = 0.75f64.max(sd * 0.01);
            if (sd - od).abs() > tol {
                mismatches.push(format!(
                    "Video stream duration mismatch: source {sd:.3}s vs output {od:.3}s"
                ));
            }
        }
    }
}

fn count_video_frames(ffprobe_path: &Path, file_path: &Path) -> Result<u64, String> {
    let mut cmd = Command::new(ffprobe_path);
    cmd.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-select_streams",
        "v:0",
        "-count_frames",
        "-show_entries",
        "stream=nb_read_frames",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ])
    .arg(file_path);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("ffprobe frame count error: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let txt = String::from_utf8_lossy(&out.stdout);
    txt.trim()
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<u64>().ok())
        .ok_or_else(|| format!("Unexpected frame count output: {}", txt.trim()))
}

pub fn run_full_decode_check(ffmpeg_path: &Path, media_path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args([
        "-v",
        "error",
        "-xerror",
        "-protocol_whitelist",
        "file,pipe",
        "-i",
    ])
    .arg(media_path)
    .args(["-map", "0", "-f", "null", "-"]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ffmpeg for full decode: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("Full decode failed with exit {:?}", out.status.code())
        } else {
            stderr
        });
    }
    Ok(())
}
