pub mod analysis;
pub mod catalog;
pub mod catalog_commands;
pub mod cleanup;
pub mod commands;
pub mod db;
pub mod duplicates;
pub mod integrity;
pub mod intelligence;
pub mod metadata_interop;
pub mod models;
pub mod organize;
pub mod planner;
pub mod platform;
pub mod preview;
pub mod recovery;
pub mod repair;
pub mod runner;
pub mod scanner;
pub mod scheduler;
pub mod tasks;
pub mod tools;
pub mod updater;
pub mod verify;

use commands::AppState;
use db::Database;
use platform::acquire_instance_lock;
use scheduler::Scheduler;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tasks::TaskManager;
use tauri::{Emitter, Manager};
use tauri_plugin_decorum::WebviewWindowExt;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_decorum::init())
        .register_uri_scheme_protocol("preview", |ctx, req| {
            // Serve sample media files strictly from the app's preview directory with full HTTP Range support
            let state = ctx.app_handle().state::<AppState>();
            let preview_root = state.scheduler.previews.preview_dir.clone();
            let raw_path = req.uri().path().trim_start_matches('/');
            let decoded = percent_decode(raw_path);
            let candidate = preview_root.join(decoded);
            if let (Ok(root_c), Ok(cand_c)) = (preview_root.canonicalize(), candidate.canonicalize()) {
                if cand_c.starts_with(&root_c) && cand_c.is_file() {
                    if let Ok(bytes) = std::fs::read(&cand_c) {
                        let total_len = bytes.len();
                        let mime = match cand_c.extension().and_then(|e| e.to_str()) {
                            Some("mkv") => "video/x-matroska",
                            Some("jpg") | Some("jpeg") => "image/jpeg",
                            _ => "video/mp4",
                        };
                        if let Some(range_hdr) = req.headers().get("range").and_then(|v| v.to_str().ok()) {
                            if let Some((start, end)) = parse_byte_range(range_hdr, total_len) {
                                let slice = bytes[start..=end].to_vec();
                                return tauri::http::Response::builder()
                                    .status(206)
                                    .header("Content-Type", mime)
                                    .header("Accept-Ranges", "bytes")
                                    .header("Content-Range", format!("bytes {start}-{end}/{total_len}"))
                                    .header("Content-Length", slice.len().to_string())
                                    .body(slice)
                                    .unwrap();
                            }
                        }
                        return tauri::http::Response::builder()
                            .status(200)
                            .header("Content-Type", mime)
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", total_len.to_string())
                            .body(bytes)
                            .unwrap();
                    }
                }
            }
            tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap()
        })
        .register_uri_scheme_protocol("thumb", |ctx, req| {
            // Serve cached thumbnails and face crops strictly from analysis_cache directory
            let state = ctx.app_handle().state::<AppState>();
            let cache_root = crate::analysis::analysis_cache_dir(&state.scheduler.db);
            let raw_path = req.uri().path().trim_start_matches('/');
            let decoded = percent_decode(raw_path);
            let candidate = cache_root.join(decoded);
            if let (Ok(root_c), Ok(cand_c)) = (cache_root.canonicalize(), candidate.canonicalize()) {
                if cand_c.starts_with(&root_c) && cand_c.is_file() {
                    if let Ok(bytes) = std::fs::read(&cand_c) {
                        let mime = match cand_c.extension().and_then(|e| e.to_str()) {
                            Some("png") => "image/png",
                            Some("webp") => "image/webp",
                            _ => "image/jpeg",
                        };
                        return tauri::http::Response::builder()
                            .status(200)
                            .header("Content-Type", mime)
                            .header("Content-Length", bytes.len().to_string())
                            .body(bytes)
                            .unwrap();
                    }
                }
            }
            tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap()
        })
        .setup(|app| {
            let app_data_dir: PathBuf = resolve_app_data_dir(app);
            let _ = std::fs::create_dir_all(&app_data_dir);
            let instance_lock = acquire_instance_lock(&app_data_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::AlreadyExists, e))?;
            let database = Database::open(&app_data_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let _ = crate::organize::seed_builtin_collections(&database);
            let task_manager = Arc::new(TaskManager::new(database.clone()));
            let _ = task_manager.reconcile_interrupted_tasks_on_launch();
            let scheduler = Arc::new(Scheduler::new(database.clone()));

            let sched_for_reconcile = scheduler.clone();
            crate::catalog::start_catalog_reconciler(
                database,
                Arc::new(move || {
                    sched_for_reconcile
                        .tool_status
                        .lock()
                        .ok()
                        .and_then(|t| t.capabilities.as_ref().map(|c| PathBuf::from(&c.ffprobe_path)))
                }),
            );

            let app_handle = app.handle().clone();
            scheduler.set_event_sink(move |name, payload| {
                let _ = app_handle.emit(name, payload);
            });
            scheduler.start_background_loop();

            app.manage(AppState {
                scheduler,
                task_manager,
                active_scans: Arc::new(Mutex::new(HashMap::new())),
                _instance_lock: Arc::new(Mutex::new(Some(instance_lock))),
            });

            if let Some(main_window) = app.get_webview_window("main") {
                let _ = main_window.create_overlay_titlebar();
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::locate_ffmpeg_tools,
            commands::recheck_tools,
            commands::create_queue_tab,
            commands::rename_queue_tab,
            commands::delete_queue_tab,
            commands::select_queue_tab,
            commands::set_queue_tab_type,
            commands::update_repair_session,
            commands::run_repair_diagnosis,
            commands::start_video_repair,
            commands::export_repair_report,
            commands::update_queue_settings,
            commands::apply_queue_settings_to_waiting,
            commands::save_named_preset,
            commands::delete_named_preset,
            commands::save_profile_from_queue,
            commands::rename_profile,
            commands::duplicate_profile,
            commands::delete_profile,
            commands::select_profile_for_queue,
            commands::start_import_scan,
            commands::cancel_import_scan,
            commands::commit_staged_import,
            commands::update_pending_job,
            commands::reorder_jobs,
            commands::remove_pending_jobs,
            commands::start_queue,
            commands::enqueue_jobs,
            commands::dequeue_jobs,
            commands::start_all_ready_queues,
            commands::pause_after_current,
            commands::resume_queue,
            commands::cancel_current_file,
            commands::retry_jobs,
            commands::reveal_output,
            commands::open_output_player,
            commands::set_volume_reserve,
            commands::recheck_volumes,
            commands::get_cleanup_candidates,
            commands::trash_selected_originals,
            commands::request_sample_preview,
            commands::get_comparison_frame,
            commands::start_live_preview,
            commands::stop_live_preview,
            commands::get_live_preview_frame,
            commands::get_live_comparison_bundle,
            commands::seek_live_preview,
            commands::get_sample_comparison_pair,
            commands::check_close_status,
            commands::stop_and_quit,
            catalog_commands::catalog_add_root,
            catalog_commands::catalog_list_roots,
            catalog_commands::catalog_remove_root,
            catalog_commands::catalog_scan_root,
            catalog_commands::catalog_refresh_volumes,
            catalog_commands::catalog_query,
            catalog_commands::catalog_get_asset_detail,
            catalog_commands::catalog_get_stats,
            catalog_commands::catalog_update_asset_metadata,
            catalog_commands::catalog_backup_database,
            catalog_commands::tags_list,
            catalog_commands::tags_suggest,
            catalog_commands::tags_create,
            catalog_commands::tags_add_alias,
            catalog_commands::tags_merge,
            catalog_commands::tags_assign_bulk,
            catalog_commands::tags_remove_bulk,
            catalog_commands::collections_list,
            catalog_commands::collections_save,
            catalog_commands::collections_delete,
            catalog_commands::collections_evaluate,
            catalog_commands::organize_preview_rename,
            catalog_commands::organize_preview_move_copy,
            catalog_commands::organize_execute_batch,
            catalog_commands::organize_list_history,
            catalog_commands::organize_undo,
            catalog_commands::duplicates_run_scan,
            catalog_commands::duplicates_list_groups,
            catalog_commands::duplicates_dismiss_edge,
            catalog_commands::duplicates_merge_tags,
            catalog_commands::duplicates_trash_copies,
            catalog_commands::metadata_sync_os_tags,
            catalog_commands::metadata_write_embedded,
            catalog_commands::metadata_export_sidecars,
            catalog_commands::metadata_import_sidecar,
            catalog_commands::metadata_check_tools,
            catalog_commands::integrity_verify_files,
            catalog_commands::integrity_export_manifest,
            catalog_commands::integrity_list_history,
            catalog_commands::integrity_create_par2,
            catalog_commands::repair_diagnose,
            catalog_commands::repair_execute,
            catalog_commands::transcode_enqueue_from_catalog,
            catalog_commands::tasks_list,
            catalog_commands::tasks_pause,
            catalog_commands::tasks_resume,
            catalog_commands::tasks_cancel,
            catalog_commands::tasks_set_priority,
            catalog_commands::tasks_get_budgets,
            catalog_commands::tasks_save_budgets,
            catalog_commands::cache_get_stats,
            catalog_commands::cache_evict,
            catalog_commands::cache_clear,
            catalog_commands::ocr_analyze_files,
            catalog_commands::ocr_list_detections,
            catalog_commands::ocr_review_batch,
            catalog_commands::ocr_list_creator_aliases,
            catalog_commands::ocr_save_creator_alias,
            catalog_commands::semantic_index_files,
            catalog_commands::semantic_search,
            catalog_commands::faces_get_enabled,
            catalog_commands::faces_set_enabled,
            catalog_commands::faces_run_grouping,
            catalog_commands::faces_list_groups,
            catalog_commands::faces_list_observations,
            catalog_commands::faces_label_group,
            catalog_commands::faces_merge_groups,
            catalog_commands::faces_split_observation,
            catalog_commands::faces_exclude_observation,
            catalog_commands::faces_purge_all_biometrics,
            catalog_commands::llm_get_config,
            catalog_commands::llm_save_config,
            catalog_commands::llm_propose_organization,
            catalog_commands::analyze_black_frozen,
            commands::set_tab_workspace_meta,
            commands::set_job_decision,
            commands::forward_resources_to_tab,
            commands::run_tab_dedupe_scan,
            commands::set_tab_dedupe_role,
            commands::trash_tab_duplicates,
            commands::run_tab_repair_diagnosis,
            commands::run_tab_repair_execute,
            commands::export_tab_repair_report,
            commands::get_hash_cache_stats,
            commands::clear_hash_cache,
            commands::trash_tab_specific_items,
            commands::preview_tab_trash,
            commands::execute_tab_trash_plan,
            commands::check_path_trash_support,
            commands::read_local_media_preview,
            commands::tab_get_organize_overview,
            commands::tab_extract_watermarks,
            commands::tab_tag_resources,
            commands::tab_untag_resource,
            commands::tab_run_face_grouping,
            commands::tab_label_face_and_tag,
            commands::tab_move_by_tag,
            commands::check_for_updates,
            commands::download_and_install_update,
            commands::open_external_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Media Shack");
}

fn resolve_app_data_dir(app: &tauri::App) -> PathBuf {
    if let Some(custom) = std::env::var_os("MEDIA_SHACK_DATA_DIR") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            if exe_dir.join("portable.flag").is_file()
                || exe_dir.join("portable.txt").is_file()
                || exe_dir.join("data").is_dir()
            {
                return exe_dir.join("data");
            }
        }
    }
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("media-shack"))
}

fn parse_byte_range(header: &str, total_len: usize) -> Option<(usize, usize)> {
    if total_len == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (s, e) = spec.split_once('-')?;
    if s.is_empty() {
        let suffix: usize = e.trim().parse().ok()?;
        let start = total_len.saturating_sub(suffix);
        return Some((start, total_len - 1));
    }
    let start: usize = s.trim().parse().ok()?;
    if start >= total_len {
        return None;
    }
    let end = if e.trim().is_empty() {
        total_len - 1
    } else {
        e.trim().parse::<usize>().ok()?.min(total_len - 1)
    };
    if start <= end {
        Some((start, end))
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
