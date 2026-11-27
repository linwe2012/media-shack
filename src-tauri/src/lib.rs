pub mod cleanup;
pub mod commands;
pub mod db;
pub mod models;
pub mod planner;
pub mod platform;
pub mod preview;
pub mod runner;
pub mod scanner;
pub mod scheduler;
pub mod tools;
pub mod verify;

use commands::AppState;
use db::Database;
use platform::acquire_instance_lock;
use scheduler::Scheduler;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
                        let mime = if cand_c.extension().and_then(|e| e.to_str()) == Some("mkv") {
                            "video/x-matroska"
                        } else {
                            "video/mp4"
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
        .setup(|app| {
            let app_data_dir: PathBuf = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("media-shack"));
            let instance_lock = acquire_instance_lock(&app_data_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::AlreadyExists, e))?;
            let database = Database::open(&app_data_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let scheduler = Arc::new(Scheduler::new(database));

            let app_handle = app.handle().clone();
            scheduler.set_event_sink(move |name, payload| {
                let _ = app_handle.emit(name, payload);
            });
            scheduler.start_background_loop();

            app.manage(AppState {
                scheduler,
                active_scans: Arc::new(Mutex::new(HashMap::new())),
                _instance_lock: Arc::new(Mutex::new(Some(instance_lock))),
            });
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
            commands::check_close_status,
            commands::stop_and_quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Media Shack");
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
