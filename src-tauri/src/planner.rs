use crate::models::*;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const BLOCKED_PLAYLIST_EXTS: &[&str] = &[
    "m3u", "m3u8", "pls", "xspf", "mpd", "ism", "isml", "asx", "ram", "strm",
];

pub const VIDEO_HINT_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "avi", "webm", "mts", "m2ts", "ts", "mpg", "mpeg", "wmv",
    "flv", "3gp", "3g2", "vob", "ogv", "mxf", "dv", "divx", "asf", "rmvb",
];

pub fn is_blocked_input_path(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    if s.contains("://") && !s.starts_with("file://") {
        return Some("Network protocols and remote URLs are blocked.".to_string());
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_ascii_lowercase();
        if BLOCKED_PLAYLIST_EXTS.contains(&lower.as_str()) {
            return Some(format!("Playlist or manifest files (.{lower}) are blocked."));
        }
    }
    None
}

pub fn is_video_hint_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_HINT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn probe_file(ffprobe_path: &Path, input_path: &Path) -> Result<ProbeResult, String> {
    if let Some(reason) = is_blocked_input_path(input_path) {
        return Err(reason);
    }
    let meta = std::fs::symlink_metadata(input_path)
        .map_err(|e| format!("Cannot access input file: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("Input must be a regular local file (not a directory or symlink).".to_string());
    }

    let mut cmd = Command::new(ffprobe_path);
    cmd.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_format",
        "-show_streams",
        "-show_chapters",
        "-of",
        "json",
    ])
    .arg(input_path);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("ffprobe exited with {:?}", out.status.code())
        } else {
            err
        });
    }

    let root: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("Invalid ffprobe JSON output: {e}"))?;
    parse_ffprobe_json(&root, meta.len())
}

