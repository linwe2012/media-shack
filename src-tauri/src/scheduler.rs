use crate::cleanup::{execute_cleanup_for_job, reconcile_cleanup_intents_on_launch};
use crate::db::Database;
use crate::models::*;
use crate::planner::{
    apply_output_regex_to_stem, build_track_plan, probe_file,
    resolve_destination_path_with_config,
};
use crate::platform::{
    capture_file_identity, capture_file_identity_with_blake3, destination_capabilities,
    disk_space_for_path, fault_tolerant_replace_file, publish_staged_file, PowerAssertion,
    PublishOutcome,
};
use crate::preview::PreviewManager;
use crate::runner::{run_ffmpeg_attempt, EncodeRunOutcome, PreviewPacketBuffer, RunEncodeParams};
use crate::tools::discover_and_probe;
use crate::verify::verify_staged_output;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PendingSampleRequest {
    pub job_id: String,
    pub start_secs: Option<f64>,
}

pub struct Scheduler {
    pub db: Database,
    pub previews: PreviewManager,
    pub tool_status: Arc<Mutex<ToolStatus>>,
    pub active_progress: Arc<Mutex<Option<JobProgressEvent>>>,
    pub active_speed_history: Arc<Mutex<Vec<SpeedSample>>>,
    pub active_cancel_flag: Arc<Mutex<Option<(String, Arc<AtomicBool>)>>>,
    pub blocked_volumes: Arc<Mutex<HashMap<String, String>>>,
    pub cleanup_lock: Arc<Mutex<()>>,
    pub pending_sample: Arc<Mutex<Option<PendingSampleRequest>>>,
    pub power_assertion: Arc<Mutex<Option<PowerAssertion>>>,
    pub busy_reason: Arc<Mutex<Option<String>>>,
    pub notify_pair: Arc<(Mutex<bool>, Condvar)>,
    pub event_sink: Arc<Mutex<Option<Box<dyn Fn(&str, serde_json::Value) + Send + Sync>>>>,
}

impl Scheduler {
    pub fn new(db: Database) -> Self {
        let previews = PreviewManager::new(&db.app_data_dir);
        let s = Self {
            db,
            previews,
            tool_status: Arc::new(Mutex::new(ToolStatus {
                discovered: false,
                capabilities: None,
                error: None,
                repair_hint: None,
            })),
            active_progress: Arc::new(Mutex::new(None)),
            active_speed_history: Arc::new(Mutex::new(Vec::new())),
            active_cancel_flag: Arc::new(Mutex::new(None)),
            blocked_volumes: Arc::new(Mutex::new(HashMap::new())),
            cleanup_lock: Arc::new(Mutex::new(())),
            pending_sample: Arc::new(Mutex::new(None)),
            power_assertion: Arc::new(Mutex::new(None)),
            busy_reason: Arc::new(Mutex::new(None)),
            notify_pair: Arc::new((Mutex::new(false), Condvar::new())),
            event_sink: Arc::new(Mutex::new(None)),
        };
        s.refresh_tools(false);
        let _ = s.reconcile_on_launch();
        s
    }

    pub fn set_event_sink<F>(&self, f: F)
    where
        F: Fn(&str, serde_json::Value) + Send + Sync + 'static,
    {
        if let Ok(mut sink) = self.event_sink.lock() {
            *sink = Some(Box::new(f));
        }
    }

    pub fn emit_event(&self, name: &str, payload: serde_json::Value) {
        if let Ok(sink) = self.event_sink.lock() {
            if let Some(ref f) = *sink {
                f(name, payload);
            }
        }
    }

    pub fn wake_scheduler(&self) {
        let (lock, cv) = &*self.notify_pair;
        if let Ok(mut g) = lock.lock() {
            *g = true;
            cv.notify_all();
        }
    }

    pub fn refresh_tools(&self, force: bool) -> ToolStatus {
        let configured_dir: Option<String> = self.db.get_setting("tools_dir").ok().flatten();
        let explicit_ff: Option<String> = self.db.get_setting("ffmpeg_path").ok().flatten();
        let explicit_fp: Option<String> = self.db.get_setting("ffprobe_path").ok().flatten();
        let status = discover_and_probe(
            &self.db,
            configured_dir.as_deref(),
            explicit_ff.as_deref(),
            explicit_fp.as_deref(),
            force,
        );
        if let Ok(mut ts) = self.tool_status.lock() {
            *ts = status.clone();
        }
        status
    }

