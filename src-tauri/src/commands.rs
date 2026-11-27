use crate::cleanup::{
    build_cleanup_candidates, execute_cleanup_for_job, record_batch_cleanup_consent,
};
use crate::models::*;
use crate::planner::{
    build_track_plan, derive_destination_root_from_job, probe_file, resolve_destination_path,
};
use crate::platform::{
    capture_file_identity, disk_space_for_path, open_in_default_player, reveal_in_file_manager,
};
use crate::scanner::scan_import_paths;
use crate::scheduler::{PendingSampleRequest, Scheduler};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{ipc::Response, State};
use uuid::Uuid;

pub struct AppState {
    pub scheduler: Arc<Scheduler>,
    pub active_scans: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub _instance_lock: Arc<Mutex<Option<crate::platform::InstanceLock>>>,
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.scheduler.build_snapshot()
}

#[tauri::command]
pub fn locate_ffmpeg_tools(
    state: State<'_, AppState>,
    ffmpeg_path: Option<String>,
    ffprobe_path: Option<String>,
    tools_dir: Option<String>,
) -> Result<ToolStatus, String> {
    if let Some(ref ff) = ffmpeg_path {
        state.scheduler.db.set_setting("ffmpeg_path", ff)?;
    }
    if let Some(ref fp) = ffprobe_path {
        state.scheduler.db.set_setting("ffprobe_path", fp)?;
    }
    if let Some(ref dir) = tools_dir {
        state.scheduler.db.set_setting("tools_dir", dir)?;
    }
    let ts = state.scheduler.refresh_tools(true);
    state.scheduler.emit_snapshot_changed();
    Ok(ts)
}

#[tauri::command]
pub fn recheck_tools(state: State<'_, AppState>) -> Result<ToolStatus, String> {
    let ts = state.scheduler.refresh_tools(true);
    state.scheduler.emit_snapshot_changed();
    Ok(ts)
}

