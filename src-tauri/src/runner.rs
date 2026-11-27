use crate::models::*;
use crate::planner::materialize_argv;
use crate::platform::{disk_space_for_path, spawn_supervised_ffmpeg};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum EncodeRunOutcome {
    Finished {
        staging_dir: PathBuf,
        partial_path: PathBuf,
        diagnostics: String,
    },
    Canceled {
        diagnostics: String,
    },
    DiskReserveCrossed {
        free_bytes: u64,
        reserve_bytes: u64,
        diagnostics: String,
    },
    Failed {
        exit_code: Option<i32>,
        error_message: String,
        diagnostics: String,
    },
}

/// Bounded packet-copy buffer for optional mid-encode MP4 live preview branch.
/// Always drained continuously by a parent thread so FFmpeg never blocks even if preview is closed or stalls.
#[derive(Clone)]
pub struct PreviewPacketBuffer {
    pub inner: Arc<Mutex<PreviewBufferState>>,
}

pub struct PreviewBufferState {
    pub header: Vec<u8>,
    pub header_complete: bool,
    pub chunks: VecDeque<Vec<u8>>,
    pub total_buffered_bytes: usize,
    pub dropped_chunks: u64,
}

impl PreviewPacketBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PreviewBufferState {
                header: Vec::new(),
                header_complete: false,
                chunks: VecDeque::new(),
                total_buffered_bytes: 0,
                dropped_chunks: 0,
            })),
        }
    }

    pub fn push_bytes(&self, data: &[u8]) {
        const MAX_HEADER: usize = 32 * 1024;
        const MAX_RING_BYTES: usize = 6 * 1024 * 1024;
        let Ok(mut st) = self.inner.lock() else {
            return;
        };
        if !st.header_complete {
            // Detect Matroska Cluster ID (0x1F 0x43 0xB6 0x75) so we capture clean EBML+Tracks header
            if let Some(pos) = data.windows(4).position(|w| w == [0x1f, 0x43, 0xb6, 0x75]) {
                st.header.extend_from_slice(&data[..pos]);
                st.header_complete = true;
                let rest = data[pos..].to_vec();
                if !rest.is_empty() {
                    st.total_buffered_bytes += rest.len();
                    st.chunks.push_back(rest);
                }
            } else if st.header.len() + data.len() <= MAX_HEADER {
                st.header.extend_from_slice(data);
                return;
            } else {
                st.header_complete = true;
            }
        } else {
            st.total_buffered_bytes += data.len();
            st.chunks.push_back(data.to_vec());
        }

        while st.total_buffered_bytes > MAX_RING_BYTES {
            if let Some(old) = st.chunks.pop_front() {
                st.total_buffered_bytes = st.total_buffered_bytes.saturating_sub(old.len());
                st.dropped_chunks += 1;
            } else {
                break;
            }
        }
    }

    pub fn snapshot_stream_bytes(&self) -> Vec<u8> {
        let Ok(st) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(st.header.len() + st.total_buffered_bytes);
        out.extend_from_slice(&st.header);
        for c in &st.chunks {
            out.extend_from_slice(c);
        }
        out
    }

    /// Return EBML+Tracks header plus only the most recent clusters (capped at ~1.5 MiB)
    /// starting cleanly at a Matroska Cluster boundary (`0x1F 0x43 0xB6 0x75`) for fast live decode.
    pub fn snapshot_tail_stream_bytes(&self) -> Vec<u8> {
        let Ok(st) = self.inner.lock() else {
            return Vec::new();
        };
        const MAX_TAIL: usize = 1536 * 1024;
        if st.total_buffered_bytes <= MAX_TAIL {
            let mut out = Vec::with_capacity(st.header.len() + st.total_buffered_bytes);
            out.extend_from_slice(&st.header);
            for c in &st.chunks {
                out.extend_from_slice(c);
            }
            return out;
        }
        let mut tail = Vec::new();
        for c in st.chunks.iter().rev() {
            tail.splice(0..0, c.iter().copied());
            if tail.len() >= MAX_TAIL {
                break;
            }
        }
        if let Some(pos) = tail.windows(4).position(|w| w == [0x1f, 0x43, 0xb6, 0x75]) {
            let mut out = Vec::with_capacity(st.header.len() + (tail.len() - pos));
            out.extend_from_slice(&st.header);
            out.extend_from_slice(&tail[pos..]);
            out
        } else {
            let mut out = Vec::with_capacity(st.header.len() + tail.len());
            out.extend_from_slice(&st.header);
            out.extend_from_slice(&tail);
            out
        }
    }
}

