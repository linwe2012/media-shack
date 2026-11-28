use crate::analysis::{
    clear_analysis_cache, detect_black_and_frozen_frames, evict_cache_to_budget, get_cache_stats,
    BlackFrozenReport, CacheStats,
};
use crate::catalog::{
    add_library_root, get_asset_detail, get_catalog_stats, list_library_roots, query_catalog,
    refresh_catalog_volumes, remove_library_root, scan_library_root, update_asset_metadata_fields,
    AssetDetail, CatalogFilter, CatalogPage, CatalogStats, LibraryRoot, VolumeRecord,
};
use crate::commands::AppState;
use crate::duplicates::{
    dismiss_similarity_edge, list_duplicate_groups, merge_tags_from_copies,
    run_full_duplicate_scan_with_progress, DuplicateGroup, DuplicateProgressCb,
};
use crate::integrity::{
    create_par2_parity_if_available, discover_par2, export_checksum_manifest_file,
    list_integrity_history, verify_file_integrity, IntegrityCheckRecord,
};
use crate::intelligence::{
    exclude_face_observation, generate_llm_organization_proposal, get_creator_aliases,
    get_llm_config, index_semantic_embeddings, is_face_grouping_enabled, label_face_group,
    list_face_groups, list_face_observations, list_ocr_detections, merge_face_groups,
    purge_all_biometric_data, review_ocr_detections_batch, run_face_grouping_for_files,
    run_ocr_and_watermark_analysis, save_creator_alias, save_llm_config, semantic_media_search,
    set_face_grouping_enabled, split_face_observation, FaceGroupRecord, FaceObservationRecord,
    LlmConfig, LlmOrganizationProposal, OcrDetectionRecord, SemanticSearchHit,
};
use crate::metadata_interop::{
    discover_exiftool, export_sidecars_for_file, import_sidecar_for_file, sync_os_tags_for_file,
    write_embedded_metadata_lossless, ConflictPolicy, MetadataSyncReport,
};
use crate::models::{CompatOverrides, JobRecord, JobStatus};
use crate::organize::{
    add_tag_alias, assign_tags_bulk, create_or_get_tag, delete_collection, evaluate_collection,
    execute_batch_file_operations, list_collections, list_operation_history, list_tags, merge_tags,
    preview_batch_move_or_copy, preview_batch_rename, remove_tags_bulk, save_collection,
    suggest_tags, undo_operation, BatchOpExecResult, CollectionRecord, FileOpPreviewItem,
    FileOpPreviewResult, OperationHistoryRecord, RenameOptions, TagRecord,
};
use crate::planner::{build_track_plan, probe_file, resolve_destination_path_with_config};
use crate::platform::{capture_file_identity, disk_space_for_path};
use crate::repair::{diagnose_media_file, repair_damaged_video, RepairDiagnosis, RepairResult};
use crate::tasks::{ResourceBudgets, TaskRecord};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{Emitter, State};
use uuid::Uuid;

fn tool_paths(state: &AppState) -> (Option<PathBuf>, Option<PathBuf>) {
    state
        .scheduler
        .tool_status
        .lock()
        .ok()
        .and_then(|t| {
            t.capabilities.as_ref().map(|c| {
                (
                    Some(PathBuf::from(&c.ffmpeg_path)),
                    Some(PathBuf::from(&c.ffprobe_path)),
                )
            })
        })
        .unwrap_or((None, None))
}

// ============================================================================
// Milestone 1: Catalog & Scanning Commands
// ============================================================================

#[tauri::command]
pub async fn catalog_add_root(
    state: State<'_, AppState>,
    path: String,
    label: Option<String>,
    recursive: Option<bool>,
    exclusions: Option<Vec<String>>,
    include_hidden: Option<bool>,
    auto_analysis_level: Option<i32>,
) -> Result<LibraryRoot, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || {
        add_library_root(
            &db,
            Path::new(&path),
            label.as_deref(),
            recursive.unwrap_or(true),
            exclusions.unwrap_or_default(),
            include_hidden.unwrap_or(false),
            auto_analysis_level.unwrap_or(1),
        )
    })
    .await
    .map_err(|e| format!("catalog_add_root task failed: {e}"))?
}

#[tauri::command]
pub fn catalog_list_roots(state: State<'_, AppState>) -> Result<Vec<LibraryRoot>, String> {
    list_library_roots(&state.scheduler.db)
}

#[tauri::command]
pub fn catalog_remove_root(
    state: State<'_, AppState>,
    root_id: String,
    remove_entries: bool,
) -> Result<(), String> {
    remove_library_root(&state.scheduler.db, &root_id, remove_entries)
}