#[tauri::command]
pub fn create_queue_tab(
    state: State<'_, AppState>,
    name: Option<String>,
) -> Result<QueueTab, String> {
    let queues = state.scheduler.db.list_queues()?;
    let next_order = queues.iter().map(|q| q.tab_order).max().unwrap_or(-1) + 1;
    let tab_name = name.unwrap_or_else(|| format!("Queue {}", queues.len() + 1));
    let q = QueueTab {
        id: Uuid::new_v4().to_string(),
        name: tab_name,
        tab_order: next_order,
        profile_id: None,
        profile_name: None,
        recipe: EncodeRecipe::archive_balanced_default(),
        priority: ProcessPriority::Background,
        recursive_import: true,
        destination_root: None,
        keep_awake: true,
        error_policy: ErrorPolicy::Continue,
        min_savings_enabled: false,
        min_savings_pct: 15.0,
        cleanup_policy: CleanupPolicy::KeepOriginals,
        full_decode_verify: false,
        is_running: false,
        is_paused: false,
        pause_after_current: false,
        waiting_reason: None,
    };
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.db.set_setting("active_queue_id", &q.id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(q)
}

#[tauri::command]
pub fn rename_queue_tab(
    state: State<'_, AppState>,
    queue_id: String,
    name: String,
) -> Result<(), String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    q.name = name.trim().to_string();
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn delete_queue_tab(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    let queues = state.scheduler.db.list_queues()?;
    if queues.len() <= 1 {
        return Err("At least one queue tab must remain open.".to_string());
    }
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    if jobs.iter().any(|j| {
        matches!(
            j.status,
            JobStatus::Encoding | JobStatus::Verifying | JobStatus::Publishing
        )
    }) {
        return Err("Stop or cancel active job before closing this queue tab.".to_string());
    }
    state.scheduler.db.delete_queue(&queue_id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn select_queue_tab(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    state.scheduler.db.set_setting("active_queue_id", &queue_id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
pub struct QueueSettingsPatch {
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
}

#[tauri::command]
pub fn update_queue_settings(
    state: State<'_, AppState>,
    queue_id: String,
    patch: QueueSettingsPatch,
) -> Result<QueueTab, String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let mut recipe = patch.recipe;
    if recipe.family == PresetFamily::Archive {
        let (min_crf, max_crf) = recipe.codec.crf_range();
        recipe.crf = recipe.crf.clamp(min_crf, max_crf);
    }
    q.recipe = recipe;
    q.priority = patch.priority;
    q.recursive_import = patch.recursive_import;
    q.destination_root = patch.destination_root;
    q.keep_awake = patch.keep_awake;
    q.error_policy = patch.error_policy;
    q.min_savings_enabled = patch.min_savings_enabled;
    q.min_savings_pct = patch.min_savings_pct.clamp(1.0, 95.0);
    q.cleanup_policy = patch.cleanup_policy;
    q.full_decode_verify = patch.full_decode_verify;
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(q)
}

#[tauri::command]
pub fn apply_queue_settings_to_waiting(
    state: State<'_, AppState>,
    queue_id: String,
    job_ids: Option<Vec<String>>,
) -> Result<usize, String> {
    let q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let ffprobe_opt = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));
    let all_jobs = state.scheduler.db.list_all_jobs()?;
    let mut reserved: HashSet<String> = all_jobs
        .iter()
        .map(|j| j.destination_path.to_ascii_lowercase())
        .collect();

    let id_filter: Option<HashSet<String>> = job_ids.map(|v| v.into_iter().collect());
    let mut updated = 0usize;

    for mut job in state.scheduler.db.list_jobs_for_queue(&queue_id)? {
        if !job.status.is_pending_editable() {
            continue;
        }
        if let Some(ref set) = id_filter {
            if !set.contains(&job.id) {
                continue;
            }
        }
        reserved.remove(&job.destination_path.to_ascii_lowercase());
        job.recipe = q.recipe.clone();
        job.min_savings_enabled = q.min_savings_enabled;
        job.min_savings_pct = q.min_savings_pct;
        // Applying settings requires fresh cleanup consent if policy changed
        job.cleanup_policy = q.cleanup_policy;
        job.consent_id = None;

        let dest_root = q
            .destination_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                derive_destination_root_from_job(
                    Path::new(&job.destination_path),
                    Path::new(&job.source_rel_path),
                )
            });
        let new_dest = resolve_destination_path(
            Path::new(&job.source_path),
            Path::new(&job.source_rel_path),
            &dest_root,
            &job.recipe,
            &mut reserved,
        );
        job.destination_path = new_dest.to_string_lossy().to_string();
        if let Some(dp) = Path::new(&job.destination_path).parent() {
            if let Ok(vs) = disk_space_for_path(dp) {
                job.dest_volume_id = Some(vs.volume_id);
            }
        }

        // Fresh preflight
        if let Some(ref fp_bin) = ffprobe_opt {
            match probe_file(fp_bin, Path::new(&job.source_path)) {
                Ok(probe) => {
                    let plan = build_track_plan(
                        Path::new(&job.source_path),
                        &probe,
                        &job.recipe,
                        &job.compat_overrides,
                    );
                    job.status = if plan.is_ready {
                        JobStatus::Ready
                    } else {
                        JobStatus::NeedsAttention
                    };
                    job.probe = Some(probe);
                    job.plan = Some(plan);
                }
                Err(e) => {
                    job.status = JobStatus::NeedsAttention;
                    job.error_stage = Some("preflight".to_string());
                    job.error_message = Some(e);
                }
            }
        }

        state.scheduler.previews.mark_sample_stale_if_matching(&job.id);
        job.updated_at = Utc::now().timestamp();
        state.scheduler.db.upsert_job(&job)?;
        updated += 1;
    }

    state.scheduler.emit_snapshot_changed();
    Ok(updated)
}

#[tauri::command]
pub fn save_named_preset(
    state: State<'_, AppState>,
    name: String,
    recipe: EncodeRecipe,
) -> Result<NamedPreset, String> {
    let p = state.scheduler.db.save_named_preset(name.trim(), &recipe)?;
    state.scheduler.emit_snapshot_changed();
    Ok(p)
}

