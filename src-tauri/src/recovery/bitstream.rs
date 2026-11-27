use crate::recovery::{
    ProgressCallback, RecoveredSegmentInfo, RecoveryProgress, ReferenceProfile, UncertaintyGap,
};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoBitstreamCodec {
    H264,
    Hevc,
}

#[derive(Debug, Clone)]
pub struct ExtractedSegmentStream {
    pub index: usize,
    pub codec: VideoBitstreamCodec,
    pub annex_b_path: PathBuf,
    pub aac_adts_path: Option<PathBuf>,
    pub source_byte_start: u64,
    pub source_byte_end: u64,
    pub frame_count: u64,
    pub idr_count: u64,
    pub has_b_frames_hint: bool,
    pub aac_frames_recovered: u64,
}

#[derive(Debug, Clone)]
pub struct BitstreamScanOutput {
    pub codec: VideoBitstreamCodec,
    pub segments: Vec<ExtractedSegmentStream>,
    pub gaps: Vec<UncertaintyGap>,
    pub total_recognized_video_bytes: u64,
    pub total_recognized_audio_bytes: u64,
    pub total_nals: u64,
    pub timing_note: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
struct ParsedNal {
    file_offset: u64,
    len_prefix_bytes: usize,
    payload_len: usize,
    is_vcl: bool,
    is_idr: bool,
    is_param_set: bool,
    is_first_slice: bool,
    is_b_slice: bool,
}

/// Strategy C / D / E / F core engine:
/// Streams physical `mdat` (or whole file if headless) in sliding windows, locks onto
/// valid H.264/HEVC NAL chains even across interleaved raw AAC chunks, groups NALs into
/// complete Access Units anchored on IDR/CRA random-access points, splits at unrecoverable gaps
/// into clean playable segments, and extracts interstitial AAC frames when reliable.
pub fn scan_and_extract_streams(
    input_path: &Path,
    ranges: &[(u64, u64)],
    reference: Option<&ReferenceProfile>,
    work_dir: &Path,
    deep_scan: bool,
    timeout_secs: u64,
    progress_cb: Option<&ProgressCallback>,
    start_ts: Instant,
) -> Result<Option<BitstreamScanOutput>, String> {
    let mut file = File::open(input_path).map_err(|e| format!("Cannot open {}: {e}", input_path.display()))?;
    let total_bytes: u64 = ranges.iter().map(|(s, e)| e.saturating_sub(*s)).sum();
    if total_bytes == 0 {
        return Ok(None);
    }

    // Decide codec candidates and NAL length field sizes to attempt
    let mut candidates: Vec<(VideoBitstreamCodec, u8)> = Vec::new();
    if let Some(r) = reference {
        let nl = if (1..=4).contains(&r.nal_length_size) {
            r.nal_length_size
        } else {
            4
        };
        match r.video_codec.as_deref() {
            Some("hevc") | Some("h265") => candidates.push((VideoBitstreamCodec::Hevc, nl)),
            Some("h264") => candidates.push((VideoBitstreamCodec::H264, nl)),
            _ => {}
        }
    }
    for pair in [
        (VideoBitstreamCodec::H264, 4u8),
        (VideoBitstreamCodec::Hevc, 4u8),
        (VideoBitstreamCodec::H264, 2u8),
        (VideoBitstreamCodec::Hevc, 2u8),
    ] {
        if !candidates.contains(&pair) {
            if deep_scan || pair.1 == 4 {
                candidates.push(pair);
            }
        }
    }

    // Quick codec auto-detection over first 4 MiB across candidates unless reference already specified
    let (chosen_codec, chosen_nls) = if let Some(r) = reference {
        if matches!(r.video_codec.as_deref(), Some("h264") | Some("hevc")) {
            candidates[0]
        } else {
            detect_best_codec_and_nls(&mut file, ranges, &candidates)?
        }
    } else {
        detect_best_codec_and_nls(&mut file, ranges, &candidates)?
    };

    // First also check if the input actually contains Annex B start codes (`00 00 00 01`) directly
    let annex_b_mode = reference.is_none() && has_dense_annex_b(&mut file, ranges)?;

    let out = if annex_b_mode {
        scan_annex_b_stream(
            &mut file,
            ranges,
            chosen_codec,
            reference,
            work_dir,
            timeout_secs,
            progress_cb,
            start_ts,
            total_bytes,
        )?
    } else {
        scan_length_prefixed_stream(
            &mut file,
            ranges,
            chosen_codec,
            chosen_nls,
            reference,
            work_dir,
            deep_scan,
            timeout_secs,
            progress_cb,
            start_ts,
            total_bytes,
        )?
    };

    if out.segments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

pub fn probe_dominant_bitstream_codec(path: &Path, ranges: &[(u64, u64)]) -> Option<&'static str> {
    if ranges.is_empty() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let candidates = [
        (VideoBitstreamCodec::H264, 4u8),
        (VideoBitstreamCodec::Hevc, 4u8),
    ];
    let (s, e) = ranges[0];
    let probe_len = (e - s).min(2 * 1024 * 1024) as usize;
    if probe_len < 64 {
        return None;
    }
    let mut sample = vec![0u8; probe_len];
    file.seek(SeekFrom::Start(s)).ok()?;
    file.read_exact(&mut sample).ok()?;
    let mut h264_hits = 0usize;
    let mut hevc_hits = 0usize;
    for &(codec, nls) in &candidates {
        let mut hits = 0usize;
        let mut pos = 0usize;
        while pos + (nls as usize) + 4 <= sample.len() {
            if let Some(p) = parse_nal_at(&sample, pos, s + pos as u64, codec, nls) {
                let end = pos + p.len_prefix_bytes + p.payload_len;
                if end <= sample.len() && confirm_follow_on(&sample, end, s, codec, nls, 16 * 1024).is_some() {
                    hits += 1;
                    pos = end;
                    if hits >= 24 {
                        break;
                    }
                    continue;
                }
            }
            pos += 1;
        }
        match codec {
            VideoBitstreamCodec::H264 => h264_hits = hits,
            VideoBitstreamCodec::Hevc => hevc_hits = hits,
        }
    }
    if h264_hits == 0 && hevc_hits == 0 {
        None
    } else if hevc_hits > h264_hits {
        Some("hevc")
    } else {
        Some("h264")
    }
}

fn detect_best_codec_and_nls(
    file: &mut File,
    ranges: &[(u64, u64)],
    candidates: &[(VideoBitstreamCodec, u8)],
) -> Result<(VideoBitstreamCodec, u8), String> {
    let (s, e) = ranges[0];
    let probe_len = (e - s).min(4 * 1024 * 1024) as usize;
    let mut sample = vec![0u8; probe_len];
    file.seek(SeekFrom::Start(s)).map_err(|err| err.to_string())?;
    file.read_exact(&mut sample).map_err(|err| err.to_string())?;

    let mut best = candidates[0];
    let mut best_score = 0usize;
    for &(codec, nls) in candidates {
        let mut hits = 0usize;
        let mut pos = 0usize;
        while pos + (nls as usize) + 4 <= sample.len() {
            if let Some(p) = parse_nal_at(&sample, pos, s + pos as u64, codec, nls) {
                let end = pos + p.len_prefix_bytes + p.payload_len;
                if end <= sample.len() && confirm_follow_on(&sample, end, s, codec, nls, 16 * 1024).is_some() {
                    hits += 1;
                    pos = end;
                    if hits >= 24 {
                        break;
                    }
                    continue;
                }
            }
            pos += 1;
        }
        if hits > best_score {
            best_score = hits;
            best = (codec, nls);
        }
    }
    Ok(best)
}

fn has_dense_annex_b(file: &mut File, ranges: &[(u64, u64)]) -> Result<bool, String> {
    let (s, e) = ranges[0];
    let probe_len = (e - s).min(1024 * 1024) as usize;
    if probe_len < 64 {
        return Ok(false);
    }
    let mut buf = vec![0u8; probe_len];
    file.seek(SeekFrom::Start(s)).map_err(|err| err.to_string())?;
    file.read_exact(&mut buf).map_err(|err| err.to_string())?;
    let count = buf.windows(4).filter(|w| *w == [0, 0, 0, 1]).count();
    Ok(count >= 8)
}

fn scan_length_prefixed_stream(
    file: &mut File,
    ranges: &[(u64, u64)],
    codec: VideoBitstreamCodec,
    nls: u8,
    reference: Option<&ReferenceProfile>,
    work_dir: &Path,
    _deep_scan: bool,
    timeout_secs: u64,
    progress_cb: Option<&ProgressCallback>,
    start_ts: Instant,
    total_bytes: u64,
) -> Result<BitstreamScanOutput, String> {
    const WINDOW_BYTES: usize = 4 * 1024 * 1024;
    const GAP_SPLIT_THRESHOLD: u64 = 512 * 1024;

    let param_sets = reference
        .map(|r| r.parameter_sets_annex_b.clone())
        .unwrap_or_default();

    let mut segments: Vec<ExtractedSegmentStream> = Vec::new();
    let mut gaps: Vec<UncertaintyGap> = Vec::new();

    let mut cur_writer: Option<BufWriter<File>> = None;
    let mut cur_aac_writer: Option<BufWriter<File>> = None;
    let mut cur_seg_path: Option<PathBuf> = None;
    let mut cur_aac_path: Option<PathBuf> = None;
    let mut cur_seg_start: u64 = 0;
    let mut cur_seg_end: u64 = 0;
    let mut cur_frames: u64 = 0;
    let mut cur_idrs: u64 = 0;
    let mut cur_aac_frames: u64 = 0;
    let mut has_b_frames = reference.map(|r| r.has_b_frames).unwrap_or(false);
    let mut inline_param_seen = false;
    let mut last_nal_end: Option<u64> = None;

    let mut total_recognized_video_bytes = 0u64;
    let mut total_recognized_audio_bytes = 0u64;
    let mut total_nals = 0u64;
    let mut prior_ranges_scanned = 0u64;
    let mut timed_out = false;

    let finalize_segment = |writer_opt: &mut Option<BufWriter<File>>,
                            aac_writer_opt: &mut Option<BufWriter<File>>,
                            path_opt: &mut Option<PathBuf>,
                            aac_path_opt: &mut Option<PathBuf>,
                            s: u64,
                            e: u64,
                            frames: u64,
                            idrs: u64,
                            aac_frames: u64,
                            has_b: bool,
                            segs: &mut Vec<ExtractedSegmentStream>| {
        if let Some(mut w) = writer_opt.take() {
            let _ = w.flush();
        }
        if let Some(mut aw) = aac_writer_opt.take() {
            let _ = aw.flush();
        }
        if let Some(p) = path_opt.take() {
            let ap = aac_path_opt.take().filter(|_| aac_frames > 0);
            if frames > 0 && idrs > 0 {
                segs.push(ExtractedSegmentStream {
                    index: segs.len() + 1,
                    codec,
                    annex_b_path: p,
                    aac_adts_path: ap,
                    source_byte_start: s,
                    source_byte_end: e,
                    frame_count: frames,
                    idr_count: idrs,
                    has_b_frames_hint: has_b,
                    aac_frames_recovered: aac_frames,
                });
            } else {
                let _ = std::fs::remove_file(p);
                if let Some(ap) = ap {
                    let _ = std::fs::remove_file(ap);
                }
            }
        }
    };

    let mut buf = vec![0u8; WINDOW_BYTES];

    for &(range_start, range_end) in ranges {
        let mut cursor = range_start;
        let mut locked = false;
        let mut unknown_gap_start: Option<u64> = None;

        while cursor < range_end {
            if timeout_secs > 0 && start_ts.elapsed().as_secs() >= timeout_secs {
                timed_out = true;
                break;
            }

            let avail = (range_end - cursor).min(WINDOW_BYTES as u64) as usize;
            if avail < (nls as usize) + 3 {
                break;
            }
            file.seek(SeekFrom::Start(cursor)).map_err(|e| e.to_string())?;
            file.read_exact(&mut buf[..avail]).map_err(|e| e.to_string())?;

            let mut off = 0usize;
            let safe_limit = if avail == WINDOW_BYTES {
                // Keep trailing 1 MiB for overlap into next read unless end of range
                avail - 1024 * 1024
            } else {
                avail
            };

            while off < safe_limit {
                if timeout_secs > 0 && start_ts.elapsed().as_secs() >= timeout_secs {
                    timed_out = true;
                    break;
                }

                if let Some(parsed) = parse_nal_at(&buf[..avail], off, cursor + off as u64, codec, nls) {
                    let nal_total = parsed.len_prefix_bytes + parsed.payload_len;
                    let nal_payload_end_in_buf = off + nal_total;

                    // Handle rare large NAL (> safe window): read straight from File if valid end fits range
                    let nal_abs_end = parsed.file_offset + nal_total as u64;
                    if nal_abs_end > range_end {
                        off += 1;
                        continue;
                    }

                    // Sanity cap on non-IDR slice length when unverified by direct +0 boundary
                    let direct_adjacent = nal_payload_end_in_buf + (nls as usize) + 3 <= avail
                        && parse_nal_at(
                            &buf[..avail],
                            nal_payload_end_in_buf,
                            nal_abs_end,
                            codec,
                            nls,
                        )
                        .is_some();

                    // Inline SPS/PPS/VPS inside MP4 mdat are ONLY genuine when immediately (+0) followed by PPS/IDR/SEI/VCL!
                    if parsed.is_param_set && !direct_adjacent {
                        off += 1;
                        continue;
                    }

                    let non_idr_huge = !parsed.is_idr && !parsed.is_param_set && parsed.payload_len > 512 * 1024 && !direct_adjacent;
                    let any_insane = parsed.payload_len > 3_500_000 && !direct_adjacent;
                    if non_idr_huge || any_insane {
                        off += 1;
                        continue;
                    }

                    // Prevent false length-prefix (>4KB) inside audio/noise from jumping over nearby 3-hop real NALs
                    if !direct_adjacent && parsed.payload_len > 4096 && off + 2048 <= avail {
                        let scan_lim = avail.min(off + 2048).min(nal_payload_end_in_buf);
                        let mut subsumed = false;
                        let mut p2 = off + 1;
                        while p2 + (nls as usize) + 4 <= scan_lim {
                            if let Some(n2) = parse_nal_at(&buf[..avail], p2, cursor + p2 as u64, codec, nls) {
                                let end2 = p2 + n2.len_prefix_bytes + n2.payload_len;
                                if end2 < nal_payload_end_in_buf
                                    && (n2.is_first_slice || n2.is_param_set)
                                    && confirm_chain_depth(&buf[..avail], p2, cursor, codec, nls, 3, 8192)
                                {
                                    subsumed = true;
                                    break;
                                }
                            }
                            p2 += 1;
                        }
                        if subsumed {
                            off += 1;
                            continue;
                        }
                    }

                    // Verification before accepting
                    let verified = if direct_adjacent {
                        true
                    } else if nal_payload_end_in_buf + 64 <= avail {
                        confirm_follow_on(
                            &buf[..avail],
                            nal_payload_end_in_buf,
                            cursor,
                            codec,
                            nls,
                            if locked { 24 * 1024 } else { 12 * 1024 },
                        )
                        .is_some()
                    } else {
                        // Near buffer boundary or very large frame: verify directly via tiny seek read
                        verify_follow_on_from_file(file, nal_abs_end, range_end, codec, nls)
                    };

                    if verified {
                        // If we were unlocked, acquire lock only when chain has >= 3 hits or starts with IDR/SPS
                        if !locked {
                            let chain_ok = if parsed.is_idr || parsed.is_param_set {
                                confirm_chain_depth(&buf[..avail], off, cursor, codec, nls, 2, 32 * 1024)
                            } else if nal_payload_end_in_buf <= avail {
                                confirm_chain_depth(&buf[..avail], off, cursor, codec, nls, 3, 32 * 1024)
                            } else {
                                false
                            };
                            if !chain_ok {
                                off += 1;
                                continue;
                            }
                            locked = true;
                        }

                        // Record any skipped corruption/unknown gap before this resync
                        if let Some(gs) = unknown_gap_start.take() {
                            if parsed.file_offset > gs {
                                let gap_len = parsed.file_offset - gs;
                                if gap_len >= GAP_SPLIT_THRESHOLD && cur_writer.is_some() {
                                    // Flush previous segment cleanly before starting new one
                                    finalize_segment(
                                        &mut cur_writer,
                                        &mut cur_aac_writer,
                                        &mut cur_seg_path,
                                        &mut cur_aac_path,
                                        cur_seg_start,
                                        cur_seg_end,
                                        cur_frames,
                                        cur_idrs,
                                        cur_aac_frames,
                                        has_b_frames,
                                        &mut segments,
                                    );
                                    cur_frames = 0;
                                    cur_idrs = 0;
                                    cur_aac_frames = 0;
                                    inline_param_seen = false;
                                }
                                if gap_len >= 64 * 1024 || cur_writer.is_none() {
                                    gaps.push(UncertaintyGap {
                                        source_byte_start: gs,
                                        source_byte_end: parsed.file_offset,
                                        reason: if cur_writer.is_none() {
                                            "Unaligned/non-keyframe prefix or unidentifiable bytes".into()
                                        } else {
                                            "Stream discontinuity / damaged gap resynchronized".into()
                                        },
                                    });
                                }
                            }
                        }

                        // Check interstitial audio chunk between previous confirmed video NAL end and this NAL start
                        if let Some(prev_end) = last_nal_end {
                            if parsed.file_offset > prev_end && (parsed.file_offset - prev_end) <= 32 * 1024 {
                                if let (Some(ref mut aw), Some(ref r)) = (&mut cur_aac_writer, reference) {
                                    let gap_sz = (parsed.file_offset - prev_end) as usize;
                                    if prev_end >= cursor && (parsed.file_offset - cursor) as usize <= avail {
                                        let rel_s = (prev_end - cursor) as usize;
                                        let rel_e = (parsed.file_offset - cursor) as usize;
                                        let aac_written = try_extract_interstitial_aac(
                                            &buf[rel_s..rel_e],
                                            r,
                                            aw,
                                        );
                                        if aac_written > 0 {
                                            cur_aac_frames += aac_written as u64;
                                            total_recognized_audio_bytes += gap_sz as u64;
                                        }
                                    }
                                }
                            }
                        }

                        // Start a new segment only at IDR (or param set followed by IDR)
                        if cur_writer.is_none() {
                            if !parsed.is_idr && !parsed.is_param_set {
                                // Skip non-keyframe leading slices until first valid random-access picture
                                total_nals += 1;
                                last_nal_end = Some(nal_abs_end);
                                if nal_payload_end_in_buf <= safe_limit {
                                    off = nal_payload_end_in_buf;
                                } else {
                                    cursor = nal_abs_end;
                                    break;
                                }
                                continue;
                            }
                            let seg_idx = segments.len() + 1;
                            let seg_ext = if codec == VideoBitstreamCodec::H264 { "h264" } else { "h265" };
                            let sp = work_dir.join(format!("seg_{seg_idx:03}.{seg_ext}"));
                            let ap = work_dir.join(format!("seg_{seg_idx:03}.aac"));
                            let wf = File::create(&sp).map_err(|e| e.to_string())?;
                            cur_writer = Some(BufWriter::with_capacity(512 * 1024, wf));
                            if reference.and_then(|r| r.audio_sample_rate_index).is_some()
                                && reference.and_then(|r| r.audio_channels).is_some()
                            {
                                if let Ok(af) = File::create(&ap) {
                                    cur_aac_writer = Some(BufWriter::with_capacity(256 * 1024, af));
                                    cur_aac_path = Some(ap);
                                }
                            }
                            cur_seg_path = Some(sp);
                            cur_seg_start = parsed.file_offset;
                            cur_seg_end = nal_abs_end;
                            cur_frames = 0;
                            cur_idrs = 0;
                            cur_aac_frames = 0;
                            inline_param_seen = false;
                        }

                        if let Some(ref mut w) = cur_writer {
                            if parsed.is_param_set {
                                inline_param_seen = true;
                            }
                            if parsed.is_idr && !inline_param_seen && !param_sets.is_empty() {
                                // Prepend reference SPS/PPS/VPS before IDR Access Unit!
                                w.write_all(&param_sets).map_err(|e| e.to_string())?;
                            }
                            if parsed.is_idr {
                                inline_param_seen = false; // reset so future IDR gets ref SPS/PPS if no inline params precede it
                                cur_idrs += 1;
                            }
                            if parsed.is_vcl && parsed.is_first_slice {
                                cur_frames += 1;
                            }
                            if parsed.is_b_slice {
                                has_b_frames = true;
                            }

                            // Write Annex B start code + payload (from buf or direct file read if very large NAL)
                            w.write_all(&[0x00, 0x00, 0x00, 0x01]).map_err(|e| e.to_string())?;
                            if nal_payload_end_in_buf <= avail {
                                w.write_all(&buf[off + parsed.len_prefix_bytes..nal_payload_end_in_buf])
                                    .map_err(|e| e.to_string())?;
                            } else {
                                write_payload_from_file(
                                    file,
                                    parsed.file_offset + parsed.len_prefix_bytes as u64,
                                    parsed.payload_len,
                                    w,
                                )?;
                            }
                            cur_seg_end = nal_abs_end;
                            last_nal_end = Some(nal_abs_end);
                            total_nals += 1;
                            total_recognized_video_bytes += nal_total as u64;
                        }

                        if let Some(cb) = progress_cb {
                            if total_nals % 500 == 0 {
                                cb(RecoveryProgress {
                                    stage: "bitstream_scan".into(),
                                    current_strategy: "Strategy C: Custom NAL/AU binary scan".into(),
                                    bytes_scanned: (prior_ranges_scanned + (parsed.file_offset - range_start)).min(total_bytes),
                                    total_bytes,
                                    recognized_packets: total_nals,
                                    recovered_frames: cur_frames
                                        + segments.iter().map(|s| s.frame_count).sum::<u64>(),
                                    elapsed_secs: start_ts.elapsed().as_secs_f64(),
                                });
                            }
                        }

                        if nal_payload_end_in_buf <= safe_limit {
                            off = nal_payload_end_in_buf;
                            continue;
                        } else {
                            // Advance file cursor straight to end of this NAL!
                            cursor = nal_abs_end;
                            break;
                        }
                    }
                }

                // Not a valid NAL at `off`. Track gap when locked too long.
                if unknown_gap_start.is_none() {
                    unknown_gap_start = Some(cursor + off as u64);
                }
                if locked {
                    if let Some(gs) = unknown_gap_start {
                        if (cursor + off as u64).saturating_sub(gs) > 128 * 1024 {
                            locked = false;
                        }
                    }
                }
                off += 1;
            }

            if timed_out {
                break;
            }

            if off >= safe_limit {
                let advance = off as u64;
                cursor = cursor.saturating_add(advance);
            }
        }
        prior_ranges_scanned += range_end.saturating_sub(range_start);

        if let Some(gs) = unknown_gap_start.take() {
            if range_end > gs + 1024 {
                gaps.push(UncertaintyGap {
                    source_byte_start: gs,
                    source_byte_end: range_end,
                    reason: "Trailing unrecognized or audio bytes at end of media range".into(),
                });
            }
        }
    }

    finalize_segment(
        &mut cur_writer,
        &mut cur_aac_writer,
        &mut cur_seg_path,
        &mut cur_aac_path,
        cur_seg_start,
        cur_seg_end,
        cur_frames,
        cur_idrs,
        cur_aac_frames,
        has_b_frames,
        &mut segments,
    );

    let timing_note = if has_b_frames {
        "Video timing reconstructed from reference framerate/PTS order (B-frame reordering present)".into()
    } else {
        "Video timing inferred from reference or default 25fps/30fps constant cadence".into()
    };

    Ok(BitstreamScanOutput {
        codec,
        segments,
        gaps,
        total_recognized_video_bytes,
        total_recognized_audio_bytes,
        total_nals,
        timing_note,
        timed_out,
    })
}

fn scan_annex_b_stream(
    file: &mut File,
    ranges: &[(u64, u64)],
    codec: VideoBitstreamCodec,
    reference: Option<&ReferenceProfile>,
    work_dir: &Path,
    timeout_secs: u64,
    _progress_cb: Option<&ProgressCallback>,
    start_ts: Instant,
    _total_bytes: u64,
) -> Result<BitstreamScanOutput, String> {
    let seg_ext = if codec == VideoBitstreamCodec::H264 { "h264" } else { "h265" };
    let sp = work_dir.join(format!("seg_001.{seg_ext}"));
    let mut writer = BufWriter::new(File::create(&sp).map_err(|e| e.to_string())?);
    let mut param_sets = reference.map(|r| r.parameter_sets_annex_b.clone()).unwrap_or_default();
    if !param_sets.is_empty() {
        writer.write_all(&param_sets).map_err(|e| e.to_string())?;
    }

    let mut total_bytes = 0u64;
    let mut frames = 0u64;
    let mut idrs = 0u64;
    let mut buf = vec![0u8; 2 * 1024 * 1024];
    let mut timed_out = false;

    for &(rs, re) in ranges {
        let mut pos = rs;
        file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        while pos < re {
            if timeout_secs > 0 && start_ts.elapsed().as_secs() >= timeout_secs {
                timed_out = true;
                break;
            }
            let to_read = (re - pos).min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).map_err(|e| e.to_string())?;
            writer.write_all(&buf[..to_read]).map_err(|e| e.to_string())?;
            for w in buf[..to_read].windows(5) {
                if w[0..4] == [0, 0, 0, 1] {
                    if codec == VideoBitstreamCodec::H264 {
                        let nt = w[4] & 0x1F;
                        if nt == 5 {
                            idrs += 1;
                            frames += 1;
                        } else if nt == 1 {
                            frames += 1;
                        }
                    } else {
                        let nt = (w[4] >> 1) & 0x3F;
                        if matches!(nt, 19 | 20 | 21) {
                            idrs += 1;
                            frames += 1;
                        } else if nt <= 18 {
                            frames += 1;
                        }
                    }
                }
            }
            pos += to_read as u64;
            total_bytes += to_read as u64;
        }
    }
    let _ = param_sets.len();
    param_sets.clear();
    writer.flush().map_err(|e| e.to_string())?;