#[tauri::command]
pub fn catalog_scan_root(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    root_id: String,
    target_level: Option<i32>,
) -> Result<TaskRecord, String> {
    let task = state.task_manager.enqueue_task(
        "scan_library_root",
        "scan",
        vec![root_id.clone()],
        2,
        "disk_io",
        serde_json::json!({ "root_id": root_id, "target_level": target_level }),
    )?;
    let (cancel, pause) = state.task_manager.register_control_flags(&task.id);
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let (_, ffprobe_opt) = tool_paths(&state);
    let tid = task.id.clone();

    std::thread::spawn(move || {
        let _ = tm.update_task_progress(&tid, "running", 5.0, Some("Scanning filesystem…"), None);
        let _ = app.emit("tasks_changed", ());
        match scan_library_root(&db, &root_id, ffprobe_opt.as_deref(), target_level, &cancel, Some(&pause)) {
            Ok(rep) => {
                let speed = if rep.elapsed_ms > 0 {
                    format!(
                        "{:.1} files/s ({} added, {} moved, {} offline)",
                        (rep.discovered_files as f64) / ((rep.elapsed_ms as f64) / 1000.0).max(0.01),
                        rep.added_files,
                        rep.moved_or_renamed_files,
                        rep.marked_offline
                    )
                } else {
                    format!("{} files", rep.discovered_files)
                };
                let state_str = if rep.canceled { "canceled" } else { "completed" };
                let _ = tm.update_task_progress(&tid, state_str, 100.0, Some(&speed), None);
                let _ = app.emit("catalog_changed", rep);
            }
            Err(e) => {
                let _ = tm.update_task_progress(&tid, "failed", 0.0, None, Some(&e));
            }
        }
        let _ = app.emit("tasks_changed", ());
    });

    Ok(task)
}

#[tauri::command]
pub async fn catalog_refresh_volumes(state: State<'_, AppState>) -> Result<Vec<VolumeRecord>, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || refresh_catalog_volumes(&db))
        .await
        .map_err(|e| format!("catalog_refresh_volumes task failed: {e}"))?
}

#[tauri::command]
pub async fn catalog_query(state: State<'_, AppState>, filter: CatalogFilter) -> Result<CatalogPage, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || query_catalog(&db, &filter))
        .await
        .map_err(|e| format!("catalog_query task failed: {e}"))?
}

#[tauri::command]
pub async fn catalog_get_asset_detail(state: State<'_, AppState>, file_key: String) -> Result<AssetDetail, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || get_asset_detail(&db, &file_key))
        .await
        .map_err(|e| format!("catalog_get_asset_detail task failed: {e}"))?
}

#[tauri::command]
pub async fn catalog_get_stats(state: State<'_, AppState>) -> Result<CatalogStats, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || get_catalog_stats(&db))
        .await
        .map_err(|e| format!("catalog_get_stats task failed: {e}"))?
}

#[tauri::command]
pub fn catalog_update_asset_metadata(
    state: State<'_, AppState>,
    asset_id: String,
    title: Option<String>,
    description: Option<String>,
    creator: Option<String>,
    recorded_at: Option<String>,
) -> Result<(), String> {
    update_asset_metadata_fields(
        &state.scheduler.db,
        &asset_id,
        title.as_deref(),
        description.as_deref(),
        creator.as_deref(),
        recorded_at.as_deref(),
    )
}

#[tauri::command]
pub fn catalog_backup_database(state: State<'_, AppState>, dest_path: String) -> Result<(), String> {
    state.scheduler.db.backup_database(Path::new(&dest_path))
}

// ============================================================================
// Milestone 2: Tags, Collections & Batch File Operations
// ============================================================================

#[tauri::command]
pub fn tags_list(state: State<'_, AppState>) -> Result<Vec<TagRecord>, String> {
    list_tags(&state.scheduler.db)
}

#[tauri::command]
pub fn tags_suggest(state: State<'_, AppState>, prefix: String) -> Result<Vec<TagRecord>, String> {
    suggest_tags(&state.scheduler.db, &prefix)
}

#[tauri::command]
pub fn tags_create(
    state: State<'_, AppState>,
    name: String,
    color: Option<String>,
) -> Result<TagRecord, String> {
    create_or_get_tag(&state.scheduler.db, &name, color.as_deref())
}

#[tauri::command]
pub fn tags_add_alias(state: State<'_, AppState>, tag_id: String, alias: String) -> Result<(), String> {
    add_tag_alias(&state.scheduler.db, &tag_id, &alias)
}

#[tauri::command]
pub fn tags_merge(
    state: State<'_, AppState>,
    source_tag_id: String,
    target_tag_id: String,
) -> Result<usize, String> {
    merge_tags(&state.scheduler.db, &source_tag_id, &target_tag_id)
}

#[tauri::command]
pub fn tags_assign_bulk(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    tag_names: Vec<String>,
    source: Option<String>,
    confidence: Option<f64>,
) -> Result<usize, String> {
    assign_tags_bulk(
        &state.scheduler.db,
        &asset_ids,
        &tag_names,
        source.as_deref().unwrap_or("manual"),
        confidence.unwrap_or(1.0),
    )
}

#[tauri::command]
pub fn tags_remove_bulk(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    tag_ids: Vec<String>,
    mark_rejected: Option<bool>,
) -> Result<usize, String> {
    remove_tags_bulk(&state.scheduler.db, &asset_ids, &tag_ids, mark_rejected.unwrap_or(false))
}