#[tauri::command]
pub fn delete_named_preset(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.scheduler.db.delete_named_preset(&id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn save_profile_from_queue(
    state: State<'_, AppState>,
    queue_id: String,
    name: String,
    existing_profile_id: Option<String>,
) -> Result<Profile, String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let now = Utc::now().timestamp();
    let existing = existing_profile_id
        .and_then(|pid| {
            state
                .scheduler
                .db
                .list_profiles()
                .ok()
                .and_then(|list| list.into_iter().find(|p| p.id == pid))
        });

    let profile = Profile {
        id: existing
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: name.trim().to_string(),
        recipe: q.recipe.clone(),
        priority: q.priority,
        recursive_import: q.recursive_import,
        destination_root: q.destination_root.clone(),
        keep_awake: q.keep_awake,
        error_policy: q.error_policy,
        min_savings_enabled: q.min_savings_enabled,
        min_savings_pct: q.min_savings_pct,
        cleanup_policy: q.cleanup_policy,
        full_decode_verify: q.full_decode_verify,
        created_at: existing.as_ref().map(|p| p.created_at).unwrap_or(now),
        updated_at: now,
    };

    state.scheduler.db.save_profile(&profile)?;
    q.profile_id = Some(profile.id.clone());
    q.profile_name = Some(profile.name.clone());
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(profile)
}

#[tauri::command]
pub fn rename_profile(
    state: State<'_, AppState>,
    profile_id: String,
    new_name: String,
) -> Result<Profile, String> {
    let mut profile = state
        .scheduler
        .db
        .list_profiles()?
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| "Profile not found".to_string())?;
    profile.name = new_name.trim().to_string();
    profile.updated_at = Utc::now().timestamp();
    state.scheduler.db.save_profile(&profile)?;
    for mut q in state.scheduler.db.list_queues()? {
        if q.profile_id.as_deref() == Some(&profile_id) {
            q.profile_name = Some(profile.name.clone());
            let _ = state.scheduler.db.upsert_queue(&q);
        }
    }
    state.scheduler.emit_snapshot_changed();
    Ok(profile)
}

#[tauri::command]
pub fn duplicate_profile(
    state: State<'_, AppState>,
    profile_id: String,
    new_name: String,
) -> Result<Profile, String> {
    let src = state
        .scheduler
        .db
        .list_profiles()?
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| "Profile not found".to_string())?;
    let now = Utc::now().timestamp();
    let copy = Profile {
        id: Uuid::new_v4().to_string(),
        name: new_name.trim().to_string(),
        created_at: now,
        updated_at: now,
        ..src
    };
    state.scheduler.db.save_profile(&copy)?;
    state.scheduler.emit_snapshot_changed();
    Ok(copy)
}

#[tauri::command]
pub fn delete_profile(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    state.scheduler.db.delete_profile(&profile_id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn select_profile_for_queue(
    state: State<'_, AppState>,
    queue_id: String,
    profile_id: String,
) -> Result<QueueTab, String> {
    let profile = state
        .scheduler
        .db
        .list_profiles()?
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| "Profile not found".to_string())?;
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    q.profile_id = Some(profile.id);
    q.profile_name = Some(profile.name);
    q.recipe = profile.recipe;
    q.priority = profile.priority;
    q.recursive_import = profile.recursive_import;
    q.destination_root = profile.destination_root;
    q.keep_awake = profile.keep_awake;
    q.error_policy = profile.error_policy;
    q.min_savings_enabled = profile.min_savings_enabled;
    q.min_savings_pct = profile.min_savings_pct;
    q.cleanup_policy = profile.cleanup_policy;
    q.full_decode_verify = profile.full_decode_verify;
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(q)
}

#[tauri::command]
pub async fn start_import_scan(
    state: State<'_, AppState>,
    queue_id: String,
    paths: Vec<String>,
    recursive: Option<bool>,
    destination_root_override: Option<String>,
) -> Result<ImportScanResult, String> {
    // Capture queue & settings snapshot immediately at drop/import time!
    let queue = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let existing_jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let ffprobe_path = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));
    let rec = recursive.unwrap_or(queue.recursive_import);
    let scan_cancel = Arc::new(AtomicBool::new(false));
    let temp_key = Uuid::new_v4().to_string();
    if let Ok(mut map) = state.active_scans.lock() {
        map.insert(temp_key.clone(), scan_cancel.clone());
    }

    let path_bufs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let res = tokio::task::spawn_blocking(move || {
        scan_import_paths(
            ffprobe_path.as_deref(),
            &queue,
            &existing_jobs,
            &path_bufs,
            rec,
            destination_root_override.as_deref(),
            &scan_cancel,
        )
    })
    .await
    .map_err(|e| format!("Scan task failed: {e}"))?;

    if let Ok(mut map) = state.active_scans.lock() {
        map.remove(&temp_key);
    }
    Ok(res)
}

