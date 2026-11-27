use crate::models::*;
use crate::runner::PreviewPacketBuffer;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Manages both the 15-second pre-encode sample comparison and the on-demand live mid-encode preview worker.
#[derive(Clone)]
pub struct PreviewManager {
    pub preview_dir: PathBuf,
    pub sample_state: Arc<Mutex<Option<SamplePreviewState>>>,
    pub live_info: Arc<Mutex<Option<LivePreviewInfo>>>,
    pub latest_live_jpeg: Arc<Mutex<Option<(u64, f64, Vec<u8>)>>>,
    pub live_stop_flag: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    pub live_media_time: Arc<Mutex<f64>>,
    pub active_packet_buffer: Arc<Mutex<Option<(String, PreviewPacketBuffer)>>>,
    pub active_partial_path: Arc<Mutex<Option<(String, PathBuf, ContainerFormat)>>>,
}

impl PreviewManager {
    pub fn new(app_data_dir: &Path) -> Self {
        let preview_dir = app_data_dir.join("previews");
        let _ = std::fs::create_dir_all(&preview_dir);
        Self {
            preview_dir,
            sample_state: Arc::new(Mutex::new(None)),
            live_info: Arc::new(Mutex::new(None)),
            latest_live_jpeg: Arc::new(Mutex::new(None)),
            live_stop_flag: Arc::new(Mutex::new(None)),
            live_media_time: Arc::new(Mutex::new(0.0)),
            active_packet_buffer: Arc::new(Mutex::new(None)),
            active_partial_path: Arc::new(Mutex::new(None)),
        }
    }

    pub fn mark_sample_stale_if_matching(&self, job_id: &str) {
        if let Ok(mut st) = self.sample_state.lock() {
            if let Some(ref mut s) = *st {
                if s.job_id == job_id {
                    s.stale = true;
                }
            }
        }
    }

    pub fn set_active_encode_source(
        &self,
        job_id: &str,
        partial_path: &Path,
        container: ContainerFormat,
        packet_buffer: Option<PreviewPacketBuffer>,
    ) {
        if let Ok(mut mt) = self.live_media_time.lock() {
            *mt = 0.0;
        }
        if let Ok(mut lj) = self.latest_live_jpeg.lock() {
            *lj = None;
        }
        if let Ok(mut ap) = self.active_partial_path.lock() {
            *ap = Some((job_id.to_string(), partial_path.to_path_buf(), container));
        }
        if let Ok(mut pb) = self.active_packet_buffer.lock() {
            *pb = packet_buffer.map(|b| (job_id.to_string(), b));
        }
    }

    pub fn clear_active_encode_source(&self) {
        self.stop_live_preview();
        if let Ok(mut mt) = self.live_media_time.lock() {
            *mt = 0.0;
        }
        if let Ok(mut ap) = self.active_partial_path.lock() {
            *ap = None;
        }
        if let Ok(mut pb) = self.active_packet_buffer.lock() {
            *pb = None;
        }
    }

    /// Encode a 15-second sample (clamped to available duration) using exact selected recipe settings.
    /// Runs inside the scheduler's single-encoder lock when called from the scheduler!
    pub fn run_sample_encode(
        &self,
        ffmpeg_path: &Path,
        job: &JobRecord,
        requested_start_secs: Option<f64>,
    ) -> SamplePreviewState {
        let total_dur = job
            .probe
            .as_ref()
            .and_then(|p| p.format.duration_secs)
            .filter(|d| *d > 0.0);

        let (start_secs, sample_dur) = match total_dur {
            Some(d) => {
                let dur = d.min(15.0);
                let default_start = if d > 15.0 {
                    (d * 0.25).min((d - 15.0).max(0.0))
                } else {
                    0.0
                };
                let st = requested_start_secs
                    .unwrap_or(default_start)
                    .clamp(0.0, (d - dur).max(0.0));
                (st, dur)
            }
            None => (requested_start_secs.unwrap_or(0.0).max(0.0), 15.0),
        };

        let sample_folder = self.preview_dir.join(&job.id);
        let _ = std::fs::create_dir_all(&sample_folder);
        let ext = job.recipe.container.ext();
        let orig_clip = sample_folder.join("original_sample.mp4");
        let enc_clip = sample_folder.join(format!("encoded_sample.{ext}"));
        let _ = std::fs::remove_file(&orig_clip);
        let _ = std::fs::remove_file(&enc_clip);

        let video_map_spec = if let Some(idx) = job.compat_overrides.selected_video_stream {
            format!("0:{idx}")
        } else if let Some(vs) = job
            .probe
            .as_ref()
            .and_then(|p| p.streams.iter().find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0))
        {
            format!("0:{}", vs.index)
        } else {
            "0:v:0".to_string()
        };

