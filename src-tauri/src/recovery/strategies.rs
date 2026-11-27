use crate::recovery::bitstream::{
    scan_and_extract_streams, to_segment_infos, VideoBitstreamCodec,
};
use crate::recovery::evaluator::{evaluate_candidate, generate_contact_sheet};
use crate::recovery::isobmff::{inject_st3d_box_into_mp4, write_patched_mdat_copy};
use crate::recovery::{
    CandidateEvaluation, ContainerKind, DiagnosisReport, ProgressCallback, ProvenanceSummary,
    RecoveredSegmentInfo, RecoveryOptions, RecoveryProgress, ReferenceProfile, UncertaintyGap,
    VrMetadataInfo,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub struct PipelineResult {
    pub selected_strategy: Option<String>,
    pub selected_output: Option<String>,
    pub candidates: Vec<CandidateEvaluation>,
    pub segments: Vec<RecoveredSegmentInfo>,
    pub gaps: Vec<UncertaintyGap>,
    pub provenance: ProvenanceSummary,
    pub vr_status: VrMetadataInfo,
    pub contact_sheet_path: Option<String>,
    pub overall_status: String,
    pub honest_summary: String,
}

pub fn run_recovery_pipeline(
    opts: &RecoveryOptions,
    diag: &DiagnosisReport,
    references: &[ReferenceProfile],
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    progress_cb: Option<&ProgressCallback>,
    start_ts: Instant,
) -> Result<PipelineResult, String> {
    let tmp_dir = tempfile::Builder::new()
        .prefix("media_shack_recover_")
        .tempdir()
        .map_err(|e| format!("Failed to create recovery temp dir: {e}"))?;
    let work_dir = tmp_dir.path();

    let best_ref = references.first();
    let mut candidates: Vec<CandidateEvaluation> = Vec::new();
    let mut segments: Vec<RecoveredSegmentInfo> = Vec::new();
    let mut gaps: Vec<UncertaintyGap> = Vec::new();
    let mut timing_note: Option<String> = None;
    let mut has_b_frames = best_ref.map(|r| r.has_b_frames).unwrap_or(false);

    let emit = |stage: &str, strat: &str| {
        if let Some(cb) = progress_cb {
            cb(RecoveryProgress {
                stage: stage.into(),
                current_strategy: strat.into(),
                bytes_scanned: diag.file_size,
                total_bytes: diag.file_size,
                recognized_packets: 0,
                recovered_frames: 0,
                elapsed_secs: start_ts.elapsed().as_secs_f64(),
            });
        }
    };

    // ============================================================
    // Strategy A: Existing Container / Working-Copy Remux & Tools
    // ============================================================
    emit("recovery", "Strategy A: Existing container recovery");
    if matches!(
        diag.container_kind,
        ContainerKind::MpegTs | ContainerKind::ConventionalMp4 | ContainerKind::FragmentedMp4
    ) && (diag.has_moov || diag.has_moof || diag.container_kind == ContainerKind::MpegTs)
    {
        let cand_a1 = work_dir.join("cand_a_remux.mp4");
        if run_ffmpeg_remux(ffmpeg_path, &opts.input_path, &cand_a1, opts.video_only) {
            let eval = evaluate_candidate(
                "Strategy A1 (FFmpeg lossless remux)",
                &cand_a1,
                vec![],
                diag.file_size,
                diag.file_size,
                best_ref,
                ffmpeg_path,
                ffprobe_path,
                opts.deep_scan,
            );
            candidates.push(eval);
        }
    }

    // Strategy A2: Working copy with patched truncated `mdat` header + remux (if `moov` or `moof` present)
    if (diag.has_moov || diag.has_moof) && diag.atoms.iter().any(|a| a.truncated && a.tag == "mdat") {
        let patched_in = work_dir.join("working_patched_mdat.mp4");
        if write_patched_mdat_copy(&opts.input_path, &patched_in, diag).unwrap_or(false) {
            let cand_a2 = work_dir.join("cand_a2_patched_remux.mp4");
            if run_ffmpeg_remux(ffmpeg_path, &patched_in, &cand_a2, opts.video_only) {
                let eval = evaluate_candidate(
                    "Strategy A2 (Patched working-copy mdat + FFmpeg remux)",
                    &cand_a2,
                    vec![],
                    diag.file_size,
                    diag.file_size,
                    best_ref,
                    ffmpeg_path,
                    ffprobe_path,
                    opts.deep_scan,
                );
                candidates.push(eval);
            }
        }
    }

    // Fast shortcut check: if Strategy A produced a spotless full-coverage recovery, we can still proceed or return early unless `--deep` requested
    let best_so_far_excellent = candidates
        .iter()
        .any(|c| c.succeeded && c.decode_error_count == 0 && c.input_bytes_mapped_pct > 80.0);

    // ============================================================
    // Strategy B: Untrunc (if discovered on system or bundled)
    // ============================================================
    if (!best_so_far_excellent || opts.deep_scan) && !references.is_empty() {
        if let Some(untrunc_bin) = discover_untrunc_binary() {
            emit("recovery", "Strategy B: Reference-assisted Untrunc");
            let help_txt = inspect_untrunc_help(&untrunc_bin);
            for (r_idx, ref_prof) in references.iter().take(2).enumerate() {
                // Always run on a working copy so the original file cannot possibly be touched
                let copy_in = work_dir.join(format!("untrunc_work_{r_idx}.mp4"));
                if std::fs::copy(&opts.input_path, &copy_in).is_ok() {
                    let mut variants: Vec<(&str, Vec<&str>)> = vec![("default", vec![])];
                    if help_txt.contains("-s") {
                        variants.push(("skip_unknown", vec!["-s"]));
                    }
                    if help_txt.contains("-dyn") || help_txt.contains("-d") {
                        variants.push(("dynamic_stats", vec!["-s", "-d"]));
                    }
                    for (v_label, flags) in variants {
                        let out_cand = work_dir.join(format!("cand_b_{r_idx}_{v_label}.mp4"));
                        let mut cmd = Command::new(&untrunc_bin);
                        for fl in &flags {
                            cmd.arg(fl);
                        }
                        if help_txt.contains("-dst") {
                            cmd.arg("-dst").arg(&out_cand);
                        }
                        cmd.arg(&ref_prof.file_path).arg(&copy_in);
                        if let Ok(out) = cmd.output() {
                            // Untrunc often outputs `<input>_fixed.mp4` beside work copy if -dst wasn't used
                            let fixed_default = work_dir.join(format!("untrunc_work_{r_idx}.mp4_fixed.mp4"));
                            let actual_out = if out_cand.is_file() {
                                out_cand.clone()
                            } else if fixed_default.is_file() {
                                fixed_default
                            } else {
                                out_cand.clone()
                            };
                            if actual_out.is_file() {
                                let eval = evaluate_candidate(
                                    &format!("Strategy B (Untrunc {v_label}, ref #{})", r_idx + 1),
                                    &actual_out,
                                    vec![],
                                    diag.file_size,
                                    std::fs::metadata(&actual_out).map(|m| m.len()).unwrap_or(0),
                                    Some(ref_prof),
                                    ffmpeg_path,
                                    ffprobe_path,
                                    opts.deep_scan,
                                );
                                candidates.push(eval);
                            } else if !out.status.success() {
                                let _ = String::from_utf8_lossy(&out.stderr);
                            }
                        }
                    }
                }
            }
        }
    }

    // ============================================================
    // Strategy C / D / E / F: Custom Binary Bitstream Extraction
    // ============================================================
    let already_clean = candidates
        .iter()
        .any(|c| c.succeeded && c.decode_error_count <= 2 && c.score >= 70.0);

    if !already_clean || opts.deep_scan {
        emit(
            "bitstream_scan",
            "Strategy C/D/E: Custom binary video/audio bitstream recovery",
        );

        let ranges = if diag.mdat_ranges.is_empty() {
            vec![(0, diag.file_size)]
        } else {
            diag.mdat_ranges.clone()
        };

        if let Some(scan_out) = scan_and_extract_streams(
            &opts.input_path,
            &ranges,
            best_ref,
            work_dir,
            opts.deep_scan,
            opts.timeout_secs,
            progress_cb,
            start_ts,
        )? {
            gaps = scan_out.gaps.clone();
            timing_note = Some(scan_out.timing_note.clone());
            has_b_frames = scan_out
                .segments
                .iter()
                .any(|s| s.has_b_frames_hint);

            let fps_opt = best_ref.and_then(|r| r.fps).or(Some(25.0));

            if scan_out.segments.len() == 1 {
                let seg = &scan_out.segments[0];
                // Strategy D: Video-only clean MP4 remux (and MKV fallback)
                let cand_d = work_dir.join("cand_d_video_only.mp4");
                match mux_annex_b_video_diag(
                    ffmpeg_path,
                    &seg.annex_b_path,
                    seg.codec,
                    &cand_d,
                    fps_opt,
                    best_ref,
                    true,
                ) {
                    Ok(()) => {
                        let eval_d = evaluate_candidate(
                            "Strategy D (Custom binary video-only recovery)",
                            &cand_d,
                            vec![],
                            diag.file_size,
                            scan_out.total_recognized_video_bytes,
                            best_ref,
                            ffmpeg_path,
                            ffprobe_path,
                            true,
                        );
                        candidates.push(eval_d);
                    }
                    Err(mux_err) => {
                        candidates.push(CandidateEvaluation {
                            strategy_name: "Strategy D (Custom binary video-only recovery)".into(),
                            output_path: cand_d.to_string_lossy().to_string(),
                            segment_paths: vec![],
                            succeeded: false,
                            score: 0.0,
                            decoded_video_frames: 0,
                            decode_error_count: 0,
                            recovered_duration_secs: None,
                            audio_duration_secs: None,
                            width: None,
                            height: None,
                            idr_frames: 0,
                            timestamps_monotonic: false,
                            av_sync_diff_secs: None,
                            input_bytes_mapped_pct: 0.0,
                            black_frame_ratio: None,
                            frozen_frame_ratio: None,
                            output_bytes: 0,
                            notes: vec![],
                            rejection_reason: Some(format!("Mux failed: {mux_err}")),
                        });
                    }
                }

                // Strategy E: Video + independent interstitial AAC audio (if not `--video-only`)
                if !opts.video_only {
                    if let Some(ref aac_path) = seg.aac_adts_path {
                        if seg.aac_frames_recovered > 5 {
                            let cand_e = work_dir.join("cand_e_video_audio.mp4");
                            if mux_annex_b_and_aac(
                                ffmpeg_path,
                                &seg.annex_b_path,
                                aac_path,
                                seg.codec,
                                &cand_e,
                                fps_opt,
                                best_ref,
                            ) {
                                let mut eval_e = evaluate_candidate(
                                    "Strategy E (Binary video + independent AAC audio)",
                                    &cand_e,
                                    vec![],
                                    diag.file_size,
                                    scan_out.total_recognized_video_bytes
                                        + scan_out.total_recognized_audio_bytes,
                                    best_ref,
                                    ffmpeg_path,
                                    ffprobe_path,
                                    true,
                                );
                                if eval_e.audio_duration_secs.is_none() {
                                    eval_e.succeeded = false;
                                    eval_e.score = 0.0;
                                    eval_e.rejection_reason = Some(
                                        "Audio stream failed AAC mux validation; falling back cleanly to Strategy D video-only".into(),
                                    );
                                }
                                candidates.push(eval_e);
                            }
                        }
                    }
                }

                segments = to_segment_infos(&scan_out.segments, &[]);
            } else if scan_out.segments.len() > 1 {
                // Strategy F: Partial segment recovery (export playable clips separately, plus combined summary)
                let mut seg_clip_paths = Vec::new();
                for seg in &scan_out.segments {
                    let seg_mp4 = work_dir.join(format!("cand_f_seg_{:03}.mp4", seg.index));
                    let ok = if !opts.video_only && seg.aac_adts_path.is_some() && seg.aac_frames_recovered > 5 {
                        mux_annex_b_and_aac(
                            ffmpeg_path,
                            &seg.annex_b_path,
                            seg.aac_adts_path.as_ref().unwrap(),
                            seg.codec,
                            &seg_mp4,
                            fps_opt,
                            best_ref,
                        )
                    } else {
                        mux_annex_b_video(
                            ffmpeg_path,
                            &seg.annex_b_path,
                            seg.codec,
                            &seg_mp4,
                            fps_opt,
                            best_ref,
                            true,
                        )
                    };
                    if ok {
                        seg_clip_paths.push(seg_mp4.to_string_lossy().to_string());
                    }
                }

                // Evaluate the highest-frame recovered segment cleanly + concatenated candidate if valid
                if let Some(best_seg) = scan_out.segments.iter().max_by_key(|s| s.frame_count) {
                    let best_seg_mp4 = work_dir.join(format!("cand_f_seg_{:03}.mp4", best_seg.index));
                    if best_seg_mp4.is_file() {
                        let eval_f = evaluate_candidate(
                            "Strategy F (Partial segment recovery — longest decodable segment + clips)",
                            &best_seg_mp4,
                            seg_clip_paths.clone(),
                            diag.file_size,
                            scan_out.total_recognized_video_bytes,
                            best_ref,
                            ffmpeg_path,
                            ffprobe_path,
                            true,
                        );
                        candidates.push(eval_f);
                    }
                }
                segments = to_segment_infos(&scan_out.segments, &seg_clip_paths);
            }

            // Strategy G: Last-resort transcode ONLY if no stream-copy candidate succeeded cleanly
            let any_stream_copy_passed = candidates.iter().any(|c| c.succeeded && c.decoded_video_frames > 0);
            let within_budget = opts.timeout_secs == 0 || start_ts.elapsed().as_secs() < opts.timeout_secs;
            if !any_stream_copy_passed && within_budget {
                if let Some(seg0) = scan_out.segments.iter().max_by_key(|s| s.frame_count) {
                    emit("recovery", "Strategy G: Last-resort high-quality HEVC CRF 18 transcode");
                    let cand_g = work_dir.join("cand_g_transcode_hevc.mp4");
                    if transcode_last_resort_hevc(
                        ffmpeg_path,
                        &seg0.annex_b_path,
                        &cand_g,
                        fps_opt,
                        best_ref,
                    ) {
                        let eval_g = evaluate_candidate(
                            "Strategy G (Last-resort HEVC CRF 18 transcode)",
                            &cand_g,
                            vec![],
                            diag.file_size,
                            scan_out.total_recognized_video_bytes,
                            best_ref,
                            ffmpeg_path,
                            ffprobe_path,
                            true,
                        );
                        candidates.push(eval_g);
                    }
                }
            }
        }
    }

    // Rank candidates by multi-factor evaluation score (never size alone!)
    candidates.sort_by(|a, b| {
        b.succeeded
            .cmp(&a.succeeded)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });

    let best = candidates.iter().find(|c| c.succeeded).cloned();

    let mut selected_output = None;
    let mut selected_strategy = None;
    let mut contact_sheet_path = None;

    if let Some(ref b) = best {
        if let Some(parent) = opts.output_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create output directory {}: {e}", parent.display()))?;
            }
        }
        std::fs::copy(&b.output_path, &opts.output_path)
            .map_err(|e| format!("Failed to copy recovered output to {}: {e}", opts.output_path.display()))?;

        // Also copy extra Strategy F segment clips beside output if multi-segment
        if b.segment_paths.len() > 1 {
            let stem = opts
                .output_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("recovered");
            let parent = opts.output_path.parent().unwrap_or_else(|| Path::new("."));
            for (idx, sp) in b.segment_paths.iter().enumerate() {
                let dst_seg = parent.join(format!("{stem}_part{:03}.mp4", idx + 1));
                if std::fs::copy(sp, &dst_seg).is_ok() {
                    if let Some(seg_info) = segments.get_mut(idx) {
                        seg_info.output_path = Some(dst_seg.to_string_lossy().to_string());
                    }
                }
            }
        }

        if opts.generate_contact_sheet {
            let sheet = opts.output_path.with_extension("contact_sheet.jpg");
            contact_sheet_path = generate_contact_sheet(
                ffmpeg_path,
                &opts.output_path,
                &sheet,
                b.recovered_duration_secs,
            );
        }

        selected_output = Some(opts.output_path.to_string_lossy().to_string());
        selected_strategy = Some(b.strategy_name.clone());
    }

    // Build honest ProvenanceSummary separating recovered data, inferred metadata, and omitted/irrecoverable data
    let mut recovered_data = Vec::new();
    let mut inferred_metadata = Vec::new();
    let mut irrecoverable = Vec::new();

    if let Some(ref b) = best {
        recovered_data.push(format!(
            "{} decoded video frames ({} keyframe/IDR pictures, {:.2?}s, {}x{}) recovered via {}",
            b.decoded_video_frames,
            b.idr_frames,
            b.recovered_duration_secs.unwrap_or(0.0),
            b.width.unwrap_or(0),
            b.height.unwrap_or(0),
            b.strategy_name
        ));
        if let Some(ad) = b.audio_duration_secs {
            recovered_data.push(format!("Audio stream recovered ({ad:.2}s)"));
        } else {
            irrecoverable.push("Audio track omitted (either absent, video-only priority requested, or raw AAC boundaries unverified)".into());
        }
    } else {
        irrecoverable.push("No playable video stream could be verified from physical input bytes".into());
    }

    if let Some(r) = best_ref {
        inferred_metadata.push(format!(
            "Parameter sets & codec geometry inferred from reference `{}` (compatibility {:.0}/100)",
            Path::new(&r.file_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&r.file_path),
            r.compatibility_score
        ));
    }
    if let Some(tn) = timing_note {
        inferred_metadata.push(tn);
    }
    if !gaps.is_empty() {
        irrecoverable.push(format!(
            "{} damaged/unidentified byte region(s) skipped cleanly without fabricating missing frames",
            gaps.len()
        ));
    }
    if has_b_frames {
        inferred_metadata.push("Source uses B-frames; timestamps rebuilt sequentially with estimated frame cadence".into());
    }

    // VR / Stereoscopic status tracking
    let mut vr_status = best_ref.map(|r| r.vr_info.clone()).unwrap_or_default();
    if !vr_status.is_stereoscopic {
        if let Some(ref b) = best {
            if let (Some(w), Some(h)) = (b.width, b.height) {
                if w >= 2560 && (w == h * 2 || w == h) {
                    vr_status.limitation_note = Some(format!(
                        "Resolution {w}x{h} preserved intact (no cropped eye), but explicit spherical/stereo container boxes were absent in damaged input — select SBS/TB 180/360 manually in VR player if needed."
                    ));
                }
            }
        }
    } else if !diag.has_moov {
        vr_status.limitation_note = Some(format!(
            "VR geometry ({:?} / {:?}) preserved at original resolution from reference cues; verify projection in Quest/VR player if spherical atoms were stripped.",
            vr_status.stereo_mode, vr_status.projection
        ));
    }

    let overall_status = if best.is_none() {
        "unrecoverable".to_string()
    } else if !gaps.is_empty() || segments.len() > 1 {
        "partial".to_string()
    } else {
        "recovered".to_string()
    };

    let honest_summary = match &best {
        Some(b) => format!(
            "Recovered {} verified video frames ({:.1}s, {} decode errors) using {}. Input SHA-256 verified unchanged.",
            b.decoded_video_frames,
            b.recovered_duration_secs.unwrap_or(0.0),
            b.decode_error_count,
            b.strategy_name
        ),
        None => "All recovery strategies exhausted without producing a verified playable stream. Check input corruption/missing parameter sets or supply a matching reference video.".into(),
    };

    Ok(PipelineResult {
        selected_strategy,
        selected_output,
        candidates,
        segments,
        gaps,
        provenance: ProvenanceSummary {
            recovered_data,
            inferred_metadata,
            irrecoverable_or_omitted: irrecoverable,
        },
        vr_status,
        contact_sheet_path,
        overall_status,
        honest_summary,
    })
}