#[tauri::command]
pub fn cancel_import_scan(state: State<'_, AppState>) -> Result<(), String> {
    if let Ok(map) = state.active_scans.lock() {
        for flag in map.values() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
pub struct CommitImportItem {
    pub source_path: String,
    pub source_rel_path: String,
    pub source_root: Option<String>,
    pub destination_path: String,
    pub input_bytes: u64,
    pub probe: Option<ProbeResult>,
    pub plan: Option<TrackPlan>,
    pub unreadable_error: Option<String>,
}

#[tauri::command]
pub fn commit_staged_import(
    state: State<'_, AppState>,
    queue_id: String,
    destination_root: String,
    mut recipe: EncodeRecipe,
    items: Vec<CommitImportItem>,
) -> Result<Vec<JobRecord>, String> {
    if recipe.family == PresetFamily::Archive {
        let (min_crf, max_crf) = recipe.codec.crf_range();
        recipe.crf = recipe.crf.clamp(min_crf, max_crf);
    }
    let mut queue = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    if queue.destination_root.is_none() {
        queue.destination_root = Some(destination_root.clone());
        state.scheduler.db.upsert_queue(&queue)?;
    }

    let all_jobs = state.scheduler.db.list_all_jobs()?;
    let mut reserved: HashSet<String> = all_jobs
        .iter()
        .map(|j| j.destination_path.to_ascii_lowercase())
        .collect();
    let mut next_pos = state.scheduler.db.next_job_position(&queue_id)?;
    let batch_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let dest_root_path = PathBuf::from(&destination_root);
    let mut added = Vec::new();

    for it in items {
        let src_path = PathBuf::from(&it.source_path);
        let fp = capture_file_identity(&src_path).ok();
        let final_dest = resolve_destination_path(
            &src_path,
            Path::new(&it.source_rel_path),
            &dest_root_path,
            &recipe,
            &mut reserved,
        );
        let dest_vol_id = disk_space_for_path(final_dest.parent().unwrap_or(&dest_root_path))
            .ok()
            .map(|v| v.volume_id);

        let recomputed_plan = it
            .probe
            .as_ref()
            .map(|pr| build_track_plan(&src_path, pr, &recipe, &CompatOverrides::default()))
            .or(it.plan);

        let (status, error_stage, error_message) = if let Some(err) = it.unreadable_error {
            (
                JobStatus::NeedsAttention,
                Some("probe".to_string()),
                Some(err),
            )
        } else if let Some(ref pl) = recomputed_plan {
            if pl.is_ready {
                (JobStatus::Ready, None, None)
            } else {
                (
                    JobStatus::NeedsAttention,
                    Some("planning".to_string()),
                    pl.issues.first().map(|i| i.summary.clone()),
                )
            }
        } else {
            (
                JobStatus::NeedsAttention,
                Some("probe".to_string()),
                Some("Media not probed yet".to_string()),
            )
        };

        let job = JobRecord {
            id: Uuid::new_v4().to_string(),
            queue_id: queue_id.clone(),
            batch_id: batch_id.clone(),
            position: next_pos,
            source_path: it.source_path,
            source_rel_path: it.source_rel_path,
            source_root: it.source_root,
            source_fingerprint: fp,
            destination_path: final_dest.to_string_lossy().to_string(),
            dest_volume_id: dest_vol_id,
            recipe: recipe.clone(),
            compat_overrides: CompatOverrides::default(),
            status,
            status_reason: None,
            probe: it.probe,
            plan: recomputed_plan,
            input_bytes: it.input_bytes,
            output_bytes: None,
            output_fingerprint: None,
            error_stage,
            error_message,
            diagnostics: None,
            cleanup_policy: queue.cleanup_policy,
            cleanup_outcome: CleanupOutcome::None,
            cleanup_error: None,
            consent_id: None,
            full_decode_verified: false,
            min_savings_enabled: queue.min_savings_enabled,
            min_savings_pct: queue.min_savings_pct,
            tool_versions: None,
            current_attempt_id: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        next_pos += 1;
        state.scheduler.db.upsert_job(&job)?;
        added.push(job);
    }

    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
    Ok(added)
}

#[derive(Debug, serde::Deserialize)]
pub struct PendingJobPatch {
    pub recipe: Option<EncodeRecipe>,
    pub compat_overrides: Option<CompatOverrides>,
    pub destination_path: Option<String>,
}

#[tauri::command]
pub fn update_pending_job(
    state: State<'_, AppState>,
    job_id: String,
    patch: PendingJobPatch,
) -> Result<JobRecord, String> {
    let mut job = state
        .scheduler
        .db
        .get_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    if !job.status.is_pending_editable() {
        return Err("Only pending or needs-attention jobs can be modified.".to_string());
    }

    if let Some(mut r) = patch.recipe {
        if r.family == PresetFamily::Archive {
            let (min_crf, max_crf) = r.codec.crf_range();
            r.crf = r.crf.clamp(min_crf, max_crf);
        }
        // Update destination extension/tag if recipe changed and user didn't pass custom destination_path
        if patch.destination_path.is_none() && r != job.recipe {
            let mut reserved: HashSet<String> = state
                .scheduler
                .db
                .list_all_jobs()?
                .into_iter()
                .filter(|j| j.id != job.id)
                .map(|j| j.destination_path.to_ascii_lowercase())
                .collect();
            let q_root = state
                .scheduler
                .db
                .get_queue(&job.queue_id)?
                .and_then(|q| q.destination_root)
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    derive_destination_root_from_job(
                        Path::new(&job.destination_path),
                        Path::new(&job.source_rel_path),
                    )
                });
            let new_dest = resolve_destination_path(
                Path::new(&job.source_path),
                Path::new(&job.source_rel_path),
                &q_root,
                &r,
                &mut reserved,
            );
            job.destination_path = new_dest.to_string_lossy().to_string();
        }
        job.recipe = r;
        job.consent_id = None;
    }
    if let Some(ov) = patch.compat_overrides {
        job.compat_overrides = ov;
    }
    if let Some(dp) = patch.destination_path {
        let p = PathBuf::from(&dp);
        if p == Path::new(&job.source_path) {
            return Err("Destination cannot be the same as source file.".to_string());
        }
        job.destination_path = dp;
    }

    if let Some(parent) = Path::new(&job.destination_path).parent() {
        if let Ok(vs) = disk_space_for_path(parent) {
            job.dest_volume_id = Some(vs.volume_id);
        }
    }

    // Re-run preflight
    let ffprobe_opt = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));
    if let Some(fp_bin) = ffprobe_opt {
        match probe_file(&fp_bin, Path::new(&job.source_path)) {
            Ok(probe) => {
                let plan = build_track_plan(
                    Path::new(&job.source_path),
                    &probe,
                    &job.recipe,
                    &job.compat_overrides,
                );
                job.status = if plan.is_ready {
                    JobStatus::Ready
                } else {
                    JobStatus::NeedsAttention
                };
                job.error_stage = None;
                job.error_message = plan.issues.first().map(|i| i.summary.clone());
                job.probe = Some(probe);
                job.plan = Some(plan);
            }
            Err(e) => {
                job.status = JobStatus::NeedsAttention;
                job.error_stage = Some("preflight".to_string());
                job.error_message = Some(e);
            }
        }
    }

    state.scheduler.previews.mark_sample_stale_if_matching(&job.id);
    job.updated_at = Utc::now().timestamp();
    state.scheduler.db.upsert_job(&job)?;
    state.scheduler.emit_snapshot_changed();
    Ok(job)
}

