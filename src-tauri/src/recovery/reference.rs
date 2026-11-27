use crate::recovery::isobmff::{
    extract_mp4_codec_configs, parse_avcc_to_annex_b, parse_hvcc_to_annex_b,
};
use crate::recovery::{DiagnosisReport, ReferenceProfile, VrMetadataInfo};
use std::path::Path;
use std::process::Command;

pub fn analyze_references(
    ref_paths: &[&Path],
    ffprobe_path: Option<&Path>,
    diag: &DiagnosisReport,
) -> Vec<ReferenceProfile> {
    let detected_codec = crate::recovery::bitstream::probe_dominant_bitstream_codec(
        Path::new(&diag.file_path),
        &diag.mdat_ranges,
    );

    let mut profiles: Vec<ReferenceProfile> = ref_paths
        .iter()
        .filter(|p| p.is_file())
        .filter_map(|p| analyze_single_reference(p, ffprobe_path, diag).ok())
        .collect();

    if let Some(dc) = detected_codec {
        for p in &mut profiles {
            if let Some(ref vc) = p.video_codec {
                if vc == dc {
                    p.compatibility_score = (p.compatibility_score + 20.0).min(100.0);
                    p.compatibility_notes.push(format!("Video codec `{vc}` matches damaged bitstream NAL syntax"));
                } else {
                    p.compatibility_score = (p.compatibility_score - 50.0).max(0.0);
                    p.compatibility_notes.push(format!("Mismatch: reference is `{vc}` while damaged bitstream looks like `{dc}`"));
                }
            }
        }
    }

    // Sort highest compatibility score first
    profiles.sort_by(|a, b| {
        b.compatibility_score
            .partial_cmp(&a.compatibility_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    profiles
}

pub fn analyze_single_reference(
    path: &Path,
    ffprobe_path: Option<&Path>,
    diag: &DiagnosisReport,
) -> Result<ReferenceProfile, String> {
    let mut prof = ReferenceProfile {
        file_path: path.to_string_lossy().to_string(),
        compatibility_score: 50.0,
        compatibility_notes: Vec::new(),
        video_codec: None,
        codec_tag: None,
        width: None,
        height: None,
        fps_num: None,
        fps_den: None,
        fps: None,
        timescale: None,
        has_b_frames: false,
        pix_fmt: None,
        bit_depth: None,
        profile: None,
        level: None,
        color_primaries: None,
        color_transfer: None,
        color_space: None,
        color_range: None,
        sample_aspect_ratio: None,
        display_aspect_ratio: None,
        nal_length_size: 4,
        parameter_sets_annex_b: Vec::new(),
        avcc_or_hvcc_raw: None,
        audio_codec: None,
        audio_sample_rate: None,
        audio_channels: None,
        audio_sample_rate_index: None,
        audio_object_type: None,
        audio_specific_config: None,
        typical_gop_frames: None,
        avg_video_packet_bytes: None,
        avg_audio_packet_bytes: None,
        interleave_pattern: None,
        vr_info: VrMetadataInfo::default(),
    };

    // 1. Binary ISOBMFF parameter set extraction (`avcC`/`hvcC`/`esds`/`st3d`/`sv3d`/`mdhd`)
    if let Ok(bin_cfg) = extract_mp4_codec_configs(path) {
        if let Some(nls) = bin_cfg.nal_length_size {
            prof.nal_length_size = nls;
        }
        if !bin_cfg.parameter_sets_annex_b.is_empty() {
            prof.parameter_sets_annex_b = bin_cfg.parameter_sets_annex_b;
        }
        prof.avcc_or_hvcc_raw = bin_cfg.avcc_raw.or(bin_cfg.hvcc_raw);
        if let Some(asc) = bin_cfg.esds_asc {
            if let Some((aot, sr_idx, ch)) = decode_asc_fields(&asc) {
                prof.audio_object_type = Some(aot);
                prof.audio_sample_rate_index = Some(sr_idx);
                prof.audio_channels = Some(ch);
            }
            prof.audio_specific_config = Some(asc);
        }
        prof.timescale = bin_cfg.timescale;
        prof.vr_info = bin_cfg.vr_info;
    }

    // 2. FFprobe stream + packet sample statistics
    if let Some(fp) = ffprobe_path {
        if let Ok(out) = Command::new(fp)
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
        {
            if out.status.success() {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    apply_ffprobe_streams(&json, &mut prof);
                }
            }
        }

        // Probe first ~120 packets for GOP, packet sizes, and A/V interleaving pattern
        if let Ok(pkt_out) = Command::new(fp)
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_packets",
                "-read_intervals",
                "%+#120",
            ])
            .arg(path)
            .output()
        {
            if pkt_out.status.success() {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&pkt_out.stdout) {
                    apply_packet_stats(&json, &mut prof);
                }
            }
        }
    }

    // Populate audio sampling index from sample rate if `esds` was absent
    if prof.audio_sample_rate_index.is_none() {
        if let Some(sr) = prof.audio_sample_rate {
            prof.audio_sample_rate_index = sample_rate_to_index(sr);
        }
    }
    if prof.audio_object_type.is_none() && prof.audio_codec.as_deref() == Some("aac") {
        prof.audio_object_type = Some(2); // AAC-LC
    }

    // Compute compatibility score against damaged diagnosis
    let mut score = 50.0f64;
    if !prof.parameter_sets_annex_b.is_empty() {
        score += 25.0;
        prof.compatibility_notes
            .push("Provides valid SPS/PPS/VPS decoder configuration".into());
    } else {
        score -= 30.0;
        prof.compatibility_notes
            .push("Missing video extradata/SPS-PPS".into());
    }
    if matches!(prof.video_codec.as_deref(), Some("h264") | Some("hevc")) {
        score += 15.0;
    }
    if diag.has_ftyp && path.extension().and_then(|e| e.to_str()).map(|s| s.eq_ignore_ascii_case("mp4") || s.eq_ignore_ascii_case("mov")).unwrap_or(false) {
        score += 10.0;
        prof.compatibility_notes
            .push("Matches MP4/MOV container family".into());
    }
    if diag.file_size > 10_000_000 && prof.width.unwrap_or(0) >= 720 {
        score += 5.0;
    }
    if prof.vr_info.is_stereoscopic {
        prof.compatibility_notes.push(format!(
            "VR/Stereoscopic reference detected ({:?}, {:?})",
            prof.vr_info.stereo_mode, prof.vr_info.projection
        ));
    }
    prof.compatibility_score = score.clamp(0.0, 100.0);

    Ok(prof)
}