#[tauri::command]
pub fn collections_list(state: State<'_, AppState>) -> Result<Vec<CollectionRecord>, String> {
    list_collections(&state.scheduler.db)
}

#[tauri::command]
pub fn collections_save(
    state: State<'_, AppState>,
    id: Option<String>,
    name: String,
    description: Option<String>,
    filter: CatalogFilter,
) -> Result<CollectionRecord, String> {
    save_collection(&state.scheduler.db, id.as_deref(), &name, description.as_deref(), &filter)
}

#[tauri::command]
pub fn collections_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    delete_collection(&state.scheduler.db, &id)
}

#[tauri::command]
pub fn collections_evaluate(state: State<'_, AppState>, id: String) -> Result<CatalogPage, String> {
    evaluate_collection(&state.scheduler.db, &id)
}

#[tauri::command]
pub async fn organize_preview_rename(
    state: State<'_, AppState>,
    file_keys: Vec<String>,
    options: RenameOptions,
) -> Result<FileOpPreviewResult, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || preview_batch_rename(&db, &file_keys, &options))
        .await
        .map_err(|e| format!("organize_preview_rename task failed: {e}"))?
}

#[tauri::command]
pub async fn organize_preview_move_copy(
    state: State<'_, AppState>,
    file_keys: Vec<String>,
    dest_root: String,
    folder_template: Option<String>,
    is_copy: bool,
    multi_value_policy: Option<String>,
) -> Result<FileOpPreviewResult, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || {
        preview_batch_move_or_copy(
            &db,
            &file_keys,
            Path::new(&dest_root),
            folder_template.as_deref(),
            is_copy,
            multi_value_policy.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("organize_preview_move_copy task failed: {e}"))?
}

#[tauri::command]
pub async fn organize_execute_batch(
    state: State<'_, AppState>,
    items: Option<Vec<FileOpPreviewItem>>,
    summary: String,
    plan_id: Option<String>,
) -> Result<BatchOpExecResult, String> {
    let sched = state.scheduler.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = sched.cleanup_lock.lock();
        let res = if let Some(pid) = plan_id.filter(|s| !s.trim().is_empty()) {
            crate::organize::execute_batch_plan_by_id(&sched.db, &pid, &summary)?
        } else {
            let list = items.unwrap_or_default();
            execute_batch_file_operations(&sched.db, &list, &summary)?
        };
        sched.emit_snapshot_changed();
        Ok(res)
    })
    .await
    .map_err(|e| format!("organize_execute_batch task failed: {e}"))?
}

#[tauri::command]
pub fn organize_list_history(state: State<'_, AppState>) -> Result<Vec<OperationHistoryRecord>, String> {
    list_operation_history(&state.scheduler.db)
}

#[tauri::command]
pub async fn organize_undo(state: State<'_, AppState>, operation_id: String) -> Result<BatchOpExecResult, String> {
    let sched = state.scheduler.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = sched.cleanup_lock.lock();
        let res = undo_operation(&sched.db, &operation_id)?;
        sched.emit_snapshot_changed();
        Ok(res)
    })
    .await
    .map_err(|e| format!("organize_undo task failed: {e}"))?
}

// ============================================================================
// Milestone 3: Duplicate & Similarity Detection
// ============================================================================