        // Extract browser-playable original reference segment (or timestamp-matched frames fallback)
        let _ = Command::new(ffmpeg_path)
            .args([
                "-hide_banner",
                "-y",
                "-protocol_whitelist",
                "file,pipe",
                "-ss",
                &format!("{start_secs:.3}"),
                "-i",
                &job.source_path,
                "-t",
                &format!("{sample_dur:.3}"),
                "-map",
                &video_map_spec,
                "-map",
                "0:a?",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "16",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ])
            .arg(&orig_clip)
            .output();

        // Build actual sample encode command mirroring the real recipe
        let mut args = vec![
            "-hide_banner".to_string(),
            "-y".to_string(),
            "-protocol_whitelist".to_string(),
            "file,pipe".to_string(),
            "-ss".to_string(),
            format!("{start_secs:.3}"),
        ];
        let preserve_rot = job.plan.as_ref().map(|p| p.preserve_rotation).unwrap_or(false);
        if preserve_rot && job.recipe.family == PresetFamily::Archive {
            args.push("-noautorotate".to_string());
        }
        args.extend([
            "-i".to_string(),
            job.source_path.clone(),
            "-t".to_string(),
            format!("{sample_dur:.3}"),
            "-map".to_string(),
            video_map_spec,
            "-map".to_string(),
            "0:a?".to_string(),
            "-c".to_string(),
            "copy".to_string(),
        ]);

        if job.recipe.family == PresetFamily::Archive {
            args.push("-c:v:0".to_string());
            args.push(job.recipe.codec.encoder_name().to_string());
            args.push("-preset:v:0".to_string());
            args.push(job.recipe.effort.clone());
            args.push("-crf:v:0".to_string());
            args.push(job.recipe.crf.to_string());
            if job.recipe.codec == VideoCodec::Hevc && job.recipe.container == ContainerFormat::Mp4 {
                args.push("-tag:v:0".to_string());
                args.push("hvc1".to_string());
            }
            if let Some(ref pf) = job.plan.as_ref().and_then(|p| p.target_pix_fmt.clone()) {
                args.push("-pix_fmt:v:0".to_string());
                args.push(pf.clone());
            }
            args.push("-fps_mode:v:0".to_string());
            args.push("passthrough".to_string());
        }

        if job.recipe.container == ContainerFormat::Mp4 {
            // Ensure compatible audio if original audio isn't MP4 friendly for sample preview
            if let Some(ref plan) = job.plan {
                for t in &plan.tracks {
                    if t.codec_type == "audio" && matches!(t.action, TrackAction::TranscodeAudioAac) {
                        args.extend(["-c:a".to_string(), "aac".to_string()]);
                        break;
                    }
                }
            }
            args.extend(["-movflags".to_string(), "+faststart".to_string()]);
        }

        args.extend([
            "-f".to_string(),
            job.recipe.container.muxer().to_string(),
            enc_clip.to_string_lossy().to_string(),
        ]);

        let t0 = Instant::now();
        let mut cmd = Command::new(ffmpeg_path);
        cmd.args(&args);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let res = cmd.output();
        let elapsed_secs = t0.elapsed().as_secs_f64();

        let state = match res {
            Ok(out) if out.status.success() => {
                let measured_bytes = std::fs::metadata(&enc_clip).map(|m| m.len()).ok();
                let (projected_bytes, projected_secs) = match (total_dur, measured_bytes) {
                    (Some(td), Some(mb)) if td >= 20.0 && sample_dur >= 5.0 => {
                        let scale = td / sample_dur;
                        (
                            Some(((mb as f64) * scale) as u64),
                            Some(elapsed_secs * scale),
                        )
                    }
                    _ => (None, None),
                };
                SamplePreviewState {
                    job_id: job.id.clone(),
                    start_secs,
                    duration_secs: sample_dur,
                    status: "ready".to_string(),
                    progress_pct: 100.0,
                    measured_bytes,
                    elapsed_secs: Some(elapsed_secs),
                    projected_bytes,
                    projected_secs,
                    stale: false,
                    original_sample_path: if orig_clip.exists() {
                        Some(orig_clip.to_string_lossy().to_string())
                    } else {
                        None
                    },
                    encoded_sample_path: if enc_clip.exists() {
                        Some(enc_clip.to_string_lossy().to_string())
                    } else {
                        None
                    },
                    recipe_snapshot: job.recipe.clone(),
                    error: None,
                    waiting_reason: None,
                }
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ");
                SamplePreviewState {
                    job_id: job.id.clone(),
                    start_secs,
                    duration_secs: sample_dur,
                    status: "failed".to_string(),
                    progress_pct: 0.0,
                    measured_bytes: None,
                    elapsed_secs: Some(elapsed_secs),
                    projected_bytes: None,
                    projected_secs: None,
                    stale: false,
                    original_sample_path: None,
                    encoded_sample_path: None,
                    recipe_snapshot: job.recipe.clone(),
                    error: Some(err),
                    waiting_reason: None,
                }
            }
            Err(e) => SamplePreviewState {
                job_id: job.id.clone(),
                start_secs,
                duration_secs: sample_dur,
                status: "failed".to_string(),
                progress_pct: 0.0,
                measured_bytes: None,
                elapsed_secs: Some(elapsed_secs),
                projected_bytes: None,
                projected_secs: None,
                stale: false,
                original_sample_path: None,
                encoded_sample_path: None,
                recipe_snapshot: job.recipe.clone(),
                error: Some(e.to_string()),
                waiting_reason: None,
            },
        };