    pub fn reconcile_on_launch(&self) -> Result<(), String> {
        // 1. Reconcile pending publish intents
        for intent in self.db.list_pending_publish_intents()? {
            let final_p = Path::new(&intent.final_path);
            let staged_p = Path::new(&intent.staged_path);
            let mut handled = false;

            if final_p.exists() {
                if let Ok(fp) = capture_file_identity(final_p) {
                    if fp.file_id == intent.staged_file_id && fp.size_bytes == intent.staged_size {
                        let _ = self
                            .db
                            .update_publish_intent_state(&intent.intent_id, "published", None);
                        if let Ok(Some(mut job)) = self.db.get_job(&intent.job_id) {
                            job.status = JobStatus::Completed;
                            job.output_bytes = Some(intent.staged_size);
                            job.output_fingerprint = Some(fp);
                            job.status_reason = Some("Recovered completed publication".to_string());
                            job.completed_at = Some(Utc::now().timestamp());
                            job.updated_at = Utc::now().timestamp();
                            let _ = self.db.upsert_job(&job);
                        }
                        handled = true;
                    }
                }
            }

            if !handled && staged_p.exists() {
                if let Ok(fp) = capture_file_identity(staged_p) {
                    if fp.file_id == intent.staged_file_id && fp.size_bytes == intent.staged_size {
                        if intent.state == "awaiting_confirmation" {
                            if let Ok(Some(mut job)) = self.db.get_job(&intent.job_id) {
                                job.status = JobStatus::NeedsAttention;
                                job.output_bytes = Some(intent.staged_size);
                                job.error_stage = Some("confirm_replace".to_string());
                                job.error_message = Some(
                                    "Encoded & verified before restart. Compare and confirm how to finalize."
                                        .to_string(),
                                );
                                job.updated_at = Utc::now().timestamp();
                                let _ = self.db.upsert_job(&job);
                            }
                            continue;
                        }
                        match publish_staged_file(staged_p, final_p, true) {
                            PublishOutcome::Published => {
                                let _ = self.db.update_publish_intent_state(
                                    &intent.intent_id,
                                    "published",
                                    None,
                                );
                                if let Ok(Some(mut job)) = self.db.get_job(&intent.job_id) {
                                    job.status = JobStatus::Completed;
                                    job.output_bytes = Some(intent.staged_size);
                                    job.output_fingerprint = capture_file_identity_with_blake3(final_p).ok();
                                    job.completed_at = Some(Utc::now().timestamp());
                                    job.updated_at = Utc::now().timestamp();
                                    let _ = self.db.upsert_job(&job);
                                }
                            }
                            PublishOutcome::DestinationExists | PublishOutcome::Error(_) => {
                                let _ = self.db.update_publish_intent_state(
                                    &intent.intent_id,
                                    "needs_attention",
                                    None,
                                );
                                if let Ok(Some(mut job)) = self.db.get_job(&intent.job_id) {
                                    job.status = JobStatus::NeedsAttention;
                                    job.error_stage = Some("publishing".to_string());
                                    job.error_message = Some(
                                        "Recovered interrupted publication; destination conflict or volume guarantee uncertain."
                                            .to_string(),
                                    );
                                    job.updated_at = Utc::now().timestamp();
                                    let _ = self.db.upsert_job(&job);
                                }
                            }
                        }
                        handled = true;
                    }
                }
            }

            if !handled {
                let _ = self
                    .db
                    .update_publish_intent_state(&intent.intent_id, "uncertain", None);
                if let Ok(Some(mut job)) = self.db.get_job(&intent.job_id) {
                    job.status = JobStatus::NeedsAttention;
                    job.error_stage = Some("publishing".to_string());
                    job.error_message = Some(
                        "Uncertain publication state across restart; staged output missing."
                            .to_string(),
                    );
                    job.updated_at = Utc::now().timestamp();
                    let _ = self.db.upsert_job(&job);
                }
            }
        }

        // 2. Reconcile stale active attempts & remove ONLY tracked orphaned staging files
        for (attempt_id, job_id, staging_dir, partial_path) in self.db.list_stale_attempts()? {
            let _ = self
                .db
                .record_attempt_finish(&attempt_id, None, "interrupted");
            let p = Path::new(&partial_path);
            if p.is_file() && partial_path.contains(".ffq-staging") {
                let _ = std::fs::remove_file(p);
            }
            let sd = Path::new(&staging_dir);
            // Non-recursive remove_dir only deletes the staging folder when empty,
            // preserving any unexpected user files inside it for inspection.
            if sd.is_dir() && staging_dir.contains(".ffq-staging") {
                let _ = std::fs::remove_dir(sd);
            }
            if let Ok(Some(mut job)) = self.db.get_job(&job_id) {
                if matches!(
                    job.status,
                    JobStatus::Probing | JobStatus::Encoding | JobStatus::Verifying
                ) {
                    job.status = JobStatus::Interrupted;
                    job.status_reason = Some("Interrupted by previous exit".to_string());
                    job.updated_at = Utc::now().timestamp();
                    let _ = self.db.upsert_job(&job);
                }
            }
        }

        // 3. Reconcile any leftover jobs in active states without stale attempt rows
        //    and migrate legacy "Canceled (Not selected)" rows back to clean Listed items.
        for mut job in self.db.list_all_jobs()? {
            if matches!(
                job.status,
                JobStatus::Probing | JobStatus::Encoding | JobStatus::Verifying | JobStatus::Publishing
            ) {
                job.status = JobStatus::Interrupted;
                job.status_reason = Some("Interrupted by previous exit".to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
            } else if job.status == JobStatus::Canceled
                && job.status_reason.as_deref() == Some("Not selected")
            {
                job.status = JobStatus::Listed;
                job.status_reason = None;
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
            }
        }

        // 4. Pause previously running or queued tabs on launch so user explicitly resumes instead of auto-encoding on startup
        for mut q in self.db.list_queues()? {
            let has_waiting_ready = !q.is_running
                && !q.is_paused
                && q.tab_type == TabType::Encode
                && self
                    .db
                    .list_jobs_for_queue(&q.id)
                    .map(|js| js.iter().any(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued)))
                    .unwrap_or(false);
            if q.is_running || has_waiting_ready {
                q.is_running = false;
                q.is_paused = true;
                q.pause_after_current = false;
                q.waiting_reason = Some("Paused after app restart — click Resume Queue to continue".to_string());
                let _ = self.db.upsert_queue(&q);
            }
        }

        // 5. Reconcile cleanup intents & organizer operations
        reconcile_cleanup_intents_on_launch(&self.db)?;
        crate::organize::reconcile_organizer_on_launch(&self.db)?;
        Ok(())
    }

    pub fn start_background_loop(self: &Arc<Self>) {
        let sched = self.clone();
        std::thread::spawn(move || {
            let mut rr_cursor: usize = 0;
            loop {
                // First handle any queued 15-second sample preview in the single-encoder budget
                let sample_req = sched.pending_sample.lock().ok().and_then(|mut g| g.take());
                if let Some(req) = sample_req {
                    sched.execute_sample_request(req);
                    continue;
                }

                // Pick next runnable job across started, non-paused queues (round-robin one file per queue)
                match sched.pick_next_job(&mut rr_cursor) {
                    Some((queue, job)) => {
                        sched.execute_job(queue, job);
                    }
                    None => {
                        sched.release_power_assertion();
                        let (lock, cv) = &*sched.notify_pair;
                        if let Ok(mut signaled) = lock.lock() {
                            while !*signaled {
                                let (g, _) = cv
                                    .wait_timeout(signaled, std::time::Duration::from_secs(2))
                                    .unwrap_or_else(|e| e.into_inner());
                                signaled = g;
                                break;
                            }
                            *signaled = false;
                        }
                    }
                }
            }
        });
    }

    fn execute_sample_request(&self, req: PendingSampleRequest) {
        let ff_opt = self
            .tool_status
            .lock()
            .ok()
            .and_then(|ts| ts.capabilities.as_ref().map(|c| PathBuf::from(&c.ffmpeg_path)));
        let Some(ffmpeg_path) = ff_opt else {
            return;
        };
        let Ok(Some(job)) = self.db.get_job(&req.job_id) else {
            return;
        };
        if let Ok(mut br) = self.busy_reason.lock() {
            *br = Some(format!("Encoding 15s preview sample for {}", job.source_rel_path));
        }
        let state = self
            .previews
            .run_sample_encode(&ffmpeg_path, &job, req.start_secs);
        if let Ok(mut br) = self.busy_reason.lock() {
            *br = None;
        }
        self.emit_event(
            "sample_preview_updated",
            serde_json::to_value(&state).unwrap_or_default(),
        );
        self.emit_snapshot_changed();
    }