#[tauri::command]
pub fn duplicates_run_scan(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    also_sha256: Option<bool>,
    include_perceptual: Option<bool>,
) -> Result<TaskRecord, String> {
    let task = state.task_manager.enqueue_task(
        "duplicate_similarity_scan",
        "duplicates",
        Vec::new(),
        3,
        "cpu",
        serde_json::json!({
            "also_sha256": also_sha256.unwrap_or(false),
            "include_perceptual": include_perceptual.unwrap_or(true)
        }),
    )?;
    let (cancel, _) = state.task_manager.register_control_flags(&task.id);
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let (ffmpeg_opt, _) = tool_paths(&state);
    let tid = task.id.clone();
    let sha = also_sha256.unwrap_or(false);
    let perc = include_perceptual.unwrap_or(true);

    std::thread::spawn(move || {
        let _ = tm.update_task_progress(&tid, "running", 2.0, Some("Starting duplicate scan…"), None);
        let _ = app.emit(
            "catalog_dup_progress",
            serde_json::json!({
                "task_id": tid,
                "state": "running",
                "pct": 2.0,
                "phase": "Starting",
                "done": 0,
                "total": 0,
                "current_file": "",
                "label": "Starting duplicate scan…"
            }),
        );
        let _ = app.emit("tasks_changed", ());

        let app_ev = app.clone();
        let tm_ev = tm.clone();
        let tid_ev = tid.clone();
        let last_emit = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)));
        let cb: DuplicateProgressCb = std::sync::Arc::new(move |phase, pct, done, total, current_file| {
            let mut should_emit = done == 0 || (total > 0 && done >= total);
            if !should_emit {
                if let Ok(mut last) = last_emit.lock() {
                    if last.elapsed() >= std::time::Duration::from_millis(160) {
                        *last = std::time::Instant::now();
                        should_emit = true;
                    }
                }
            }
            if !should_emit {
                return;
            }
            let lbl = if total > 0 {
                format!("{phase}: {done}/{total}")
            } else {
                phase.to_string()
            };
            let _ = tm_ev.update_task_progress(&tid_ev, "running", pct, Some(&lbl), None);
            let _ = app_ev.emit(
                "catalog_dup_progress",
                serde_json::json!({
                    "task_id": tid_ev,
                    "state": "running",
                    "pct": pct,
                    "phase": phase,
                    "done": done,
                    "total": total,
                    "current_file": current_file,
                    "label": lbl
                }),
            );
            let _ = app_ev.emit("tasks_changed", ());
        });

        match run_full_duplicate_scan_with_progress(&db, ffmpeg_opt.as_deref(), sha, perc, &cancel, Some(&cb)) {
            Ok(sum) => {
                let mb_s = if sum.elapsed_ms > 0 {
                    ((sum.bytes_hashed as f64) / (1024.0 * 1024.0))
                        / ((sum.elapsed_ms as f64) / 1000.0).max(0.01)
                } else {
                    0.0
                };
                let label = format!(
                    "{:.1} MB/s · {} exact, {} equiv, {} perc, {} partial",
                    mb_s,
                    sum.exact_groups_found,
                    sum.equivalent_pairs_found,
                    sum.perceptual_pairs_found,
                    sum.partial_overlaps_found
                );
                let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&label), None);
                let _ = app.emit(
                    "catalog_dup_progress",
                    serde_json::json!({
                        "task_id": tid,
                        "state": "completed",
                        "pct": 100.0,
                        "phase": "Completed",
                        "done": 1,
                        "total": 1,
                        "current_file": "",
                        "label": label
                    }),
                );
                let _ = app.emit("duplicates_changed", sum);
            }
            Err(e) => {
                let _ = tm.update_task_progress(&tid, "failed", 0.0, None, Some(&e));
                let _ = app.emit(
                    "catalog_dup_progress",
                    serde_json::json!({
                        "task_id": tid,
                        "state": "failed",
                        "pct": 0.0,
                        "phase": "Failed",
                        "done": 0,
                        "total": 0,
                        "current_file": "",
                        "label": e
                    }),
                );
            }
        }
        let _ = app.emit("tasks_changed", ());
    });
    Ok(task)
}

#[tauri::command]
pub async fn duplicates_list_groups(
    state: State<'_, AppState>,
    match_kind: Option<String>,
) -> Result<Vec<DuplicateGroup>, String> {
    let db = state.scheduler.db.clone();
    tokio::task::spawn_blocking(move || list_duplicate_groups(&db, match_kind.as_deref()))
        .await
        .map_err(|e| format!("duplicates_list_groups task failed: {e}"))?
}

#[tauri::command]
pub fn duplicates_dismiss_edge(state: State<'_, AppState>, edge_id: String) -> Result<(), String> {
    dismiss_similarity_edge(&state.scheduler.db, &edge_id)
}

#[tauri::command]
pub fn duplicates_merge_tags(
    state: State<'_, AppState>,
    from_asset_ids: Vec<String>,
    into_asset_id: String,
) -> Result<usize, String> {
    merge_tags_from_copies(&state.scheduler.db, &from_asset_ids, &into_asset_id)
}

#[tauri::command]
pub async fn duplicates_trash_copies(
    state: State<'_, AppState>,
    file_keys: Vec<String>,
) -> Result<Vec<(String, Result<String, String>)>, String> {
    let sched = state.scheduler.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = sched.cleanup_lock.lock();
        let plan = crate::cleanup::preview_guarded_trash(
            &sched.db,
            None,
            &file_keys,
            crate::models::RemovalReason::LibraryDuplicateCopy,
        )?;
        if !plan.trash_supported && plan.eligible_count == 0 {
            if let Some(err) = plan.trash_support_error {
                return Err(err);
            }
        }
        let res = crate::cleanup::execute_guarded_trash_plan(&sched.db, &plan.plan_id)?;
        sched.emit_snapshot_changed();
        Ok(res
            .outcomes
            .into_iter()
            .map(|o| {
                if o.moved {
                    (o.item_id, Ok("Moved to Trash".to_string()))
                } else {
                    (
                        o.item_id,
                        Err(o.error.unwrap_or_else(|| "Trash failed".to_string())),
                    )
                }
            })
            .collect())
    })
    .await
    .map_err(|e| format!("duplicates_trash_copies task failed: {e}"))?
}

// ============================================================================
// Milestone 4: OS, ExifTool & Sidecar Metadata Interoperability
// ============================================================================

#[tauri::command]
pub fn metadata_sync_os_tags(
    state: State<'_, AppState>,
    file_key: String,
    direction: String,
    policy: Option<ConflictPolicy>,
) -> Result<MetadataSyncReport, String> {
    sync_os_tags_for_file(
        &state.scheduler.db,
        &file_key,
        &direction,
        policy.unwrap_or(ConflictPolicy::Manual),
    )
}