        if let Ok(mut st) = self.sample_state.lock() {
            *st = Some(state.clone());
        }
        state
    }

    /// Timestamp-matched JPEG frame pair for embedded comparison fallback & 100% inspection.
    pub fn decode_comparison_frame(
        &self,
        ffmpeg_path: &Path,
        media_path: &Path,
        timestamp_secs: f64,
        full_resolution: bool,
    ) -> Result<Vec<u8>, String> {
        let mut args = vec![
            "-hide_banner".to_string(),
            "-v".to_string(),
            "error".to_string(),
            "-protocol_whitelist".to_string(),
            "file,pipe".to_string(),
            "-ss".to_string(),
            format!("{timestamp_secs:.3}"),
            "-i".to_string(),
            media_path.to_string_lossy().to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
        ];
        if !full_resolution {
            args.extend(["-vf".to_string(), "scale='min(960,iw)':-2".to_string()]);
        }
        args.extend([
            "-f".to_string(),
            "image2pipe".to_string(),
            "-vcodec".to_string(),
            "mjpeg".to_string(),
            "-q:v".to_string(),
            "3".to_string(),
            "pipe:1".to_string(),
        ]);

        let mut cmd = Command::new(ffmpeg_path);
        cmd.args(&args);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(format!(
                "Failed to decode frame: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(out.stdout)
    }

    pub fn start_live_preview(&self, ffmpeg_path: PathBuf, job_id: String, current_media_time: f64) {
        self.stop_live_preview();

        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut sf) = self.live_stop_flag.lock() {
            *sf = Some(stop.clone());
        }

        let partial_opt = self
            .active_partial_path
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|(jid, _, _)| jid == &job_id);

        let pkt_buf_opt = self
            .active_packet_buffer
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|(jid, _)| jid == &job_id)
            .map(|(_, b)| b);

        let mode = match (&partial_opt, &pkt_buf_opt) {
            (_, Some(_)) => "packet_branch".to_string(),
            (Some((_, _, ContainerFormat::Mkv)), _) => "direct_mkv".to_string(),
            _ => "direct_read".to_string(),
        };

        if let Ok(mut li) = self.live_info.lock() {
            *li = Some(LivePreviewInfo {
                job_id: job_id.clone(),
                active: true,
                mode: mode.clone(),
                media_time_secs: current_media_time,
                waiting_first_frame: true,
                frame_seq: 0,
                worker_pid: None,
                limitation: None,
            });
        }

        let live_info_arc = self.live_info.clone();
        let latest_jpeg_arc = self.latest_live_jpeg.clone();
        let active_partial_arc = self.active_partial_path.clone();
        let active_pkt_arc = self.active_packet_buffer.clone();
        let live_mt_arc = self.live_media_time.clone();

        std::thread::spawn(move || {
            let mut seq = 0u64;
            let mut last_ts = current_media_time;

            while !stop.load(Ordering::Relaxed) {
                let tick_start = Instant::now();
                let current_enc_ts = live_mt_arc.lock().map(|g| *g).unwrap_or(last_ts).max(last_ts);

                // Branch 1: MP4 bounded packet-copy buffer
                let pkt_buf = active_pkt_arc
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .filter(|(jid, _)| jid == &job_id)
                    .map(|(_, b)| b);

                if let Some(buf) = pkt_buf {
                    let stream_bytes = buf.snapshot_tail_stream_bytes();
                    if stream_bytes.len() > 1024 {
                        if let Ok((jpeg, _ts, wpid)) = decode_latest_frame_from_stream_bytes(&ffmpeg_path, &stream_bytes) {
                            seq += 1;
                            last_ts = current_enc_ts;
                            if let Ok(mut lj) = latest_jpeg_arc.lock() {
                                *lj = Some((seq, last_ts, jpeg));
                            }
                            if let Ok(mut li) = live_info_arc.lock() {
                                if let Some(ref mut info) = *li {
                                    info.waiting_first_frame = false;
                                    info.frame_seq = seq;
                                    info.media_time_secs = info.media_time_secs.max(last_ts);
                                    info.worker_pid = wpid;
                                }
                            }
                        }
                    }
                } else if let Some((_, partial, container)) = active_partial_arc
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .filter(|(jid, _, _)| jid == &job_id)
                {
                    if container == ContainerFormat::Mkv && partial.exists() {
                        // Direct read-only incremental decode near current active encode media time
                        let seek_ts = (current_enc_ts - 1.0).max(0.0);
                        if let Ok((jpeg, decoded_ts, wpid)) =
                            decode_latest_frame_direct_mkv(&ffmpeg_path, &partial, seek_ts)
                        {
                            seq += 1;
                            last_ts = current_enc_ts.max(decoded_ts);
                            if let Ok(mut lj) = latest_jpeg_arc.lock() {
                                *lj = Some((seq, last_ts, jpeg));
                            }
                            if let Ok(mut li) = live_info_arc.lock() {
                                if let Some(ref mut info) = *li {
                                    info.waiting_first_frame = false;
                                    info.frame_seq = seq;
                                    info.media_time_secs = info.media_time_secs.max(last_ts);
                                    info.worker_pid = wpid;
                                }
                            }
                        }
                    } else if container == ContainerFormat::Mp4 {
                        if let Ok(mut li) = live_info_arc.lock() {
                            if let Some(ref mut info) = *li {
                                info.limitation = Some(
                                    "Standard MP4 live preview requires packet-copy branch (available on macOS/Unix); finalized MP4 becomes readable at completion.".to_string(),
                                );
                            }
                        }
                    }
                } else {
                    break;
                }

                // Cap wall-clock refresh rate (~2 Hz)
                let elapsed = tick_start.elapsed();
                if elapsed < Duration::from_millis(500) {
                    std::thread::sleep(Duration::from_millis(500) - elapsed);
                }
            }
        });
    }

    pub fn stop_live_preview(&self) {
        if let Ok(mut sf) = self.live_stop_flag.lock() {
            if let Some(flag) = sf.take() {
                flag.store(true, Ordering::Relaxed);
            }
        }
        if let Ok(mut lj) = self.latest_live_jpeg.lock() {
            *lj = None;
        }
        if let Ok(mut li) = self.live_info.lock() {
            *li = None;
        }
    }

    pub fn update_live_media_time(&self, media_time: f64) {
        if let Ok(mut mt) = self.live_media_time.lock() {
            if media_time > *mt {
                *mt = media_time;
            }
        }
        if let Ok(mut li) = self.live_info.lock() {
            if let Some(ref mut info) = *li {
                if media_time > info.media_time_secs {
                    info.media_time_secs = media_time;
                }
            }
        }
    }
}