    fn pick_next_job(&self, rr_cursor: &mut usize) -> Option<(QueueTab, JobRecord)> {
        let mut queues = self.db.list_queues().ok()?;
        if queues.is_empty() {
            return None;
        }
        let n = queues.len();
        let blocked = self.blocked_volumes.lock().ok()?.clone();
        let tools_caps = self
            .tool_status
            .lock()
            .ok()
            .and_then(|t| t.capabilities.clone());

        for offset in 0..n {
            let idx = (*rr_cursor + offset) % n;
            let mut q = queues[idx].clone();
            if q.tab_type != TabType::Encode || !q.is_running || q.is_paused {
                continue;
            }
            let jobs = self.db.list_jobs_for_queue(&q.id).ok()?;
            let mut found_ready: Option<JobRecord> = None;
            let mut block_reason: Option<String> = None;

            for j in &jobs {
                if j.status != JobStatus::Ready && j.status != JobStatus::Queued {
                    continue;
                }
                if j.plan.as_ref().map(|p| !p.is_ready).unwrap_or(true) {
                    continue;
                }
                if let Some(ref vid) = j.dest_volume_id {
                    if let Some(reason) = blocked.get(vid) {
                        block_reason = Some(reason.clone());
                        continue;
                    }
                }
                // Tool / encoder check
                match &tools_caps {
                    None => {
                        block_reason = Some("FFmpeg tools missing — click Locate FFmpeg tools".to_string());
                        continue;
                    }
                    Some(caps) if j.recipe.family == PresetFamily::Archive => {
                        let enc_ok = match j.recipe.codec {
                            VideoCodec::Hevc => caps.has_libx265,
                            VideoCodec::Av1 => caps.has_libsvtav1,
                        };
                        if !enc_ok {
                            block_reason = Some(format!(
                                "Required encoder {} is not available in current FFmpeg build",
                                j.recipe.codec.encoder_name()
                            ));
                            continue;
                        }
                    }
                    _ => {}
                }
                found_ready = Some(j.clone());
                break;
            }

            if let Some(job) = found_ready {
                *rr_cursor = (idx + 1) % n;
                // Update waiting_reason for other running queues
                for (o_idx, oq) in queues.iter_mut().enumerate() {
                    if o_idx != idx && oq.is_running && !oq.is_paused {
                        oq.waiting_reason = Some(format!(
                            "Waiting for {} to finish its current file",
                            q.name
                        ));
                        let _ = self.db.upsert_queue(oq);
                    } else if o_idx == idx && q.waiting_reason.is_some() {
                        q.waiting_reason = None;
                        let _ = self.db.upsert_queue(&q);
                    }
                }
                return Some((q, job));
            } else {
                // Check if any ready/queued jobs remain at all for this queue
                let has_pending_work = jobs.iter().any(|j| {
                    matches!(j.status, JobStatus::Ready | JobStatus::Queued)
                        && j.plan.as_ref().map(|p| p.is_ready).unwrap_or(false)
                });
                if !has_pending_work {
                    q.is_running = false;
                    q.pause_after_current = false;
                    q.waiting_reason = None;
                    let _ = self.db.upsert_queue(&q);
                    self.emit_snapshot_changed();
                } else if block_reason.is_some() && q.waiting_reason != block_reason {
                    q.waiting_reason = block_reason;
                    let _ = self.db.upsert_queue(&q);
                    self.emit_snapshot_changed();
                }
            }
        }

        None
    }

    fn acquire_power_assertion_if_needed(&self, queue: &QueueTab) {
        if !queue.keep_awake {
            return;
        }
        if let Ok(mut pa) = self.power_assertion.lock() {
            if pa.is_none() {
                if let Ok(a) = PowerAssertion::acquire("Media Shack overnight encoding") {
                    *pa = Some(a);
                }
            }
        }
    }

    fn release_power_assertion(&self) {
        if let Ok(mut pa) = self.power_assertion.lock() {
            *pa = None;
        }
    }