#[tauri::command]
pub fn metadata_write_embedded(
    state: State<'_, AppState>,
    file_key: String,
    tags: Vec<String>,
    title: Option<String>,
    creator: Option<String>,
) -> Result<String, String> {
    let (ffmpeg_opt, _) = tool_paths(&state);
    let et_setting: Option<String> = state.scheduler.db.get_setting("exiftool_path").ok().flatten();
    let et = discover_exiftool(et_setting.as_deref());
    write_embedded_metadata_lossless(
        &state.scheduler.db,
        et.as_deref(),
        ffmpeg_opt.as_deref(),
        &file_key,
        &tags,
        title.as_deref(),
        creator.as_deref(),
    )
}

#[tauri::command]
pub fn metadata_export_sidecars(
    state: State<'_, AppState>,
    file_key: String,
    write_json: bool,
    write_xmp: bool,
) -> Result<Vec<String>, String> {
    export_sidecars_for_file(&state.scheduler.db, &file_key, write_json, write_xmp)
}

#[tauri::command]
pub fn metadata_import_sidecar(
    state: State<'_, AppState>,
    file_key: String,
    policy: Option<ConflictPolicy>,
) -> Result<MetadataSyncReport, String> {
    import_sidecar_for_file(
        &state.scheduler.db,
        &file_key,
        policy.unwrap_or(ConflictPolicy::Manual),
    )
}

#[tauri::command]
pub fn metadata_check_tools(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let et_setting: Option<String> = state.scheduler.db.get_setting("exiftool_path").ok().flatten();
    let et = discover_exiftool(et_setting.as_deref());
    let par2 = discover_par2();
    let ml_worker = crate::intelligence::discover_ml_worker();
    Ok(serde_json::json!({
        "exiftool_path": et.map(|p| p.to_string_lossy().to_string()),
        "par2_path": par2.map(|p| p.to_string_lossy().to_string()),
        "ml_worker_path": ml_worker.map(|p| p.to_string_lossy().to_string()),
    }))
}

// ============================================================================
// Milestone 5: Integrity, Repair, Transcode Integration & Shared Tasks
// ============================================================================

#[tauri::command]
pub async fn integrity_verify_files(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    file_keys: Vec<String>,
    full_decode: bool,
) -> Result<Vec<IntegrityCheckRecord>, String> {
    let (ffmpeg_opt, ffprobe_opt) = tool_paths(&state);
    let task = state.task_manager.enqueue_task(
        "integrity_verification",
        "integrity",
        file_keys.clone(),
        4,
        "disk_io",
        serde_json::json!({ "full_decode": full_decode }),
    )?;
    let (cancel, pause) = state.task_manager.register_control_flags(&task.id);
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let sched = state.scheduler.clone();
    let tid = task.id.clone();

    tokio::task::spawn_blocking(move || {
        let total = file_keys.len().max(1);
        let mut out = Vec::new();
        for (i, fk) in file_keys.iter().enumerate() {
            tm.wait_for_budget_or_pause(&sched, Some(&pause), Some(&cancel));
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tm.update_task_progress(&tid, "canceled", (i as f64 / total as f64) * 100.0, None, None);
                let _ = app.emit("tasks_changed", ());
                return Ok(out);
            }
            if let Ok(rec) = verify_file_integrity(&db, ffmpeg_opt.as_deref(), ffprobe_opt.as_deref(), fk, full_decode) {
                out.push(rec);
            }
            let _ = tm.update_task_progress(&tid, "running", ((i + 1) as f64 / total as f64) * 100.0, Some(&format!("{}/{} verified", i + 1, total)), None);
        }
        let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&format!("{} verified", out.len())), None);
        let _ = app.emit("tasks_changed", ());
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn integrity_export_manifest(
    state: State<'_, AppState>,
    dest_path: String,
    format: String,
    file_keys: Option<Vec<String>>,
) -> Result<usize, String> {
    export_checksum_manifest_file(
        &state.scheduler.db,
        Path::new(&dest_path),
        &format,
        file_keys.as_deref(),
    )
}

