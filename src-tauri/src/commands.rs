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
use crate::scanner::scan_import_paths_with_progress;
use crate::scheduler::{PendingSampleRequest, Scheduler};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{ipc::Response, Emitter, State};
use uuid::Uuid;

pub struct AppState {
    pub scheduler: Arc<Scheduler>,
    pub task_manager: Arc<crate::tasks::TaskManager>,
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
    tab_type: Option<TabType>,
) -> Result<QueueTab, String> {
    let queues = state.scheduler.db.list_queues()?;
    let next_order = queues.iter().map(|q| q.tab_order).max().unwrap_or(-1) + 1;
    let resolved_type = tab_type.unwrap_or(TabType::Encode);
    let tab_name = name.unwrap_or_else(|| match resolved_type {
        TabType::Explore => format!("Workspace {}", queues.len() + 1),
        TabType::Organize => format!("Organize {}", queues.len() + 1),
        TabType::Repair => format!("Repair {}", queues.len() + 1),
        TabType::Encode => format!("Queue {}", queues.len() + 1),
    });
    let q = QueueTab {
        id: Uuid::new_v4().to_string(),
        name: tab_name,
        tab_order: next_order,
        tab_type: resolved_type,
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
    if let Ok(mut meta_map) = state.scheduler.db.get_workspace_json::<HashMap<String, TabWorkspaceMeta>>("ws_tab_meta") {
        meta_map.remove(&queue_id);
        let _ = state.scheduler.db.set_workspace_json("ws_tab_meta", &meta_map);
    }
    if let Ok(mut dedupe_map) = state.scheduler.db.get_workspace_json::<HashMap<String, Vec<TabDedupeGroup>>>("ws_tab_dedupe_groups") {
        dedupe_map.remove(&queue_id);
        let _ = state.scheduler.db.set_workspace_json("ws_tab_dedupe_groups", &dedupe_map);
    }
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn select_queue_tab(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    state.scheduler.db.set_setting("active_queue_id", &queue_id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn set_queue_tab_type(
    state: State<'_, AppState>,
    queue_id: String,
    tab_type: TabType,
) -> Result<QueueTab, String> {
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue tab not found".to_string())?;
    q.tab_type = tab_type;
    state.scheduler.db.upsert_queue(&q)?;
    state.scheduler.emit_snapshot_changed();
    Ok(q)
}

#[tauri::command]
pub fn update_repair_session(
    state: State<'_, AppState>,
    queue_id: String,
    input_path: String,
    reference_paths: Vec<String>,
    output_path: Option<String>,
    video_only: bool,
    deep_scan: bool,
    timeout_secs: u64,
) -> Result<RepairJobRecord, String> {
    let existing = state.scheduler.db.get_repair_session(&queue_id)?;
    let input_changed = existing
        .as_ref()
        .map(|e| e.input_path != input_path)
        .unwrap_or(true);
    let rec = RepairJobRecord {
        queue_id,
        input_path,
        reference_paths,
        output_path,
        video_only,
        deep_scan,
        timeout_secs: timeout_secs.clamp(10, 3600),
        status: if input_changed {
            "idle".to_string()
        } else {
            existing.as_ref().map(|e| e.status.clone()).unwrap_or_else(|| "idle".to_string())
        },
        diagnosis: if input_changed { None } else { existing.as_ref().and_then(|e| e.diagnosis.clone()) },
        report: if input_changed { None } else { existing.as_ref().and_then(|e| e.report.clone()) },
        progress: None,
        error: None,
        preview_sheet_filename: if input_changed {
            None
        } else {
            existing.and_then(|e| e.preview_sheet_filename)
        },
        updated_at: Utc::now().timestamp(),
    };
    state.scheduler.db.upsert_repair_session(&rec)?;
    state.scheduler.emit_snapshot_changed();
    Ok(rec)
}

#[tauri::command]
pub fn run_repair_diagnosis(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    let mut rec = state
        .scheduler
        .db
        .get_repair_session(&queue_id)?
        .ok_or_else(|| "Select a damaged video file first.".to_string())?;
    if rec.input_path.trim().is_empty() {
        return Err("Select a damaged video file first.".to_string());
    }
    rec.status = "diagnosing".to_string();
    rec.error = None;
    rec.updated_at = Utc::now().timestamp();
    state.scheduler.db.upsert_repair_session(&rec)?;
    state.scheduler.emit_snapshot_changed();

    let sched = state.scheduler.clone();
    std::thread::spawn(move || {
        let sched_ev = sched.clone();
        let qid_ev = queue_id.clone();
        let cb: crate::recovery::ProgressCallback = Arc::new(move |p| {
            sched_ev.emit_event(
                "repair_progress",
                serde_json::json!({ "queue_id": qid_ev, "progress": p }),
            );
        });
        let res = crate::recovery::diagnose_file(Path::new(&rec.input_path), Some(&cb));
        match res {
            Ok(diag) => {
                rec.diagnosis = Some(diag);
                rec.status = "diagnosed".to_string();
                rec.error = None;
            }
            Err(err) => {
                rec.status = "failed".to_string();
                rec.error = Some(err);
            }
        }
        rec.progress = None;
        rec.updated_at = Utc::now().timestamp();
        let _ = sched.db.upsert_repair_session(&rec);
        sched.emit_snapshot_changed();
    });
    Ok(())
}

#[tauri::command]
pub fn start_video_repair(state: State<'_, AppState>, queue_id: String) -> Result<(), String> {
    let mut rec = state
        .scheduler
        .db
        .get_repair_session(&queue_id)?
        .ok_or_else(|| "Select a damaged video file first.".to_string())?;
    if rec.input_path.trim().is_empty() {
        return Err("Select a damaged video file first.".to_string());
    }
    let ts = state.scheduler.refresh_tools(false);
    let caps = ts
        .capabilities
        .ok_or_else(|| "FFmpeg and ffprobe are required before running recovery.".to_string())?;
    let ffmpeg = PathBuf::from(&caps.ffmpeg_path);
    let ffprobe = PathBuf::from(&caps.ffprobe_path);

    let in_p = PathBuf::from(&rec.input_path);
    let out_p = rec
        .output_path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let stem = in_p.file_stem().and_then(|s| s.to_str()).unwrap_or("recovered");
            let parent = in_p.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{stem}_recovered.mp4"))
        });
    rec.output_path = Some(out_p.to_string_lossy().to_string());
    rec.status = "recovering".to_string();
    rec.error = None;
    rec.updated_at = Utc::now().timestamp();
    state.scheduler.db.upsert_repair_session(&rec)?;
    state.scheduler.emit_snapshot_changed();

    let opts = crate::recovery::RecoveryOptions {
        input_path: in_p,
        reference_paths: rec.reference_paths.iter().map(PathBuf::from).collect(),
        output_path: out_p,
        video_only: rec.video_only,
        deep_scan: rec.deep_scan,
        timeout_secs: rec.timeout_secs.max(30),
        generate_contact_sheet: true,
    };

    let sched = state.scheduler.clone();
    std::thread::spawn(move || {
        let sched_ev = sched.clone();
        let qid_ev = queue_id.clone();
        let cb: crate::recovery::ProgressCallback = Arc::new(move |p| {
            sched_ev.emit_event(
                "repair_progress",
                serde_json::json!({ "queue_id": qid_ev, "progress": p }),
            );
        });
        match crate::recovery::recover_file(&opts, &ffmpeg, &ffprobe, Some(&cb)) {
            Ok(report) => {
                rec.diagnosis = Some(report.diagnosis.clone());
                rec.status = if report.overall_status == "unrecoverable" {
                    "failed".to_string()
                } else {
                    report.overall_status.clone()
                };
                if let Some(ref sheet) = report.contact_sheet_path {
                    let preview_fname = format!("repair_{queue_id}.jpg");
                    let _ = std::fs::create_dir_all(&sched.previews.preview_dir);
                    let dst = sched.previews.preview_dir.join(&preview_fname);
                    if std::fs::copy(sheet, &dst).is_ok() {
                        rec.preview_sheet_filename = Some(preview_fname);
                    }
                }
                if report.overall_status != "unrecoverable" {
                    if let Some(ref recovered_out) = report.output_path {
                        let details = serde_json::json!({
                            "strategy": report.selected_strategy,
                            "overall_status": report.overall_status,
                            "segments": report.segments.len(),
                        });
                        let _ = crate::catalog::register_derived_file(
                            &sched.db,
                            &report.input_path,
                            Path::new(recovered_out),
                            "recovered_from",
                            details,
                            true,
                            Some(&ffprobe),
                        );
                    }
                }
                rec.report = Some(report);
                rec.error = None;
            }
            Err(err) => {
                rec.status = "failed".to_string();
                rec.error = Some(err);
            }
        }
        rec.progress = None;
        rec.updated_at = Utc::now().timestamp();
        let _ = sched.db.upsert_repair_session(&rec);
        sched.emit_snapshot_changed();
    });
    Ok(())
}

#[tauri::command]
pub fn export_repair_report(
    state: State<'_, AppState>,
    queue_id: String,
    dest_path: String,
    format: String,
) -> Result<(), String> {
    let rec = state
        .scheduler
        .db
        .get_repair_session(&queue_id)?
        .ok_or_else(|| "No repair session found for tab.".to_string())?;
    let report = rec
        .report
        .ok_or_else(|| "Run repair before exporting a full recovery report.".to_string())?;
    let content = if format == "json" {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        crate::recovery::format_recovery_human(&report)
    };
    std::fs::write(&dest_path, content).map_err(|e| format!("Failed writing report: {e}"))?;
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
                    let was_listed = job.status == JobStatus::Listed
                        || (job.status == JobStatus::NeedsAttention
                            && matches!(
                                job.error_stage.as_deref(),
                                Some("probe" | "planning" | "preflight") | None
                            ));
                    job.status = if plan.is_ready {
                        if was_listed {
                            JobStatus::Listed
                        } else {
                            JobStatus::Ready
                        }
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeStatsProgress {
    pub queue_id: String,
    pub done: usize,
    pub total: usize,
    pub current_file: String,
}

#[tauri::command]
pub async fn start_import_scan(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    queue_id: String,
    paths: Vec<String>,
    recursive: Option<bool>,
    destination_root_override: Option<String>,
    defer_probe: Option<bool>,
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
    let fast_defer = defer_probe.unwrap_or(queue.tab_type != TabType::Encode);
    let scan_cancel = Arc::new(AtomicBool::new(false));
    let temp_key = Uuid::new_v4().to_string();
    if let Ok(mut map) = state.active_scans.lock() {
        map.insert(temp_key.clone(), scan_cancel.clone());
    }

    let path_bufs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let app_handle = app.clone();
    let res = tokio::task::spawn_blocking(move || {
        scan_import_paths_with_progress(
            ffprobe_path.as_deref(),
            &queue,
            &existing_jobs,
            &path_bufs,
            rec,
            destination_root_override.as_deref(),
            &scan_cancel,
            fast_defer,
            move |prog| {
                let _ = app_handle.emit("import_scan_progress", prog);
            },
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
    app: tauri::AppHandle,
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

        let (status, status_reason, error_stage, error_message) = if let Some(err) = it.unreadable_error {
            (
                JobStatus::NeedsAttention,
                None,
                Some("probe".to_string()),
                Some(err),
            )
        } else if let Some(ref pl) = recomputed_plan {
            if pl.is_ready {
                (JobStatus::Listed, None, None, None)
            } else {
                (
                    JobStatus::NeedsAttention,
                    None,
                    Some("planning".to_string()),
                    pl.issues.first().map(|i| i.summary.clone()),
                )
            }
        } else {
            // Deferred probe: stays in Video List (Listed) while stats compute asynchronously
            (
                JobStatus::Listed,
                Some("Computing media stats…".to_string()),
                None,
                None,
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
            status_reason,
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

    if !added.is_empty() {
        let mut meta_map: HashMap<String, TabWorkspaceMeta> =
            state.scheduler.db.get_workspace_json("ws_tab_meta").unwrap_or_default();
        let entry = meta_map.entry(queue_id.clone()).or_default();
        if entry.resource_source.is_none() {
            let inferred_src = added
                .first()
                .and_then(|j| j.source_root.clone())
                .or_else(|| {
                    added.first().and_then(|j| {
                        Path::new(&j.source_path)
                            .parent()
                            .map(|p| p.to_string_lossy().to_string())
                    })
                });
            entry.resource_source = inferred_src;
            let _ = state.scheduler.db.set_workspace_json("ws_tab_meta", &meta_map);
        }
    }

    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();

    // Asynchronously probe any files that were loaded fast without stats yet
    let pending_stats_ids: Vec<String> = added
        .iter()
        .filter(|j| j.probe.is_none() && j.error_message.is_none())
        .map(|j| j.id.clone())
        .collect();
    if !pending_stats_ids.is_empty() {
        let sched = state.scheduler.clone();
        let fp_opt = sched
            .tool_status
            .lock()
            .ok()
            .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));
        let app_bg = app.clone();
        let qid_bg = queue_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let total = pending_stats_ids.len();
            for (idx, jid) in pending_stats_ids.iter().enumerate() {
                let Ok(Some(mut job)) = sched.db.get_job(jid) else {
                    continue;
                };
                let src = PathBuf::from(&job.source_path);
                let fname = src
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| job.source_rel_path.clone());
                let _ = app_bg.emit(
                    "probe_stats_progress",
                    ProbeStatsProgress {
                        queue_id: qid_bg.clone(),
                        done: idx,
                        total,
                        current_file: fname.clone(),
                    },
                );
                if let Some(ref fp_bin) = fp_opt {
                    match probe_file(fp_bin, &src) {
                        Ok(pr) => {
                            let pl = build_track_plan(
                                &src,
                                &pr,
                                &job.recipe,
                                &job.compat_overrides,
                            );
                            job.status = if pl.is_ready {
                                JobStatus::Listed
                            } else {
                                JobStatus::NeedsAttention
                            };
                            if !pl.is_ready {
                                job.error_stage = Some("planning".to_string());
                                job.error_message = pl.issues.first().map(|i| i.summary.clone());
                            } else {
                                job.error_stage = None;
                                job.error_message = None;
                            }
                            job.probe = Some(pr);
                            job.plan = Some(pl);
                            job.status_reason = None;
                        }
                        Err(err) => {
                            job.status = JobStatus::NeedsAttention;
                            job.status_reason = None;
                            job.error_stage = Some("probe".to_string());
                            job.error_message = Some(err);
                        }
                    }
                } else {
                    job.status = JobStatus::NeedsAttention;
                    job.status_reason = None;
                    job.error_stage = Some("probe".to_string());
                    job.error_message = Some("ffprobe not configured yet.".to_string());
                }
                job.updated_at = Utc::now().timestamp();
                let _ = sched.db.upsert_job(&job);
                if (idx + 1) % 3 == 0 || idx + 1 == total {
                    sched.emit_snapshot_changed();
                }
            }
            let _ = app_bg.emit(
                "probe_stats_progress",
                ProbeStatsProgress {
                    queue_id: qid_bg,
                    done: total,
                    total,
                    current_file: String::new(),
                },
            );
            sched.wake_scheduler();
        });
    }

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
                let was_listed = job.status == JobStatus::Listed
                    || (job.status == JobStatus::NeedsAttention
                        && matches!(
                            job.error_stage.as_deref(),
                            Some("probe" | "planning" | "preflight") | None
                        ));
                job.status = if plan.is_ready {
                    if was_listed {
                        JobStatus::Listed
                    } else {
                        JobStatus::Ready
                    }
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
    selected_job_ids: Option<Vec<String>>,
) -> Result<StartQueueResponse, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Queue not found".to_string())?;
    let mut jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;

    if let Some(sel_ids) = selected_job_ids {
        let sel_set: HashSet<String> = sel_ids.into_iter().collect();
        let mut changed = false;
        let mut next_pos = state.scheduler.db.next_job_position(&queue_id)?;
        for j in &mut jobs {
            if sel_set.contains(&j.id)
                && matches!(
                    j.status,
                    JobStatus::Listed | JobStatus::Canceled | JobStatus::Failed | JobStatus::Interrupted
                )
                && j.plan.as_ref().map(|p| p.is_ready).unwrap_or(false)
            {
                j.status = JobStatus::Ready;
                j.status_reason = None;
                j.error_stage = None;
                j.error_message = None;
                j.position = next_pos;
                next_pos += 1;
                j.updated_at = Utc::now().timestamp();
                state.scheduler.db.upsert_job(j)?;
                changed = true;
            }
        }
        if changed {
            state.scheduler.emit_snapshot_changed();
        }
        let selected_ready: Vec<JobRecord> = jobs
            .iter()
            .filter(|j| {
                sel_set.contains(&j.id)
                    && matches!(j.status, JobStatus::Ready | JobStatus::Queued)
            })
            .cloned()
            .collect();
        if selected_ready.is_empty() {
            return Err("No checked ready files selected to encode.".to_string());
        }
    }

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

/// Add ONLY the explicitly selected video items from a tab's Video List to the Encode Queue,
/// and immediately ensure the ongoing Queue processor starts running them.
/// Unselected items stay untouched in the Video List (`JobStatus::Listed`).
#[tauri::command]
pub fn enqueue_jobs(
    state: State<'_, AppState>,
    queue_id: String,
    job_ids: Vec<String>,
    start_now: Option<bool>,
    consent_confirmed: Option<bool>,
) -> Result<StartQueueResponse, String> {
    let _ = start_now; // Queue is always an ongoing process; adding items starts processing automatically.
    let _guard = state.scheduler.cleanup_lock.lock();
    let mut q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Tab not found".to_string())?;
    let mut jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let sel_set: HashSet<String> = job_ids.iter().cloned().collect();
    if sel_set.is_empty() {
        return Err("Select at least one video to add to the Queue.".to_string());
    }

    // Reject cross-tab IDs cleanly
    for jid in &job_ids {
        if !jobs.iter().any(|j| &j.id == jid) {
            return Err(format!("Selected item '{jid}' does not belong to this tab."));
        }
    }

    let candidate_ids: Vec<String> = job_ids
        .iter()
        .filter(|jid| {
            jobs.iter().any(|j| {
                &j.id == *jid
                    && matches!(
                        j.status,
                        JobStatus::Listed
                            | JobStatus::Canceled
                            | JobStatus::Failed
                            | JobStatus::Interrupted
                            | JobStatus::BelowSavingsTarget
                    )
                    && j.plan.as_ref().map(|p| p.is_ready).unwrap_or(false)
            })
        })
        .cloned()
        .collect();

    if candidate_ids.is_empty()
        && !jobs
            .iter()
            .any(|j| sel_set.contains(&j.id) && matches!(j.status, JobStatus::Ready | JobStatus::Queued))
    {
        return Err("No checked ready videos could be added to the Queue.".to_string());
    }

    // Check TrashAfterEachFile batch consent BEFORE moving Listed items into the Queue so
    // dismissing the prompt leaves them unselected in the Video List.
    if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile && consent_confirmed != Some(true) {
        let mut unconsented: Vec<JobRecord> = jobs
            .iter()
            .filter(|j| {
                (candidate_ids.contains(&j.id)
                    || matches!(j.status, JobStatus::Ready | JobStatus::Queued))
                    && j.consent_id.is_none()
            })
            .cloned()
            .collect();
        if !unconsented.is_empty() {
            // Preserve selection order
            unconsented.sort_by_key(|j| {
                job_ids
                    .iter()
                    .position(|id| id == &j.id)
                    .unwrap_or(usize::MAX)
            });
            let total_bytes = unconsented.iter().map(|j| j.input_bytes).sum();
            return Ok(StartQueueResponse {
                started: false,
                requires_cleanup_consent: true,
                consent_batch_id: Some(Uuid::new_v4().to_string()),
                consent_jobs: unconsented,
                consent_total_bytes: total_bytes,
                destination_root: q.destination_root,
            });
        }
    }

    let mut next_pos = state.scheduler.db.next_job_position(&queue_id)?;
    let now = Utc::now().timestamp();

    // Preserve explicit selection order when appending to Queue
    for jid in &candidate_ids {
        let Some(j) = jobs.iter_mut().find(|x| &x.id == jid) else {
            continue;
        };
        j.status = JobStatus::Ready;
        j.status_reason = None;
        j.error_stage = None;
        j.error_message = None;
        j.output_bytes = None;
        j.output_fingerprint = None;
        j.consent_id = None;
        j.position = next_pos;
        next_pos += 1;
        j.updated_at = now;
        state.scheduler.db.upsert_job(j)?;
    }

    if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile && consent_confirmed == Some(true) {
        let unconsented_ids: Vec<String> = jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued) && j.consent_id.is_none())
            .map(|j| j.id.clone())
            .collect();
        if !unconsented_ids.is_empty() {
            let batch_id = Uuid::new_v4().to_string();
            let _ = record_batch_cleanup_consent(
                &state.scheduler.db,
                &queue_id,
                &batch_id,
                &unconsented_ids,
            )?;
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

/// Remove items from the Encode Queue back to their tab's Video List (`JobStatus::Listed`)
/// without deleting the source video or completed output on disk.
#[tauri::command]
pub fn dequeue_jobs(state: State<'_, AppState>, job_ids: Vec<String>) -> Result<usize, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let now = Utc::now().timestamp();
    let mut touched_queues = HashSet::new();
    let mut removed = 0usize;

    for jid in job_ids {
        let Some(mut job) = state.scheduler.db.get_job(&jid)? else {
            continue;
        };
        if matches!(
            job.status,
            JobStatus::Probing | JobStatus::Encoding | JobStatus::Verifying | JobStatus::Publishing
        ) {
            return Err(format!(
                "Cannot remove '{}' while actively processing. Cancel it first.",
                job.source_rel_path
            ));
        }
        touched_queues.insert(job.queue_id.clone());
        let is_preflight_issue = job.status == JobStatus::NeedsAttention
            && matches!(
                job.error_stage.as_deref(),
                Some("probe" | "planning" | "preflight") | None
            );
        if is_preflight_issue {
            continue;
        }
        job.status = JobStatus::Listed;
        job.status_reason = None;
        job.error_stage = None;
        job.error_message = None;
        job.consent_id = None;
        job.output_bytes = None;
        job.output_fingerprint = None;
        job.full_decode_verified = false;
        job.cleanup_outcome = CleanupOutcome::None;
        job.cleanup_error = None;
        job.updated_at = now;
        state.scheduler.db.upsert_job(&job)?;
        removed += 1;
    }

    for qid in touched_queues {
        if let Ok(Some(mut q)) = state.scheduler.db.get_queue(&qid) {
            let still_ready = state
                .scheduler
                .db
                .list_jobs_for_queue(&qid)?
                .iter()
                .any(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued));
            if !still_ready && q.is_running {
                q.is_running = false;
                q.pause_after_current = false;
                q.waiting_reason = None;
                state.scheduler.db.upsert_queue(&q)?;
            }
        }
    }

    state.scheduler.emit_snapshot_changed();
    Ok(removed)
}

/// Resume processing for tabs that are paused with waiting/interrupted items in the Encode Queue.
#[tauri::command]
pub fn start_all_ready_queues(
    state: State<'_, AppState>,
    consent_confirmed: Option<bool>,
) -> Result<StartQueueResponse, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let queues = state.scheduler.db.list_queues()?;

    // Re-queue Interrupted items on paused Encode tabs first so Resume continues cleanly
    for q in &queues {
        if q.tab_type == TabType::Encode && q.is_paused {
            for j in state.scheduler.db.list_jobs_for_queue(&q.id)? {
                if j.status == JobStatus::Interrupted {
                    let _ = state.scheduler.retry_job_with_fresh_preflight(&j.id);
                }
            }
        }
    }

    let all_jobs = state.scheduler.db.list_all_jobs()?;

    let mut candidate_queues: Vec<(QueueTab, Vec<JobRecord>)> = Vec::new();
    for q in queues {
        if q.tab_type != TabType::Encode {
            continue;
        }
        let q_ready: Vec<JobRecord> = all_jobs
            .iter()
            .filter(|j| {
                j.queue_id == q.id
                    && matches!(j.status, JobStatus::Ready | JobStatus::Queued)
                    && j.plan.as_ref().map(|p| p.is_ready).unwrap_or(false)
            })
            .cloned()
            .collect();
        if !q_ready.is_empty() {
            candidate_queues.push((q, q_ready));
        }
    }

    if candidate_queues.is_empty() {
        return Err("No items waiting in the Encode Queue.".to_string());
    }

    let mut all_unconsented = Vec::new();
    for (q, q_ready) in &candidate_queues {
        if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile {
            for j in q_ready {
                if j.consent_id.is_none() {
                    all_unconsented.push(j.clone());
                }
            }
        }
    }
    if !all_unconsented.is_empty() && consent_confirmed != Some(true) {
        let total_bytes = all_unconsented.iter().map(|j| j.input_bytes).sum();
        return Ok(StartQueueResponse {
            started: false,
            requires_cleanup_consent: true,
            consent_batch_id: Some(Uuid::new_v4().to_string()),
            consent_jobs: all_unconsented,
            consent_total_bytes: total_bytes,
            destination_root: candidate_queues
                .first()
                .and_then(|(q, _)| q.destination_root.clone()),
        });
    }

    for (mut q, q_ready) in candidate_queues {
        if q.cleanup_policy == CleanupPolicy::TrashAfterEachFile && consent_confirmed == Some(true) {
            let ids: Vec<String> = q_ready
                .iter()
                .filter(|j| j.consent_id.is_none())
                .map(|j| j.id.clone())
                .collect();
            if !ids.is_empty() {
                let batch_id = Uuid::new_v4().to_string();
                let _ = record_batch_cleanup_consent(&state.scheduler.db, &q.id, &batch_id, &ids)?;
            }
        }
        q.is_running = true;
        q.is_paused = false;
        q.pause_after_current = false;
        q.waiting_reason = None;
        state.scheduler.db.upsert_queue(&q)?;
    }

    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
    Ok(StartQueueResponse {
        started: true,
        requires_cleanup_consent: false,
        consent_batch_id: None,
        consent_jobs: Vec::new(),
        consent_total_bytes: 0,
        destination_root: None,
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
    let _guard = state.scheduler.cleanup_lock.lock();
    let mut count = 0usize;
    let mut errors = Vec::new();
    let mut touched_queues = HashSet::new();
    for id in job_ids {
        match state.scheduler.retry_job_with_fresh_preflight(&id) {
            Ok(job) => {
                if job.status == JobStatus::Ready {
                    touched_queues.insert(job.queue_id);
                }
                count += 1;
            }
            Err(e) => errors.push(e),
        }
    }
    for qid in touched_queues {
        if let Ok(Some(mut q)) = state.scheduler.db.get_queue(&qid) {
            let needs_consent = q.cleanup_policy == CleanupPolicy::TrashAfterEachFile
                && state
                    .scheduler
                    .db
                    .list_jobs_for_queue(&qid)
                    .map(|js| {
                        js.iter()
                            .any(|j| matches!(j.status, JobStatus::Ready | JobStatus::Queued) && j.consent_id.is_none())
                    })
                    .unwrap_or(false);
            if !needs_consent {
                q.is_running = true;
                q.is_paused = false;
                q.pause_after_current = false;
                q.waiting_reason = None;
                let _ = state.scheduler.db.upsert_queue(&q);
            }
        }
    }
    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
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
        let Some(job) = state.scheduler.db.get_job(&jid)? else {
            results.push((jid, Err("Job record not found.".to_string())));
            continue;
        };
        let src_p = Path::new(&job.source_path);
        match crate::cleanup::is_file_protected(&state.scheduler.db, Some(src_p), job.source_fingerprint.as_ref()) {
            Ok(true) => {
                results.push((
                    jid,
                    Err("Protected source ★ — original is locked against cleanup across tabs.".to_string()),
                ));
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                results.push((jid, Err(format!("Cannot verify protection state ({e}); kept safely."))));
                continue;
            }
        }
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

// ============================================================================
// First-Class Resource Tabs: Decisions, Forwarding, Tab Dedupe & Tab Repair
// ============================================================================

#[tauri::command]
pub fn set_tab_workspace_meta(
    state: State<'_, AppState>,
    queue_id: String,
    meta: TabWorkspaceMeta,
) -> Result<TabWorkspaceMeta, String> {
    let mut map: HashMap<String, TabWorkspaceMeta> =
        state.scheduler.db.get_workspace_json("ws_tab_meta")?;
    map.insert(queue_id, meta.clone());
    state.scheduler.db.set_workspace_json("ws_tab_meta", &map)?;
    state.scheduler.emit_snapshot_changed();
    Ok(meta)
}

#[tauri::command]
pub fn set_job_decision(
    state: State<'_, AppState>,
    job_id: String,
    decision: String,
) -> Result<(), String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let job = state
        .scheduler
        .db
        .get_job(&job_id)?
        .ok_or_else(|| "Resource item not found".to_string())?;
    let mut dec_map: HashMap<String, String> =
        state.scheduler.db.get_workspace_json_strict("ws_job_decisions")?;
    if decision == "unreviewed" {
        dec_map.remove(&job_id);
    } else {
        dec_map.insert(job_id.clone(), decision.clone());
    }
    state
        .scheduler
        .db
        .set_workspace_json("ws_job_decisions", &dec_map)?;

    crate::cleanup::sync_protection_entry(
        &state.scheduler.db,
        Path::new(&job.source_path),
        decision == "protected",
        Some(&job_id),
    )?;
    let _ = state.scheduler.db.invalidate_reviewed_plans_for_queue(&job.queue_id);
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn forward_resources_to_tab(
    state: State<'_, AppState>,
    from_queue_id: String,
    target_queue_id: Option<String>,
    new_tab_name: Option<String>,
    new_tab_mode: Option<TabType>,
    job_ids: Vec<String>,
    use_output_file: bool,
) -> Result<QueueTab, String> {
    let from_q = state
        .scheduler
        .db
        .get_queue(&from_queue_id)?
        .ok_or_else(|| "Source tab not found".to_string())?;
    let dec_map: HashMap<String, String> =
        state.scheduler.db.get_workspace_json("ws_job_decisions")?;
    let repair_map: HashMap<String, TabRepairItemState> =
        state.scheduler.db.get_workspace_json("ws_tab_repair_items")?;

    let dest_tab = if let Some(tid) = target_queue_id {
        state
            .scheduler
            .db
            .get_queue(&tid)?
            .ok_or_else(|| "Destination tab not found".to_string())?
    } else {
        let queues = state.scheduler.db.list_queues()?;
        let next_order = queues.iter().map(|q| q.tab_order).max().unwrap_or(-1) + 1;
        let mode = new_tab_mode.unwrap_or(TabType::Encode);
        let tname = new_tab_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{} · Forwarded", from_q.name));
        let q = QueueTab {
            id: Uuid::new_v4().to_string(),
            name: tname,
            tab_order: next_order,
            tab_type: mode,
            profile_id: from_q.profile_id.clone(),
            profile_name: from_q.profile_name.clone(),
            recipe: EncodeRecipe::archive_balanced_default(),
            priority: ProcessPriority::Background,
            recursive_import: true,
            destination_root: from_q.destination_root.clone(),
            keep_awake: true,
            error_policy: ErrorPolicy::Continue,
            min_savings_enabled: from_q.min_savings_enabled,
            min_savings_pct: from_q.min_savings_pct,
            cleanup_policy: CleanupPolicy::KeepOriginals,
            full_decode_verify: false,
            is_running: false,
            is_paused: false,
            pause_after_current: false,
            waiting_reason: None,
        };
        state.scheduler.db.upsert_queue(&q)?;
        q
    };

    let mut meta_map: HashMap<String, TabWorkspaceMeta> =
        state.scheduler.db.get_workspace_json("ws_tab_meta")?;
    let dest_meta = meta_map.entry(dest_tab.id.clone()).or_default();
    dest_meta.forwarded_from = Some(from_q.name.clone());
    if dest_meta.resource_source.is_none() {
        dest_meta.resource_source = Some(format!("Forwarded from {}", from_q.name));
    }
    state
        .scheduler
        .db
        .set_workspace_json("ws_tab_meta", &meta_map)?;

    let existing_in_dest = state.scheduler.db.list_jobs_for_queue(&dest_tab.id)?;
    let mut existing_paths: HashSet<String> = existing_in_dest
        .iter()
        .map(|j| j.source_path.to_ascii_lowercase())
        .collect();
    let mut reserved: HashSet<String> = state
        .scheduler
        .db
        .list_all_jobs()?
        .into_iter()
        .map(|j| j.destination_path.to_ascii_lowercase())
        .collect();
    let mut next_pos = state.scheduler.db.next_job_position(&dest_tab.id)?;
    let batch_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let ffprobe_opt = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));

    for jid in job_ids {
        let Some(src_job) = state.scheduler.db.get_job(&jid)? else {
            continue;
        };
        let resolved_src_str = if use_output_file {
            if let Some(rep) = repair_map.get(&jid).and_then(|r| r.output_path.as_ref()) {
                rep.clone()
            } else if src_job.status == JobStatus::Completed && Path::new(&src_job.destination_path).exists() {
                src_job.destination_path.clone()
            } else {
                src_job.source_path.clone()
            }
        } else {
            src_job.source_path.clone()
        };

        if existing_paths.contains(&resolved_src_str.to_ascii_lowercase()) {
            continue;
        }
        existing_paths.insert(resolved_src_str.to_ascii_lowercase());

        let src_path = PathBuf::from(&resolved_src_str);
        let rel_path = Path::new(&resolved_src_str)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| src_job.source_rel_path.clone());
        let input_bytes = std::fs::metadata(&src_path)
            .map(|m| m.len())
            .unwrap_or(src_job.input_bytes);
        let fp = capture_file_identity(&src_path).ok().or(src_job.source_fingerprint.clone());

        let decision = dec_map.get(&jid).cloned();
        let recipe = match decision.as_deref() {
            Some("compress_compact") => EncodeRecipe::default_for(
                PresetFamily::Archive,
                PresetVariant::Balanced,
                VideoCodec::Av1,
                ContainerFormat::Mkv,
            ),
            Some("compress_balanced") => EncodeRecipe::archive_balanced_default(),
            _ => dest_tab.recipe.clone(),
        };

        let dest_root = dest_tab
            .destination_root
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                src_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf()
            });
        let final_dest = resolve_destination_path(
            &src_path,
            Path::new(&rel_path),
            &dest_root,
            &recipe,
            &mut reserved,
        );
        let dest_vol_id = disk_space_for_path(final_dest.parent().unwrap_or(&dest_root))
            .ok()
            .map(|v| v.volume_id);

        let fresh_probe = if use_output_file || src_job.probe.is_none() {
            ffprobe_opt
                .as_ref()
                .and_then(|fp_bin| probe_file(fp_bin, &src_path).ok())
                .or(src_job.probe.clone())
        } else {
            src_job.probe.clone()
        };

        let recomputed_plan = fresh_probe
            .as_ref()
            .map(|pr| build_track_plan(&src_path, pr, &recipe, &CompatOverrides::default()));

        let (status, error_stage, error_message) = if let Some(ref pl) = recomputed_plan {
            if pl.is_ready {
                (JobStatus::Listed, None, None)
            } else {
                (
                    JobStatus::NeedsAttention,
                    Some("planning".to_string()),
                    pl.issues.first().map(|i| i.summary.clone()),
                )
            }
        } else {
            (JobStatus::Listed, None, None)
        };

        let new_job = JobRecord {
            id: Uuid::new_v4().to_string(),
            queue_id: dest_tab.id.clone(),
            batch_id: batch_id.clone(),
            position: next_pos,
            source_path: resolved_src_str,
            source_rel_path: rel_path,
            source_root: src_path.parent().map(|p| p.to_string_lossy().to_string()),
            source_fingerprint: fp,
            destination_path: final_dest.to_string_lossy().to_string(),
            dest_volume_id: dest_vol_id,
            recipe,
            compat_overrides: CompatOverrides::default(),
            status,
            status_reason: None,
            probe: fresh_probe,
            plan: recomputed_plan,
            input_bytes,
            output_bytes: None,
            output_fingerprint: None,
            error_stage,
            error_message,
            diagnostics: None,
            cleanup_policy: dest_tab.cleanup_policy,
            cleanup_outcome: CleanupOutcome::None,
            cleanup_error: None,
            consent_id: None,
            full_decode_verified: dest_tab.full_decode_verify,
            min_savings_enabled: dest_tab.min_savings_enabled,
            min_savings_pct: dest_tab.min_savings_pct,
            tool_versions: None,
            current_attempt_id: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        next_pos += 1;
        state.scheduler.db.upsert_job(&new_job)?;
    }

    state
        .scheduler
        .db
        .set_setting("active_queue_id", &dest_tab.id)?;
    state.scheduler.wake_scheduler();
    state.scheduler.emit_snapshot_changed();
    Ok(dest_tab)
}

#[tauri::command]
pub async fn run_tab_dedupe_scan(
    state: State<'_, AppState>,
    queue_id: String,
    also_sha256: bool,
    include_perceptual: bool,
) -> Result<Vec<TabDedupeGroup>, String> {
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let sched = state.scheduler.clone();
    let ffmpeg_opt = sched
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffmpeg_path)));

    let groups = tokio::task::spawn_blocking(move || -> Result<Vec<TabDedupeGroup>, String> {
        let mut by_hash: HashMap<String, Vec<JobRecord>> = HashMap::new();
        let mut cached_perceptual: HashMap<String, Vec<(u64, u64)>> = HashMap::new();

        for j in &jobs {
            let p = Path::new(&j.source_path);
            if !p.is_file() {
                continue;
            }
            let fp_opt = capture_file_identity(p).ok();
            let (size_b, mtime_ns) = fp_opt
                .as_ref()
                .map(|f| (f.size_bytes, f.mtime_ns))
                .unwrap_or((j.input_bytes, 0));

            let mut b3_val: Option<String> = None;
            if let Ok(Some((b3, sha_opt, perc_opt))) =
                sched.db.get_cached_file_hash(&j.source_path, size_b, mtime_ns)
            {
                if !also_sha256 || sha_opt.is_some() {
                    b3_val = Some(b3);
                }
                if let Some(pj) = perc_opt {
                    if let Ok(parsed) = serde_json::from_str::<Vec<(u64, u64)>>(&pj) {
                        if !parsed.is_empty() {
                            cached_perceptual.insert(j.id.clone(), parsed);
                        }
                    }
                }
            }

            let final_b3 = if let Some(b) = b3_val {
                b
            } else if let Ok((b3, sha_opt)) = crate::duplicates::compute_file_hashes(p, also_sha256) {
                let _ = sched.db.upsert_cached_file_hash(
                    &j.source_path,
                    size_b,
                    mtime_ns,
                    &b3,
                    sha_opt.as_deref(),
                    None,
                );
                b3
            } else {
                continue;
            };
            by_hash.entry(final_b3).or_default().push(j.clone());
        }

        let mut out_groups: Vec<TabDedupeGroup> = Vec::new();
        let mut grouped_ids: HashSet<String> = HashSet::new();

        for (hash, mut members) in by_hash {
            if members.len() < 2 {
                continue;
            }
            members.sort_by(|a, b| a.position.cmp(&b.position));
            let items = members
                .iter()
                .enumerate()
                .map(|(i, j)| {
                    grouped_ids.insert(j.id.clone());
                    make_tab_dedupe_item(j, if i == 0 { "keep" } else { "trash" })
                })
                .collect();
            out_groups.push(TabDedupeGroup {
                id: format!("dg-exact-{}", &hash[..hash.len().min(12)]),
                match_type: if also_sha256 {
                    "Exact Match · BLAKE3 + SHA-256 identical (cached)".to_string()
                } else {
                    "Exact Match · Byte-identical (cached)".to_string()
                },
                similarity: 100.0,
                items,
            });
        }

        // Perceptual visual matching (supports both videos via FFmpeg & images directly)
        if include_perceptual {
            let remaining: Vec<JobRecord> = jobs
                .iter()
                .filter(|j| !grouped_ids.contains(&j.id) && Path::new(&j.source_path).is_file())
                .cloned()
                .collect();
            let mut sigs: Vec<(JobRecord, Vec<(u64, u64)>)> = Vec::new();
            for j in remaining {
                if let Some(cached_sig) = cached_perceptual.remove(&j.id) {
                    sigs.push((j, cached_sig));
                    continue;
                }
                let p = Path::new(&j.source_path);
                let mut hashes = Vec::new();
                if crate::catalog::classify_media_type(p) == "image" {
                    if let Ok(img) = image::open(p) {
                        hashes.push(crate::duplicates::compute_frame_phash_dhash(&img));
                    }
                } else if let Some(ref ff) = ffmpeg_opt {
                    let dur = j
                        .probe
                        .as_ref()
                        .and_then(|pr| pr.format.duration_secs)
                        .unwrap_or(10.0)
                        .max(1.0);
                    for frac in [0.15, 0.35, 0.55, 0.75, 0.9] {
                        let ts = (dur * frac).min((dur - 0.1).max(0.0));
                        if let Ok(bytes) = sched.previews.decode_comparison_frame(ff, p, ts, false) {
                            if let Ok(img) = image::load_from_memory(&bytes) {
                                hashes.push(crate::duplicates::compute_frame_phash_dhash(&img));
                            }
                        }
                    }
                }
                if !hashes.is_empty() {
                    if let Ok(fp) = capture_file_identity(p) {
                        if let Ok(pj) = serde_json::to_string(&hashes) {
                            let _ = sched.db.upsert_cached_file_hash(
                                &j.source_path,
                                fp.size_bytes,
                                fp.mtime_ns,
                                &fp.head_hash,
                                None,
                                Some(&pj),
                            );
                        }
                    }
                    sigs.push((j, hashes));
                }
            }

            let mut used = vec![false; sigs.len()];
            for i in 0..sigs.len() {
                if used[i] {
                    continue;
                }
                let mut cluster = vec![sigs[i].0.clone()];
                let mut best_sim = 0.0f64;
                for k in (i + 1)..sigs.len() {
                    if used[k] {
                        continue;
                    }
                    let is_img_a = crate::catalog::classify_media_type(Path::new(&sigs[i].0.source_path)) == "image";
                    let is_img_b = crate::catalog::classify_media_type(Path::new(&sigs[k].0.source_path)) == "image";
                    if is_img_a != is_img_b {
                        continue;
                    }
                    if !is_img_a {
                        let dur_a = sigs[i].0.probe.as_ref().and_then(|p| p.format.duration_secs).unwrap_or(0.0);
                        let dur_b = sigs[k].0.probe.as_ref().and_then(|p| p.format.duration_secs).unwrap_or(0.0);
                        if (dur_a - dur_b).abs() > 3.0 {
                            continue;
                        }
                    }
                    let pairs = sigs[i].1.len().min(sigs[k].1.len());
                    if pairs == 0 {
                        continue;
                    }
                    let mut total_dist = 0u32;
                    for idx in 0..pairs {
                        let d_p = (sigs[i].1[idx].0 ^ sigs[k].1[idx].0).count_ones();
                        let d_d = (sigs[i].1[idx].1 ^ sigs[k].1[idx].1).count_ones();
                        total_dist += d_p.min(d_d);
                    }
                    let avg_dist = total_dist as f64 / pairs as f64;
                    let sim = ((64.0 - avg_dist) / 64.0) * 100.0;
                    if sim >= 88.0 {
                        used[k] = true;
                        cluster.push(sigs[k].0.clone());
                        best_sim = best_sim.max(sim);
                    }
                }
                if cluster.len() >= 2 {
                    used[i] = true;
                    cluster.sort_by(|a, b| {
                        let area = |j: &JobRecord| -> u64 {
                            j.probe
                                .as_ref()
                                .and_then(|p| p.streams.iter().find(|s| s.codec_type == "video"))
                                .map(|v| v.width.unwrap_or(0) as u64 * v.height.unwrap_or(0) as u64)
                                .unwrap_or(0)
                        };
                        area(b)
                            .cmp(&area(a))
                            .then_with(|| b.input_bytes.cmp(&a.input_bytes))
                    });
                    let items = cluster
                        .iter()
                        .map(|j| make_tab_dedupe_item(j, "review"))
                        .collect();
                    out_groups.push(TabDedupeGroup {
                        id: format!("dg-vis-{}", Uuid::new_v4().simple()),
                        match_type: format!("Visual Match · {:.1}% similar", best_sim),
                        similarity: best_sim,
                        items,
                    });
                }
            }
        }
        Ok(out_groups)
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut all_map: HashMap<String, Vec<TabDedupeGroup>> = state
        .scheduler
        .db
        .get_workspace_json("ws_tab_dedupe_groups")?;
    all_map.insert(queue_id, groups.clone());
    state
        .scheduler
        .db
        .set_workspace_json("ws_tab_dedupe_groups", &all_map)?;
    state.scheduler.emit_snapshot_changed();
    Ok(groups)
}

fn make_tab_dedupe_item(j: &JobRecord, role: &str) -> TabDedupeItem {
    let vid = j
        .probe
        .as_ref()
        .and_then(|p| p.streams.iter().find(|s| s.codec_type == "video"));
    let res = vid
        .and_then(|v| Some(format!("{}×{}", v.width?, v.height?)))
        .unwrap_or_else(|| "—".to_string());
    let codec = vid
        .map(|v| v.codec_name.to_uppercase())
        .unwrap_or_else(|| "VIDEO".to_string());
    let dur = j
        .probe
        .as_ref()
        .and_then(|p| p.format.duration_secs)
        .unwrap_or(0.0);
    let mbps = j
        .probe
        .as_ref()
        .and_then(|p| p.format.bit_rate)
        .map(|b| b as f64 / 1_000_000.0)
        .unwrap_or_else(|| {
            if dur > 0.0 {
                (j.input_bytes as f64 * 8.0) / (dur * 1_000_000.0)
            } else {
                0.0
            }
        });
    TabDedupeItem {
        job_id: j.id.clone(),
        path: j.source_path.clone(),
        name: Path::new(&j.source_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| j.source_rel_path.clone()),
        rel_path: j.source_rel_path.clone(),
        input_bytes: j.input_bytes,
        resolution: res,
        bitrate_mbps: (mbps * 10.0).round() / 10.0,
        codec,
        duration_secs: dur,
        role: role.to_string(),
        trashed: false,
    }
}

#[tauri::command]
pub fn set_tab_dedupe_role(
    state: State<'_, AppState>,
    queue_id: String,
    group_id: String,
    job_id: String,
    role: String,
) -> Result<(), String> {
    let mut all_map: HashMap<String, Vec<TabDedupeGroup>> = state
        .scheduler
        .db
        .get_workspace_json("ws_tab_dedupe_groups")?;
    if let Some(groups) = all_map.get_mut(&queue_id) {
        for g in groups.iter_mut() {
            if g.id == group_id {
                for it in g.items.iter_mut() {
                    if it.job_id == job_id {
                        it.role = role.clone();
                    }
                }
            }
        }
    }
    state
        .scheduler
        .db
        .set_workspace_json("ws_tab_dedupe_groups", &all_map)?;
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub fn preview_tab_trash(
    state: State<'_, AppState>,
    queue_id: Option<String>,
    item_ids: Vec<String>,
    removal_reason: RemovalReason,
) -> Result<TrashPreviewPlan, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    crate::cleanup::preview_guarded_trash(
        &state.scheduler.db,
        queue_id.as_deref(),
        &item_ids,
        removal_reason,
    )
}

#[tauri::command]
pub fn execute_tab_trash_plan(
    state: State<'_, AppState>,
    plan_id: String,
) -> Result<TrashBatchResult, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let res = crate::cleanup::execute_guarded_trash_plan(&state.scheduler.db, &plan_id)?;
    state.scheduler.emit_snapshot_changed();
    Ok(res)
}

#[tauri::command]
pub fn check_path_trash_support(path: String) -> Result<bool, String> {
    crate::platform::check_trash_support(Path::new(&path)).map(|_| true)
}

#[tauri::command]
pub fn trash_tab_duplicates(state: State<'_, AppState>, queue_id: String) -> Result<usize, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let all_map: HashMap<String, Vec<TabDedupeGroup>> = state
        .scheduler
        .db
        .get_workspace_json_strict("ws_tab_dedupe_groups")?;
    let mut item_ids = Vec::new();
    if let Some(groups) = all_map.get(&queue_id) {
        for g in groups {
            for it in &g.items {
                if it.role == "trash" && !it.trashed {
                    item_ids.push(it.job_id.clone());
                }
            }
        }
    }
    if item_ids.is_empty() {
        return Ok(0);
    }
    let plan = crate::cleanup::preview_guarded_trash(
        &state.scheduler.db,
        Some(&queue_id),
        &item_ids,
        RemovalReason::QueueDuplicateGroup,
    )?;
    if !plan.trash_supported && plan.eligible_count == 0 {
        if let Some(err) = plan.trash_support_error {
            return Err(err);
        }
    }
    let res = crate::cleanup::execute_guarded_trash_plan(&state.scheduler.db, &plan.plan_id)?;
    state.scheduler.emit_snapshot_changed();
    if res.moved_count == 0 && res.failed_count > 0 {
        let first_err = res
            .outcomes
            .iter()
            .find_map(|o| o.error.clone())
            .unwrap_or_else(|| "Unable to move duplicates to Trash.".to_string());
        return Err(first_err);
    }
    Ok(res.moved_count)
}

#[tauri::command]
pub fn run_tab_repair_diagnosis(
    state: State<'_, AppState>,
    queue_id: String,
    job_id: String,
) -> Result<(), String> {
    let job = state
        .scheduler
        .db
        .get_job(&job_id)?
        .ok_or_else(|| "Resource not found".to_string())?;
    let mut repair_map: HashMap<String, TabRepairItemState> = state
        .scheduler
        .db
        .get_workspace_json("ws_tab_repair_items")?;
    let rec = TabRepairItemState {
        job_id: job_id.clone(),
        queue_id: queue_id.clone(),
        status: "diagnosing".to_string(),
        diagnosis: None,
        report: None,
        error: None,
        output_path: None,
        output_bytes: None,
        preview_sheet_filename: None,
        updated_at: Utc::now().timestamp(),
    };
    repair_map.insert(job_id.clone(), rec.clone());
    state
        .scheduler
        .db
        .set_workspace_json("ws_tab_repair_items", &repair_map)?;
    state.scheduler.emit_snapshot_changed();

    let sched = state.scheduler.clone();
    std::thread::spawn(move || {
        let sched_ev = sched.clone();
        let qid_ev = queue_id.clone();
        let jid_ev = job_id.clone();
        let cb: crate::recovery::ProgressCallback = Arc::new(move |p| {
            sched_ev.emit_event(
                "repair_progress",
                serde_json::json!({ "queue_id": qid_ev, "job_id": jid_ev, "progress": p }),
            );
        });
        let res = crate::recovery::diagnose_file(Path::new(&job.source_path), Some(&cb));
        let mut repair_map: HashMap<String, TabRepairItemState> = sched
            .db
            .get_workspace_json("ws_tab_repair_items")
            .unwrap_or_default();
        let cur = repair_map.entry(job_id.clone()).or_insert_with(|| rec);
        match res {
            Ok(diag) => {
                cur.diagnosis = Some(diag);
                cur.status = "diagnosed".to_string();
                cur.error = None;
            }
            Err(e) => {
                cur.status = "failed".to_string();
                cur.error = Some(e);
            }
        }
        cur.updated_at = Utc::now().timestamp();
        let _ = sched.db.set_workspace_json("ws_tab_repair_items", &repair_map);
        sched.emit_snapshot_changed();
    });
    Ok(())
}

#[tauri::command]
pub fn run_tab_repair_execute(
    state: State<'_, AppState>,
    queue_id: String,
    job_ids: Vec<String>,
) -> Result<(), String> {
    let ts = state.scheduler.refresh_tools(false);
    let caps = ts
        .capabilities
        .ok_or_else(|| "FFmpeg and ffprobe are required before running recovery.".to_string())?;
    let ffmpeg = PathBuf::from(&caps.ffmpeg_path);
    let ffprobe = PathBuf::from(&caps.ffprobe_path);

    let meta_map: HashMap<String, TabWorkspaceMeta> =
        state.scheduler.db.get_workspace_json("ws_tab_meta")?;
    let tab_meta = meta_map.get(&queue_id).cloned().unwrap_or_default();

    let mut jobs_to_run = Vec::new();
    let mut repair_map: HashMap<String, TabRepairItemState> = state
        .scheduler
        .db
        .get_workspace_json("ws_tab_repair_items")?;
    for jid in job_ids {
        let Some(job) = state.scheduler.db.get_job(&jid)? else {
            continue;
        };
        let state_rec = repair_map
            .entry(jid.clone())
            .or_insert_with(|| TabRepairItemState {
                job_id: jid.clone(),
                queue_id: queue_id.clone(),
                status: "recovering".to_string(),
                diagnosis: None,
                report: None,
                error: None,
                output_path: None,
                output_bytes: None,
                preview_sheet_filename: None,
                updated_at: Utc::now().timestamp(),
            });
        state_rec.status = "recovering".to_string();
        state_rec.error = None;
        state_rec.updated_at = Utc::now().timestamp();
        jobs_to_run.push(job);
    }
    state
        .scheduler
        .db
        .set_workspace_json("ws_tab_repair_items", &repair_map)?;
    state.scheduler.emit_snapshot_changed();

    let sched = state.scheduler.clone();
    std::thread::spawn(move || {
        for job in jobs_to_run {
            let in_p = PathBuf::from(&job.source_path);
            let stem = in_p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("recovered");
            let parent = in_p.parent().unwrap_or_else(|| Path::new("."));
            let out_p = parent.join(format!("{stem}_recovered.mp4"));

            let opts = crate::recovery::RecoveryOptions {
                input_path: in_p,
                reference_paths: tab_meta.repair_refs.iter().map(PathBuf::from).collect(),
                output_path: out_p,
                video_only: tab_meta.repair_video_only,
                deep_scan: tab_meta.repair_deep_scan,
                timeout_secs: tab_meta.repair_timeout_secs.max(30),
                generate_contact_sheet: true,
            };

            let sched_ev = sched.clone();
            let qid_ev = queue_id.clone();
            let jid_ev = job.id.clone();
            let cb: crate::recovery::ProgressCallback = Arc::new(move |p| {
                sched_ev.emit_event(
                    "repair_progress",
                    serde_json::json!({ "queue_id": qid_ev, "job_id": jid_ev, "progress": p }),
                );
            });

            let res = crate::recovery::recover_file(&opts, &ffmpeg, &ffprobe, Some(&cb));
            let mut rmap: HashMap<String, TabRepairItemState> = sched
                .db
                .get_workspace_json("ws_tab_repair_items")
                .unwrap_or_default();
            if let Some(cur) = rmap.get_mut(&job.id) {
                match res {
                    Ok(report) => {
                        cur.diagnosis = Some(report.diagnosis.clone());
                        cur.status = if report.overall_status == "unrecoverable" {
                            "failed".to_string()
                        } else {
                            report.overall_status.clone()
                        };
                        if let Some(ref sheet) = report.contact_sheet_path {
                            let preview_fname = format!("repair_{}.jpg", job.id);
                            let _ = std::fs::create_dir_all(&sched.previews.preview_dir);
                            let dst = sched.previews.preview_dir.join(&preview_fname);
                            if std::fs::copy(sheet, &dst).is_ok() {
                                cur.preview_sheet_filename = Some(preview_fname);
                            }
                        }
                        if let Some(ref out_file) = report.output_path {
                            cur.output_path = Some(out_file.clone());
                            cur.output_bytes = std::fs::metadata(out_file).ok().map(|m| m.len());
                        }
                        cur.report = Some(report);
                        cur.error = None;
                    }
                    Err(e) => {
                        cur.status = "failed".to_string();
                        cur.error = Some(e);
                    }
                }
                cur.updated_at = Utc::now().timestamp();
                let _ = sched.db.set_workspace_json("ws_tab_repair_items", &rmap);
                sched.emit_snapshot_changed();
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn export_tab_repair_report(
    state: State<'_, AppState>,
    job_id: String,
    dest_path: String,
    format: String,
) -> Result<(), String> {
    let rmap: HashMap<String, TabRepairItemState> = state
        .scheduler
        .db
        .get_workspace_json("ws_tab_repair_items")?;
    let rec = rmap
        .get(&job_id)
        .ok_or_else(|| "No repair record found for this item.".to_string())?;
    let report = rec
        .report
        .as_ref()
        .ok_or_else(|| "Run repair before exporting a recovery report.".to_string())?;
    let content = if format == "json" {
        serde_json::to_string_pretty(report).map_err(|e| e.to_string())?
    } else {
        crate::recovery::format_recovery_human(report)
    };
    std::fs::write(&dest_path, content).map_err(|e| format!("Failed writing report: {e}"))?;
    Ok(())
}

// ============================================================================
// Organize Tab Sub-Capabilities (Hash Cache, Preview, Watermark OCR, Faces, Move by Tag)
// ============================================================================

#[tauri::command]
pub fn get_hash_cache_stats(state: State<'_, AppState>) -> Result<HashCacheStats, String> {
    state.scheduler.db.get_hash_cache_stats()
}

#[tauri::command]
pub fn clear_hash_cache(state: State<'_, AppState>) -> Result<usize, String> {
    let deleted = state.scheduler.db.clear_hash_cache()?;
    state.scheduler.emit_snapshot_changed();
    Ok(deleted)
}

#[tauri::command]
pub fn trash_tab_specific_items(
    state: State<'_, AppState>,
    queue_id: String,
    job_ids: Vec<String>,
) -> Result<usize, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    let plan = crate::cleanup::preview_guarded_trash(
        &state.scheduler.db,
        Some(&queue_id),
        &job_ids,
        RemovalReason::QueueSpecificItems,
    )?;
    if !plan.trash_supported && plan.eligible_count == 0 {
        if let Some(err) = plan.trash_support_error {
            return Err(err);
        }
    }
    let res = crate::cleanup::execute_guarded_trash_plan(&state.scheduler.db, &plan.plan_id)?;
    state.scheduler.emit_snapshot_changed();
    if res.moved_count == 0 && res.failed_count > 0 {
        let first_err = res
            .outcomes
            .iter()
            .find_map(|o| o.error.clone())
            .unwrap_or_else(|| "Unable to move items to Trash.".to_string());
        return Err(first_err);
    }
    Ok(res.moved_count)
}

#[tauri::command]
pub fn read_local_media_preview(
    state: State<'_, AppState>,
    media_path: String,
) -> Result<Response, String> {
    let target = Path::new(&media_path);
    let canon = target.canonicalize().map_err(|e| e.to_string())?;
    let thumb_root = crate::analysis::analysis_cache_dir(&state.scheduler.db)
        .canonicalize()
        .ok();
    let in_thumb = thumb_root.map(|r| canon.starts_with(r)).unwrap_or(false);
    let allowed = in_thumb
        || state
            .scheduler
            .db
            .list_all_jobs()?
            .iter()
            .any(|j| Path::new(&j.source_path) == target || Path::new(&j.destination_path) == target);
    if !allowed {
        return Err("Unauthorized media preview path.".to_string());
    }
    let bytes = std::fs::read(&canon).map_err(|e| format!("Failed reading image preview: {e}"))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub fn tab_get_organize_overview(
    state: State<'_, AppState>,
    queue_id: String,
) -> Result<TabOrganizeOverview, String> {
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let hash_cache = state.scheduler.db.get_hash_cache_stats().unwrap_or_default();

    // Map paths of jobs in this tab -> file_key & asset_id
    let mut path_to_job: HashMap<String, String> = HashMap::new();
    for j in &jobs {
        path_to_job.insert(crate::catalog::normalize_display_path(Path::new(&j.source_path)), j.id.clone());
        path_to_job.insert(j.source_path.clone(), j.id.clone());
    }

    let mut job_tags: HashMap<String, Vec<String>> = HashMap::new();
    let mut avail_set: HashSet<String> = HashSet::new();
    let mut fkey_to_job: HashMap<String, String> = HashMap::new();

    {
        let conn = state.scheduler.db.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT fl.path, fl.file_key, mf.asset_id FROM file_locations fl
                 JOIN media_files mf ON mf.file_key = fl.file_key
                 WHERE fl.is_current = 1",
            )
            .map_err(|e| e.to_string())?;
        let mut asset_to_jobs: HashMap<String, Vec<String>> = HashMap::new();
        for (p, fk, aid) in stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            if let Some(jid) = path_to_job.get(&p) {
                fkey_to_job.insert(fk, jid.clone());
                asset_to_jobs.entry(aid).or_default().push(jid.clone());
            }
        }
        drop(stmt);

        if !asset_to_jobs.is_empty() {
            let mut tstmt = conn
                .prepare(
                    "SELECT mt.asset_id, t.name FROM media_tags mt
                     JOIN tags t ON t.id = mt.tag_id
                     WHERE mt.status = 'active'",
                )
                .map_err(|e| e.to_string())?;
            for (aid, tname) in tstmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?
                .flatten()
            {
                if let Some(jids) = asset_to_jobs.get(&aid) {
                    avail_set.insert(tname.clone());
                    for jid in jids {
                        let list = job_tags.entry(jid.clone()).or_default();
                        if !list.contains(&tname) {
                            list.push(tname.clone());
                        }
                    }
                }
            }
        }
    }

    let ocr_detections: Vec<crate::intelligence::OcrDetectionRecord> =
        crate::intelligence::list_ocr_detections(&state.scheduler.db, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|o| fkey_to_job.contains_key(&o.file_key) || path_to_job.contains_key(&o.path))
            .collect();

    let mut face_groups = Vec::new();
    if let Ok(all_groups) = crate::intelligence::list_face_groups(&state.scheduler.db) {
        for g in all_groups {
            let obs = crate::intelligence::list_face_observations(&state.scheduler.db, &g.id).unwrap_or_default();
            let matched: Vec<String> = obs
                .iter()
                .filter_map(|o| fkey_to_job.get(&o.file_key).cloned().or_else(|| path_to_job.get(&o.path).cloned()))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            if !matched.is_empty() {
                face_groups.push(TabFaceGroupView {
                    group_id: g.id,
                    label: g.label,
                    sample_crop_path: g.sample_crop_path,
                    observation_count: obs.len(),
                    matched_job_ids: matched,
                });
            }
        }
    }

    let mut available_tags: Vec<String> = avail_set.into_iter().collect();
    available_tags.sort();

    Ok(TabOrganizeOverview {
        job_tags,
        available_tags,
        ocr_detections,
        face_groups,
        hash_cache,
    })
}

#[tauri::command]
pub async fn tab_extract_watermarks(
    state: State<'_, AppState>,
    queue_id: String,
) -> Result<usize, String> {
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let (ff, fp_opt) = {
        let ts = state.scheduler.tool_status.lock().map_err(|e| e.to_string())?;
        let caps = ts.capabilities.as_ref().ok_or_else(|| "FFmpeg not configured.".to_string())?;
        (PathBuf::from(&caps.ffmpeg_path), Some(PathBuf::from(&caps.ffprobe_path)))
    };
    let db = state.scheduler.db.clone();
    let count = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let mut hits = 0usize;
        for j in jobs {
            let p = Path::new(&j.source_path);
            if !p.exists() {
                continue;
            }
            if let Ok((fk, _)) = crate::catalog::ensure_path_in_catalog(&db, p, fp_opt.as_deref()) {
                if let Ok(recs) = crate::intelligence::run_ocr_and_watermark_analysis(&db, &ff, &fk) {
                    hits += recs.len();
                }
            }
        }
        Ok(hits)
    })
    .await
    .map_err(|e| e.to_string())??;
    state.scheduler.emit_snapshot_changed();
    Ok(count)
}

#[tauri::command]
pub fn tab_tag_resources(
    state: State<'_, AppState>,
    job_ids: Vec<String>,
    tag_names: Vec<String>,
) -> Result<usize, String> {
    let fp_opt = state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)));
    let mut asset_ids = Vec::new();
    for jid in job_ids {
        if let Some(job) = state.scheduler.db.get_job(&jid)? {
            let p = Path::new(&job.source_path);
            if p.exists() {
                if let Ok((_, aid)) = crate::catalog::ensure_path_in_catalog(&state.scheduler.db, p, fp_opt.as_deref()) {
                    asset_ids.push(aid);
                }
            }
        }
    }
    let clean_tags: Vec<String> = tag_names.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if asset_ids.is_empty() || clean_tags.is_empty() {
        return Ok(0);
    }
    let n = crate::organize::assign_tags_bulk(&state.scheduler.db, &asset_ids, &clean_tags, "manual", 1.0)?;
    state.scheduler.emit_snapshot_changed();
    Ok(n)
}