    fn execute_job(&self, mut queue: QueueTab, mut job: JobRecord) {
        self.acquire_power_assertion_if_needed(&queue);

        let Some(caps) = self
            .tool_status
            .lock()
            .ok()
            .and_then(|t| t.capabilities.clone())
        else {
            return;
        };
        let ffmpeg_path = PathBuf::from(&caps.ffmpeg_path);
        let ffprobe_path = PathBuf::from(&caps.ffprobe_path);

        // 1. Pre-encode source & destination volume re-checks
        let src_path = Path::new(&job.source_path);
        let dest_path = PathBuf::from(&job.destination_path);
        let dest_parent = dest_path.parent().unwrap_or_else(|| Path::new("."));

        match capture_file_identity(src_path) {
            Ok(fp) => {
                if let Some(ref prev_fp) = job.source_fingerprint {
                    if fp.file_id != prev_fp.file_id || fp.size_bytes != prev_fp.size_bytes {
                        self.mark_job_failed(
                            &mut queue,
                            &mut job,
                            "preflight",
                            "Source file changed or was replaced since import. Retry with fresh preflight.",
                            "",
                        );
                        return;
                    }
                }
                job.source_fingerprint = Some(fp);
            }
            Err(e) => {
                self.mark_job_failed(
                    &mut queue,
                    &mut job,
                    "preflight",
                    &format!("Source unavailable: {e}"),
                    "",
                );
                return;
            }
        }

        // Check destination regex, aliasing, writability, and volume identity BEFORE encoding
        if let Err(e) = apply_output_regex_to_stem("check", &queue.output_config) {
            job.status = JobStatus::NeedsAttention;
            job.error_stage = Some("preflight".to_string());
            job.error_message = Some(e);
            job.updated_at = Utc::now().timestamp();
            let _ = self.db.upsert_job(&job);
            self.emit_snapshot_changed();
            return;
        }
        if job.output_mode == OutputMode::TemplateFolder && dest_path == src_path {
            job.status = JobStatus::NeedsAttention;
            job.error_stage = Some("preflight".to_string());
            job.error_message = Some(
                "Destination path matches source path. Switch to Replace Original mode or adjust output template."
                    .to_string(),
            );
            job.updated_at = Utc::now().timestamp();
            let _ = self.db.upsert_job(&job);
            self.emit_snapshot_changed();
            return;
        }
        if let Ok(caps) = destination_capabilities(dest_parent) {
            if caps.is_read_only || !caps.is_writable {
                job.status = JobStatus::NeedsAttention;
                job.error_stage = Some("preflight".to_string());
                job.error_message = Some(format!(
                    "Destination filesystem ({}, {}) is read-only or not writable.",
                    caps.fs_type, caps.mount_point
                ));
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
                self.emit_snapshot_changed();
                return;
            }
        }

        // Create destination dir only if volume identity matches!
        // Check nearest existing ancestor's volume_id against recorded dest_volume_id
        let vol_stats = match disk_space_for_path(dest_parent) {
            Ok(vs) => vs,
            Err(e) => {
                if let Some(ref vid) = job.dest_volume_id {
                    if let Ok(mut bv) = self.blocked_volumes.lock() {
                        bv.insert(vid.clone(), format!("Destination drive unavailable: {e}"));
                    }
                }
                self.set_queue_waiting_reason(&queue.id, Some(format!("Waiting for destination drive: {e}")));
                self.emit_snapshot_changed();
                return;
            }
        };

        if let Some(ref expected_vid) = job.dest_volume_id {
            if &vol_stats.volume_id != expected_vid {
                let reason = format!(
                    "Destination drive disconnected or remounted (expected {expected_vid}, found {}). Refusing to write to fallback local mount.",
                    vol_stats.volume_id
                );
                if let Ok(mut bv) = self.blocked_volumes.lock() {
                    bv.insert(expected_vid.clone(), reason.clone());
                }
                self.set_queue_waiting_reason(&queue.id, Some("Waiting for destination drive".to_string()));
                self.emit_snapshot_changed();
                return;
            }
        } else {
            job.dest_volume_id = Some(vol_stats.volume_id.clone());
        }

        let reserve_bytes = self
            .db
            .get_volume_reserve(&vol_stats.volume_id)
            .ok()
            .flatten()
            .unwrap_or(0);
        if reserve_bytes > 0 && vol_stats.free_bytes < reserve_bytes {
            let reason = format!(
                "Waiting for space on {} (free {} < reserve {})",
                vol_stats.mount_point,
                format_bytes_short(vol_stats.free_bytes),
                format_bytes_short(reserve_bytes)
            );
            if let Ok(mut bv) = self.blocked_volumes.lock() {
                bv.insert(vol_stats.volume_id.clone(), reason.clone());
            }
            self.set_queue_waiting_reason(&queue.id, Some(reason));
            self.emit_snapshot_changed();
            return;
        }
        // Working-space coexistence budget check (when source is > 10 MiB)
        let working_needed = if job.recipe.family == PresetFamily::Remux {
            job.input_bytes
        } else {
            job.input_bytes / 2
        };
        if job.input_bytes > 10 * 1024 * 1024
            && vol_stats.free_bytes.saturating_sub(reserve_bytes) < working_needed
        {
            let reason = format!(
                "Waiting for working space on {} (free above reserve {} < needed {})",
                vol_stats.mount_point,
                format_bytes_short(vol_stats.free_bytes.saturating_sub(reserve_bytes)),
                format_bytes_short(working_needed)
            );
            if let Ok(mut bv) = self.blocked_volumes.lock() {
                bv.insert(vol_stats.volume_id.clone(), reason.clone());
            }
            self.set_queue_waiting_reason(&queue.id, Some(reason));
            self.emit_snapshot_changed();
            return;
        }

        // 2. Record attempt before spawning FFmpeg
        let attempt_id = Uuid::new_v4().to_string();
        let has_custom_dest = job.output_mode == OutputMode::ReplaceOriginal
            || queue
                .output_config
                .path_template
                .as_deref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false);
        let dest_root_for_staging = if has_custom_dest {
            dest_parent
                .ancestors()
                .find(|p| !p.as_os_str().is_empty() && p.is_dir())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dest_parent.to_path_buf())
        } else {
            queue
                .destination_root
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| dest_parent.to_path_buf())
        };
        let staging_dir = dest_root_for_staging.join(".ffq-staging").join(&attempt_id);
        let partial_path = staging_dir.join(format!(
            "part.{}.{}.ffq-part",
            attempt_id,
            job.recipe.container.ext()
        ));

        if let Err(e) = self.db.record_attempt_start(
            &attempt_id,
            &job.id,
            &queue.id,
            &staging_dir.to_string_lossy(),
            &partial_path.to_string_lossy(),
        ) {
            self.mark_job_failed(&mut queue, &mut job, "attempt_init", &e, "");
            return;
        }

        job.status = JobStatus::Encoding;
        job.status_reason = None;
        job.error_stage = None;
        job.error_message = None;
        job.current_attempt_id = Some(attempt_id.clone());
        job.tool_versions = Some(format!("{} | {}", caps.ffmpeg_version, caps.ffprobe_version));
        job.updated_at = Utc::now().timestamp();
        let _ = self.db.upsert_job(&job);
        self.emit_snapshot_changed();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut cf) = self.active_cancel_flag.lock() {
            *cf = Some((job.id.clone(), cancel_flag.clone()));
        }

        let pkt_buf = if cfg!(unix)
            && job.recipe.container == ContainerFormat::Mp4
            && job.recipe.family == PresetFamily::Archive
        {
            Some(PreviewPacketBuffer::new())
        } else {
            None
        };
        self.previews.set_active_encode_source(
            &job.id,
            &partial_path,
            job.recipe.container,
            pkt_buf.clone(),
        );
        self.previews
            .set_active_encode_source_path(&job.id, Path::new(&job.source_path));

        let progress_arc = self.active_progress.clone();
        let speed_hist_arc = self.active_speed_history.clone();
        if let Ok(mut sh) = speed_hist_arc.lock() {
            sh.clear();
        }
        let previews_for_cb = self.previews.clone();
        let sink_arc = self.event_sink.clone();
        let preview_pipe_dir = self.previews.preview_dir.clone();

        let outcome = run_ffmpeg_attempt(RunEncodeParams {
            ffmpeg_path: &ffmpeg_path,
            job: &job,
            attempt_id: &attempt_id,
            staging_dir: &staging_dir,
            partial_path: &partial_path,
            priority: queue.priority,
            reserve_bytes,
            cancel_flag: &cancel_flag,
            preview_pipe_dir: Some(&preview_pipe_dir),
            preview_buffer: pkt_buf,
            on_progress: Box::new(move |ev| {
                previews_for_cb.update_live_media_time(ev.out_time_secs);
                if let Ok(mut ap) = progress_arc.lock() {
                    *ap = Some(ev.clone());
                }
                if let Some(sp) = ev.speed {
                    if let Ok(mut sh) = speed_hist_arc.lock() {
                        sh.push(SpeedSample {
                            elapsed_secs: ev.elapsed_secs,
                            out_time_secs: ev.out_time_secs,
                            speed: sp,
                            fps: ev.fps,
                            bytes_written: ev.bytes_written,
                            est_final_ratio: ev.est_final_ratio,
                        });
                        if sh.len() > 240 {
                            // Thin older points by keeping every 2nd sample so timeline stays full-range
                            let mut compacted = Vec::with_capacity(140);
                            for (i, item) in sh.iter().enumerate() {
                                if i % 2 == 0 || i + 1 == sh.len() {
                                    compacted.push(item.clone());
                                }
                            }
                            *sh = compacted;
                        }
                    }
                }
                if let Ok(sink) = sink_arc.lock() {
                    if let Some(ref f) = *sink {
                        f("job_progress", serde_json::to_value(&ev).unwrap_or_default());
                    }
                }
            }),
        });

        self.previews.clear_active_encode_source();
        if let Ok(mut cf) = self.active_cancel_flag.lock() {
            *cf = None;
        }
        if let Ok(mut ap) = self.active_progress.lock() {
            *ap = None;
        }
        if let Ok(mut sh) = self.active_speed_history.lock() {
            sh.clear();
        }

        match outcome {
            EncodeRunOutcome::Canceled { diagnostics } => {
                let _ = self.db.record_attempt_finish(&attempt_id, None, "canceled");
                job.status = JobStatus::Canceled;
                job.status_reason = Some("Canceled by user".to_string());
                job.diagnostics = Some(diagnostics);
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
            }
            EncodeRunOutcome::DiskReserveCrossed {
                free_bytes,
                reserve_bytes,
                diagnostics,
            } => {
                let _ = self
                    .db
                    .record_attempt_finish(&attempt_id, None, "reserve_crossed");
                let reason = format!(
                    "Stopped: destination free space ({}) crossed reserve ({}). Choose another destination or free space to retry.",
                    format_bytes_short(free_bytes),
                    format_bytes_short(reserve_bytes)
                );
                if let Ok(mut bv) = self.blocked_volumes.lock() {
                    bv.insert(vol_stats.volume_id.clone(), reason.clone());
                }
                job.status = JobStatus::NeedsAttention;
                job.error_stage = Some("disk_space".to_string());
                job.error_message = Some(reason.clone());
                job.diagnostics = Some(diagnostics);
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
                self.set_queue_waiting_reason(&queue.id, Some(reason));
            }
            EncodeRunOutcome::Failed {
                exit_code,
                error_message,
                diagnostics,
            } => {
                let _ = self
                    .db
                    .record_attempt_finish(&attempt_id, exit_code, "failed");
                self.mark_job_failed(&mut queue, &mut job, "encoding", &error_message, &diagnostics);
            }
            EncodeRunOutcome::Finished {
                staging_dir,
                partial_path,
                diagnostics,
            } => {
                let _ = self.db.record_attempt_finish(&attempt_id, Some(0), "encoded");
                self.verify_and_publish(
                    &mut queue,
                    &mut job,
                    &attempt_id,
                    &ffmpeg_path,
                    &ffprobe_path,
                    &staging_dir,
                    &partial_path,
                    &vol_stats.volume_id,
                    reserve_bytes,
                    &diagnostics,
                );
            }
        }

        // Respect Pause after current file
        if let Ok(Some(mut latest_q)) = self.db.get_queue(&queue.id) {
            if latest_q.pause_after_current {
                latest_q.pause_after_current = false;
                latest_q.is_paused = true;
                latest_q.waiting_reason = Some("Paused by you".to_string());
                let _ = self.db.upsert_queue(&latest_q);
            }
        }

        self.emit_snapshot_changed();
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_and_publish(
        &self,
        queue: &mut QueueTab,
        job: &mut JobRecord,
        attempt_id: &str,
        ffmpeg_path: &Path,
        ffprobe_path: &Path,
        staging_dir: &Path,
        partial_path: &Path,
        dest_volume_id: &str,
        reserve_bytes: u64,
        diagnostics: &str,
    ) {
        job.status = JobStatus::Verifying;
        job.updated_at = Utc::now().timestamp();
        let _ = self.db.upsert_job(job);
        self.emit_snapshot_changed();

        let Some(ref src_probe) = job.probe else {
            let _ = std::fs::remove_file(partial_path);
            let _ = std::fs::remove_dir_all(staging_dir);
            self.mark_job_failed(queue, job, "verifying", "Missing source probe metadata", diagnostics);
            return;
        };
        let Some(ref plan) = job.plan else {
            let _ = std::fs::remove_file(partial_path);
            let _ = std::fs::remove_dir_all(staging_dir);
            self.mark_job_failed(queue, job, "verifying", "Missing track plan", diagnostics);
            return;
        };

        let require_full_decode =
            queue.full_decode_verify || job.cleanup_policy == CleanupPolicy::TrashAfterEachFile;

        let report = verify_staged_output(
            ffprobe_path,
            ffmpeg_path,
            Path::new(&job.source_path),
            src_probe,
            partial_path,
            &job.recipe,
            plan,
            require_full_decode,
        );

        if !report.passed {
            let _ = std::fs::remove_file(partial_path);
            let _ = std::fs::remove_dir_all(staging_dir);
            self.mark_job_failed(
                queue,
                job,
                "verifying",
                &format!("{}: {}", report.summary, report.details),
                diagnostics,
            );
            return;
        }

        // Minimum size reduction check (evaluated after verification and before publication)
        if job.min_savings_enabled {
            let threshold_bps = (job.min_savings_pct.clamp(0.0, 99.0) * 100.0).round() as u128;
            let max_allowed_bps = 10_000u128.saturating_sub(threshold_bps);
            let lhs = (report.output_bytes as u128) * 10_000u128;
            let rhs = (job.input_bytes as u128) * max_allowed_bps;
            if lhs > rhs {
                let _ = std::fs::remove_file(partial_path);
                let _ = std::fs::remove_dir_all(staging_dir);
                job.status = JobStatus::BelowSavingsTarget;
                job.output_bytes = Some(report.output_bytes);
                job.status_reason = Some(format!(
                    "Below savings target ({:.1}% required) · original kept",
                    job.min_savings_pct
                ));
                job.cleanup_outcome = CleanupOutcome::Kept;
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
                return;
            }
        }

        // Persist staged output intent immediately after verification so staged file is always recoverable
        let staged_fp = match capture_file_identity(partial_path) {
            Ok(fp) => fp,
            Err(e) => {
                self.mark_job_failed(queue, job, "publishing", &format!("Staged file identity error: {e}"), diagnostics);
                return;
            }
        };

        let intent_id = Uuid::new_v4().to_string();
        let mut final_dest = PathBuf::from(&job.destination_path);
        let _ = self.db.save_publish_intent(
            &intent_id,
            &job.id,
            attempt_id,
            &partial_path.to_string_lossy(),
            &staged_fp.volume_id,
            &staged_fp.file_id,
            report.output_bytes,
            &final_dest.to_string_lossy(),
        );

        // Final free-space & volume identity recheck before publication (preserve staged file on trouble!)
        let dest_parent = final_dest.parent().unwrap_or_else(|| Path::new("."));
        if let Ok(vs) = disk_space_for_path(dest_parent) {
            if vs.volume_id != dest_volume_id {
                let reason = "Destination drive changed before publication; staged output preserved for review.".to_string();
                if let Ok(mut bv) = self.blocked_volumes.lock() {
                    bv.insert(dest_volume_id.to_string(), reason.clone());
                }
                let _ = self.db.update_publish_intent_state(&intent_id, "needs_attention", None);
                job.status = JobStatus::NeedsAttention;
                job.output_bytes = Some(report.output_bytes);
                job.error_stage = Some("publishing".to_string());
                job.error_message = Some(reason);
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
                return;
            }
            if reserve_bytes > 0 && vs.free_bytes < reserve_bytes {
                let reason = "Destination free space below reserve before publication; staged output preserved.".to_string();
                if let Ok(mut bv) = self.blocked_volumes.lock() {
                    bv.insert(dest_volume_id.to_string(), reason.clone());
                }
                let _ = self.db.update_publish_intent_state(&intent_id, "needs_attention", None);
                job.status = JobStatus::NeedsAttention;
                job.output_bytes = Some(report.output_bytes);
                job.error_stage = Some("publishing".to_string());
                job.error_message = Some(reason);
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
                return;
            }
        }

        // Durable publication
        job.status = JobStatus::Publishing;
        job.updated_at = Utc::now().timestamp();
        let _ = self.db.upsert_job(job);
        self.emit_snapshot_changed();

        if job.output_mode == OutputMode::ReplaceOriginal {
            let src_path = PathBuf::from(&job.source_path);
            if job.replace_behavior == ReplaceBehavior::KeepForComparison {
                if final_dest != src_path && !final_dest.exists() {
                    match publish_staged_file(partial_path, &final_dest, true) {
                        PublishOutcome::Published => {
                            let _ = self.db.update_publish_intent_state(&intent_id, "published", None);
                            let _ = std::fs::remove_dir_all(staging_dir);
                            job.status = JobStatus::NeedsAttention;
                            job.output_bytes = Some(report.output_bytes);
                            job.output_fingerprint = capture_file_identity_with_blake3(&final_dest).ok();
                            job.full_decode_verified = report.full_decode_verified;
                            job.error_stage = Some("confirm_replace".to_string());
                            job.error_message = Some(
                                "Encoded output saved alongside original. Compare quality before removing original."
                                    .to_string(),
                            );
                            job.cleanup_outcome = CleanupOutcome::Pending;
                            job.diagnostics = Some(diagnostics.to_string());
                            job.updated_at = Utc::now().timestamp();
                            let _ = self.db.upsert_job(job);
                            return;
                        }
                        PublishOutcome::DestinationExists | PublishOutcome::Error(_) => {}
                    }
                }
                let _ = self
                    .db
                    .update_publish_intent_state(&intent_id, "awaiting_confirmation", None);
                job.status = JobStatus::NeedsAttention;
                job.output_bytes = Some(report.output_bytes);
                job.full_decode_verified = report.full_decode_verified;
                job.error_stage = Some("confirm_replace".to_string());
                job.error_message = Some(
                    "Encoded & verified. Compare Original vs Encoded before replacing (staged file preserved)."
                        .to_string(),
                );
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
                return;
            }

            let prefer_trash = matches!(destination_capabilities(&final_dest), Ok(c) if c.supports_trash);
            match fault_tolerant_replace_file(
                partial_path,
                &src_path,
                &final_dest,
                report.output_bytes,
                job.source_fingerprint.as_ref(),
                prefer_trash,
                false,
            ) {
                Ok(_) => {
                    let _ = self
                        .db
                        .update_publish_intent_state(&intent_id, "published", None);
                    let _ = std::fs::remove_dir_all(staging_dir);
                    self.complete_published_job(
                        job,
                        Some(ffprobe_path),
                        Some(ffmpeg_path),
                        &final_dest,
                        report.output_bytes,
                        report.full_decode_verified,
                        report.summary,
                        diagnostics,
                        true,
                    );
                }
                Err(e) => {
                    let _ = self
                        .db
                        .update_publish_intent_state(&intent_id, "failed", None);
                    job.status = JobStatus::NeedsAttention;
                    job.output_bytes = Some(report.output_bytes);
                    job.full_decode_verified = report.full_decode_verified;
                    job.error_stage = Some("publishing".to_string());
                    job.error_message = Some(e);
                    job.diagnostics = Some(diagnostics.to_string());
                    job.updated_at = Utc::now().timestamp();
                    let _ = self.db.upsert_job(job);
                }
            }
            return;
        }

        let mut publish_res = publish_staged_file(partial_path, &final_dest, true);
        if publish_res == PublishOutcome::DestinationExists {
            let mut reserved: HashSet<String> = self
                .db
                .list_all_jobs()
                .unwrap_or_default()
                .into_iter()
                .map(|j| j.destination_path.to_ascii_lowercase())
                .collect();
            let dest_root = queue
                .destination_root
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| final_dest.parent().unwrap_or(Path::new(".")).to_path_buf());
            if let Ok(resolved) = resolve_destination_path_with_config(
                Path::new(&job.source_path),
                Path::new(&job.source_rel_path),
                &dest_root,
                &job.recipe,
                &queue.output_config,
                &mut reserved,
            ) {
                final_dest = resolved;
                job.destination_path = final_dest.to_string_lossy().to_string();
                let _ = self.db.update_publish_intent_state(
                    &intent_id,
                    "pending",
                    Some(&job.destination_path),
                );
                publish_res = publish_staged_file(partial_path, &final_dest, true);
            }
        }

        match publish_res {
            PublishOutcome::Published => {
                let _ = self
                    .db
                    .update_publish_intent_state(&intent_id, "published", None);
                let _ = std::fs::remove_dir_all(staging_dir);
                self.complete_published_job(
                    job,
                    Some(ffprobe_path),
                    Some(ffmpeg_path),
                    &final_dest,
                    report.output_bytes,
                    report.full_decode_verified,
                    report.summary,
                    diagnostics,
                    false,
                );
            }
            PublishOutcome::DestinationExists => {
                let _ = self
                    .db
                    .update_publish_intent_state(&intent_id, "failed", None);
                job.status = JobStatus::NeedsAttention;
                job.output_bytes = Some(report.output_bytes);
                job.full_decode_verified = report.full_decode_verified;
                job.error_stage = Some("publishing".to_string());
                job.error_message = Some(
                    "Destination file already exists; staged output preserved for your decision."
                        .to_string(),
                );
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
            }
            PublishOutcome::Error(e) => {
                let _ = self
                    .db
                    .update_publish_intent_state(&intent_id, "failed", None);
                job.status = JobStatus::NeedsAttention;
                job.output_bytes = Some(report.output_bytes);
                job.full_decode_verified = report.full_decode_verified;
                job.error_stage = Some("publishing".to_string());
                job.error_message = Some(e);
                job.diagnostics = Some(diagnostics.to_string());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(job);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_published_job(
        &self,
        job: &mut JobRecord,
        ffprobe_path: Option<&Path>,
        ffmpeg_path: Option<&Path>,
        final_dest: &Path,
        output_bytes: u64,
        full_decode_verified: bool,
        summary: String,
        diagnostics: &str,
        replaced_original: bool,
    ) {
        job.status = JobStatus::Completed;
        job.destination_path = final_dest.to_string_lossy().to_string();
        job.output_bytes = Some(output_bytes);
        job.output_fingerprint = capture_file_identity_with_blake3(final_dest).ok();
        job.full_decode_verified = full_decode_verified;
        job.error_stage = None;
        job.error_message = None;
        job.status_reason = Some(summary);
        if !diagnostics.is_empty() {
            job.diagnostics = Some(diagnostics.to_string());
        }
        job.cleanup_outcome = if replaced_original {
            CleanupOutcome::Trashed
        } else {
            match job.cleanup_policy {
                CleanupPolicy::KeepOriginals => CleanupOutcome::Kept,
                CleanupPolicy::ReviewAfterCompletion => CleanupOutcome::Pending,
                CleanupPolicy::TrashAfterEachFile => CleanupOutcome::Pending,
            }
        };
        job.completed_at = Some(Utc::now().timestamp());
        job.updated_at = Utc::now().timestamp();
        let _ = self.db.upsert_job(job);

        let rel_type = if job.recipe.family == PresetFamily::Remux {
            "remuxed_from"
        } else {
            "transcoded_from"
        };
        let details = serde_json::json!({
            "job_id": job.id,
            "recipe": job.recipe,
            "input_bytes": job.input_bytes,
            "output_bytes": output_bytes,
        });
        let _ = crate::catalog::register_derived_file(
            &self.db,
            &job.source_path,
            final_dest,
            rel_type,
            details,
            true,
            ffprobe_path,
        );

        if !replaced_original && job.cleanup_policy == CleanupPolicy::TrashAfterEachFile {
            let _guard = self.cleanup_lock.lock();
            let _ = execute_cleanup_for_job(&self.db, ffmpeg_path, &job.id, true);
        }
    }

    fn mark_job_failed(
        &self,
        queue: &mut QueueTab,
        job: &mut JobRecord,
        stage: &str,
        cause: &str,
        diagnostics: &str,
    ) {
        job.status = JobStatus::Failed;
        job.error_stage = Some(stage.to_string());
        job.error_message = Some(cause.to_string());
        if !diagnostics.is_empty() {
            job.diagnostics = Some(diagnostics.to_string());
        }
        job.updated_at = Utc::now().timestamp();
        let _ = self.db.upsert_job(job);

        if let Ok(Some(mut latest_q)) = self.db.get_queue(&queue.id) {
            if latest_q.error_policy == ErrorPolicy::PauseOnFirst {
                latest_q.is_paused = true;
                latest_q.waiting_reason = Some(format!("Paused on error: {cause}"));
                let _ = self.db.upsert_queue(&latest_q);
            }
        }
    }

    fn set_queue_waiting_reason(&self, queue_id: &str, reason: Option<String>) {
        if let Ok(Some(mut latest_q)) = self.db.get_queue(queue_id) {
            latest_q.waiting_reason = reason;
            let _ = self.db.upsert_queue(&latest_q);
        }
    }

    pub fn retry_job_with_fresh_preflight(&self, job_id: &str) -> Result<JobRecord, String> {
        let mut job = self
            .db
            .get_job(job_id)?
            .ok_or_else(|| "Job not found".to_string())?;
        if !job.status.is_retryable() {
            return Err("Only failed, canceled, interrupted, or needs-attention jobs can be retried.".to_string());
        }

        let ffprobe_path = self
            .tool_status
            .lock()
            .ok()
            .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)))
            .ok_or_else(|| "ffprobe is not configured".to_string())?;

        let src_path = Path::new(&job.source_path);
        let fp = match capture_file_identity(src_path) {
            Ok(fp) => fp,
            Err(e) => {
                let msg = format!("Retry preflight failed: {e}");
                job.error_stage = Some("preflight".to_string());
                job.error_message = Some(msg.clone());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
                self.emit_snapshot_changed();
                return Err(msg);
            }
        };
        let probe = match probe_file(&ffprobe_path, src_path) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Retry preflight probe failed: {e}");
                job.error_stage = Some("preflight".to_string());
                job.error_message = Some(msg.clone());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
                self.emit_snapshot_changed();
                return Err(msg);
            }
        };
        let plan = build_track_plan(src_path, &probe, &job.recipe, &job.compat_overrides);

        // Verify destination volume matches recorded dest_volume_id (never silently fall back to boot drive!)
        let dest_parent = Path::new(&job.destination_path)
            .parent()
            .unwrap_or_else(|| Path::new("."));
        match disk_space_for_path(dest_parent) {
            Ok(vs) => {
                if let Some(ref expected_vid) = job.dest_volume_id {
                    if &vs.volume_id != expected_vid {
                        let msg = format!(
                            "Destination volume is disconnected or remounted (expected {expected_vid}, found {}). Reconnect drive or choose a new destination.",
                            vs.volume_id
                        );
                        job.error_stage = Some("preflight".to_string());
                        job.error_message = Some(msg.clone());
                        job.updated_at = Utc::now().timestamp();
                        let _ = self.db.upsert_job(&job);
                        self.emit_snapshot_changed();
                        return Err(msg);
                    }
                } else {
                    job.dest_volume_id = Some(vs.volume_id);
                }
            }
            Err(e) => {
                let msg = format!("Destination volume unavailable: {e}");
                job.error_stage = Some("preflight".to_string());
                job.error_message = Some(msg.clone());
                job.updated_at = Utc::now().timestamp();
                let _ = self.db.upsert_job(&job);
                self.emit_snapshot_changed();
                return Err(msg);
            }
        }

        // Clear any volume block for this job's volume on explicit retry/recheck
        if let Some(ref vid) = job.dest_volume_id {
            if let Ok(mut bv) = self.blocked_volumes.lock() {
                bv.remove(vid);
            }
        }

        job.source_fingerprint = Some(fp);
        job.input_bytes = probe.format.size_bytes.unwrap_or(job.input_bytes);
        job.probe = Some(probe);
        job.status = if plan.is_ready {
            JobStatus::Ready
        } else {
            JobStatus::NeedsAttention
        };
        job.plan = Some(plan);
        job.error_stage = None;
        job.error_message = None;
        job.status_reason = None;
        job.updated_at = Utc::now().timestamp();
        self.db.upsert_job(&job)?;
        self.wake_scheduler();
        self.emit_snapshot_changed();
        Ok(job)
    }

    pub fn recheck_volumes_and_resume(&self) {
        if let Ok(mut bv) = self.blocked_volumes.lock() {
            bv.clear();
        }
        if let Ok(queues) = self.db.list_queues() {
            for mut q in queues {
                if q.is_running && !q.is_paused {
                    q.waiting_reason = None;
                    let _ = self.db.upsert_queue(&q);
                }
            }
        }
        self.wake_scheduler();
        self.emit_snapshot_changed();
    }

    pub fn build_morning_summary(&self, queue_id: &str) -> Result<MorningSummary, String> {
        let jobs = self.db.list_jobs_for_queue(queue_id)?;
        let mut s = MorningSummary {
            queue_id: queue_id.to_string(),
            completed_count: 0,
            failed_count: 0,
            needs_attention_count: 0,
            below_savings_count: 0,
            canceled_count: 0,
            interrupted_count: 0,
            total_input_bytes: 0,
            total_output_bytes: 0,
            trashed_count: 0,
            kept_count: 0,
            cleanup_failed_count: 0,
            cleanup_uncertain_count: 0,
            recoverable_bytes_by_volume: HashMap::new(),
        };

        for j in &jobs {
            match j.status {
                JobStatus::Completed => {
                    s.completed_count += 1;
                    s.total_input_bytes += j.input_bytes;
                    s.total_output_bytes += j.output_bytes.unwrap_or(0);
                    match j.cleanup_outcome {
                        CleanupOutcome::Trashed => s.trashed_count += 1,
                        CleanupOutcome::Failed => s.cleanup_failed_count += 1,
                        CleanupOutcome::Uncertain => s.cleanup_uncertain_count += 1,
                        _ => {
                            s.kept_count += 1;
                            if let Some(ref fp) = j.source_fingerprint {
                                *s.recoverable_bytes_by_volume
                                    .entry(fp.volume_id.clone())
                                    .or_insert(0) += j.input_bytes;
                            }
                        }
                    }
                }
                JobStatus::Failed => s.failed_count += 1,
                JobStatus::NeedsAttention => s.needs_attention_count += 1,
                JobStatus::BelowSavingsTarget => s.below_savings_count += 1,
                JobStatus::Canceled => s.canceled_count += 1,
                JobStatus::Interrupted => s.interrupted_count += 1,
                _ => {}
            }
        }
        Ok(s)
    }

    pub fn collect_volume_infos(&self) -> Vec<VolumeSpaceInfo> {
        let mut seen: HashMap<String, VolumeSpaceInfo> = HashMap::new();
        let reserves: HashMap<String, u64> = self
            .db
            .list_volume_reserves()
            .unwrap_or_default()
            .into_iter()
            .map(|(vid, _, rb)| (vid, rb))
            .collect();
        let blocked = self.blocked_volumes.lock().ok().map(|b| b.clone()).unwrap_or_default();

        let mut candidate_paths: Vec<PathBuf> = Vec::new();
        if let Ok(queues) = self.db.list_queues() {
            for q in queues {
                if let Some(ref dr) = q.destination_root {
                    candidate_paths.push(PathBuf::from(dr));
                }
            }
        }
        if let Ok(jobs) = self.db.list_all_jobs() {
            for j in jobs {
                if let Some(p) = Path::new(&j.destination_path).parent() {
                    candidate_paths.push(p.to_path_buf());
                }
            }
        }

        for p in candidate_paths {
            if let Ok(vs) = disk_space_for_path(&p) {
                let res = reserves.get(&vs.volume_id).copied().unwrap_or(0);
                let blk = blocked.get(&vs.volume_id).cloned();
                seen.entry(vs.volume_id.clone()).or_insert(VolumeSpaceInfo {
                    volume_id: vs.volume_id,
                    mount_point: vs.mount_point,
                    free_bytes: vs.free_bytes,
                    total_bytes: vs.total_bytes,
                    reserve_bytes: res,
                    blocked_reason: blk,
                });
            }
        }

        seen.into_values().collect()
    }

    pub fn build_snapshot(&self) -> Result<AppSnapshot, String> {
        let queues = self.db.list_queues()?;
        let mut active_queue_id: String = self
            .db
            .get_setting("active_queue_id")?
            .unwrap_or_default();
        if !queues.iter().any(|q| q.id == active_queue_id) {
            if let Some(first) = queues.first() {
                active_queue_id = first.id.clone();
                let _ = self.db.set_setting("active_queue_id", &active_queue_id);
            }
        }

        let jobs = self.db.list_all_jobs()?;
        let profiles = self.db.list_profiles()?;
        let named_presets = self.db.list_named_presets()?;
        let tools = self
            .tool_status
            .lock()
            .map(|t| t.clone())
            .unwrap_or(ToolStatus {
                discovered: false,
                capabilities: None,
                error: None,
                repair_hint: None,
            });
        let volumes = self.collect_volume_infos();
        let morning_summary = self.build_morning_summary(&active_queue_id)?;
        let active_progress = self.active_progress.lock().ok().and_then(|g| g.clone());
        let active_speed_history = self
            .active_speed_history
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let sample_preview = self.previews.sample_state.lock().ok().and_then(|g| g.clone());
        let live_preview = self.previews.live_info.lock().ok().and_then(|g| g.clone());
        let power_assertion_active = self
            .power_assertion
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false);
        let scheduler_busy_reason = self.busy_reason.lock().ok().and_then(|b| b.clone());

        let repair_sessions = self.db.list_repair_sessions()?;
        let tab_meta: HashMap<String, TabWorkspaceMeta> = self.db.get_workspace_json("ws_tab_meta")?;
        let job_decisions: HashMap<String, String> = self.db.get_workspace_json("ws_job_decisions")?;
        let protected_sources: Vec<String> = self.db.get_workspace_json("ws_protected_sources")?;
        let tab_dedupe_groups: HashMap<String, Vec<TabDedupeGroup>> =
            self.db.get_workspace_json("ws_tab_dedupe_groups")?;
        let tab_repair_items: HashMap<String, TabRepairItemState> =
            self.db.get_workspace_json("ws_tab_repair_items")?;
        let hash_cache_stats = self.db.get_hash_cache_stats().unwrap_or_default();

        Ok(AppSnapshot {
            queues,
            active_queue_id,
            jobs,
            repair_sessions,
            tab_meta,
            job_decisions,
            protected_sources,
            tab_dedupe_groups,
            tab_repair_items,
            hash_cache_stats,
            profiles,
            named_presets,
            tools,
            volumes,
            morning_summary,
            active_progress,
            active_speed_history,
            sample_preview,
            live_preview,
            power_assertion_active,
            scheduler_busy_reason,
        })
    }

    pub fn emit_snapshot_changed(&self) {
        self.emit_event("snapshot_changed", serde_json::json!({}));
    }
}

pub fn format_bytes_short(b: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if b as f64 >= GIB {
        format!("{:.2} GiB", (b as f64) / GIB)
    } else {
        format!("{:.1} MiB", (b as f64) / MIB)
    }
}