fn apply_ffprobe_streams(json: &serde_json::Value, prof: &mut ReferenceProfile) {
    let Some(streams) = json.get("streams").and_then(|v| v.as_array()) else {
        return;
    };
    for s in streams {
        let ctype = s.get("codec_type").and_then(|v| v.as_str()).unwrap_or("");
        if ctype == "video" && prof.video_codec.is_none() {
            prof.video_codec = s.get("codec_name").and_then(|v| v.as_str()).map(str::to_string);
            prof.codec_tag = s.get("codec_tag_string").and_then(|v| v.as_str()).map(str::to_string);
            prof.width = s.get("width").and_then(|v| v.as_u64()).map(|v| v as u32);
            prof.height = s.get("height").and_then(|v| v.as_u64()).map(|v| v as u32);
            prof.has_b_frames = s.get("has_b_frames").and_then(|v| v.as_u64()).unwrap_or(0) > 0;
            prof.pix_fmt = s.get("pix_fmt").and_then(|v| v.as_str()).map(str::to_string);
            prof.profile = s.get("profile").and_then(|v| v.as_str()).map(str::to_string);
            prof.level = s.get("level").and_then(|v| v.as_i64()).map(|v| v as i32);
            prof.color_primaries = s.get("color_primaries").and_then(|v| v.as_str()).map(str::to_string);
            prof.color_transfer = s.get("color_transfer").and_then(|v| v.as_str()).map(str::to_string);
            prof.color_space = s.get("color_space").and_then(|v| v.as_str()).map(str::to_string);
            prof.color_range = s.get("color_range").and_then(|v| v.as_str()).map(str::to_string);
            prof.sample_aspect_ratio = s
                .get("sample_aspect_ratio")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            prof.display_aspect_ratio = s
                .get("display_aspect_ratio")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            if let Some(b) = s
                .get("bits_per_raw_sample")
                .and_then(|v| v.as_str())
                .and_then(|st| st.parse::<u8>().ok())
            {
                prof.bit_depth = Some(b);
            } else if let Some(pf) = prof.pix_fmt.as_deref() {
                if pf.contains("10le") || pf.contains("10be") {
                    prof.bit_depth = Some(10);
                } else {
                    prof.bit_depth = Some(8);
                }
            }

            if let Some(nls) = s
                .get("nal_length_size")
                .and_then(|v| v.as_str())
                .and_then(|st| st.parse::<u8>().ok())
            {
                if (1..=4).contains(&nls) {
                    prof.nal_length_size = nls;
                }
            }

            if let Some(rate_str) = s
                .get("avg_frame_rate")
                .or_else(|| s.get("r_frame_rate"))
                .and_then(|v| v.as_str())
            {
                if let Some((num, den)) = parse_fraction(rate_str) {
                    prof.fps_num = Some(num);
                    prof.fps_den = Some(den);
                    prof.fps = Some(num as f64 / den as f64);
                }
            }

            // Detect spherical / stereo3d side data from FFprobe
            if let Some(sd_list) = s.get("side_data_list").and_then(|v| v.as_array()) {
                for sd in sd_list {
                    let stype = sd.get("side_data_type").and_then(|v| v.as_str()).unwrap_or("");
                    if stype.contains("Stereo 3D") {
                        prof.vr_info.is_stereoscopic = true;
                        if let Some(mode) = sd.get("type").and_then(|v| v.as_str()) {
                            prof.vr_info.stereo_mode = Some(mode.to_string());
                        }
                    } else if stype.contains("Spherical Mapping") {
                        if let Some(proj) = sd.get("projection").and_then(|v| v.as_str()) {
                            prof.vr_info.projection = Some(proj.to_string());
                        }
                    }
                }
            }
            // Heuristic 2:1 / 1:1 SBS / TB VR frame resolution check if tags weren't explicit
            if let (Some(w), Some(h)) = (prof.width, prof.height) {
                if w >= 2560 && (w == h * 2 || w == h) && prof.vr_info.projection.is_none() {
                    // Common VR aspect ratio; flag note without overclaiming
                    prof.compatibility_notes.push(format!(
                        "Resolution {w}x{h} matches typical VR/stereoscopic layout if content is 3D/180/360"
                    ));
                }
            }

            // Check extradata fallback if binary avcC/hvcC wasn't already parsed
            if prof.parameter_sets_annex_b.is_empty() {
                if let Some(raw_bytes) = prof.avcc_or_hvcc_raw.as_ref() {
                    if prof.video_codec.as_deref() == Some("h264") {
                        if let Some((nl, ab)) = parse_avcc_to_annex_b(raw_bytes) {
                            prof.nal_length_size = nl;
                            prof.parameter_sets_annex_b = ab;
                        }
                    } else if prof.video_codec.as_deref() == Some("hevc") {
                        if let Some((nl, ab)) = parse_hvcc_to_annex_b(raw_bytes) {
                            prof.nal_length_size = nl;
                            prof.parameter_sets_annex_b = ab;
                        }
                    }
                }
            }
        } else if ctype == "audio" && prof.audio_codec.is_none() {
            prof.audio_codec = s.get("codec_name").and_then(|v| v.as_str()).map(str::to_string);
            prof.audio_sample_rate = s
                .get("sample_rate")
                .and_then(|v| v.as_str())
                .and_then(|st| st.parse::<u32>().ok());
            prof.audio_channels = s.get("channels").and_then(|v| v.as_u64()).map(|v| v as u16);
        }
    }
}