#[tauri::command]
pub fn tab_untag_resource(
    state: State<'_, AppState>,
    job_id: String,
    tag_name: String,
) -> Result<(), String> {
    let Some(job) = state.scheduler.db.get_job(&job_id)? else {
        return Ok(());
    };
    let norm = crate::catalog::normalize_display_path(Path::new(&job.source_path));
    let conn = state.scheduler.db.lock_conn()?;
    let asset_id: Option<String> = conn
        .query_row(
            "SELECT mf.asset_id FROM file_locations fl JOIN media_files mf ON mf.file_key = fl.file_key WHERE (fl.path = ?1 OR fl.path = ?2) AND fl.is_current = 1 LIMIT 1",
            rusqlite::params![norm, job.source_path],
            |r| r.get(0),
        )
        .ok();
    let tag_id: Option<String> = conn
        .query_row("SELECT id FROM tags WHERE LOWER(name) = LOWER(?1)", rusqlite::params![tag_name.trim()], |r| r.get(0))
        .ok();
    drop(conn);
    if let (Some(aid), Some(tid)) = (asset_id, tag_id) {
        let _ = crate::organize::remove_tags_bulk(&state.scheduler.db, &[aid], &[tid], false)?;
    }
    state.scheduler.emit_snapshot_changed();
    Ok(())
}