#[tauri::command]
pub fn reorder_jobs(
    state: State<'_, AppState>,
    queue_id: String,
    ordered_job_ids: Vec<String>,
) -> Result<(), String> {
    state.scheduler.db.reorder_jobs(&queue_id, &ordered_job_ids)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn remove_pending_jobs(state: State<'_, AppState>, job_ids: Vec<String>) -> Result<(), String> {
    for jid in job_ids {
        if let Some(job) = state.scheduler.db.get_job(&jid)? {
            if !matches!(
                job.status,
                JobStatus::Encoding | JobStatus::Verifying | JobStatus::Publishing
            ) {
                state.scheduler.db.delete_job(&jid)?;
            }
        }
    }
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct StartQueueResponse {
    pub started: bool,
    pub requires_cleanup_consent: bool,
    pub consent_batch_id: Option<String>,
    pub consent_jobs: Vec<JobRecord>,
    pub consent_total_bytes: u64,
    pub destination_root: Option<String>,
}

#[tauri::command]
pub fn start_queue(
    state: State<'_, AppState>,
    queue_id: String,
    consent_confirmed: Option<bool>,
) -> Result<StartQueueResponse, String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;

    let ready_jobs: Vec<JobRecord> = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued))
        .cloned()
        .collect();

    if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile && !ready_jobs.is_empty() {
        let unconsented: Vec<JobRecord> = ready_jobs
            .iter()
            .filter(|j| j.consent_id.is_none())
            .cloned()
            .collect();
        if !unconsented.is_empty() && consent_confirmed != Some(true) {
            let total_bytes = unconsented.iter().map(|j| j.input_bytes).sum();
            return Ok(StartQueueResponse {
                started: false,
                requires_cleanup_consent: true,
                consent_batch_id: Some(Uuid::new_v4().to_string()),
                consent_jobs: unconsented,
                consent_total_bytes: total_bytes,
                destination_root: q.destination_root.clone(),
            });
        }
        if !unconsented.is_empty() && consent_confirmed == Some(true) {
            let batch_id = Uuid::new_v4().to_string();
            let ids: Vec<String> = unconsented.iter().map(|j| j.id.clone()).collect();
            let _ = record_batch_cleanup_consent(&state.scheduler.db, &queue_id, &batch_id, &ids)?;
        }
    }

    q.is_running = true;
    q.is_paused = false;
    q.pause_after_current = false;
    q.waiting_reason = None;
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();

    Ok(StartQueueResponse {
        started: true,
        requires_cleanup_consent: false,
        consent_batch_id: None,
        consent_jobs: Vec::new(),
        consent_total_bytes: 0,
        destination_root: q.destination_root,
    })
}

