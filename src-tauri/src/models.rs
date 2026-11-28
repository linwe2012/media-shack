use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const RECIPE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetFamily {
    Archive,
    Remux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetVariant {
    Best,
    Balanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    Hevc,
    Av1,
}

impl VideoCodec {
    pub fn encoder_name(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "libx265",
            VideoCodec::Av1 => "libsvtav1",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "HEVC (libx265)",
            VideoCodec::Av1 => "AV1 (libsvtav1)",
        }
    }

    pub fn short_tag(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "hevc",
            VideoCodec::Av1 => "av1",
        }
    }

    pub fn crf_range(self) -> (u32, u32) {
        match self {
            VideoCodec::Hevc => (0, 51),
            VideoCodec::Av1 => (0, 63),
        }
    }

    pub fn valid_efforts(self) -> &'static [&'static str] {
        match self {
            VideoCodec::Hevc => &["medium", "slow", "slower", "veryslow"],
            VideoCodec::Av1 => &["3", "4", "5", "6", "7"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerFormat {
    Mkv,
    Mp4,
}

impl ContainerFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ContainerFormat::Mkv => "mkv",
            ContainerFormat::Mp4 => "mp4",
        }
    }

    pub fn muxer(self) -> &'static str {
        match self {
            ContainerFormat::Mkv => "matroska",
            ContainerFormat::Mp4 => "mp4",
        }
    }
}

fn default_ten_bit() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodeRecipe {
    pub schema_version: u32,
    pub family: PresetFamily,
    pub variant: PresetVariant,
    pub codec: VideoCodec,
    pub container: ContainerFormat,
    pub crf: u32,
    pub effort: String,
    #[serde(default)]
    pub tune: Option<String>,
    #[serde(default = "default_ten_bit")]
    pub ten_bit: bool,
    #[serde(default)]
    pub aq_mode: Option<u8>,
    pub is_custom: bool,
    pub preset_name: Option<String>,
}

impl EncodeRecipe {
    pub fn default_for(
        family: PresetFamily,
        variant: PresetVariant,
        codec: VideoCodec,
        container: ContainerFormat,
    ) -> Self {
        let (crf, effort) = match (family, variant, codec) {
            (PresetFamily::Remux, _, _) => (0, "none".to_string()),
            (PresetFamily::Archive, PresetVariant::Best, VideoCodec::Hevc) => {
                (17, "slower".to_string())
            }
            (PresetFamily::Archive, PresetVariant::Balanced, VideoCodec::Hevc) => {
                (20, "slow".to_string())
            }
            (PresetFamily::Archive, PresetVariant::Best, VideoCodec::Av1) => {
                (22, "3".to_string())
            }
            (PresetFamily::Archive, PresetVariant::Balanced, VideoCodec::Av1) => {
                (27, "5".to_string())
            }
        };
        Self {
            schema_version: RECIPE_SCHEMA_VERSION,
            family,
            variant,
            codec,
            container,
            crf,
            effort,
            tune: None,
            ten_bit: family == PresetFamily::Archive,
            aq_mode: None,
            is_custom: false,
            preset_name: None,
        }
    }

    pub fn archive_balanced_default() -> Self {
        Self::default_for(
            PresetFamily::Archive,
            PresetVariant::Balanced,
            VideoCodec::Hevc,
            ContainerFormat::Mkv,
        )
    }

    pub fn matches_builtin_defaults(&self) -> bool {
        let base = Self::default_for(self.family, self.variant, self.codec, self.container);
        self.crf == base.crf
            && self.effort == base.effort
            && self.tune.as_deref().unwrap_or("").is_empty()
            && self.ten_bit == base.ten_bit
            && self.aq_mode.is_none()
    }