    let mut segs = Vec::new();
    if frames > 0 && idrs > 0 {
        segs.push(ExtractedSegmentStream {
            index: 1,
            codec,
            annex_b_path: sp,
            aac_adts_path: None,
            source_byte_start: ranges.first().map(|r| r.0).unwrap_or(0),
            source_byte_end: ranges.last().map(|r| r.1).unwrap_or(0),
            frame_count: frames,
            idr_count: idrs,
            has_b_frames_hint: reference.map(|r| r.has_b_frames).unwrap_or(false),
            aac_frames_recovered: 0,
        });
    }

    Ok(BitstreamScanOutput {
        codec,
        segments: segs,
        gaps: Vec::new(),
        total_recognized_video_bytes: total_bytes,
        total_recognized_audio_bytes: 0,
        total_nals: frames,
        timing_note: "Recovered directly from Annex B start-coded bitstream".into(),
        timed_out,
    })
}

fn confirm_follow_on(
    buf: &[u8],
    after_end: usize,
    buf_file_base: u64,
    codec: VideoBitstreamCodec,
    nls: u8,
    max_gap: usize,
) -> Option<usize> {
    let limit = buf.len().min(after_end.saturating_add(max_gap));
    if after_end + (nls as usize) + 3 > limit {
        return None;
    }
    // Check direct back-to-back first
    if parse_nal_at(buf, after_end, buf_file_base + after_end as u64, codec, nls).is_some() {
        return Some(after_end);
    }
    // Scan over potential interleaved AAC frames (up to max_gap), requiring strict first-slice or param set
    let mut probe = after_end + 1;
    while probe + (nls as usize) + 3 <= limit {
        if let Some(p) = parse_nal_at(buf, probe, buf_file_base + probe as u64, codec, nls) {
            if p.is_first_slice || p.is_param_set {
                // Verify this candidate itself continues cleanly so random bytes inside gaps never look valid
                let p_end = probe + p.len_prefix_bytes + p.payload_len;
                if p_end + (nls as usize) + 3 <= buf.len() {
                    if parse_nal_at(buf, p_end, buf_file_base + p_end as u64, codec, nls).is_some() {
                        return Some(probe);
                    }
                    // Allow 1 AAC gap after `probe` up to 8 KiB
                    let mut p2 = p_end + 1;
                    let lim2 = buf.len().min(p_end.saturating_add(8 * 1024));
                    while p2 + (nls as usize) + 3 <= lim2 {
                        if let Some(q) = parse_nal_at(buf, p2, buf_file_base + p2 as u64, codec, nls) {
                            if q.is_first_slice || q.is_param_set {
                                return Some(probe);
                            }
                        }
                        p2 += 1;
                    }
                } else {
                    return Some(probe);
                }
            }
        }
        probe += 1;
    }
    None
}