pub fn parse_ffprobe_json(root: &Value, file_size_fallback: u64) -> Result<ProbeResult, String> {
    let fmt = root
        .get("format")
        .ok_or_else(|| "ffprobe JSON missing format object".to_string())?;
    let format_name = fmt
        .get("format_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    // Reject playlist/concat/network demuxers if ffprobe detected them by content
    let lower_fmt = format_name.to_ascii_lowercase();
    if lower_fmt.contains("hls")
        || lower_fmt.contains("dash")
        || lower_fmt.contains("concat")
        || lower_fmt.contains("tty")
    {
        return Err(format!("Demuxer '{format_name}' is not supported for local files."));
    }

    let format = FormatInfo {
        format_name,
        format_long_name: fmt
            .get("format_long_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        duration_secs: parse_opt_f64(fmt.get("duration")),
        size_bytes: parse_opt_u64(fmt.get("size")).or(Some(file_size_fallback)),
        bit_rate: parse_opt_u64(fmt.get("bit_rate")),
        tags: parse_tags(fmt.get("tags")),
    };

    let mut streams = Vec::new();
    if let Some(arr) = root.get("streams").and_then(|v| v.as_array()) {
        for s in arr {
            let index = s.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let codec_type = s
                .get("codec_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let codec_name = s
                .get("codec_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let tags = parse_tags(s.get("tags"));
            let disp_val = s.get("disposition");
            let disposition = DispositionInfo {
                default: disp_flag(disp_val, "default"),
                dub: disp_flag(disp_val, "dub"),
                original: disp_flag(disp_val, "original"),
                comment: disp_flag(disp_val, "comment"),
                lyrics: disp_flag(disp_val, "lyrics"),
                karaoke: disp_flag(disp_val, "karaoke"),
                forced: disp_flag(disp_val, "forced"),
                hearing_impaired: disp_flag(disp_val, "hearing_impaired"),
                visual_impaired: disp_flag(disp_val, "visual_impaired"),
                attached_pic: disp_flag(disp_val, "attached_pic"),
            };

            let mut side_data_types = Vec::new();
            let mut rotation: Option<i32> = tags
                .get("rotate")
                .and_then(|r| r.trim().parse::<i32>().ok());
            if let Some(sd_arr) = s.get("side_data_list").and_then(|v| v.as_array()) {
                for sd in sd_arr {
                    if let Some(t) = sd.get("side_data_type").and_then(|v| v.as_str()) {
                        side_data_types.push(t.to_string());
                    }
                    if rotation.is_none() {
                        if let Some(rot) = sd.get("rotation").and_then(|v| v.as_i64()) {
                            rotation = Some(rot as i32);
                        } else if let Some(rotf) = sd.get("rotation").and_then(|v| v.as_f64()) {
                            rotation = Some(rotf.round() as i32);
                        }
                    }
                }
            }

            let pix_fmt = s
                .get("pix_fmt")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let bit_depth = parse_opt_u64(s.get("bits_per_raw_sample"))
                .map(|b| b as u8)
                .or_else(|| pix_fmt.as_deref().and_then(infer_bit_depth_from_pix_fmt));

            let r_fr = s
                .get("r_frame_rate")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let avg_fr = s
                .get("avg_frame_rate")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let is_vfr = detect_vfr(r_fr.as_deref(), avg_fr.as_deref());

            let language = find_tag_ci(&tags, "language");
            let title = find_tag_ci(&tags, "title");

            streams.push(StreamInfo {
                index,
                codec_type,
                codec_name,
                codec_long_name: s
                    .get("codec_long_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                profile: s.get("profile").and_then(|v| v.as_str()).map(str::to_string),
                width: s.get("width").and_then(|v| v.as_u64()).map(|n| n as u32),
                height: s.get("height").and_then(|v| v.as_u64()).map(|n| n as u32),
                sample_aspect_ratio: s
                    .get("sample_aspect_ratio")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                display_aspect_ratio: s
                    .get("display_aspect_ratio")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                pix_fmt,
                bit_depth,
                color_range: s
                    .get("color_range")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                color_space: s
                    .get("color_space")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                color_transfer: s
                    .get("color_transfer")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                color_primaries: s
                    .get("color_primaries")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                field_order: s
                    .get("field_order")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                r_frame_rate: r_fr,
                avg_frame_rate: avg_fr,
                is_vfr,
                time_base: s
                    .get("time_base")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                duration_secs: parse_opt_f64(s.get("duration")).or(format.duration_secs),
                bit_rate: parse_opt_u64(s.get("bit_rate")),
                channels: s.get("channels").and_then(|v| v.as_u64()).map(|n| n as u32),
                channel_layout: s
                    .get("channel_layout")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                sample_rate: parse_opt_u64(s.get("sample_rate")).map(|n| n as u32),
                nb_frames: parse_opt_u64(s.get("nb_frames")),
                language,
                title,
                rotation,
                side_data_types,
                disposition,
                tags,
            });
        }
    }

    let mut chapters = Vec::new();
    if let Some(arr) = root.get("chapters").and_then(|v| v.as_array()) {
        for c in arr {
            let tags = parse_tags(c.get("tags"));
            chapters.push(ChapterInfo {
                id: c.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                start_time: c
                    .get("start_time")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                end_time: c
                    .get("end_time")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                title: find_tag_ci(&tags, "title"),
            });
        }
    }

    Ok(ProbeResult {
        format,
        streams,
        chapters,
    })
}

fn disp_flag(v: Option<&Value>, key: &str) -> i32 {
    v.and_then(|o| o.get(key))
        .and_then(|x| x.as_i64())
        .unwrap_or(0) as i32
}

fn parse_tags(v: Option<&Value>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(obj) = v.and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    }
    map
}

fn find_tag_ci(tags: &HashMap<String, String>, target: &str) -> Option<String> {
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(target))
        .map(|(_, v)| v.clone())
}

fn parse_opt_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn parse_opt_u64(v: Option<&Value>) -> Option<u64> {
    match v {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

pub fn parse_rational(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() == 2 {
        let num: f64 = parts[0].trim().parse().ok()?;
        let den: f64 = parts[1].trim().parse().ok()?;
        if den > 0.0 && num > 0.0 {
            return Some(num / den);
        }
    } else if let Ok(v) = s.trim().parse::<f64>() {
        if v > 0.0 {
            return Some(v);
        }
    }
    None
}

fn detect_vfr(r_fr: Option<&str>, avg_fr: Option<&str>) -> bool {
    match (r_fr.and_then(parse_rational), avg_fr.and_then(parse_rational)) {
        (Some(r), Some(a)) => (r - a).abs() / r.max(a) > 0.02,
        _ => false,
    }
}

fn infer_bit_depth_from_pix_fmt(pf: &str) -> Option<u8> {
    if pf.contains("10") || pf.starts_with("p010") {
        Some(10)
    } else if pf.contains("12") || pf.starts_with("p012") {
        Some(12)
    } else if pf.contains("16") {
        Some(16)
    } else if pf.starts_with("yuv") || pf.starts_with("rgb") || pf.starts_with("nv") {
        Some(8)
    } else {
        None
    }
}

pub fn build_track_plan(
    input_path: &Path,
    probe: &ProbeResult,
    recipe: &EncodeRecipe,
    overrides: &CompatOverrides,
) -> TrackPlan {
    let mut issues: Vec<CompatIssue> = Vec::new();
    let mut tracks: Vec<TrackDecision> = Vec::new();
    let mut target_pix_fmt: Option<String> = None;
    let mut preserve_rotation = false;

    let moving_videos: Vec<&StreamInfo> = probe
        .streams
        .iter()
        .filter(|s| s.codec_type == "video" && s.disposition.attached_pic == 0)
        .collect();

    if moving_videos.is_empty() {
        issues.push(CompatIssue {
            kind: CompatIssueKind::NoVideoStream,
            stream_index: None,
            summary: "No moving video stream found".to_string(),
            details: "This file does not contain a standard moving video stream.".to_string(),
            can_use_mkv: false,
            can_remux: false,
            can_opt_in_fallback: false,
            fallback_description: None,
        });
    }

    let selected_video_idx: Option<usize> = if moving_videos.len() > 1 {
        if let Some(chosen) = overrides.selected_video_stream {
            if moving_videos.iter().any(|s| s.index == chosen) {
                Some(chosen)
            } else {
                None
            }
        } else {
            issues.push(CompatIssue {
                kind: CompatIssueKind::MultipleMovingVideoStreams,
                stream_index: None,
                summary: format!("Multiple video streams ({}) require selection", moving_videos.len()),
                details: "Multi-angle or multi-track video inputs are blocked in v1 until you select which video stream to process.".to_string(),
                can_use_mkv: false,
                can_remux: true,
                can_opt_in_fallback: false,
                fallback_description: None,
            });
            None
        }
    } else {
        moving_videos.first().map(|s| s.index)
    };

    let mut out_audio_idx = 0usize;
    let mut out_sub_idx = 0usize;
    let mut mapped_streams: Vec<(usize, TrackAction, Option<String>)> = Vec::new();
    let mut encoded_video_source_stream: Option<&StreamInfo> = None;

    for s in &probe.streams {
        let is_attached_pic = s.codec_type == "video" && s.disposition.attached_pic != 0;

        if s.codec_type == "video" && !is_attached_pic {
            if Some(s.index) != selected_video_idx {
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::Omit,
                    reason: Some("Unselected secondary video stream".to_string()),
                    is_attached_pic: false,
                    causes_lossy_conversion: false,
                    causes_omission: true,
                });
                continue;
            }

            if recipe.family == PresetFamily::Remux {
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::Copy,
                    reason: Some("Stream copy (Remux)".to_string()),
                    is_attached_pic: false,
                    causes_lossy_conversion: false,
                    causes_omission: false,
                });
                mapped_streams.push((s.index, TrackAction::Copy, None));
                continue;
            }

            // Certify progressive SDR 8/10-bit 4:2:0
            let mut unsupported_reasons: Vec<String> = Vec::new();
            if let Some(fo) = s.field_order.as_deref() {
                if matches!(fo, "tt" | "bb" | "tb" | "bt") {
                    unsupported_reasons.push(format!("Interlaced video (field_order={fo})"));
                }
            }
            if let Some(tr) = s.color_transfer.as_deref() {
                if matches!(tr, "smpte2084" | "arib-std-b67") {
                    unsupported_reasons.push(format!("HDR transfer ({tr})"));
                }
            }
            if let Some(cp) = s.color_primaries.as_deref() {
                if cp == "bt2020" {
                    unsupported_reasons.push(format!("HDR/wide-gamut primaries ({cp})"));
                }
            }
            if let Some(cs) = s.color_space.as_deref() {
                if matches!(cs, "bt2020nc" | "bt2020c") {
                    unsupported_reasons.push(format!("HDR/wide-gamut colorspace ({cs})"));
                }
            }
            for sd in &s.side_data_types {
                let sdl = sd.to_ascii_lowercase();
                if sdl.contains("mastering display")
                    || sdl.contains("content light")
                    || sdl.contains("dovi")
                    || sdl.contains("hdr")
                    || sdl.contains("2094")
                {
                    unsupported_reasons.push(format!("HDR/dynamic metadata ({sd})"));
                }
            }
            let pf = s.pix_fmt.as_deref().unwrap_or("");
            let certified_pix = match pf {
                "yuv420p" | "yuvj420p" => Some("yuv420p".to_string()),
                "yuv420p10le" | "yuv420p10be" | "p010le" => Some("yuv420p10le".to_string()),
                "" => None,
                other => {
                    unsupported_reasons.push(format!(
                        "Unsupported pixel format '{other}' (certified: 8/10-bit SDR 4:2:0)"
                    ));
                    None
                }
            };
            if pf.is_empty() {
                unsupported_reasons.push("Unknown pixel format".to_string());
            }

            if !unsupported_reasons.is_empty() {
                issues.push(CompatIssue {
                    kind: CompatIssueKind::UnsupportedVideoFeature,
                    stream_index: Some(s.index),
                    summary: "Needs review: gated video format".to_string(),
                    details: format!(
                        "{}. Encoding is gated to prevent silent tone-mapping, deinterlacing, or chroma loss. You can Remux (stream copy) or keep the original.",
                        unsupported_reasons.join("; ")
                    ),
                    can_use_mkv: false,
                    can_remux: true,
                    can_opt_in_fallback: false,
                    fallback_description: None,
                });
            }

            target_pix_fmt = if recipe.family == PresetFamily::Archive && recipe.ten_bit && certified_pix.is_some() {
                Some("yuv420p10le".to_string())
            } else {
                certified_pix
            };
            if s.rotation.unwrap_or(0) != 0 {
                preserve_rotation = true;
            }
            encoded_video_source_stream = Some(s);

            tracks.push(TrackDecision {
                stream_index: s.index,
                codec_type: s.codec_type.clone(),
                codec_name: s.codec_name.clone(),
                language: s.language.clone(),
                title: s.title.clone(),
                action: TrackAction::EncodeVideo,
                reason: Some(format!(
                    "Encode with {} ({})",
                    recipe.codec.encoder_name(),
                    target_pix_fmt.as_deref().unwrap_or("review")
                )),
                is_attached_pic: false,
                causes_lossy_conversion: true,
                causes_omission: false,
            });
            mapped_streams.push((s.index, TrackAction::EncodeVideo, None));
        } else if is_attached_pic {
            // Cover art: MKV supports attached pics cleanly; MP4 supports mjpeg/png attached pics too in most cases,
            // though some non-standard attached pics can be problematic; copy as attached_pic.
            tracks.push(TrackDecision {
                stream_index: s.index,
                codec_type: "cover".to_string(),
                codec_name: s.codec_name.clone(),
                language: s.language.clone(),
                title: s.title.clone(),
                action: TrackAction::Copy,
                reason: Some("Copy cover artwork".to_string()),
                is_attached_pic: true,
                causes_lossy_conversion: false,
                causes_omission: false,
            });
            mapped_streams.push((s.index, TrackAction::Copy, None));
        } else if s.codec_type == "audio" {
            let compatible = match recipe.container {
                ContainerFormat::Mkv => true,
                ContainerFormat::Mp4 => is_mp4_compatible_audio(&s.codec_name),
            };
            if compatible {
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::Copy,
                    reason: Some("Copy compatible audio".to_string()),
                    is_attached_pic: false,
                    causes_lossy_conversion: false,
                    causes_omission: false,
                });
                mapped_streams.push((s.index, TrackAction::Copy, Some(format!("a:{out_audio_idx}"))));
                out_audio_idx += 1;
            } else if overrides.allow_mp4_audio_aac_fallback {
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::TranscodeAudioAac,
                    reason: Some(format!("Transcode {} → AAC for MP4", s.codec_name)),
                    is_attached_pic: false,
                    causes_lossy_conversion: true,
                    causes_omission: false,
                });
                mapped_streams.push((
                    s.index,
                    TrackAction::TranscodeAudioAac,
                    Some(format!("a:{out_audio_idx}")),
                ));
                out_audio_idx += 1;
            } else if overrides.omit_incompatible_tracks {
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::Omit,
                    reason: Some(format!("Omitted incompatible MP4 audio ({})", s.codec_name)),
                    is_attached_pic: false,
                    causes_lossy_conversion: false,
                    causes_omission: true,
                });
            } else {
                issues.push(CompatIssue {
                    kind: CompatIssueKind::Mp4IncompatibleAudio,
                    stream_index: Some(s.index),
                    summary: format!("MP4 does not accept '{}' audio directly", s.codec_name),
                    details: format!(
                        "Stream #{} ({}) cannot be copied into MP4 cleanly. Use MKV to copy lossless/bitstream audio unchanged, or opt in to AAC conversion.",
                        s.index, s.codec_name
                    ),
                    can_use_mkv: true,
                    can_remux: false,
                    can_opt_in_fallback: true,
                    fallback_description: Some(format!("Transcode {} audio to AAC (lossy)", s.codec_name)),
                });
                tracks.push(TrackDecision {
                    stream_index: s.index,
                    codec_type: s.codec_type.clone(),
                    codec_name: s.codec_name.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    action: TrackAction::Copy,
                    reason: Some("Blocked: incompatible with MP4".to_string()),
                    is_attached_pic: false,
                    causes_lossy_conversion: false,
                    causes_omission: false,
                });
            }
        } else if s.codec_type == "subtitle" {
            match recipe.container {
                ContainerFormat::Mkv => {
                    tracks.push(TrackDecision {
                        stream_index: s.index,
                        codec_type: s.codec_type.clone(),
                        codec_name: s.codec_name.clone(),
                        language: s.language.clone(),
                        title: s.title.clone(),
                        action: TrackAction::Copy,
                        reason: Some("Copy subtitles".to_string()),
                        is_attached_pic: false,
                        causes_lossy_conversion: false,
                        causes_omission: false,
                    });
                    mapped_streams.push((s.index, TrackAction::Copy, Some(format!("s:{out_sub_idx}"))));
                    out_sub_idx += 1;
                }
                ContainerFormat::Mp4 => {
                    let lower = s.codec_name.to_ascii_lowercase();
                    if lower == "mov_text" {
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::Copy,
                            reason: Some("Copy mov_text subtitles".to_string()),
                            is_attached_pic: false,
                            causes_lossy_conversion: false,
                            causes_omission: false,
                        });
                        mapped_streams.push((s.index, TrackAction::Copy, Some(format!("s:{out_sub_idx}"))));
                        out_sub_idx += 1;
                    } else if is_convertible_text_sub(&lower) && overrides.allow_mp4_sub_mov_text_fallback {
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::ConvertSubMovText,
                            reason: Some(format!("Convert {} → mov_text (loses styling)", s.codec_name)),
                            is_attached_pic: false,
                            causes_lossy_conversion: true,
                            causes_omission: false,
                        });
                        mapped_streams.push((
                            s.index,
                            TrackAction::ConvertSubMovText,
                            Some(format!("s:{out_sub_idx}")),
                        ));
                        out_sub_idx += 1;
                    } else if overrides.omit_incompatible_tracks {
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::Omit,
                            reason: Some(format!("Omitted incompatible MP4 subtitle ({})", s.codec_name)),
                            is_attached_pic: false,
                            causes_lossy_conversion: false,
                            causes_omission: true,
                        });
                    } else {
                        let convertible = is_convertible_text_sub(&lower);
                        issues.push(CompatIssue {
                            kind: CompatIssueKind::Mp4IncompatibleSubtitle,
                            stream_index: Some(s.index),
                            summary: format!("MP4 cannot copy '{}' subtitles", s.codec_name),
                            details: if convertible {
                                format!(
                                    "Subtitle stream #{} ({}) requires MKV to keep styling/formatting, or explicit conversion to mov_text.",
                                    s.index, s.codec_name
                                )
                            } else {
                                format!(
                                    "Bitmap subtitle stream #{} ({}) cannot be stored in MP4. Use MKV to keep it or omit the track.",
                                    s.index, s.codec_name
                                )
                            },
                            can_use_mkv: true,
                            can_remux: false,
                            can_opt_in_fallback: convertible,
                            fallback_description: if convertible {
                                Some(format!("Convert {} text to mov_text (ASS styling lost)", s.codec_name))
                            } else {
                                None
                            },
                        });
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::Copy,
                            reason: Some("Blocked: incompatible with MP4".to_string()),
                            is_attached_pic: false,
                            causes_lossy_conversion: false,
                            causes_omission: false,
                        });
                    }
                }
            }
        } else if s.codec_type == "attachment" {
            match recipe.container {
                ContainerFormat::Mkv => {
                    tracks.push(TrackDecision {
                        stream_index: s.index,
                        codec_type: s.codec_type.clone(),
                        codec_name: s.codec_name.clone(),
                        language: s.language.clone(),
                        title: s.title.clone(),
                        action: TrackAction::Copy,
                        reason: Some("Copy attachment (font/asset)".to_string()),
                        is_attached_pic: false,
                        causes_lossy_conversion: false,
                        causes_omission: false,
                    });
                    mapped_streams.push((s.index, TrackAction::Copy, None));
                }
                ContainerFormat::Mp4 => {
                    if overrides.omit_incompatible_tracks {
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::Omit,
                            reason: Some("Omitted attachment (unsupported in MP4)".to_string()),
                            is_attached_pic: false,
                            causes_lossy_conversion: false,
                            causes_omission: true,
                        });
                    } else {
                        issues.push(CompatIssue {
                            kind: CompatIssueKind::Mp4IncompatibleAttachment,
                            stream_index: Some(s.index),
                            summary: "MP4 does not support embedded font/file attachments".to_string(),
                            details: format!(
                                "Attachment stream #{} ({}) cannot be muxed into MP4. Use MKV to preserve fonts and attachments.",
                                s.index, s.codec_name
                            ),
                            can_use_mkv: true,
                            can_remux: false,
                            can_opt_in_fallback: false,
                            fallback_description: None,
                        });
                        tracks.push(TrackDecision {
                            stream_index: s.index,
                            codec_type: s.codec_type.clone(),
                            codec_name: s.codec_name.clone(),
                            language: s.language.clone(),
                            title: s.title.clone(),
                            action: TrackAction::Copy,
                            reason: Some("Blocked: attachment unsupported in MP4".to_string()),
                            is_attached_pic: false,
                            causes_lossy_conversion: false,
                            causes_omission: false,
                        });
                    }
                }
            }
        } else {
            // Data / unknown streams (e.g. timecode track tmcd in MOV): omit cleanly without failing if non-essential
            tracks.push(TrackDecision {
                stream_index: s.index,
                codec_type: s.codec_type.clone(),
                codec_name: s.codec_name.clone(),
                language: s.language.clone(),
                title: s.title.clone(),
                action: TrackAction::Omit,
                reason: Some(format!("Omitted non-AV {} stream", s.codec_type)),
                is_attached_pic: false,
                causes_lossy_conversion: false,
                causes_omission: true,
            });
        }
    }

    let is_ready = issues.is_empty();
    let has_omitted_tracks = tracks.iter().any(|t| t.causes_omission);
    let has_approved_conversions = tracks
        .iter()
        .any(|t| matches!(t.action, TrackAction::TranscodeAudioAac | TrackAction::ConvertSubMovText));

    let muxer = recipe.container.muxer().to_string();
    let argv = assemble_ffmpeg_argv(
        input_path,
        "${UNIQUE_PARTIAL}",
        recipe,
        &tracks,
        &mapped_streams,
        target_pix_fmt.as_deref(),
        preserve_rotation,
        encoded_video_source_stream,
    );
    let printable_command = format!("ffmpeg {}", shell_join_preview(&argv));

    TrackPlan {
        tracks,
        issues,
        is_ready,
        has_omitted_tracks,
        has_approved_conversions,
        argv,
        printable_command,
        target_pix_fmt,
        preserve_rotation,
        muxer,
    }
}