pub struct RunEncodeParams<'a> {
    pub ffmpeg_path: &'a Path,
    pub job: &'a JobRecord,
    pub attempt_id: &'a str,
    pub staging_dir: &'a Path,
    pub partial_path: &'a Path,
    pub priority: ProcessPriority,
    pub reserve_bytes: u64,
    pub cancel_flag: &'a Arc<AtomicBool>,
    pub preview_pipe_dir: Option<&'a Path>,
    pub preview_buffer: Option<PreviewPacketBuffer>,
    pub on_progress: Box<dyn Fn(JobProgressEvent) + Send + Sync>,
}

pub fn run_ffmpeg_attempt(params: RunEncodeParams<'_>) -> EncodeRunOutcome {
    if let Err(e) = std::fs::create_dir_all(params.staging_dir) {
        return EncodeRunOutcome::Failed {
            exit_code: None,
            error_message: format!("Cannot create staging directory: {e}"),
            diagnostics: String::new(),
        };
    }

    let Some(plan) = params.job.plan.as_ref() else {
        return EncodeRunOutcome::Failed {
            exit_code: None,
            error_message: "Missing track plan for job".to_string(),
            diagnostics: String::new(),
        };
    };

    let mut args = materialize_argv(&plan.argv, params.partial_path);

    // Optional bounded preview branch for ordinary MP4 when a preview side-pipe/FIFO is supplied.
    // Uses FFmpeg's `tee` muxer (`onfail=ignore`) + a non-blocking parent drain thread so the archive writer
    // never blocks or fails even if the preview worker is closed, stalled, or killed.
    #[allow(unused_mut)]
    let mut fifo_cleanup_path: Option<PathBuf> = None;
    let fifo_stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    let fifo_drain_handle: Option<std::thread::JoinHandle<()>> = {
        if params.job.recipe.container == ContainerFormat::Mp4
            && params.job.recipe.family == PresetFamily::Archive
            && args.len() >= 5
            && args[args.len() - 5..]
                == [
                    "-movflags",
                    "+faststart",
                    "-f",
                    "mp4",
                    &params.partial_path.to_string_lossy(),
                ]
        {
            if let (Some(pdir), Some(pbuf)) = (params.preview_pipe_dir, params.preview_buffer.clone()) {
                let _ = std::fs::create_dir_all(pdir);
                let fifo_path = pdir.join(format!("preview-{}.mkv.fifo", params.attempt_id));
                let _ = std::fs::remove_file(&fifo_path);
                if let Ok(c_fifo) = std::ffi::CString::new(fifo_path.as_os_str().as_encoded_bytes()) {
                    if unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) } == 0 {
                        fifo_cleanup_path = Some(fifo_path.clone());
                        // Replace trailing `-movflags +faststart -f mp4 <partial>` with `-flags +global_header -f tee ...`
                        let new_len = args.len() - 5;
                        args.truncate(new_len);
                        let escape_tee = |s: &str| s.replace('\\', "\\\\").replace(':', "\\:");
                        let tee_target = format!(
                            "[f=mp4:movflags=+faststart]{}|[select=v:f=matroska:onfail=ignore]{}",
                            escape_tee(&params.partial_path.to_string_lossy()),
                            escape_tee(&fifo_path.to_string_lossy())
                        );
                        args.extend([
                            "-flags".to_string(),
                            "+global_header".to_string(),
                            "-f".to_string(),
                            "tee".to_string(),
                            tee_target,
                        ]);

                        let fp_clone = fifo_path.clone();
                        let stop_clone = fifo_stop.clone();
                        Some(std::thread::spawn(move || {
                            use std::os::unix::io::FromRawFd;
                            let Ok(c_path) = std::ffi::CString::new(fp_clone.as_os_str().as_encoded_bytes()) else {
                                return;
                            };
                            let fd = unsafe {
                                libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC)
                            };
                            if fd < 0 {
                                return;
                            }
                            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
                            let mut buf = [0u8; 32768];
                            let mut saw_writer = false;
                            let mut stop_deadline: Option<Instant> = None;
                            loop {
                                if stop_clone.load(Ordering::Relaxed) && stop_deadline.is_none() {
                                    stop_deadline = Some(Instant::now() + Duration::from_millis(200));
                                }
                                if let Some(dl) = stop_deadline {
                                    if Instant::now() >= dl {
                                        break;
                                    }
                                }
                                match file.read(&mut buf) {
                                    Ok(0) => {
                                        if saw_writer || stop_clone.load(Ordering::Relaxed) {
                                            break;
                                        }
                                        std::thread::sleep(Duration::from_millis(15));
                                    }
                                    Ok(n) => {
                                        saw_writer = true;
                                        pbuf.push_bytes(&buf[..n]);
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        std::thread::sleep(Duration::from_millis(15));
                                    }
                                    Err(_) => break,
                                }
                            }
                        }))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    let mut supervised = match spawn_supervised_ffmpeg(params.ffmpeg_path, &args, params.priority) {
        Ok(s) => s,
        Err(e) => {
            fifo_stop.store(true, Ordering::Relaxed);
            #[cfg(unix)]
            if let Some(fh) = fifo_drain_handle {
                let _ = fh.join();
            }
            if let Some(fp) = fifo_cleanup_path {
                let _ = std::fs::remove_file(fp);
            }
            return EncodeRunOutcome::Failed {
                exit_code: None,
                error_message: e,
                diagnostics: String::new(),
            };
        }
    };

    let mut stdin_opt = supervised.stdin.take();
    let stdout = supervised.stdout.take();
    let stderr = supervised.stderr.take();

    // Drain stderr concurrently into a bounded ring buffer (last 200 lines)
    let stderr_lines = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(200)));
    let stderr_clone = stderr_lines.clone();
    let stderr_handle = std::thread::spawn(move || {
        if let Some(se) = stderr {
            let reader = BufReader::new(se);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(mut q) = stderr_clone.lock() {
                    if q.len() >= 200 {
                        q.pop_front();
                    }
                    q.push_back(line);
                }
            }
        }
    });

    // Drain stdout (-progress pipe:1) concurrently
    let duration_secs = params
        .job
        .probe
        .as_ref()
        .and_then(|p| p.format.duration_secs)
        .filter(|d| *d > 0.0);
    let input_bytes = params.job.input_bytes;
    let job_id = params.job.id.clone();
    let queue_id = params.job.queue_id.clone();
    let partial_clone = params.partial_path.to_path_buf();
    let progress_cb = Arc::new(params.on_progress);
    let progress_cb_thread = progress_cb.clone();
    let started = Instant::now();

    let stdout_handle = std::thread::spawn(move || {
        let mut out_time_secs = 0.0f64;
        let mut speed: Option<f64> = None;
        let mut fps: Option<f64> = None;
        let mut total_size: Option<u64> = None;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut saw_progress_end = false;
        let mut prev_out_time = 0.0f64;
        let mut prev_bytes = 0u64;
        let mut last_est_ratio: Option<f64> = None;

        if let Some(so) = stdout {
            let reader = BufReader::new(so);
            for line in reader.lines().map_while(Result::ok) {
                let Some((k, v)) = line.split_once('=') else {
                    continue;
                };
                let key = k.trim();
                let val = v.trim();
                match key {
                    "out_time_us" | "out_time_ms" => {
                        if let Ok(us) = val.parse::<i64>() {
                            if us >= 0 {
                                out_time_secs = us as f64 / 1_000_000.0;
                            }
                        }
                    }
                    "speed" => {
                        let cleaned = val.trim_end_matches('x').trim();
                        if let Ok(sp) = cleaned.parse::<f64>() {
                            if sp > 0.0 {
                                speed = Some(sp);
                            }
                        }
                    }
                    "fps" => {
                        if let Ok(f) = val.parse::<f64>() {
                            if f > 0.0 {
                                fps = Some(f);
                            }
                        }
                    }
                    "total_size" => {
                        if let Ok(sz) = val.parse::<u64>() {
                            total_size = Some(sz);
                        }
                    }
                    "progress" => {
                        if val == "end" {
                            saw_progress_end = true;
                        }
                        if last_emit.elapsed() >= Duration::from_millis(450) || val == "end" {
                            let bytes_written = total_size
                                .filter(|b| *b > 0)
                                .unwrap_or_else(|| {
                                    std::fs::metadata(&partial_clone)
                                        .map(|m| m.len())
                                        .unwrap_or(0)
                                });
                            let eta_secs = match (duration_secs, speed) {
                                (Some(d), Some(sp)) if sp > 0.001 && d > out_time_secs => {
                                    Some((d - out_time_secs) / sp)
                                }
                                _ => None,
                            };
                            // Estimate final ratio only after >= 10% of known timeline when advancing; smooth over transient ticks
                            let est_final_ratio = match duration_secs {
                                Some(d) if d > 0.0 && out_time_secs / d >= 0.10 => {
                                    let advancing = out_time_secs > prev_out_time && bytes_written > prev_bytes;
                                    if advancing || (val == "end" && bytes_written > 0) {
                                        let frac = (out_time_secs / d).clamp(0.10, 1.0);
                                        let est_out = (bytes_written as f64) / frac;
                                        if est_out > 0.0 && input_bytes > 0 {
                                            last_est_ratio = Some((input_bytes as f64) / est_out);
                                        }
                                    }
                                    last_est_ratio
                                }
                                _ => None,
                            };
                            prev_out_time = out_time_secs;
                            prev_bytes = bytes_written;
                            last_emit = Instant::now();

                            (progress_cb_thread)(JobProgressEvent {
                                job_id: job_id.clone(),
                                queue_id: queue_id.clone(),
                                status: JobStatus::Encoding,
                                out_time_secs,
                                duration_secs,
                                speed,
                                fps,
                                bytes_written,
                                eta_secs,
                                est_final_ratio,
                                elapsed_secs: started.elapsed().as_secs_f64(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        saw_progress_end
    });

    // Poll child exit + cancellation + periodic disk free-space reserve check
    let mut last_space_check = Instant::now();
    let mut disk_crossed: Option<(u64, u64)> = None;
    let mut was_canceled = false;
    let mut sent_quit = false;
    let mut quit_sent_at: Option<Instant> = None;

    let exit_status = loop {
        match supervised.child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }

        if params.cancel_flag.load(Ordering::Relaxed) && !was_canceled {
            was_canceled = true;
            if let Some(ref mut stdin) = stdin_opt {
                let _ = stdin.write_all(b"q\n");
                let _ = stdin.flush();
            }
            sent_quit = true;
            quit_sent_at = Some(Instant::now());
        }

        if params.reserve_bytes > 0 && last_space_check.elapsed() >= Duration::from_secs(2) {
            last_space_check = Instant::now();
            if let Ok(vs) = disk_space_for_path(params.staging_dir) {
                if vs.free_bytes < params.reserve_bytes {
                    disk_crossed = Some((vs.free_bytes, params.reserve_bytes));
                    if !sent_quit {
                        if let Some(ref mut stdin) = stdin_opt {
                            let _ = stdin.write_all(b"q\n");
                            let _ = stdin.flush();
                        }
                        sent_quit = true;
                        quit_sent_at = Some(Instant::now());
                    }
                }
            }
        }

        if let Some(qt) = quit_sent_at {
            if qt.elapsed() >= Duration::from_secs(3) {
                supervised.kill_group();
                let st = supervised.child.wait().ok();
                break st;
            }
        }

        std::thread::sleep(Duration::from_millis(120));
    };

    drop(stdin_opt);
    let _saw_end = stdout_handle.join().unwrap_or(false);
    let _ = stderr_handle.join();
    fifo_stop.store(true, Ordering::Relaxed);
    #[cfg(unix)]
    if let Some(fh) = fifo_drain_handle {
        let _ = fh.join();
    }
    if let Some(fp) = fifo_cleanup_path {
        let _ = std::fs::remove_file(fp);
    }

    let diagnostics = stderr_lines
        .lock()
        .map(|q| q.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();

    if let Some((free_bytes, reserve_bytes)) = disk_crossed {
        // Remove positively identified app-owned partial file and staging dir
        let _ = std::fs::remove_file(params.partial_path);
        let _ = std::fs::remove_dir_all(params.staging_dir);
        return EncodeRunOutcome::DiskReserveCrossed {
            free_bytes,
            reserve_bytes,
            diagnostics,
        };
    }

    if was_canceled || params.cancel_flag.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(params.partial_path);
        let _ = std::fs::remove_dir_all(params.staging_dir);
        return EncodeRunOutcome::Canceled { diagnostics };
    }

    match exit_status {
        Some(st) if st.success() => EncodeRunOutcome::Finished {
            staging_dir: params.staging_dir.to_path_buf(),
            partial_path: params.partial_path.to_path_buf(),
            diagnostics,
        },
        Some(st) => {
            let tail = diagnostics
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            let _ = std::fs::remove_file(params.partial_path);
            let _ = std::fs::remove_dir_all(params.staging_dir);
            EncodeRunOutcome::Failed {
                exit_code: st.code(),
                error_message: if tail.is_empty() {
                    format!("FFmpeg exited with status {:?}", st.code())
                } else {
                    tail
                },
                diagnostics,
            }
        }
        None => {
            let _ = std::fs::remove_file(params.partial_path);
            let _ = std::fs::remove_dir_all(params.staging_dir);
            EncodeRunOutcome::Failed {
                exit_code: None,
                error_message: "FFmpeg process terminated unexpectedly".to_string(),
                diagnostics,
            }
        }
    }
}