fn confirm_chain_depth(
    buf: &[u8],
    start_off: usize,
    buf_file_base: u64,
    codec: VideoBitstreamCodec,
    nls: u8,
    required_count: usize,
    max_gap: usize,
) -> bool {
    let mut cur = start_off;
    for step in 0..required_count {
        let Some(p) = parse_nal_at(buf, cur, buf_file_base + cur as u64, codec, nls) else {
            return false;
        };
        if step + 1 == required_count {
            return true;
        }
        let end = cur + p.len_prefix_bytes + p.payload_len;
        let Some(next) = confirm_follow_on(buf, end, buf_file_base, codec, nls, max_gap) else {
            return false;
        };
        cur = next;
    }
    true
}

fn verify_follow_on_from_file(
    file: &mut File,
    abs_offset: u64,
    range_end: u64,
    codec: VideoBitstreamCodec,
    nls: u8,
) -> bool {
    let cur_pos = file.stream_position().unwrap_or(0);
    let to_read = range_end.saturating_sub(abs_offset).min(32 * 1024) as usize;
    let ok = if to_read >= (nls as usize) + 3 {
        let mut tmp = vec![0u8; to_read];
        if file.seek(SeekFrom::Start(abs_offset)).is_ok() && file.read_exact(&mut tmp).is_ok() {
            confirm_follow_on(&tmp, 0, abs_offset, codec, nls, 16 * 1024).is_some()
        } else {
            false
        }
    } else {
        // Near very end of truncated file, accept structurally valid final VCL NAL
        true
    };
    let _ = file.seek(SeekFrom::Start(cur_pos));
    ok
}