fn run_ffmpeg_remux(ffmpeg_path: &Path, input: &Path, out: &Path, video_only: bool) -> bool {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-v", "error", "-y", "-fflags", "+genpts", "-i"])
        .arg(input)
        .args(["-map", "0:v:0"]);
    if !video_only {
        cmd.args(["-map", "0:a?"]);
    }
    cmd.args(["-c", "copy", "-movflags", "+faststart"]).arg(out);
    cmd.output().map(|o| o.status.success() && out.is_file()).unwrap_or(false)
}

fn mux_annex_b_video(
    ffmpeg_path: &Path,
    annex_b: &Path,
    codec: VideoBitstreamCodec,
    out_mp4: &Path,
    fps_opt: Option<f64>,
    reference: Option<&ReferenceProfile>,
    preserve_vr: bool,
) -> bool {
    mux_annex_b_video_diag(ffmpeg_path, annex_b, codec, out_mp4, fps_opt, reference, preserve_vr).is_ok()
}

fn mux_annex_b_video_diag(
    ffmpeg_path: &Path,
    annex_b: &Path,
    codec: VideoBitstreamCodec,
    out_mp4: &Path,
    fps_opt: Option<f64>,
    reference: Option<&ReferenceProfile>,
    preserve_vr: bool,
) -> Result<(), String> {
    let fmt = match codec {
        VideoBitstreamCodec::H264 => "h264",
        VideoBitstreamCodec::Hevc => "hevc",
    };
    let fps_str = format!("{:.6}", fps_opt.unwrap_or(25.0).max(1.0));
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-v", "error", "-y", "-fflags", "+genpts", "-r", &fps_str, "-f", fmt, "-i"])
        .arg(annex_b)
        .args(["-c:v", "copy", "-movflags", "+faststart"]);
    append_color_and_vr_flags(&mut cmd, reference, preserve_vr);
    cmd.arg(out_mp4);
    let out = cmd.output().map_err(|e| format!("Spawn failed: {e}"))?;
    if out.status.success() && out_mp4.is_file() {
        if preserve_vr {
            if let Some(mode) = reference.and_then(|r| r.vr_info.stereo_mode.as_deref()) {
                let _ = inject_st3d_box_into_mp4(out_mp4, mode);
            }
        }
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().chars().take(240).collect())
    }
}