fn apply_packet_stats(json: &serde_json::Value, prof: &mut ReferenceProfile) {
    let Some(packets) = json.get("packets").and_then(|v| v.as_array()) else {
        return;
    };
    let mut v_sizes = Vec::new();
    let mut a_sizes = Vec::new();
    let mut keyframes = Vec::new();
    let mut v_idx = 0u32;
    let mut pattern = Vec::new();

    for p in packets {
        let ctype = p.get("codec_type").and_then(|v| v.as_str()).unwrap_or("");
        let sz = p
            .get("size")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if ctype == "video" {
            v_sizes.push(sz);
            let flags = p.get("flags").and_then(|v| v.as_str()).unwrap_or("");
            if flags.contains('K') {
                keyframes.push(v_idx);
            }
            v_idx += 1;
            if pattern.len() < 16 {
                pattern.push('V');
            }
        } else if ctype == "audio" {
            a_sizes.push(sz);
            if pattern.len() < 16 {
                pattern.push('A');
            }
        }
    }

    if !v_sizes.is_empty() {
        prof.avg_video_packet_bytes = Some(v_sizes.iter().sum::<u64>() / v_sizes.len() as u64);
    }
    if !a_sizes.is_empty() {
        prof.avg_audio_packet_bytes = Some(a_sizes.iter().sum::<u64>() / a_sizes.len() as u64);
    }
    if keyframes.len() >= 2 {
        let mut diffs = Vec::new();
        for w in keyframes.windows(2) {
            diffs.push(w[1] - w[0]);
        }
        prof.typical_gop_frames = Some(diffs.iter().sum::<u32>() / diffs.len() as u32);
    }
    if !pattern.is_empty() {
        prof.interleave_pattern = Some(pattern.into_iter().collect());
    }
}