fn write_payload_from_file(
    file: &mut File,
    payload_offset: u64,
    payload_len: usize,
    writer: &mut BufWriter<File>,
) -> Result<(), String> {
    let saved = file.stream_position().unwrap_or(0);
    file.seek(SeekFrom::Start(payload_offset)).map_err(|e| e.to_string())?;
    let mut rem = payload_len;
    let mut tmp = vec![0u8; 65536];
    while rem > 0 {
        let chunk = rem.min(tmp.len());
        file.read_exact(&mut tmp[..chunk]).map_err(|e| e.to_string())?;
        writer.write_all(&tmp[..chunk]).map_err(|e| e.to_string())?;
        rem -= chunk;
    }
    file.seek(SeekFrom::Start(saved)).map_err(|e| e.to_string())?;
    Ok(())
}

fn parse_nal_at(
    buf: &[u8],
    off: usize,
    abs_offset: u64,
    codec: VideoBitstreamCodec,
    nls: u8,
) -> Option<ParsedNal> {
    let nls_u = nls as usize;
    if off + nls_u + 2 > buf.len() {
        return None;
    }
    let payload_len = match nls {
        4 => u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as usize,
        2 => u16::from_be_bytes([buf[off], buf[off + 1]]) as usize,
        1 => buf[off] as usize,
        _ => return None,
    };
    if !(2..=8_000_000).contains(&payload_len) {
        return None;
    }
    let p_off = off + nls_u;
    match codec {
        VideoBitstreamCodec::H264 => {
            let hdr = buf[p_off];
            if (hdr & 0x80) != 0 {
                return None; // forbidden_zero_bit must be 0
            }
            let nri = (hdr >> 5) & 0x03;
            let nal_type = hdr & 0x1F;
            let rbsp_avail = buf.len().saturating_sub(p_off + 1).min(payload_len - 1);
            match nal_type {
                1 | 5 => {
                    if payload_len < 4 || rbsp_avail < 2 {
                        return None;
                    }
                    if nal_type == 5 && nri == 0 {
                        return None;
                    }
                    let slice = &buf[p_off + 1..p_off + 1 + rbsp_avail];
                    let mut br = BitReader::new(slice);
                    let first_mb = br.read_ue()?;
                    let slice_type = br.read_ue()?;
                    let pps_id = br.read_ue()?;
                    if first_mb > 36_000 || slice_type > 9 || pps_id > 0 {
                        return None;
                    }
                    if nal_type == 1 && nri == 0 && !matches!(slice_type, 1 | 6) {
                        return None; // Disposable NAL (nri=0) in MP4 H.264 is B/B-skip slice
                    }
                    if nal_type == 5 && !matches!(slice_type, 2 | 4 | 7 | 9) {
                        return None; // IDR slice MUST be I/SI slice!
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: true,
                        is_idr: nal_type == 5,
                        is_param_set: false,
                        is_first_slice: first_mb == 0,
                        is_b_slice: matches!(slice_type, 1 | 6),
                    })
                }
                6 => {
                    if nri != 0 || payload_len > 8192 {
                        return None;
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: false,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                7 | 8 => {
                    if nri == 0 || payload_len > 1024 {
                        return None;
                    }
                    if nal_type == 7 && rbsp_avail >= 3 {
                        let profile = buf[p_off + 1];
                        if !matches!(profile, 44 | 66 | 77 | 83 | 86 | 88 | 100 | 110 | 118 | 122 | 128 | 134 | 138 | 144 | 244) {
                            return None;
                        }
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: true,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                9 => {
                    if nri != 0 || payload_len != 2 {
                        return None;
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: false,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                _ => None,
            }
        }
        VideoBitstreamCodec::Hevc => {
            if p_off + 2 > buf.len() || payload_len < 3 {
                return None;
            }
            let h0 = buf[p_off];
            let h1 = buf[p_off + 1];
            if (h0 & 0x80) != 0 {
                return None;
            }
            let nal_type = (h0 >> 1) & 0x3F;
            let layer_id = ((h0 & 0x01) << 5) | (h1 >> 3);
            let tid_plus1 = h1 & 0x07;
            if tid_plus1 == 0 || layer_id > 1 {
                return None;
            }
            let rbsp_avail = buf.len().saturating_sub(p_off + 2).min(payload_len - 2);
            match nal_type {
                0..=21 => {
                    if rbsp_avail < 1 {
                        return None;
                    }
                    let first_slice_flag = (buf[p_off + 2] & 0x80) != 0;
                    // For first slice in pic, check slice_pic_parameter_set_id Exp-Golomb sanity
                    if first_slice_flag && rbsp_avail >= 2 {
                        let slice = &buf[p_off + 2..p_off + 2 + rbsp_avail];
                        let mut br = BitReader::new(slice);
                        let _first = br.read_bit()?;
                        if matches!(nal_type, 16..=21) {
                            let _no_output_prior = br.read_bit()?;
                        }
                        let pps_id = br.read_ue()?;
                        if pps_id > 0 {
                            return None;
                        }
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: true,
                        is_idr: matches!(nal_type, 16..=21),
                        is_param_set: false,
                        is_first_slice: first_slice_flag,
                        is_b_slice: matches!(nal_type, 0 | 1),
                    })
                }
                32 | 33 | 34 => {
                    if payload_len > 1024 || tid_plus1 != 1 {
                        return None;
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: true,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                35 => {
                    if payload_len != 3 {
                        return None;
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: false,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                39 | 40 => {
                    if payload_len > 8192 {
                        return None;
                    }
                    Some(ParsedNal {
                        file_offset: abs_offset,
                        len_prefix_bytes: nls_u,
                        payload_len,
                        is_vcl: false,
                        is_idr: false,
                        is_param_set: false,
                        is_first_slice: false,
                        is_b_slice: false,
                    })
                }
                _ => None,
            }
        }
    }
}

/// Strategy E: Interstitial raw AAC frame extractor.
/// Validates single raw AAC payload (or 2-frame/3-frame split using reference avg packet bytes & `ID_CPE`/`ID_SCE`)
/// and wraps each raw AAC frame into an ADTS header for FFmpeg muxing.
fn try_extract_interstitial_aac(
    gap: &[u8],
    reference: &ReferenceProfile,
    out: &mut BufWriter<File>,
) -> usize {
    let Some(sr_idx) = reference.audio_sample_rate_index else {
        return 0;
    };
    let Some(channels) = reference.audio_channels else {
        return 0;
    };
    let aot = reference.audio_object_type.unwrap_or(2);
    if gap.len() < 16 || gap.len() > 16384 {
        return 0;
    }

    // Expected first 3-bit syntax element: 1 (CPE) for stereo, 0 (SCE) for mono
    let expect_ele: u8 = if channels == 1 { 0 } else { 1 };
    if (gap[0] >> 5) != expect_ele {
        return 0;
    }

    let avg = reference.avg_audio_packet_bytes.unwrap_or(365) as usize;
    let avg_lo = (avg * 45) / 100;
    let avg_hi = (avg * 145) / 100;

    // Single raw AAC frame in gap
    if gap.len() >= avg_lo.max(16) && gap.len() <= avg_hi.min(2048) {
        if write_adts_frame(out, aot, sr_idx, channels as u8, gap).is_ok() {
            return 1;
        }
        return 0;
    }

    // Multi-frame packed AAC chunk inside gap (e.g. 2 or 3 frames back-to-back)
    if let Some(splits) = split_multi_aac_chunk(gap, expect_ele, avg, avg_lo, avg_hi) {
        let mut count = 0;
        for slice in splits {
            if write_adts_frame(out, aot, sr_idx, channels as u8, slice).is_ok() {
                count += 1;
            }
        }
        return count;
    }

    0
}

fn split_multi_aac_chunk<'a>(
    gap: &'a [u8],
    expect_ele: u8,
    avg: usize,
    avg_lo: usize,
    avg_hi: usize,
) -> Option<Vec<&'a [u8]>> {
    // Try k = 2..=6 frames based on expected avg size
    let k_est = ((gap.len() as f64) / (avg.max(64) as f64)).round() as usize;
    for k in [k_est, k_est.saturating_sub(1), k_est + 1] {
        if !(2..=6).contains(&k) {
            continue;
        }
        if let Some(parts) = find_aac_split_recursive(gap, k, expect_ele, gap[0], avg_lo, avg_hi) {
            return Some(parts);
        }
    }
    None
}

fn find_aac_split_recursive<'a>(
    slice: &'a [u8],
    remaining_frames: usize,
    expect_ele: u8,
    lead_byte_hint: u8,
    lo: usize,
    hi: usize,
) -> Option<Vec<&'a [u8]>> {
    if remaining_frames == 1 {
        if slice.len() >= lo && slice.len() <= hi && (slice[0] >> 5) == expect_ele {
            return Some(vec![slice]);
        }
        return None;
    }
    let min_pos = lo.min(slice.len());
    let max_pos = hi.min(slice.len().saturating_sub(lo));
    // Prefer candidates whose next byte matches lead_byte_hint (common constant-tag ICS header like 0x21)
    let mut best_fallback: Option<Vec<&'a [u8]>> = None;
    for split in min_pos..=max_pos {
        let next_b = slice[split];
        if (next_b >> 5) != expect_ele {
            continue;
        }
        let exact_hint = (next_b & 0xF0) == (lead_byte_hint & 0xF0);
        if exact_hint {
            if let Some(mut rest) = find_aac_split_recursive(
                &slice[split..],
                remaining_frames - 1,
                expect_ele,
                lead_byte_hint,
                lo,
                hi,
            ) {
                let mut res = vec![&slice[..split]];
                res.append(&mut rest);
                return Some(res);
            }
        } else if best_fallback.is_none() {
            if let Some(mut rest) = find_aac_split_recursive(
                &slice[split..],
                remaining_frames - 1,
                expect_ele,
                lead_byte_hint,
                lo,
                hi,
            ) {
                let mut res = vec![&slice[..split]];
                res.append(&mut rest);
                best_fallback = Some(res);
            }
        }
    }
    best_fallback
}

fn write_adts_frame(
    out: &mut BufWriter<File>,
    aot: u8,
    sr_idx: u8,
    channels: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let frame_len = 7 + payload.len();
    if frame_len > 8191 {
        return Ok(());
    }
    let profile_minus_1 = aot.saturating_sub(1) & 0x03;
    let hdr = [
        0xFF,
        0xF1, // MPEG-4, layer 0, no CRC
        (profile_minus_1 << 6) | ((sr_idx & 0x0F) << 2) | ((channels >> 2) & 0x01),
        ((channels & 0x03) << 6) | (((frame_len >> 11) & 0x03) as u8),
        ((frame_len >> 3) & 0xFF) as u8,
        (((frame_len & 0x07) << 5) as u8) | 0x1F,
        0xFC,
    ];
    out.write_all(&hdr)?;
    out.write_all(payload)?;
    Ok(())
}

pub fn to_segment_infos(streams: &[ExtractedSegmentStream], paths: &[String]) -> Vec<RecoveredSegmentInfo> {
    streams
        .iter()
        .enumerate()
        .map(|(i, s)| RecoveredSegmentInfo {
            index: s.index,
            source_byte_start: s.source_byte_start,
            source_byte_end: s.source_byte_end,
            frame_count: s.frame_count,
            idr_count: s.idr_count,
            output_path: paths.get(i).cloned(),
        })
        .collect()
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.bit_pos >= self.data.len() * 8 {
            return None;
        }
        let byte = self.data[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;
        Some(((byte >> shift) & 1) as u32)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.read_bit()? == 0 {
            zeros += 1;
            if zeros > 24 {
                return None;
            }
        }
        let mut val = 1u32 << zeros;
        for i in (0..zeros).rev() {
            val |= self.read_bit()? << i;
        }
        Some(val - 1)
    }
}