#[tauri::command]
pub fn pause_after_current(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let has_active_in_queue = state
        .scheduler
        .db
        .list_jobs_for_queue(&queue_id)?
        .iter()
        .any(|j| matches!(j.status, JobStatus::Encoding | JobStatus::Verifying));

    if has_active_in_queue {
        q.pause_after_current = true;
        q.waiting_reason = Some("Pausing after current file".to_string());
    } else {
        q.is_paused = true;
        q.pause_after_current = false;
        q.waiting_reason = Some("Paused by you".to_string());
    }
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn resume_queue(
    state: State<'_, AppState>,
    queue_id: String,
    consent_confirmed: Option<bool>,
) -> Result<StartQueueResponse, String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    // Re-queue Interrupted jobs in this queue automatically on Resume
    for j in state.scheduler.db.list_jobs_for_queue(&queue_id)? {
        if j.status == JobStatus::Interrupted {
            let _ = state.scheduler.retry_job_with_fresh_preflight(&j.id);
        }
    }
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let ready_jobs: Vec<JobRecord> = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued))
        .cloned()
        .collect();
    if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile && !ready_jobs.is_empty() {
        let unconsented: Vec<JobRecord> = ready_jobs
            .iter()
            .filter(|j| j.consent_id.is_none())
            .cloned()
            .collect();
        if !unconsented.is_empty() && consent_confirmed != Some(true) {
            let total_bytes = unconsented.iter().map(|j| j.input_bytes).sum();
            return Ok(StartQueueResponse {
                started: false,
                requires_cleanup_consent: true,
                consent_batch_id: Some(Uuid::new_v4().to_string()),
                consent_jobs: unconsented,
                consent_total_bytes: total_bytes,
                destination_root: q.destination_root.clone(),
            });
        }
        if !unconsented.is_empty() && consent_confirmed == Some(true) {
            let batch_id = Uuid::new_v4().to_string();
            let ids: Vec<String> = unconsented.iter().map(|j| j.id.clone()).collect();
            let _ = record_batch_cleanup_consent(&state.scheduler.db, &queue_id, &batch_id, &ids)?;
        }
    }
    q.is_running = true;
    q.is_paused = false;
    q.pause_after_current = false;
    q.waiting_reason = None;
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
    Ok(StartQueueResponse {
        started: true,
        requires_cleanup_consent: false,
        consent_batch_id: None,
        consent_jobs: Vec::new(),
        consent_total_bytes: 0,
        destination_root: q.destination_root,
    })
}

