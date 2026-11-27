use crate::recovery::{CandidateEvaluation, ReferenceProfile};
use std::path::Path;
use std::process::Command;

pub fn evaluate_candidate(
    strategy_name: &str,
    candidate_path: &Path,
    segment_paths: Vec<String>,
    input_bytes: u64,
    estimated_mapped_bytes: u64,
    reference: Option<&ReferenceProfile>,
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    deep_checks: bool,
) -> CandidateEvaluation {
    let mut eval = CandidateEvaluation {
        strategy_name: strategy_name.to_string(),
        output_path: candidate_path.to_string_lossy().to_string(),
        segment_paths,
        succeeded: false,
        score: 0.0,
        decoded_video_frames: 0,
        decode_error_count: 0,
        recovered_duration_secs: None,
        audio_duration_secs: None,
        width: None,
        height: None,
        idr_frames: 0,
        timestamps_monotonic: true,
        av_sync_diff_secs: None,
        input_bytes_mapped_pct: if input_bytes > 0 {
            ((estimated_mapped_bytes as f64 / input_bytes as f64) * 100.0).min(100.0)
        } else {
            0.0
        },
        black_frame_ratio: None,
        frozen_frame_ratio: None,
        output_bytes: std::fs::metadata(candidate_path).map(|m| m.len()).unwrap_or(0),
        notes: Vec::new(),
        rejection_reason: None,
    };

    if eval.output_bytes < 512 {
        eval.rejection_reason = Some("Candidate file does not exist or is too small".into());
        return eval;
    }

    // 1. FFprobe structural validation
    let probe_out = Command::new(ffprobe_path)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(candidate_path)
        .output();

    let Ok(po) = probe_out else {
        eval.rejection_reason = Some("Failed to invoke ffprobe on candidate".into());
        return eval;
    };
    if !po.status.success() {
        eval.rejection_reason = Some("FFprobe could not parse candidate container".into());
        return eval;
    }
    let Ok(pjson) = serde_json::from_slice::<serde_json::Value>(&po.stdout) else {
        eval.rejection_reason = Some("Invalid JSON from ffprobe".into());
        return eval;
    };

    let streams = pjson
        .get("streams")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let v_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"));
    let Some(vs) = v_stream else {
        eval.rejection_reason = Some("No video stream found in candidate".into());
        return eval;
    };

    eval.width = vs.get("width").and_then(|v| v.as_u64()).map(|v| v as u32);
    eval.height = vs.get("height").and_then(|v| v.as_u64()).map(|v| v as u32);
    eval.recovered_duration_secs = vs
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            pjson
                .get("format")
                .and_then(|f| f.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        });

    if let Some(a_stream) = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"))
    {
        eval.audio_duration_secs = a_stream
            .get("duration")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        if let (Some(vd), Some(ad)) = (eval.recovered_duration_secs, eval.audio_duration_secs) {
            eval.av_sync_diff_secs = Some((vd - ad).abs());
        }
    }

    if eval.width.unwrap_or(0) == 0 || eval.height.unwrap_or(0) == 0 {
        eval.rejection_reason = Some("Invalid video resolution (0x0)".into());
        return eval;
    }

    // 2. Count decoded video frames & check decode error log (`ffmpeg -v error -i <cand> -map 0:v -map 0:a? -f null -`)
    let decode_res = Command::new(ffmpeg_path)
        .args([
            "-v",
            "error",
            "-stats",
            "-i",
        ])
        .arg(candidate_path)
        .args(["-map", "0:v:0", "-map", "0:a?", "-f", "null", "-"])
        .output();

    if let Ok(do_out) = decode_res {
        let err_str = String::from_utf8_lossy(&do_out.stderr);
        let mut errs = 0u64;
        for line in err_str.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("frame=") {
                continue;
            }
            errs += 1;
        }
        eval.decode_error_count = errs;
        if let Some(frames) = extract_last_frame_count(&err_str) {
            eval.decoded_video_frames = frames;
        }
        if !do_out.status.success() && eval.decoded_video_frames == 0 {
            eval.rejection_reason = Some(format!(
                "FFmpeg failed full decode verification: {}",
                err_str.lines().take(3).collect::<Vec<_>>().join(" | ")
            ));
            return eval;
        }
    }

    // Also count packets / IDR / DTS monotonicity via ffprobe `-show_packets` (sampled or full if fast)
    if let Ok(pk_out) = Command::new(ffprobe_path)
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=dts,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(candidate_path)
        .output()
    {
        if pk_out.status.success() {
            let txt = String::from_utf8_lossy(&pk_out.stdout);
            let mut pkt_count = 0u64;
            let mut idrs = 0u64;
            let mut prev_dts: Option<i64> = None;
            let mut monotonic = true;
            for line in txt.lines() {
                let mut parts = line.split(',');
                let dts_opt = parts.next().and_then(|s| s.trim().parse::<i64>().ok());
                let flags = parts.next().unwrap_or("");
                if let Some(dts) = dts_opt {
                    if let Some(pd) = prev_dts {
                        if dts < pd {
                            monotonic = false;
                        }
                    }
                    prev_dts = Some(dts);
                }
                if flags.contains('K') {
                    idrs += 1;
                }
                pkt_count += 1;
            }
            if eval.decoded_video_frames == 0 {
                eval.decoded_video_frames = pkt_count;
            }
            eval.idr_frames = idrs;
            eval.timestamps_monotonic = monotonic;
        }
    }

    if eval.decoded_video_frames == 0 {
        eval.rejection_reason = Some("Zero video frames decoded".into());
        return eval;
    }

    // 3. Optional blackdetect / freezedetect scan (on deep mode or final validation, capped cleanly)
    if deep_checks {
        if let Some(dur) = eval.recovered_duration_secs {
            if dur > 0.2 {
                if let Ok(bf_out) = Command::new(ffmpeg_path)
                    .args(["-v", "info", "-t", "60", "-i"])
                    .arg(candidate_path)
                    .args(["-vf", "blackdetect=d=0.5:pix_th=0.10", "-an", "-f", "null", "-"])
                    .output()
                {
                    let log = String::from_utf8_lossy(&bf_out.stderr);
                    let mut black_secs = 0.0f64;
                    for line in log.lines() {
                        if line.contains("black_duration:") {
                            if let Some(val) = line.split("black_duration:").nth(1) {
                                if let Ok(d) = val.split_whitespace().next().unwrap_or("0").parse::<f64>() {
                                    black_secs += d;
                                }
                            }
                        }
                    }
                    let denom = dur.min(60.0).max(0.1);
                    eval.black_frame_ratio = Some((black_secs / denom).min(1.0));
                }
            }
        }
    }

    // 4. Multi-factor scoring (NEVER ranked solely by byte size!)
    let mut score = 0.0f64;
    score += (eval.decoded_video_frames.min(10000) as f64 / 100.0).min(40.0);
    if eval.idr_frames > 0 {
        score += 15.0;
    }
    if eval.timestamps_monotonic {
        score += 10.0;
    } else {
        eval.notes.push("Non-monotonic DTS detected".into());
    }
    if let (Some(rw), Some(rh), Some(cw), Some(ch)) = (
        reference.and_then(|r| r.width),
        reference.and_then(|r| r.height),
        eval.width,
        eval.height,
    ) {
        if rw == cw && rh == ch {
            score += 15.0;
        } else {
            score -= 10.0;
            eval.notes
                .push(format!("Resolution {cw}x{ch} differs from reference {rw}x{rh}"));
        }
    } else if eval.width.unwrap_or(0) > 0 {
        score += 10.0;
    }
    if eval.audio_duration_secs.is_some() {
        score += 8.0;
        if let Some(diff) = eval.av_sync_diff_secs {
            if diff <= 2.0 {
                score += 7.0;
            } else {
                eval.notes.push(format!("A/V duration offset: {diff:.2}s"));
            }
        }
    }
    score += (eval.input_bytes_mapped_pct / 10.0).min(10.0);

    // Penalize decode errors & full-black false recoveries
    score -= (eval.decode_error_count.min(200) as f64) * 0.25;
    if let Some(br) = eval.black_frame_ratio {
        if br > 0.95 {
            score -= 25.0;
            eval.notes
                .push("Almost entirely black frames (>95% blackdetect); inspect contact sheet before trusting".into());
        }
    }

    eval.score = score.max(0.0);
    eval.succeeded = eval.decoded_video_frames > 0 && eval.decode_error_count < eval.decoded_video_frames.max(5);
    eval
}

pub fn generate_contact_sheet(
    ffmpeg_path: &Path,
    media_path: &Path,
    out_image: &Path,
    duration_secs: Option<f64>,
) -> Option<String> {
    let dur = duration_secs.unwrap_or(10.0).max(1.0);
    let interval = (dur / 6.0).max(0.5);
    let vf = format!("fps=1/{interval:.3},scale=320:-2,tile=3x2");
    let res = Command::new(ffmpeg_path)
        .args(["-v", "error", "-y", "-i"])
        .arg(media_path)
        .args(["-vf", &vf, "-frames:v", "1"])
        .arg(out_image)
        .output()
        .ok()?;
    if res.status.success() && out_image.is_file() {
        Some(out_image.to_string_lossy().to_string())
    } else {
        None
    }
}

fn extract_last_frame_count(stderr: &str) -> Option<u64> {
    let mut last = None;
    for part in stderr.split('\r').chain(stderr.lines()) {
        if let Some(pos) = part.find("frame=") {
            let rest = part[pos + 6..].trim_start();
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u64>() {
                last = Some(n);
            }
        }
    }
    last
}