fn assemble_ffmpeg_argv(
    input_path: &Path,
    output_placeholder: &str,
    recipe: &EncodeRecipe,
    tracks: &[TrackDecision],
    mapped_streams: &[(usize, TrackAction, Option<String>)],
    target_pix_fmt: Option<&str>,
    preserve_rotation: bool,
    encoded_video: Option<&StreamInfo>,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-n".to_string(),
        "-xerror".to_string(),
        "-nostats".to_string(),
        "-progress".to_string(),
        "pipe:1".to_string(),
        "-protocol_whitelist".to_string(),
        "file,pipe".to_string(),
    ];

    if preserve_rotation && recipe.family == PresetFamily::Archive {
        args.push("-noautorotate".to_string());
    }

    args.push("-i".to_string());
    args.push(input_path.to_string_lossy().to_string());

    for (idx, _, _) in mapped_streams {
        args.push("-map".to_string());
        args.push(format!("0:{idx}"));
    }

    args.push("-map_metadata".to_string());
    args.push("0".to_string());
    args.push("-map_chapters".to_string());
    args.push("0".to_string());
    args.push("-c".to_string());
    args.push("copy".to_string());

    if recipe.family == PresetFamily::Archive {
        let mut vid_ordinal = 0usize;
        for (idx, action, _) in mapped_streams {
            if matches!(action, TrackAction::EncodeVideo) {
                break;
            }
            if let Some(ev) = encoded_video {
                if *idx != ev.index {
                    // Check if earlier mapped stream is a cover art (attached_pic) which also uses v:N
                    if tracks
                        .iter()
                        .any(|t| t.stream_index == *idx && (t.codec_type == "video" || t.is_attached_pic))
                    {
                        vid_ordinal += 1;
                    }
                }
            }
        }
        let v_spec = format!("v:{vid_ordinal}");

        let encoder = recipe.codec.encoder_name();
        args.push(format!("-c:{v_spec}"));
        args.push(encoder.to_string());

        match recipe.codec {
            VideoCodec::Hevc => {
                args.push(format!("-preset:{v_spec}"));
                args.push(recipe.effort.clone());
                args.push(format!("-crf:{v_spec}"));
                args.push(recipe.crf.to_string());
                if let Some(ref tune) = recipe.tune {
                    let t = tune.trim();
                    if !t.is_empty() {
                        args.push(format!("-tune:{v_spec}"));
                        args.push(t.to_string());
                    }
                }
                if let Some(aq) = recipe.aq_mode {
                    args.push("-x265-params".to_string());
                    args.push(format!("aq-mode={aq}"));
                }
                if recipe.container == ContainerFormat::Mp4 {
                    args.push(format!("-tag:{v_spec}"));
                    args.push("hvc1".to_string());
                }
            }
            VideoCodec::Av1 => {
                args.push(format!("-preset:{v_spec}"));
                args.push(recipe.effort.clone());
                args.push(format!("-crf:{v_spec}"));
                args.push(recipe.crf.to_string());
            }
        }

        if let Some(pf) = target_pix_fmt {
            args.push(format!("-pix_fmt:{v_spec}"));
            args.push(pf.to_string());
        }
        args.push(format!("-fps_mode:{v_spec}"));
        args.push("passthrough".to_string());

        if let Some(ev) = encoded_video {
            if let Some(ref cp) = ev.color_primaries {
                if cp != "unknown" {
                    args.push(format!("-color_primaries:{v_spec}"));
                    args.push(cp.clone());
                }
            }
            if let Some(ref ct) = ev.color_transfer {
                if ct != "unknown" {
                    args.push(format!("-color_trc:{v_spec}"));
                    args.push(ct.clone());
                }
            }
            if let Some(ref cs) = ev.color_space {
                if cs != "unknown" {
                    args.push(format!("-colorspace:{v_spec}"));
                    args.push(cs.clone());
                }
            }
            if let Some(ref cr) = ev.color_range {
                if cr != "unknown" {
                    args.push(format!("-color_range:{v_spec}"));
                    args.push(cr.clone());
                }
            }
        }
    }

    for (_, action, out_spec) in mapped_streams {
        if let Some(spec) = out_spec {
            match action {
                TrackAction::TranscodeAudioAac => {
                    args.push(format!("-c:{spec}"));
                    args.push("aac".to_string());
                    args.push(format!("-b:{spec}"));
                    args.push("192k".to_string());
                }
                TrackAction::ConvertSubMovText => {
                    args.push(format!("-c:{spec}"));
                    args.push("mov_text".to_string());
                }
                _ => {}
            }
        }
    }

    if recipe.container == ContainerFormat::Mp4 {
        args.push("-movflags".to_string());
        args.push("+faststart".to_string());
    }

    args.push("-f".to_string());
    args.push(recipe.container.muxer().to_string());
    args.push(output_placeholder.to_string());
    args
}