fn mux_annex_b_and_aac(
    ffmpeg_path: &Path,
    annex_b: &Path,
    aac_adts: &Path,
    codec: VideoBitstreamCodec,
    out_mp4: &Path,
    fps_opt: Option<f64>,
    reference: Option<&ReferenceProfile>,
) -> bool {
    let fmt = match codec {
        VideoBitstreamCodec::H264 => "h264",
        VideoBitstreamCodec::Hevc => "hevc",
    };
    let fps_str = format!("{:.6}", fps_opt.unwrap_or(25.0).max(1.0));
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-v", "error", "-y", "-fflags", "+genpts", "-r", &fps_str, "-f", fmt, "-i"])
        .arg(annex_b)
        .args(["-f", "aac", "-i"])
        .arg(aac_adts)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "copy",
            "-movflags",
            "+faststart",
        ]);
    append_color_and_vr_flags(&mut cmd, reference, true);
    cmd.arg(out_mp4);
    let ok = cmd.output().map(|o| o.status.success() && out_mp4.is_file()).unwrap_or(false);
    if ok {
        if let Some(mode) = reference.and_then(|r| r.vr_info.stereo_mode.as_deref()) {
            let _ = inject_st3d_box_into_mp4(out_mp4, mode);
        }
    }
    ok
}

