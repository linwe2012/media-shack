pub mod bitstream;
pub mod evaluator;
pub mod isobmff;
pub mod reference;
pub mod strategies;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub type ProgressCallback = Arc<dyn Fn(RecoveryProgress) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    ConventionalMp4,
    FragmentedMp4,
    MpegTs,
    RawBitstream,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomNode {
    pub tag: String,
    pub offset: u64,
    pub declared_size: u64,
    pub physical_end: u64,
    pub header_size: u64,
    pub size_mode: String,
    pub truncated: bool,
    pub malformed_reason: Option<String>,
    pub children: Vec<AtomNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorruptionRegion {
    pub start: u64,
    pub end: u64,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanSignature {
    pub offset: u64,
    pub tag: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisReport {
    pub file_path: String,
    pub file_size: u64,
    pub sha256: String,
    pub container_kind: ContainerKind,
    pub atoms: Vec<AtomNode>,
    pub has_ftyp: bool,
    pub has_moov: bool,
    pub has_moof: bool,
    pub has_mdat: bool,
    pub mdat_ranges: Vec<(u64, u64)>,
    pub missing_atoms: Vec<String>,
    pub anomalies: Vec<String>,
    pub orphan_signatures: Vec<OrphanSignature>,
    pub corruption_regions: Vec<CorruptionRegion>,
    pub summary_lines: Vec<String>,
    pub recommended_strategies: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VrMetadataInfo {
    pub is_stereoscopic: bool,
    pub stereo_mode: Option<String>,
    pub projection: Option<String>,
    pub raw_st3d_detected: bool,
    pub raw_sv3d_detected: bool,
    pub limitation_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceProfile {
    pub file_path: String,
    pub compatibility_score: f64,
    pub compatibility_notes: Vec<String>,
    pub video_codec: Option<String>,
    pub codec_tag: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub fps: Option<f64>,
    pub timescale: Option<u32>,
    pub has_b_frames: bool,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u8>,
    pub profile: Option<String>,
    pub level: Option<i32>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub sample_aspect_ratio: Option<String>,
    pub display_aspect_ratio: Option<String>,
    pub nal_length_size: u8,
    #[serde(skip_serializing)]
    pub parameter_sets_annex_b: Vec<u8>,
    #[serde(skip_serializing)]
    pub avcc_or_hvcc_raw: Option<Vec<u8>>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u16>,
    pub audio_sample_rate_index: Option<u8>,
    pub audio_object_type: Option<u8>,
    #[serde(skip_serializing)]
    pub audio_specific_config: Option<Vec<u8>>,
    pub typical_gop_frames: Option<u32>,
    pub avg_video_packet_bytes: Option<u64>,
    pub avg_audio_packet_bytes: Option<u64>,
    pub interleave_pattern: Option<String>,
    pub vr_info: VrMetadataInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub strategy_name: String,
    pub output_path: String,
    pub segment_paths: Vec<String>,
    pub succeeded: bool,
    pub score: f64,
    pub decoded_video_frames: u64,
    pub decode_error_count: u64,
    pub recovered_duration_secs: Option<f64>,
    pub audio_duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub idr_frames: u64,
    pub timestamps_monotonic: bool,
    pub av_sync_diff_secs: Option<f64>,
    pub input_bytes_mapped_pct: f64,
    pub black_frame_ratio: Option<f64>,
    pub frozen_frame_ratio: Option<f64>,
    pub output_bytes: u64,
    pub notes: Vec<String>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveredSegmentInfo {
    pub index: usize,
    pub source_byte_start: u64,
    pub source_byte_end: u64,
    pub frame_count: u64,
    pub idr_count: u64,
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncertaintyGap {
    pub source_byte_start: u64,
    pub source_byte_end: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceSummary {
    pub recovered_data: Vec<String>,
    pub inferred_metadata: Vec<String>,
    pub irrecoverable_or_omitted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryProgress {
    pub stage: String,
    pub current_strategy: String,
    pub bytes_scanned: u64,
    pub total_bytes: u64,
    pub recognized_packets: u64,
    pub recovered_frames: u64,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryOptions {
    pub input_path: PathBuf,
    pub reference_paths: Vec<PathBuf>,
    pub output_path: PathBuf,
    pub video_only: bool,
    pub deep_scan: bool,
    pub timeout_secs: u64,
    pub generate_contact_sheet: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub input_path: String,
    pub output_path: Option<String>,
    pub sha256_before: String,
    pub sha256_after: String,
    pub input_untouched_verified: bool,
    pub diagnosis: DiagnosisReport,
    pub references: Vec<ReferenceProfile>,
    pub candidates: Vec<CandidateEvaluation>,
    pub selected_strategy: Option<String>,
    pub segments: Vec<RecoveredSegmentInfo>,
    pub gaps: Vec<UncertaintyGap>,
    pub provenance: ProvenanceSummary,
    pub vr_status: VrMetadataInfo,
    pub contact_sheet_path: Option<String>,
    pub elapsed_secs: f64,
    pub overall_status: String,
    pub honest_summary: String,
}

pub fn diagnose_file(
    input: &Path,
    progress_cb: Option<&ProgressCallback>,
) -> Result<DiagnosisReport, String> {
    isobmff::diagnose_mp4_file(input, progress_cb)
}

pub fn recover_file(
    opts: &RecoveryOptions,
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    progress_cb: Option<&ProgressCallback>,
) -> Result<RecoveryReport, String> {
    let start = Instant::now();
    if opts.input_path == opts.output_path {
        return Err("Refusing to write output over original damaged input path.".into());
    }

    let diag = isobmff::diagnose_mp4_file(&opts.input_path, progress_cb)?;
    let sha_before = diag.sha256.clone();

    let ref_slice: Vec<&Path> = opts.reference_paths.iter().map(|p| p.as_path()).collect();
    let mut references = reference::analyze_references(&ref_slice, Some(ffprobe_path), &diag);

    // Fallback: if FFprobe didn't get parameter sets via binary avcC/hvcC (e.g. MKV/TS reference), use FFmpeg extradata dump
    for r in &mut references {
        if r.parameter_sets_annex_b.is_empty() {
            if let Some(codec) = r.video_codec.as_deref() {
                if let Some(ps) = reference::extract_annex_b_param_sets_via_ffmpeg(
                    ffmpeg_path,
                    Path::new(&r.file_path),
                    codec,
                ) {
                    r.parameter_sets_annex_b = ps;
                    r.compatibility_score = (r.compatibility_score + 20.0).min(100.0);
                    r.compatibility_notes
                        .push("Extracted SPS/PPS/VPS via FFmpeg bitstream filter fallback".into());
                }
            }
        }
    }
    references.sort_by(|a, b| {
        b.compatibility_score
            .partial_cmp(&a.compatibility_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let pipeline_out = strategies::run_recovery_pipeline(
        opts,
        &diag,
        &references,
        ffmpeg_path,
        ffprobe_path,
        progress_cb,
        start,
    )?;

    // Section 1 Safety Requirement: verify original input SHA-256 unchanged!
    let sha_after = isobmff::sha256_file(&opts.input_path)?;
    if sha_before != sha_after {
        return Err(format!(
            "CRITICAL SAFETY VIOLATION: Original input SHA-256 changed! Before={sha_before}, After={sha_after}"
        ));
    }

    Ok(RecoveryReport {
        input_path: opts.input_path.to_string_lossy().to_string(),
        output_path: pipeline_out.selected_output,
        sha256_before: sha_before,
        sha256_after: sha_after,
        input_untouched_verified: true,
        diagnosis: diag,
        references,
        candidates: pipeline_out.candidates,
        selected_strategy: pipeline_out.selected_strategy,
        segments: pipeline_out.segments,
        gaps: pipeline_out.gaps,
        provenance: pipeline_out.provenance,
        vr_status: pipeline_out.vr_status,
        contact_sheet_path: pipeline_out.contact_sheet_path,
        elapsed_secs: start.elapsed().as_secs_f64(),
        overall_status: pipeline_out.overall_status,
        honest_summary: pipeline_out.honest_summary,
    })
}

pub fn format_diagnosis_human(d: &DiagnosisReport) -> String {
    let mut out = String::new();
    out.push_str("=== MP4 / Media Diagnosis Report ===\n");
    for line in &d.summary_lines {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str("\nTop-level Atom Structure:\n");
    if d.atoms.is_empty() {
        out.push_str("  (No valid top-level ISO BMFF atoms parsed)\n");
    } else {
        for a in &d.atoms {
            out.push_str(&format!(
                "  [offset {:>10}] `{}` declared={} physical_end={} ({}){}\n",
                a.offset,
                a.tag,
                a.declared_size,
                a.physical_end,
                a.size_mode,
                if a.truncated { " [TRUNCATED]" } else { "" }
            ));
        }
    }
    if !d.anomalies.is_empty() {
        out.push_str("\nAnomalies:\n");
        for an in &d.anomalies {
            out.push_str(&format!("  * {an}\n"));
        }
    }
    if !d.corruption_regions.is_empty() {
        out.push_str("\nSuspicious / Corrupt Byte Regions:\n");
        for r in d.corruption_regions.iter().take(20) {
            out.push_str(&format!("  * {}..{}: [{}] {}\n", r.start, r.end, r.kind, r.detail));
        }
    }
    if !d.recommended_strategies.is_empty() {
        out.push_str("\nRecommended Recovery Strategies:\n");
        for s in &d.recommended_strategies {
            out.push_str(&format!("  -> {s}\n"));
        }
    }
    out
}

pub fn format_recovery_human(r: &RecoveryReport) -> String {
    let mut out = format_diagnosis_human(&r.diagnosis);
    out.push_str("\n=== Recovery Outcome ===\n");
    out.push_str(&format!("Status: {}\n", r.overall_status));
    out.push_str(&format!(
        "Input Integrity Check: {} (SHA-256 unchanged: {})\n",
        if r.input_untouched_verified { "VERIFIED" } else { "FAILED" },
        r.sha256_after
    ));
    if let Some(ref s) = r.selected_strategy {
        out.push_str(&format!("Selected Strategy: {s}\n"));
    }
    if let Some(ref p) = r.output_path {
        out.push_str(&format!("Output File: {p}\n"));
    }
    if let Some(ref cs) = r.contact_sheet_path {
        out.push_str(&format!("Contact Sheet: {cs}\n"));
    }
    out.push_str(&format!("Summary: {}\n", r.honest_summary));

    out.push_str("\nCandidates Evaluated:\n");
    for c in &r.candidates {
        out.push_str(&format!(
            "  - [{}] {} | score={:.1} | frames={} | idr={} | errs={} | dur={:.2?}s | out={}\n",
            if c.succeeded { "PASS" } else { "FAIL" },
            c.strategy_name,
            c.score,
            c.decoded_video_frames,
            c.idr_frames,
            c.decode_error_count,
            c.recovered_duration_secs,
            c.output_path
        ));
        if let Some(ref rej) = c.rejection_reason {
            out.push_str(&format!("      Reason: {rej}\n"));
        }
    }

    out.push_str("\nData Provenance:\n");
    for item in &r.provenance.recovered_data {
        out.push_str(&format!("  [RECOVERED] {item}\n"));
    }
    for item in &r.provenance.inferred_metadata {
        out.push_str(&format!("  [INFERRED]  {item}\n"));
    }
    for item in &r.provenance.irrecoverable_or_omitted {
        out.push_str(&format!("  [OMITTED]   {item}\n"));
    }

    if !r.segments.is_empty() {
        out.push_str(&format!("\nRecovered Segments ({}):\n", r.segments.len()));
        for seg in &r.segments {
            out.push_str(&format!(
                "  #{}: bytes {}..{} | frames={} | idr={}{}\n",
                seg.index,
                seg.source_byte_start,
                seg.source_byte_end,
                seg.frame_count,
                seg.idr_count,
                seg.output_path
                    .as_ref()
                    .map(|p| format!(" -> {p}"))
                    .unwrap_or_default()
            ));
        }
    }
    if !r.gaps.is_empty() {
        out.push_str(&format!("\nIrrecoverable / Uncertain Gaps ({}):\n", r.gaps.len()));
        for g in r.gaps.iter().take(20) {
            out.push_str(&format!(
                "  * bytes {}..{} ({} bytes): {}\n",
                g.source_byte_start,
                g.source_byte_end,
                g.source_byte_end.saturating_sub(g.source_byte_start),
                g.reason
            ));
        }
    }
    out
}