pub fn materialize_argv(template: &[String], partial_path: &Path) -> Vec<String> {
    let p_str = partial_path.to_string_lossy().to_string();
    template
        .iter()
        .map(|arg| {
            if arg == "${UNIQUE_PARTIAL}" {
                p_str.clone()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn is_mp4_compatible_audio(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "aac" | "ac3" | "eac3" | "mp3" | "alac" | "flac" | "mp2" | "opus"
    )
}

fn is_convertible_text_sub(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "text" | "mov_text"
    )
}

fn shell_join_preview(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.contains(' ') || a.contains('"') || a.contains('\'') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve a non-colliding destination path mirroring relative subdirectories,
/// preserving stems with internal dots, handling Windows reserved names & length limits,
/// and never aliasing the input path.
pub fn resolve_destination_path(
    input_path: &Path,
    source_rel_path: &Path,
    dest_root: &Path,
    recipe: &EncodeRecipe,
    reserved_ci: &mut HashSet<String>,
) -> PathBuf {
    resolve_destination_path_with_config(
        input_path,
        source_rel_path,
        dest_root,
        recipe,
        &OutputConfig::default(),
        reserved_ci,
    )
    .unwrap_or_else(|_| {
        let parent = dest_root.to_path_buf();
        parent.join(format!("video.{}.{}", recipe.filename_tag(), recipe.container.ext()))
    })
}

/// Apply optional regex find/replace to a filename stem.
pub fn apply_output_regex_to_stem(stem: &str, cfg: &OutputConfig) -> Result<String, String> {
    let find = cfg.regex_find.as_deref().unwrap_or("").trim();
    if find.is_empty() {
        return Ok(stem.to_string());
    }
    let re = regex::Regex::new(find).map_err(|e| format!("Invalid output filename regex: {e}"))?;
    let rep = cfg.regex_replace.as_deref().unwrap_or("");
    let replaced = re.replace_all(stem, rep).to_string();
    let trimmed = replaced.trim();
    if trimmed.is_empty() {
        Ok(stem.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Lexically normalize `.` and `..` segments without hitting the filesystem,
/// so expressions like `$sourceFilePath/../foo.mkv` or `$sourceFolder/../foo.mkv` resolve cleanly.
fn normalize_lexical_path(raw: &str, anchor_if_relative: &Path) -> PathBuf {
    let mut normalized = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch == '\\' {
            normalized.push('/');
        } else {
            normalized.push(ch);
        }
    }
    let candidate = Path::new(&normalized);
    let base = if candidate.is_absolute() || raw.starts_with("\\\\") {
        PathBuf::from(raw)
    } else {
        anchor_if_relative.join(candidate)
    };

    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    let mut has_root = false;
    let mut prefix_comp: Option<std::ffi::OsString> = None;
    for comp in base.components() {
        match comp {
            Component::Prefix(p) => {
                prefix_comp = Some(p.as_os_str().to_os_string());
            }
            Component::RootDir => {
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = stack.last() {
                    if last != ".." {
                        stack.pop();
                    } else if !has_root {
                        stack.push(std::ffi::OsString::from(".."));
                    }
                } else if !has_root {
                    stack.push(std::ffi::OsString::from(".."));
                }
            }
            Component::Normal(seg) => {
                stack.push(seg.to_os_string());
            }
        }
    }

    let mut out = PathBuf::new();
    if let Some(p) = prefix_comp {
        out.push(p);
    }
    if has_root {
        out.push(std::path::MAIN_SEPARATOR_STR);
    }
    for seg in stack {
        out.push(seg);
    }
    out
}

/// Evaluate raw target directory and base filename (before collision numbering) for the given config.
pub fn evaluate_output_target(
    input_path: &Path,
    source_rel_path: &Path,
    dest_root: &Path,
    recipe: &EncodeRecipe,
    output_config: &OutputConfig,
) -> Result<(PathBuf, String, String), String> {
    let raw_stem = input_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "video".to_string());
    let transformed_stem = apply_output_regex_to_stem(&raw_stem, output_config)?;
    let safe_stem = sanitize_path_segment(&transformed_stem);
    let raw_safe_stem = sanitize_path_segment(&raw_stem);
    let src_ext = input_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    let ext = recipe.container.ext().to_string();
    let tag = recipe.filename_tag();

    if output_config.mode == OutputMode::ReplaceOriginal {
        let src_dir = input_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        return Ok((src_dir, safe_stem, ext));
    }

    let rel_parent = source_rel_path.parent().unwrap_or_else(|| Path::new(""));
    let mut sanitized_rel_dir = PathBuf::new();
    for comp in rel_parent.components() {
        if let Component::Normal(seg) = comp {
            sanitized_rel_dir.push(sanitize_path_segment(&seg.to_string_lossy()));
        }
    }

    let tmpl = output_config
        .path_template
        .as_deref()
        .unwrap_or("")
        .trim();

    if !tmpl.is_empty() {
        let src_folder = input_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .to_string();
        let src_file_path = input_path.to_string_lossy().to_string();
        let src_file_name = format!("{safe_stem}.{ext}");
        let src_file_name_orig = if src_ext.is_empty() {
            raw_safe_stem.clone()
        } else {
            format!("{raw_safe_stem}.{src_ext}")
        };
        let rel_dir_str = sanitized_rel_dir.to_string_lossy().to_string();
        let dest_root_str = dest_root.to_string_lossy().to_string();
        let crf_str = recipe.crf.to_string();
        let codec_str = recipe.codec.short_tag().to_string();

        // Replace longest tokens first, supporting both `${var}` and `$var` syntax.
        let tokens: &[(&str, &str)] = &[
            ("rawSourceFileNameStem", raw_safe_stem.as_str()),
            ("sourceFileNameStem", safe_stem.as_str()),
            ("sourceFileNameOrig", src_file_name_orig.as_str()),
            ("sourceFileName", src_file_name.as_str()),
            ("sourceFilePath", src_file_path.as_str()),
            ("sourceFolder", src_folder.as_str()),
            ("sourceExt", src_ext.as_str()),
            ("destRoot", dest_root_str.as_str()),
            ("relDir", rel_dir_str.as_str()),
            ("container", ext.as_str()),
            ("codec", codec_str.as_str()),
            ("stem", safe_stem.as_str()),
            ("crf", crf_str.as_str()),
            ("tag", tag.as_str()),
            ("ext", ext.as_str()),
        ];

        let mut expanded = tmpl.to_string();
        for (name, val) in tokens {
            let braced = format!("${{{name}}}");
            let plain = format!("${name}");
            expanded = expanded.replace(&braced, val).replace(&plain, val);
        }

        let norm = normalize_lexical_path(&expanded, dest_root);
        let target_dir = norm
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(dest_root)
            .to_path_buf();
        let final_ext = norm
            .extension()
            .and_then(|e| e.to_str())
            .filter(|s| !s.is_empty())
            .map(sanitize_path_segment)
            .unwrap_or_else(|| ext.clone());
        let final_stem = norm
            .file_stem()
            .map(|s| sanitize_path_segment(&s.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| safe_stem.clone());

        return Ok((target_dir, final_stem, final_ext));
    }

    let target_dir = dest_root.join(&sanitized_rel_dir);
    let base_name = format!("{safe_stem}.{tag}");
    Ok((target_dir, base_name, ext))
}

/// Resolve destination path supporting OutputMode (TemplateFolder vs ReplaceOriginal),
/// custom path templates (`$sourceFolder/encoded/$sourceFileName`, etc.), and filename regex.
pub fn resolve_destination_path_with_config(
    input_path: &Path,
    source_rel_path: &Path,
    dest_root: &Path,
    recipe: &EncodeRecipe,
    output_config: &OutputConfig,
    reserved_ci: &mut HashSet<String>,
) -> Result<PathBuf, String> {
    let (target_dir, mut base_name, ext) =
        evaluate_output_target(input_path, source_rel_path, dest_root, recipe, output_config)?;

    // Enforce conservative component & Windows MAX_PATH margin (240 chars)
    let max_dir_len = target_dir.to_string_lossy().len();
    if max_dir_len + base_name.len() + ext.len() + 12 > 240 {
        let allowed_base = 240usize
            .saturating_sub(max_dir_len + ext.len() + 16)
            .max(12);
        base_name = base_name.chars().take(allowed_base).collect();
    }

    if output_config.mode == OutputMode::ReplaceOriginal {
        let candidate = target_dir.join(format!("{base_name}.{ext}"));
        reserved_ci.insert(candidate.to_string_lossy().to_ascii_lowercase());
        return Ok(candidate);
    }

    let input_canon = input_path.canonicalize().ok();

    for n in 1u32..10_000 {
        let file_name = if n == 1 {
            format!("{base_name}.{ext}")
        } else {
            format!("{base_name} ({n}).{ext}")
        };
        let candidate = target_dir.join(&file_name);
        let ci_key = candidate.to_string_lossy().to_ascii_lowercase();
        if reserved_ci.contains(&ci_key) {
            continue;
        }
        if candidate.exists() {
            continue;
        }
        if let Some(ref ic) = input_canon {
            if let Ok(cc) = candidate.canonicalize() {
                if &cc == ic {
                    continue;
                }
            } else if candidate == input_path {
                continue;
            }
        }
        reserved_ci.insert(ci_key);
        return Ok(candidate);
    }

    let fallback = target_dir.join(format!("{base_name}.{}.{ext}", uuid::Uuid::new_v4()));
    reserved_ci.insert(fallback.to_string_lossy().to_ascii_lowercase());
    Ok(fallback)
}

/// Derive destination root from an existing job's destination path by stripping
/// the mirrored relative directory components of `source_rel_path`.
pub fn derive_destination_root_from_job(dest_path: &Path, source_rel_path: &Path) -> PathBuf {
    let mut root = dest_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    if let Some(rel_parent) = source_rel_path.parent() {
        let depth = rel_parent
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .count();
        for _ in 0..depth {
            if !root.pop() {
                break;
            }
        }
    }
    root
}

pub fn sanitize_path_segment(seg: &str) -> String {
    let mut cleaned: String = seg
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        cleaned = "item".to_string();
    }
    let upper_stem = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let is_win_reserved = matches!(
        upper_stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if is_win_reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}