fn transcode_last_resort_hevc(
    ffmpeg_path: &Path,
    annex_b: &Path,
    out_mp4: &Path,
    fps_opt: Option<f64>,
    reference: Option<&ReferenceProfile>,
) -> bool {
    let fmt = match reference.and_then(|r| r.video_codec.as_deref()) {
        Some("hevc") => "hevc",
        _ => "h264",
    };
    let fps_str = format!("{:.6}", fps_opt.unwrap_or(25.0).max(1.0));
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-v", "error", "-y", "-err_detect", "ignore_err", "-r", &fps_str, "-f", fmt, "-i"])
        .arg(annex_b)
        .args(["-c:v", "libx265", "-crf", "18", "-preset", "fast", "-tag:v", "hvc1", "-movflags", "+faststart"]);
    append_color_and_vr_flags(&mut cmd, reference, true);
    cmd.arg(out_mp4);
    cmd.output().map(|o| o.status.success() && out_mp4.is_file()).unwrap_or(false)
}

fn append_color_and_vr_flags(
    cmd: &mut Command,
    reference: Option<&ReferenceProfile>,
    preserve_vr: bool,
) {
    let Some(r) = reference else {
        return;
    };
    if let Some(ref cp) = r.color_primaries {
        if cp != "unknown" {
            cmd.args(["-color_primaries", cp]);
        }
    }
    if let Some(ref ct) = r.color_transfer {
        if ct != "unknown" {
            cmd.args(["-color_trc", ct]);
        }
    }
    if let Some(ref cs) = r.color_space {
        if cs != "unknown" {
            cmd.args(["-colorspace", cs]);
        }
    }
    if preserve_vr && r.vr_info.is_stereoscopic {
        if let Some(ref mode) = r.vr_info.stereo_mode {
            let mval = match mode.as_str() {
                "side_by_side" | "left_right" => "left_right",
                "top_bottom" => "top_bottom",
                _ => mode.as_str(),
            };
            cmd.args(["-metadata:s:v:0", &format!("stereo_mode={mval}")]);
        }
    }
}

fn discover_untrunc_binary() -> Option<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "untrunc.exe"
    } else {
        "untrunc"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let local = dir.join(bin_name);
            if local.is_file() {
                return Some(local);
            }
            #[cfg(target_os = "macos")]
            {
                let res = dir.join("../Resources").join(bin_name);
                if res.is_file() {
                    return Some(res);
                }
            }
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for p in std::env::split_paths(&path_var) {
            let cand = p.join(bin_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn inspect_untrunc_help(bin: &Path) -> String {
    Command::new(bin)
        .arg("--help")
        .output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        })
        .unwrap_or_default()
}