#[tauri::command]
pub async fn tab_run_face_grouping(
    state: State<'_, AppState>,
    queue_id: String,
) -> Result<usize, String> {
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let (ff, fp_opt) = {
        let ts = state.scheduler.tool_status.lock().map_err(|e| e.to_string())?;
        let caps = ts.capabilities.as_ref().ok_or_else(|| "FFmpeg not configured.".to_string())?;
        (PathBuf::from(&caps.ffmpeg_path), Some(PathBuf::from(&caps.ffprobe_path)))
    };
    let db = state.scheduler.db.clone();
    // Enable local face grouping explicitly when user launches it inside Organize -> People
    let _ = crate::intelligence::set_face_grouping_enabled(&db, true);

    let obs_count = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let mut fkeys = Vec::new();
        for j in jobs {
            let p = Path::new(&j.source_path);
            if p.exists() {
                if let Ok((fk, _)) = crate::catalog::ensure_path_in_catalog(&db, p, fp_opt.as_deref()) {
                    fkeys.push(fk);
                }
            }
        }
        if fkeys.is_empty() {
            return Ok(0);
        }
        crate::intelligence::run_face_grouping_for_files(&db, &ff, &fkeys)
    })
    .await
    .map_err(|e| e.to_string())??;
    state.scheduler.emit_snapshot_changed();
    Ok(obs_count)
}