fn parse_fraction(s: &str) -> Option<(u32, u32)> {
    let (n, d) = s.split_once('/')?;
    let num: u32 = n.trim().parse().ok()?;
    let den: u32 = d.trim().parse().ok()?;
    if num > 0 && den > 0 {
        Some((num, den))
    } else {
        None
    }
}

pub fn decode_asc_fields(asc: &[u8]) -> Option<(u8, u8, u16)> {
    if asc.len() < 2 {
        return None;
    }
    let val = u16::from_be_bytes([asc[0], asc[1]]);
    let aot = ((val >> 11) & 0x1F) as u8;
    let sr_idx = ((val >> 7) & 0x0F) as u8;
    let ch = ((val >> 3) & 0x0F) as u16;
    if sr_idx <= 12 && (1..=8).contains(&ch) {
        Some((aot, sr_idx, ch))
    } else {
        None
    }
}

pub fn sample_rate_to_index(sr: u32) -> Option<u8> {
    const TABLE: &[u32] = &[
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    TABLE.iter().position(|&r| r == sr).map(|i| i as u8)
}

/// Dump Annex B parameter sets from a healthy reference using FFmpeg bitstream filter
/// as a fallback when binary `avcC`/`hvcC` wasn't directly available (e.g. MKV/TS reference).
pub fn extract_annex_b_param_sets_via_ffmpeg(
    ffmpeg_path: &Path,
    ref_path: &Path,
    codec: &str,
) -> Option<Vec<u8>> {
    let tmp = tempfile::NamedTempFile::new().ok()?;
    let (bsf, fmt) = match codec {
        "h264" => ("h264_mp4toannexb", "h264"),
        "hevc" => ("hevc_mp4toannexb", "hevc"),
        _ => return None,
    };
    let out = Command::new(ffmpeg_path)
        .args(["-v", "error", "-y", "-i"])
        .arg(ref_path)
        .args(["-map", "0:v:0", "-c:v", "copy", "-frames:v", "1", "-bsf:v", bsf, "-f", fmt])
        .arg(tmp.path())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes = std::fs::read(tmp.path()).ok()?;
    // Extract ONLY parameter set NALs (SPS/PPS for H.264, VPS/SPS/PPS for HEVC)
    let mut param_bytes = Vec::new();
    let starts: Vec<usize> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(i, w)| if w == [0, 0, 0, 1] { Some(i) } else { None })
        .collect();
    for (idx, &pos) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(bytes.len());
        if pos + 4 < end {
            let hdr = bytes[pos + 4];
            let keep = if codec == "h264" {
                matches!(hdr & 0x1F, 7 | 8)
            } else {
                let nt = (hdr >> 1) & 0x3F;
                matches!(nt, 32 | 33 | 34)
            };
            if keep {
                param_bytes.extend_from_slice(&bytes[pos..end]);
            }
        }
    }
    if param_bytes.is_empty() {
        None
    } else {
        Some(param_bytes)
    }
}