    pub fn filename_tag(&self) -> String {
        match self.family {
            PresetFamily::Remux => "remux".to_string(),
            PresetFamily::Archive => {
                let v = match self.variant {
                    PresetVariant::Best => "best",
                    PresetVariant::Balanced => "balanced",
                };
                format!("archive-{}.{}", v, self.codec.short_tag())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedPreset {
    pub id: String,
    pub name: String,
    pub recipe: EncodeRecipe,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPriority {
    Background,
    BelowNormal,
    Normal,
}

impl Default for ProcessPriority {
    fn default() -> Self {
        ProcessPriority::Background
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPolicy {
    Continue,
    PauseOnFirst,
}

impl Default for ErrorPolicy {
    fn default() -> Self {
        ErrorPolicy::Continue
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    KeepOriginals,
    ReviewAfterCompletion,
    TrashAfterEachFile,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        CleanupPolicy::KeepOriginals
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub recipe: EncodeRecipe,
    pub priority: ProcessPriority,
    pub recursive_import: bool,
    pub destination_root: Option<String>,
    pub keep_awake: bool,
    pub error_policy: ErrorPolicy,
    pub min_savings_enabled: bool,
    pub min_savings_pct: f64,
    pub cleanup_policy: CleanupPolicy,
    pub full_decode_verify: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabType {
    Explore,
    #[serde(alias = "transcode")]
    Encode,
    #[serde(alias = "dedupe")]
    Organize,
    Repair,
}

impl Default for TabType {
    fn default() -> Self {
        TabType::Encode
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueTab {
    pub id: String,
    pub name: String,
    pub tab_order: i32,
    #[serde(default)]
    pub tab_type: TabType,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub recipe: EncodeRecipe,
    pub priority: ProcessPriority,
    pub recursive_import: bool,
    pub destination_root: Option<String>,
    pub keep_awake: bool,
    pub error_policy: ErrorPolicy,
    pub min_savings_enabled: bool,
    pub min_savings_pct: f64,
    pub cleanup_policy: CleanupPolicy,
    pub full_decode_verify: bool,
    pub is_running: bool,
    pub is_paused: bool,
    pub pause_after_current: bool,
    pub waiting_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Listed,
    Queued,
    Probing,
    Ready,
    NeedsAttention,
    Encoding,
    Verifying,
    Publishing,
    Completed,
    BelowSavingsTarget,
    Failed,
    Canceled,
    Interrupted,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Listed => "listed",
            JobStatus::Queued => "queued",
            JobStatus::Probing => "probing",
            JobStatus::Ready => "ready",
            JobStatus::NeedsAttention => "needs_attention",
            JobStatus::Encoding => "encoding",
            JobStatus::Verifying => "verifying",
            JobStatus::Publishing => "publishing",
            JobStatus::Completed => "completed",
            JobStatus::BelowSavingsTarget => "below_savings_target",
            JobStatus::Failed => "failed",
            JobStatus::Canceled => "canceled",
            JobStatus::Interrupted => "interrupted",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "listed" => JobStatus::Listed,
            "queued" => JobStatus::Queued,
            "probing" => JobStatus::Probing,
            "ready" => JobStatus::Ready,
            "needs_attention" => JobStatus::NeedsAttention,
            "encoding" => JobStatus::Encoding,
            "verifying" => JobStatus::Verifying,
            "publishing" => JobStatus::Publishing,
            "completed" => JobStatus::Completed,
            "below_savings_target" => JobStatus::BelowSavingsTarget,
            "failed" => JobStatus::Failed,
            "canceled" => JobStatus::Canceled,
            "interrupted" => JobStatus::Interrupted,
            _ => JobStatus::NeedsAttention,
        }
    }

    pub fn is_pending_editable(self) -> bool {
        matches!(
            self,
            JobStatus::Listed
                | JobStatus::Queued
                | JobStatus::Ready
                | JobStatus::NeedsAttention
                | JobStatus::Interrupted
                | JobStatus::Failed
                | JobStatus::Canceled
        )
    }

    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            JobStatus::Failed | JobStatus::Canceled | JobStatus::Interrupted | JobStatus::NeedsAttention
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupOutcome {
    None,
    Pending,
    Kept,
    Trashed,
    Skipped,
    Declined,
    Uncertain,
    Failed,
}

impl Default for CleanupOutcome {
    fn default() -> Self {
        CleanupOutcome::None
    }
}

impl CleanupOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanupOutcome::None => "none",
            CleanupOutcome::Pending => "pending",
            CleanupOutcome::Kept => "kept",
            CleanupOutcome::Trashed => "trashed",
            CleanupOutcome::Skipped => "skipped",
            CleanupOutcome::Declined => "declined",
            CleanupOutcome::Uncertain => "uncertain",
            CleanupOutcome::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => CleanupOutcome::Pending,
            "kept" => CleanupOutcome::Kept,
            "trashed" => CleanupOutcome::Trashed,
            "skipped" => CleanupOutcome::Skipped,
            "declined" => CleanupOutcome::Declined,
            "uncertain" => CleanupOutcome::Uncertain,
            "failed" => CleanupOutcome::Failed,
            _ => CleanupOutcome::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub volume_id: String,
    pub file_id: String,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub head_hash: String,
    pub link_count: u64,
    #[serde(default)]
    pub full_blake3: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispositionInfo {
    pub default: i32,
    pub dub: i32,
    pub original: i32,
    pub comment: i32,
    pub lyrics: i32,
    pub karaoke: i32,
    pub forced: i32,
    pub hearing_impaired: i32,
    pub visual_impaired: i32,
    pub attached_pic: i32,
}

impl Default for DispositionInfo {
    fn default() -> Self {
        Self {
            default: 0,
            dub: 0,
            original: 0,
            comment: 0,
            lyrics: 0,
            karaoke: 0,
            forced: 0,
            hearing_impaired: 0,
            visual_impaired: 0,
            attached_pic: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: usize,
    pub codec_type: String,
    pub codec_name: String,
    pub codec_long_name: Option<String>,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_aspect_ratio: Option<String>,
    pub display_aspect_ratio: Option<String>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u8>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub field_order: Option<String>,
    pub r_frame_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub is_vfr: bool,
    pub time_base: Option<String>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<u32>,
    pub nb_frames: Option<u64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub rotation: Option<i32>,
    pub side_data_types: Vec<String>,
    pub disposition: DispositionInfo,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterInfo {
    pub id: i64,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatInfo {
    pub format_name: String,
    pub format_long_name: Option<String>,
    pub duration_secs: Option<f64>,
    pub size_bytes: Option<u64>,
    pub bit_rate: Option<u64>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub format: FormatInfo,
    pub streams: Vec<StreamInfo>,
    pub chapters: Vec<ChapterInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackAction {
    EncodeVideo,
    Copy,
    TranscodeAudioAac,
    ConvertSubMovText,
    Omit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackDecision {
    pub stream_index: usize,
    pub codec_type: String,
    pub codec_name: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub action: TrackAction,
    pub reason: Option<String>,
    pub is_attached_pic: bool,
    pub causes_lossy_conversion: bool,
    pub causes_omission: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatIssueKind {
    UnsupportedVideoFeature,
    MultipleMovingVideoStreams,
    Mp4IncompatibleSubtitle,
    Mp4IncompatibleAudio,
    Mp4IncompatibleAttachment,
    NoVideoStream,
    UnreadableInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatIssue {
    pub kind: CompatIssueKind,
    pub stream_index: Option<usize>,
    pub summary: String,
    pub details: String,
    pub can_use_mkv: bool,
    pub can_remux: bool,
    pub can_opt_in_fallback: bool,
    pub fallback_description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompatOverrides {
    pub allow_mp4_audio_aac_fallback: bool,
    pub allow_mp4_sub_mov_text_fallback: bool,
    pub omit_incompatible_tracks: bool,
    pub selected_video_stream: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackPlan {
    pub tracks: Vec<TrackDecision>,
    pub issues: Vec<CompatIssue>,
    pub is_ready: bool,
    pub has_omitted_tracks: bool,
    pub has_approved_conversions: bool,
    pub argv: Vec<String>,
    pub printable_command: String,
    pub target_pix_fmt: Option<String>,
    pub preserve_rotation: bool,
    pub muxer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub queue_id: String,
    pub batch_id: String,
    pub position: i64,
    pub source_path: String,
    pub source_rel_path: String,
    pub source_root: Option<String>,
    pub source_fingerprint: Option<SourceFingerprint>,
    pub destination_path: String,
    pub dest_volume_id: Option<String>,
    pub recipe: EncodeRecipe,
    pub compat_overrides: CompatOverrides,
    pub status: JobStatus,
    pub status_reason: Option<String>,
    pub probe: Option<ProbeResult>,
    pub plan: Option<TrackPlan>,
    pub input_bytes: u64,
    pub output_bytes: Option<u64>,
    pub output_fingerprint: Option<SourceFingerprint>,
    pub error_stage: Option<String>,
    pub error_message: Option<String>,
    pub diagnostics: Option<String>,
    pub cleanup_policy: CleanupPolicy,
    pub cleanup_outcome: CleanupOutcome,
    pub cleanup_error: Option<String>,
    pub consent_id: Option<String>,
    pub full_decode_verified: bool,
    pub min_savings_enabled: bool,
    pub min_savings_pct: f64,
    pub tool_versions: Option<String>,
    pub current_attempt_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgressEvent {
    pub job_id: String,
    pub queue_id: String,
    pub status: JobStatus,
    pub out_time_secs: f64,
    pub duration_secs: Option<f64>,
    pub speed: Option<f64>,
    pub fps: Option<f64>,
    pub bytes_written: u64,
    pub eta_secs: Option<f64>,
    pub est_final_ratio: Option<f64>,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedSample {
    pub elapsed_secs: f64,
    pub out_time_secs: f64,
    pub speed: f64,
    pub fps: Option<f64>,
    pub bytes_written: u64,
    pub est_final_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeTestResult {
    pub name: String,
    pub passed: bool,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilities {
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub has_libx265: bool,
    pub has_libsvtav1: bool,
    pub has_aac: bool,
    pub has_matroska: bool,
    pub has_mp4: bool,
    pub has_mov_text: bool,
    pub available_codecs: Vec<VideoCodec>,
    pub smoke_tests: Vec<SmokeTestResult>,
    pub checked_at: i64,
    pub identity_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatus {
    pub discovered: bool,
    pub capabilities: Option<ToolCapabilities>,
    pub error: Option<String>,
    pub repair_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeSpaceInfo {
    pub volume_id: String,
    pub mount_point: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub reserve_bytes: u64,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorningSummary {
    pub queue_id: String,
    pub completed_count: usize,
    pub failed_count: usize,
    pub needs_attention_count: usize,
    pub below_savings_count: usize,
    pub canceled_count: usize,
    pub interrupted_count: usize,
    pub total_input_bytes: u64,
    pub total_output_bytes: u64,
    pub trashed_count: usize,
    pub kept_count: usize,
    pub cleanup_failed_count: usize,
    pub cleanup_uncertain_count: usize,
    pub recoverable_bytes_by_volume: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupCandidate {
    pub job_id: String,
    pub queue_id: String,
    pub source_path: String,
    pub output_path: String,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub source_volume_id: String,
    pub codec_label: String,
    pub full_decode_verified: bool,
    pub eligible: bool,
    pub ineligibility_reason: Option<String>,
    pub has_approved_conversions: bool,
    pub cleanup_outcome: CleanupOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplePreviewState {
    pub job_id: String,
    pub start_secs: f64,
    pub duration_secs: f64,
    pub status: String,
    pub progress_pct: f64,
    pub measured_bytes: Option<u64>,
    pub elapsed_secs: Option<f64>,
    pub projected_bytes: Option<u64>,
    pub projected_secs: Option<f64>,
    pub stale: bool,
    pub original_sample_path: Option<String>,
    pub encoded_sample_path: Option<String>,
    pub recipe_snapshot: EncodeRecipe,
    pub error: Option<String>,
    pub waiting_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveComparisonBundle {
    pub info: Option<LivePreviewInfo>,
    pub matched_ts_secs: Option<f64>,
    pub enc_data_url: Option<String>,
    pub orig_data_url: Option<String>,
    pub delta_data_url: Option<String>,
    pub mae_pct: Option<f64>,
    pub psnr_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameComparisonPair {
    pub orig_data_url: String,
    pub enc_data_url: String,
    pub delta_data_url: Option<String>,
    pub mae_pct: Option<f64>,
    pub psnr_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePreviewInfo {
    pub job_id: String,
    pub active: bool,
    pub mode: String,
    pub media_time_secs: f64,
    pub waiting_first_frame: bool,
    pub frame_seq: u64,
    pub worker_pid: Option<u32>,
    pub limitation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairJobRecord {
    pub queue_id: String,
    pub input_path: String,
    pub reference_paths: Vec<String>,
    pub output_path: Option<String>,
    pub video_only: bool,
    pub deep_scan: bool,
    pub timeout_secs: u64,
    pub status: String,
    pub diagnosis: Option<crate::recovery::DiagnosisReport>,
    pub report: Option<crate::recovery::RecoveryReport>,
    pub progress: Option<crate::recovery::RecoveryProgress>,
    pub error: Option<String>,
    pub preview_sheet_filename: Option<String>,
    pub updated_at: i64,
}

fn default_repair_deep_scan() -> bool {
    true
}

fn default_repair_timeout_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabWorkspaceMeta {
    pub resource_source: Option<String>,
    pub forwarded_from: Option<String>,
    #[serde(default)]
    pub repair_refs: Vec<String>,
    #[serde(default)]
    pub repair_video_only: bool,
    #[serde(default = "default_repair_deep_scan")]
    pub repair_deep_scan: bool,
    #[serde(default = "default_repair_timeout_secs")]
    pub repair_timeout_secs: u64,
}

impl Default for TabWorkspaceMeta {
    fn default() -> Self {
        Self {
            resource_source: None,
            forwarded_from: None,
            repair_refs: Vec::new(),
            repair_video_only: false,
            repair_deep_scan: true,
            repair_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabDedupeItem {
    pub job_id: String,
    pub path: String,
    pub name: String,
    pub rel_path: String,
    pub input_bytes: u64,
    pub resolution: String,
    pub bitrate_mbps: f64,
    pub codec: String,
    pub duration_secs: f64,
    pub role: String, // "keep" | "trash"
    pub trashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabDedupeGroup {
    pub id: String,
    pub match_type: String,
    pub similarity: f64,
    pub items: Vec<TabDedupeItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRepairItemState {
    pub job_id: String,
    pub queue_id: String,
    pub status: String,
    pub diagnosis: Option<crate::recovery::DiagnosisReport>,
    pub report: Option<crate::recovery::RecoveryReport>,
    pub error: Option<String>,
    pub output_path: Option<String>,
    pub output_bytes: Option<u64>,
    pub preview_sheet_filename: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HashCacheStats {
    pub entry_count: usize,
    pub total_hashed_bytes: u64,
    pub cache_bytes_estimate: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabFaceGroupView {
    pub group_id: String,
    pub label: Option<String>,
    pub sample_crop_path: Option<String>,
    pub observation_count: usize,
    pub matched_job_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabOrganizeOverview {
    pub job_tags: HashMap<String, Vec<String>>,
    pub available_tags: Vec<String>,
    pub ocr_detections: Vec<crate::intelligence::OcrDetectionRecord>,
    pub face_groups: Vec<TabFaceGroupView>,
    pub hash_cache: HashCacheStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedSourceRecord {
    pub volume_id: String,
    pub file_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    UnattendedPostEncode,
    ManualPostEncode,
    QueueSpecificItems,
    QueueDuplicateGroup,
    LibraryDuplicateCopy,
    UndoCopy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashPreviewItem {
    pub item_id: String,
    pub queue_id: Option<String>,
    pub filename: String,
    pub parent_dir: String,
    pub path: String,
    pub size_bytes: u64,
    pub volume_id: String,
    pub file_id: String,
    pub head_hash: String,
    pub group_id: Option<String>,
    pub eligible: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashPreviewPlan {
    pub plan_id: String,
    pub queue_id: Option<String>,
    pub queue_name: Option<String>,
    pub removal_reason: RemovalReason,
    pub items: Vec<TrashPreviewItem>,
    pub eligible_count: usize,
    pub total_bytes: u64,
    pub blocked_count: usize,
    pub trash_supported: bool,
    pub trash_support_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashItemOutcome {
    pub item_id: String,
    pub filename: String,
    pub path: String,
    pub moved: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashBatchResult {
    pub moved_count: usize,
    pub failed_count: usize,
    pub outcomes: Vec<TrashItemOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveByTagItem {
    pub job_id: String,
    pub filename: String,
    pub old_path: String,
    pub new_path: String,
    pub size_bytes: u64,
    pub valid: bool,
    #[serde(default)]
    pub collision: bool,
    #[serde(default)]
    pub cross_volume: bool,
    #[serde(default)]
    pub copied_original_retained: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveByTagResult {
    #[serde(default)]
    pub plan_id: Option<String>,
    pub tag: String,
    pub dest_dir: String,
    pub copy_only: bool,
    #[serde(default)]
    pub preserve_subfolders: bool,
    #[serde(default)]
    pub collision_policy: String,
    pub dry_run: bool,
    pub items: Vec<MoveByTagItem>,
    pub moved_count: usize,
    #[serde(default)]
    pub retained_count: usize,
    #[serde(default)]
    pub operation_id: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub queues: Vec<QueueTab>,
    pub active_queue_id: String,
    pub jobs: Vec<JobRecord>,
    #[serde(default)]
    pub repair_sessions: Vec<RepairJobRecord>,
    #[serde(default)]
    pub tab_meta: HashMap<String, TabWorkspaceMeta>,
    #[serde(default)]
    pub job_decisions: HashMap<String, String>,
    #[serde(default)]
    pub protected_sources: Vec<String>,
    #[serde(default)]
    pub tab_dedupe_groups: HashMap<String, Vec<TabDedupeGroup>>,
    #[serde(default)]
    pub tab_repair_items: HashMap<String, TabRepairItemState>,
    #[serde(default)]
    pub hash_cache_stats: HashCacheStats,
    pub profiles: Vec<Profile>,
    pub named_presets: Vec<NamedPreset>,
    pub tools: ToolStatus,
    pub volumes: Vec<VolumeSpaceInfo>,
    pub morning_summary: MorningSummary,
    pub active_progress: Option<JobProgressEvent>,
    #[serde(default)]
    pub active_speed_history: Vec<SpeedSample>,
    pub sample_preview: Option<SamplePreviewState>,
    pub live_preview: Option<LivePreviewInfo>,
    pub power_assertion_active: bool,
    pub scheduler_busy_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedImportItem {
    pub source_path: String,
    pub source_rel_path: String,
    pub source_root: Option<String>,
    pub destination_path: String,
    pub input_bytes: u64,
    pub bit_rate: Option<u64>,
    pub current_codec: Option<String>,
    pub duration_secs: Option<f64>,
    pub duplicate_in_queue: bool,
    pub destination_collision: bool,
    pub include: bool,
    pub probe: Option<ProbeResult>,
    pub plan: Option<TrackPlan>,
    pub unreadable_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportScanResult {
    pub scan_id: String,
    pub queue_id: String,
    pub queue_name: String,
    pub profile_name: Option<String>,
    pub destination_root: String,
    pub recipe: EncodeRecipe,
    pub items: Vec<StagedImportItem>,
    pub skipped_count: usize,
    pub unreadable_count: usize,
    pub canceled: bool,
}