#[tauri::command]
pub fn cancel_current_file(state: State<'_, AppState>, job_id: Option<String>) -> Result<(), String> {
    if let Ok(cf) = state.scheduler.active_cancel_flag.lock() {
        if let Some((active_id, flag)) = cf.as_ref() {
            if job_id.as_ref().map(|id| id == active_id).unwrap_or(true) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn retry_jobs(state: State<'_, AppState>, job_ids: Vec<String>) -> Result<usize, String> {
    let mut count = 0usize;
    let mut errors = Vec::new();
    for id in job_ids {
        match state.scheduler.retry_job_with_fresh_preflight(&id) {
            Ok(_) => count += 1,
            Err(e) => errors.push(e),
        }
    }
    if count == 0 && !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(count)
}

#[tauri::command]
pub fn reveal_output(path: String) -> Result<(), String> {
    reveal_in_file_manager(Path::new(&path))
}

#[tauri::command]
pub fn open_output_player(path: String) -> Result<(), String> {
    open_in_default_player(Path::new(&path))
}

#[tauri::command]
pub fn set_volume_reserve(
    state: State<'_, AppState>,
    volume_id: String,
    mount_hint: String,
    reserve_bytes: u64,
) -> Result<(), String> {
    state
        .scheduler
        .db
        .set_volume_reserve(&volume_id, &mount_hint, reserve_bytes)?;
    state.scheduler.recheck_volumes_and_resume();
    Ok(())
}

#[tauri::command]
pub fn recheck_volumes(state: State<'_, AppState>) -> Result<(), String> {
    state.scheduler.recheck_volumes_and_resume();
    Ok(())
}

#[tauri::command]
pub fn get_cleanup_candidates(
    state: State<'_, AppState>,
    queue_id: String,
) -> Result<Vec<CleanupCandidate>, String> {
    build_cleanup_candidates(&state.scheduler.db, &queue_id)
}

#[tauri::command]
pub fn trash_selected_originals(
    state: State<'_, AppState>,
    job_ids: Vec<String>,
) -> Result<Vec<(String, Result<String, String>)>, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let ffmpeg_opt = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffmpeg_path)));

    let mut results = Vec::new();
    for jid in job_ids {
        let res = execute_cleanup_for_job(&state.scheduler.db, ffmpeg_opt.as_deref(), &jid, false)
            .map(|_| "Moved to Trash".to_string());
        results.push((jid, res));
    }
    state.scheduler.emit_snapshot_changed();
    Ok(results)
}