#[tauri::command]
pub fn tab_label_face_and_tag(
    state: State<'_, AppState>,
    queue_id: String,
    group_id: String,
    label: String,
) -> Result<usize, String> {
    let clean = label.trim().to_string();
    if clean.is_empty() {
        return Err("Person name cannot be empty.".to_string());
    }
    crate::intelligence::label_face_group(&state.scheduler.db, &group_id, &clean)?;
    let obs = crate::intelligence::list_face_observations(&state.scheduler.db, &group_id)?;
    let obs_paths: HashSet<String> = obs.iter().map(|o| o.path.clone()).collect();
    let jobs = state.scheduler.db.list_jobs_for_queue(&queue_id)?;
    let matched_job_ids: Vec<String> = jobs
        .into_iter()
        .filter(|j| obs_paths.contains(&j.source_path) || obs_paths.contains(&crate::catalog::normalize_display_path(Path::new(&j.source_path))))
        .map(|j| j.id)
        .collect();
    let tag = if clean.contains(':') { clean } else { format!("person:{clean}") };
    let tagged = tab_tag_resources(state, matched_job_ids, vec![tag])?;
    Ok(tagged)
}

#[tauri::command]
pub fn tab_move_by_tag(
    state: State<'_, AppState>,
    queue_id: String,
    tag_name: String,
    dest_dir: String,
    copy_only: bool,
    dry_run: bool,
    preserve_subfolders: Option<bool>,
    collision_policy: Option<String>,
    plan_id: Option<String>,
) -> Result<MoveByTagResult, String> {
    let _guard = state.scheduler.cleanup_lock.lock();
    if dry_run {
        let overview = tab_get_organize_overview(state.clone(), queue_id.clone())?;
        return crate::organize::preview_tab_move_by_tag(
            &state.scheduler.db,
            &queue_id,
            &tag_name,
            &dest_dir,
            copy_only,
            preserve_subfolders.unwrap_or(false),
            collision_policy.as_deref().unwrap_or("keep_existing"),
            &overview.job_tags,
        );
    }

    let pid = plan_id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "A reviewed preview plan is required before moving or copying files.".to_string())?;
    let res = crate::organize::execute_tab_move_by_tag_plan(&state.scheduler.db, &pid)?;
    if res.moved_count > 0 || res.retained_count > 0 {
        state.scheduler.emit_snapshot_changed();
    }
    Ok(res)
}