fn decode_latest_frame_direct_mkv(
    ffmpeg_path: &Path,
    partial_mkv: &Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, f64, Option<u32>), String> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args([
        "-hide_banner",
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-ss",
        &format!("{seek_secs:.2}"),
        "-i",
    ])
    .arg(partial_mkv)
    .args([
        "-vf",
        "scale=640:-2",
        "-frames:v",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "-q:v",
        "4",
        "pipe:1",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err("Waiting for first decodable frame".to_string());
    }
    Ok((out.stdout, seek_secs, Some(pid)))
}

fn decode_latest_frame_from_stream_bytes(
    ffmpeg_path: &Path,
    stream_bytes: &[u8],
) -> Result<(Vec<u8>, f64, Option<u32>), String> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args([
        "-hide_banner",
        "-v",
        "error",
        "-f",
        "matroska",
        "-i",
        "pipe:0",
        "-vf",
        "scale=640:-2",
        "-update",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "-q:v",
        "4",
        "pipe:1",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = stream_bytes.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let mut stdout_buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_end(&mut stdout_buf);
    }
    let _ = child.wait();

    // Extract the LAST JPEG image (FFD8 .. FFD9) emitted by `-update 1` / image2pipe
    if let Some(start) = stdout_buf.windows(2).rposition(|w| w == [0xff, 0xd8]) {
        let slice = &stdout_buf[start..];
        if slice.len() > 100 {
            return Ok((slice.to_vec(), 0.0, Some(pid)));
        }
    }
    Err("Waiting for decodable keyframe in preview branch".to_string())
}