#[tauri::command]
pub fn request_sample_preview(
    state: State<'_, AppState>,
    job_id: String,
    start_secs: Option<f64>,
) -> Result<SamplePreviewState, String> {
    let job = state
        .scheduler
        .db
        .get_job(&job_id)?
        .ok_or_else(|| "Job not found".to_string())?;
    let active = state
        .scheduler
        .active_cancel_flag
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(id, _)| id.clone()));

    let waiting_reason = active.map(|_| "Waiting for current encode to finish (shares single encoder)".to_string());
    let pending_state = SamplePreviewState {
        job_id: job.id.clone(),
        start_secs: start_secs.unwrap_or(0.0),
        duration_secs: 15.0,
        status: if waiting_reason.is_some() {
            "queued".to_string()
        } else {
            "encoding".to_string()
        },
        progress_pct: 0.0,
        measured_bytes: None,
        elapsed_secs: None,
        projected_bytes: None,
        projected_secs: None,
        stale: false,
        original_sample_path: None,
        encoded_sample_path: None,
        recipe_snapshot: job.recipe.clone(),
        error: None,
        waiting_reason,
    };

    if let Ok(mut st) = state.scheduler.previews.sample_state.lock() {
        *st = Some(pending_state.clone());
    }
    if let Ok(mut ps) = state.scheduler.pending_sample.lock() {
        *ps = Some(PendingSampleRequest { job_id, start_secs });
    }
    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
    Ok(pending_state)
}

#[tauri::command]
pub fn get_comparison_frame(
    state: State<'_, AppState>,
    media_path: String,
    timestamp_secs: f64,
    full_resolution: bool,
) -> Result<Response, String> {
    let ffmpeg_path = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffmpeg_path)))
        .ok_or_else(|| "ffmpeg is not configured".to_string())?;
    // Ensure media_path is either inside previews dir or an active job's source/destination
    let target = Path::new(&media_path);
    let canon = target.canonicalize().map_err(|e| e.to_string())?;
    let preview_canon = state
        .scheduler
        .previews
        .preview_dir
        .canonicalize()
        .unwrap_or_else(|_| state.scheduler.previews.preview_dir.clone());
    if !canon.starts_with(&preview_canon) {
        let allowed = state
            .scheduler
            .db
            .list_all_jobs()?
            .iter()
            .any(|j| Path::new(&j.source_path) == target || Path::new(&j.destination_path) == target);
        if !allowed {
            return Err("Path is not an authorized preview or job media path.".to_string());
        }
    }
    let bytes = state.scheduler.previews.decode_comparison_frame(
        &ffmpeg_path,
        &canon,
        timestamp_secs,
        full_resolution,
    )?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub fn start_live_preview(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<LivePreviewInfo, String> {
    let ffmpeg_path = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffmpeg_path)))
        .ok_or_else(|| "ffmpeg is not configured".to_string())?;
    let cur_t = state
        .scheduler
        .active_progress
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.out_time_secs))
        .unwrap_or(0.0);
    state
        .scheduler
        .previews
        .start_live_preview(ffmpeg_path, job_id, cur_t);
    state
        .scheduler
        .previews
        .live_info
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Live preview unavailable".to_string())
}

#[tauri::command]
pub fn stop_live_preview(state: State<'_, AppState>) -> Result<(), String> {
    state.scheduler.previews.stop_live_preview();
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn get_live_preview_frame(state: State<'_, AppState>) -> Result<Response, String> {
    let guard = state
        .scheduler
        .previews
        .latest_live_jpeg
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some((_, _, ref bytes)) = *guard {
        Ok(Response::new(bytes.clone()))
    } else {
        Ok(Response::new(Vec::new()))
    }
}

#[tauri::command]
pub fn check_close_status(state: State<'_, AppState>) -> Result<bool, String> {
    let has_active = state
        .scheduler
        .active_cancel_flag
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    let any_running = state
        .scheduler
        .db
        .list_queues()?
        .iter()
        .any(|q| q.is_running && !q.is_paused);
    Ok(has_active || any_running)
}

#[tauri::command]
pub fn stop_and_quit(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    if let Ok(cf) = state.scheduler.active_cancel_flag.lock() {
        if let Some((_, ref flag)) = *cf {
            flag.store(true, Ordering::Relaxed);
        }
    }
    for mut q in state.scheduler.db.list_queues()? {
        if q.is_running {
            q.is_running = false;
            q.is_paused = true;
            q.waiting_reason = Some("Stopped on quit".to_string());
            let _ = state.scheduler.db.upsert_queue(&q);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(250));
    app.exit(0);
    Ok(())
}