#[tauri::command]
pub fn integrity_list_history(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<IntegrityCheckRecord>, String> {
    list_integrity_history(&state.scheduler.db, limit.unwrap_or(100))
}

#[tauri::command]
pub fn integrity_create_par2(target_path: String, redundancy_pct: Option<u32>) -> Result<String, String> {
    create_par2_parity_if_available(Path::new(&target_path), redundancy_pct.unwrap_or(10))
}

#[tauri::command]
pub fn repair_diagnose(
    state: State<'_, AppState>,
    path: String,
    file_key: Option<String>,
) -> Result<RepairDiagnosis, String> {
    let (ffmpeg_opt, ffprobe_opt) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    let fp = ffprobe_opt.ok_or_else(|| "FFprobe not configured.".to_string())?;
    diagnose_media_file(&state.scheduler.db, &ff, &fp, Path::new(&path), file_key.as_deref())
}

#[tauri::command]
pub async fn repair_execute(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    damaged_path: String,
    reference_path: Option<String>,
    output_dir: Option<String>,
    source_file_key: Option<String>,
) -> Result<RepairResult, String> {
    let (ffmpeg_opt, ffprobe_opt) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    let fp = ffprobe_opt.ok_or_else(|| "FFprobe not configured.".to_string())?;
    let task = state.task_manager.enqueue_task(
        "video_repair",
        "repair",
        source_file_key.clone().into_iter().collect(),
        2,
        "ffmpeg",
        serde_json::json!({ "damaged_path": damaged_path }),
    )?;
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let tid = task.id.clone();

    tokio::task::spawn_blocking(move || {
        let _ = tm.update_task_progress(&tid, "running", 15.0, Some("Diagnosing & recovering…"), None);
        let _ = app.emit("tasks_changed", ());
        let res = repair_damaged_video(
            &db,
            &ff,
            &fp,
            Path::new(&damaged_path),
            reference_path.as_deref().map(Path::new),
            output_dir.as_deref().map(Path::new),
            source_file_key.as_deref(),
        );
        match &res {
            Ok(r) => {
                let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&r.strategy_used), None);
            }
            Err(e) => {
                let _ = tm.update_task_progress(&tid, "failed", 0.0, None, Some(e));
            }
        }
        let _ = app.emit("tasks_changed", ());
        res
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn transcode_enqueue_from_catalog(
    state: State<'_, AppState>,
    queue_id: String,
    file_keys: Vec<String>,
) -> Result<usize, String> {
    let q = state
        .scheduler
        .db
        .get_queue(&queue_id)?
        .ok_or_else(|| "Target transcode queue not found.".to_string())?;
    let entries = crate::catalog::load_entries_by_file_keys(&state.scheduler.db, &file_keys)?;
    let map: HashMap<String, _> = entries
        .into_iter()
        .map(|e| (e.file_key.clone(), e))
        .collect();
    let (_, ffprobe_opt) = tool_paths(&state);
    let mut reserved: HashSet<String> = state
        .scheduler
        .db
        .list_all_jobs()?
        .into_iter()
        .map(|j| j.destination_path.to_ascii_lowercase())
        .collect();

    let mut pos = state.scheduler.db.next_job_position(&queue_id)?;
    let batch_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let mut added = 0usize;

    for fk in file_keys {
        let Some(entry) = map.get(&fk) else {
            continue;
        };
        if !entry.is_online || entry.media_type != "video" {
            continue;
        }
        let src_p = Path::new(&entry.path);
        let dest_root = q
            .destination_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| src_p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
        let rel = PathBuf::from(&entry.filename);
        let dest = resolve_destination_path_with_config(
            src_p,
            &rel,
            &dest_root,
            &q.recipe,
            &q.output_config,
            &mut reserved,
        )?;
        let dest_vol = dest
            .parent()
            .and_then(|p| disk_space_for_path(p).ok())
            .map(|v| v.volume_id);

        let (probe, plan, status, err_msg) = match ffprobe_opt.as_deref() {
            Some(fp) => match probe_file(fp, src_p) {
                Ok(pr) => {
                    let pl = build_track_plan(src_p, &pr, &q.recipe, &CompatOverrides::default());
                    let st = if pl.is_ready {
                        JobStatus::Listed
                    } else {
                        JobStatus::NeedsAttention
                    };
                    (Some(pr), Some(pl), st, None)
                }
                Err(e) => (None, None, JobStatus::NeedsAttention, Some(e)),
            },
            None => (None, None, JobStatus::Listed, None),
        };

        let job = JobRecord {
            id: Uuid::new_v4().to_string(),
            queue_id: queue_id.clone(),
            batch_id: batch_id.clone(),
            position: pos,
            source_path: entry.path.clone(),
            source_rel_path: entry.filename.clone(),
            source_root: Some(entry.parent_dir.clone()),
            source_fingerprint: capture_file_identity(src_p).ok(),
            destination_path: dest.to_string_lossy().to_string(),
            dest_volume_id: dest_vol,
            output_mode: q.output_config.mode,
            replace_behavior: q.output_config.replace_behavior,
            staged_path: None,
            staged_bytes: None,
            recipe: q.recipe.clone(),
            compat_overrides: CompatOverrides::default(),
            status,
            status_reason: None,
            probe,
            plan,
            input_bytes: entry.size_bytes,
            output_bytes: None,
            output_fingerprint: None,
            error_stage: err_msg.as_ref().map(|_| "preflight".to_string()),
            error_message: err_msg,
            diagnostics: None,
            cleanup_policy: q.cleanup_policy,
            cleanup_outcome: Default::default(),
            cleanup_error: None,
            consent_id: None,
            full_decode_verified: false,
            min_savings_enabled: q.min_savings_enabled,
            min_savings_pct: q.min_savings_pct,
            tool_versions: None,
            current_attempt_id: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        state.scheduler.db.upsert_job(&job)?;
        pos += 1;
        added += 1;
    }
    state.scheduler.emit_snapshot_changed();
    Ok(added)
}

#[tauri::command]
pub fn tasks_list(state: State<'_, AppState>) -> Result<Vec<TaskRecord>, String> {
    state.task_manager.list_tasks()
}

#[tauri::command]
pub fn tasks_pause(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.task_manager.pause_task(&task_id)
}

#[tauri::command]
pub fn tasks_resume(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.task_manager.resume_task(&task_id)
}

#[tauri::command]
pub fn tasks_cancel(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.task_manager.cancel_task(&task_id)
}

#[tauri::command]
pub fn tasks_set_priority(
    state: State<'_, AppState>,
    task_id: String,
    priority: i32,
) -> Result<(), String> {
    state.task_manager.set_task_priority(&task_id, priority)
}

#[tauri::command]
pub fn tasks_get_budgets(state: State<'_, AppState>) -> Result<ResourceBudgets, String> {
    state.task_manager.get_budgets()
}

#[tauri::command]
pub fn tasks_save_budgets(
    state: State<'_, AppState>,
    budgets: ResourceBudgets,
) -> Result<ResourceBudgets, String> {
    state.task_manager.save_budgets(budgets)
}

#[tauri::command]
pub fn cache_get_stats(state: State<'_, AppState>) -> Result<CacheStats, String> {
    get_cache_stats(&state.scheduler.db)
}

#[tauri::command]
pub fn cache_evict(state: State<'_, AppState>, max_bytes: u64) -> Result<u64, String> {
    evict_cache_to_budget(&state.scheduler.db, max_bytes)
}

#[tauri::command]
pub fn cache_clear(state: State<'_, AppState>) -> Result<u64, String> {
    clear_analysis_cache(&state.scheduler.db)
}

// ============================================================================
// Milestones 6 & 7: OCR, Watermarks, Semantic Search, Faces & Local LLM
// ============================================================================

#[tauri::command]
pub async fn ocr_analyze_files(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    file_keys: Vec<String>,
) -> Result<Vec<OcrDetectionRecord>, String> {
    let (ffmpeg_opt, _) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    let task = state.task_manager.enqueue_task(
        "ocr_watermark_analysis",
        "ocr",
        file_keys.clone(),
        5,
        "cpu",
        serde_json::json!({ "count": file_keys.len() }),
    )?;
    let (cancel, pause) = state.task_manager.register_control_flags(&task.id);
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let sched = state.scheduler.clone();
    let tid = task.id.clone();

    tokio::task::spawn_blocking(move || {
        let total = file_keys.len().max(1);
        let mut all = Vec::new();
        for (i, fk) in file_keys.iter().enumerate() {
            tm.wait_for_budget_or_pause(&sched, Some(&pause), Some(&cancel));
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tm.update_task_progress(&tid, "canceled", (i as f64 / total as f64) * 100.0, None, None);
                let _ = app.emit("tasks_changed", ());
                return Ok(all);
            }
            if let Ok(mut dets) = run_ocr_and_watermark_analysis(&db, &ff, fk) {
                all.append(&mut dets);
            }
            let _ = tm.update_task_progress(&tid, "running", ((i + 1) as f64 / total as f64) * 100.0, Some(&format!("{}/{} OCR", i + 1, total)), None);
        }
        let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&format!("{} detections", all.len())), None);
        let _ = app.emit("tasks_changed", ());
        Ok(all)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn ocr_list_detections(
    state: State<'_, AppState>,
    review_status: Option<String>,
) -> Result<Vec<OcrDetectionRecord>, String> {
    list_ocr_detections(&state.scheduler.db, review_status.as_deref())
}

#[tauri::command]
pub fn ocr_review_batch(
    state: State<'_, AppState>,
    detection_ids: Vec<String>,
    approve: bool,
    tag_prefix: Option<String>,
) -> Result<usize, String> {
    review_ocr_detections_batch(
        &state.scheduler.db,
        &detection_ids,
        approve,
        tag_prefix.as_deref(),
    )
}

#[tauri::command]
pub fn ocr_list_creator_aliases(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    get_creator_aliases(&state.scheduler.db)
}

#[tauri::command]
pub fn ocr_save_creator_alias(
    state: State<'_, AppState>,
    alias: String,
    canonical: String,
) -> Result<(), String> {
    save_creator_alias(&state.scheduler.db, &alias, &canonical)
}

#[tauri::command]
pub async fn semantic_index_files(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    file_keys: Vec<String>,
) -> Result<usize, String> {
    let (ffmpeg_opt, _) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    let task = state.task_manager.enqueue_task(
        "semantic_embedding_index",
        "semantic",
        file_keys.clone(),
        6,
        "cpu",
        serde_json::json!({ "count": file_keys.len() }),
    )?;
    let (cancel, pause) = state.task_manager.register_control_flags(&task.id);
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let sched = state.scheduler.clone();
    let tid = task.id.clone();

    tokio::task::spawn_blocking(move || {
        let total = file_keys.len().max(1);
        let mut count = 0usize;
        for (i, fk) in file_keys.iter().enumerate() {
            tm.wait_for_budget_or_pause(&sched, Some(&pause), Some(&cancel));
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tm.update_task_progress(&tid, "canceled", (i as f64 / total as f64) * 100.0, None, None);
                let _ = app.emit("tasks_changed", ());
                return Ok(count);
            }
            if let Ok(n) = index_semantic_embeddings(&db, &ff, fk) {
                count += n;
            }
            let _ = tm.update_task_progress(&tid, "running", ((i + 1) as f64 / total as f64) * 100.0, Some(&format!("{}/{} indexed", i + 1, total)), None);
        }
        let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&format!("{count} vectors")), None);
        let _ = app.emit("tasks_changed", ());
        Ok(count)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn semantic_search(
    state: State<'_, AppState>,
    query: String,
    filter: Option<CatalogFilter>,
    limit: Option<usize>,
) -> Result<Vec<SemanticSearchHit>, String> {
    semantic_media_search(&state.scheduler.db, &query, filter.as_ref(), limit.unwrap_or(50))
}

#[tauri::command]
pub fn faces_get_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    is_face_grouping_enabled(&state.scheduler.db)
}

#[tauri::command]
pub fn faces_set_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    set_face_grouping_enabled(&state.scheduler.db, enabled)
}

#[tauri::command]
pub async fn faces_run_grouping(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    file_keys: Vec<String>,
) -> Result<usize, String> {
    let (ffmpeg_opt, _) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    let task = state.task_manager.enqueue_task(
        "face_grouping",
        "faces",
        file_keys.clone(),
        6,
        "cpu",
        serde_json::json!({ "count": file_keys.len() }),
    )?;
    let db = state.scheduler.db.clone();
    let tm = state.task_manager.clone();
    let tid = task.id.clone();

    tokio::task::spawn_blocking(move || {
        let _ = tm.update_task_progress(&tid, "running", 15.0, Some("Detecting & grouping faces…"), None);
        let _ = app.emit("tasks_changed", ());
        let res = run_face_grouping_for_files(&db, &ff, &file_keys);
        match &res {
            Ok(n) => {
                let _ = tm.update_task_progress(&tid, "completed", 100.0, Some(&format!("{n} observations")), None);
            }
            Err(e) => {
                let _ = tm.update_task_progress(&tid, "failed", 0.0, None, Some(e));
            }
        }
        let _ = app.emit("tasks_changed", ());
        res
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn faces_list_groups(state: State<'_, AppState>) -> Result<Vec<FaceGroupRecord>, String> {
    list_face_groups(&state.scheduler.db)
}

#[tauri::command]
pub fn faces_list_observations(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<Vec<FaceObservationRecord>, String> {
    list_face_observations(&state.scheduler.db, &group_id)
}

#[tauri::command]
pub fn faces_label_group(
    state: State<'_, AppState>,
    group_id: String,
    label: String,
) -> Result<(), String> {
    label_face_group(&state.scheduler.db, &group_id, &label)
}

#[tauri::command]
pub fn faces_merge_groups(
    state: State<'_, AppState>,
    source_group_id: String,
    target_group_id: String,
) -> Result<(), String> {
    merge_face_groups(&state.scheduler.db, &source_group_id, &target_group_id)
}

#[tauri::command]
pub fn faces_split_observation(
    state: State<'_, AppState>,
    observation_id: String,
) -> Result<String, String> {
    split_face_observation(&state.scheduler.db, &observation_id)
}

#[tauri::command]
pub fn faces_exclude_observation(
    state: State<'_, AppState>,
    observation_id: String,
) -> Result<(), String> {
    exclude_face_observation(&state.scheduler.db, &observation_id)
}

#[tauri::command]
pub fn faces_purge_all_biometrics(state: State<'_, AppState>) -> Result<usize, String> {
    purge_all_biometric_data(&state.scheduler.db)
}

#[tauri::command]
pub fn llm_get_config(state: State<'_, AppState>) -> Result<LlmConfig, String> {
    get_llm_config(&state.scheduler.db)
}

#[tauri::command]
pub fn llm_save_config(state: State<'_, AppState>, config: LlmConfig) -> Result<(), String> {
    save_llm_config(&state.scheduler.db, &config)
}

#[tauri::command]
pub fn llm_propose_organization(
    state: State<'_, AppState>,
    instruction: String,
    file_keys: Vec<String>,
) -> Result<LlmOrganizationProposal, String> {
    generate_llm_organization_proposal(&state.scheduler.db, &instruction, &file_keys)
}

#[tauri::command]
pub fn analyze_black_frozen(
    state: State<'_, AppState>,
    file_key: String,
) -> Result<BlackFrozenReport, String> {
    let (ffmpeg_opt, _) = tool_paths(&state);
    let ff = ffmpeg_opt.ok_or_else(|| "FFmpeg not configured.".to_string())?;
    detect_black_and_frozen_frames(&state.scheduler.db, &ff, &file_key)
}
