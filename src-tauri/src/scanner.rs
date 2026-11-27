use crate::models::*;
use crate::planner::{
    build_track_plan, derive_destination_root_from_job, is_blocked_input_path, is_video_hint_ext,
    probe_file, resolve_destination_path,
};
use crate::platform::capture_file_identity;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use uuid::Uuid;

pub fn is_app_staging_or_partial(path: &Path) -> bool {
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy();
        if s == ".ffq-staging" || s.ends_with(".ffq-part") {
            return true;
        }
    }
    false
}

pub fn scan_import_paths(
    ffprobe_path: Option<&Path>,
    queue: &QueueTab,
    existing_queue_jobs: &[JobRecord],
    input_paths: &[PathBuf],
    recursive: bool,
    destination_root_override: Option<&str>,
    cancel_flag: &Arc<AtomicBool>,
) -> ImportScanResult {
    let scan_id = Uuid::new_v4().to_string();
    let dest_root_str = destination_root_override
        .map(str::to_string)
        .or_else(|| queue.destination_root.clone())
        .unwrap_or_else(|| default_destination_root_for(input_paths));
    let dest_root_path = PathBuf::from(&dest_root_str);
    let dest_root_canon = dest_root_path.canonicalize().ok();

    let mut discovered: Vec<(PathBuf, PathBuf, Option<String>)> = Vec::new();
    let mut skipped_count = 0usize;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();

    for raw in input_paths {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(raw) else {
            skipped_count += 1;
            continue;
        };
        if meta.file_type().is_symlink() {
            skipped_count += 1;
            continue;
        }
        if meta.is_dir() {
            let root_canon = raw.canonicalize().unwrap_or_else(|_| raw.clone());
            // Skip if input directory is inside destination tree
            if let Some(ref dc) = dest_root_canon {
                if root_canon.starts_with(dc) {
                    skipped_count += 1;
                    continue;
                }
            }
            let root_name = raw
                .file_name()
                .map(|n| PathBuf::from(n))
                .unwrap_or_else(|| PathBuf::from("import"));
            walk_dir_incremental(
                raw,
                raw,
                &root_name,
                dest_root_canon.as_deref(),
                recursive,
                cancel_flag,
                &mut visited_dirs,
                &mut discovered,
                &mut skipped_count,
            );
        } else if meta.is_file() {
            if is_app_staging_or_partial(raw) {
                skipped_count += 1;
                continue;
            }
            if let Some(ref dc) = dest_root_canon {
                if let Ok(fc) = raw.canonicalize() {
                    if fc.starts_with(dc) {
                        skipped_count += 1;
                        continue;
                    }
                }
            }
            let rel = raw
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("video"));
            discovered.push((raw.clone(), rel, None));
        }
    }

    // Build local-to-queue duplicate map: (file_id, recipe_json, destination_root)
    let mut existing_keys: HashSet<String> = HashSet::new();
    let mut reserved_dests: HashSet<String> = HashSet::new();
    for j in existing_queue_jobs {
        reserved_dests.insert(j.destination_path.to_ascii_lowercase());
        let existing_root = derive_destination_root_from_job(
            Path::new(&j.destination_path),
            Path::new(&j.source_rel_path),
        )
        .to_string_lossy()
        .to_string();
        if let Some(ref fp) = j.source_fingerprint {
            let key = format!(
                "{}|{}|{}",
                fp.file_id,
                serde_json::to_string(&j.recipe).unwrap_or_default(),
                existing_root
            );
            existing_keys.insert(key);
        } else {
            let key = format!(
                "path:{}|{}|{}",
                j.source_path,
                serde_json::to_string(&j.recipe).unwrap_or_default(),
                existing_root
            );
            existing_keys.insert(key);
        }
    }

    let recipe_json = serde_json::to_string(&queue.recipe).unwrap_or_default();
    let mut items = Vec::new();
    let mut unreadable_count = 0usize;

    for (abs_path, rel_path, source_root) in discovered {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        if let Some(blocked) = is_blocked_input_path(&abs_path) {
            unreadable_count += 1;
            items.push(StagedImportItem {
                source_path: abs_path.to_string_lossy().to_string(),
                source_rel_path: rel_path.to_string_lossy().to_string(),
                source_root,
                destination_path: String::new(),
                input_bytes: 0,
                bit_rate: None,
                current_codec: None,
                duration_secs: None,
                duplicate_in_queue: false,
                destination_collision: false,
                include: false,
                probe: None,
                plan: None,
                unreadable_error: Some(blocked),
            });
            continue;
        }

        let fp = capture_file_identity(&abs_path);
        let input_bytes = fp
            .as_ref()
            .map(|f| f.size_bytes)
            .unwrap_or_else(|_| std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0));

        let dup_key = match &fp {
            Ok(f) => format!("{}|{}|{}", f.file_id, recipe_json, dest_root_str),
            Err(_) => format!(
                "path:{}|{}|{}",
                abs_path.display(),
                recipe_json,
                dest_root_str
            ),
        };
        let duplicate_in_queue = !existing_keys.insert(dup_key);

        let initial_dest = {
            let rel_parent = rel_path.parent().unwrap_or_else(|| Path::new(""));
            let stem = abs_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "video".to_string());
            dest_root_path.join(rel_parent).join(format!(
                "{stem}.{}.{}",
                queue.recipe.filename_tag(),
                queue.recipe.container.ext()
            ))
        };
        let would_collide = initial_dest.exists()
            || reserved_dests.contains(&initial_dest.to_string_lossy().to_ascii_lowercase());

        let dest_path = resolve_destination_path(
            &abs_path,
            &rel_path,
            &dest_root_path,
            &queue.recipe,
            &mut reserved_dests,
        );

        let (probe, plan, unreadable_error) = match ffprobe_path {
            Some(fp_bin) => match probe_file(fp_bin, &abs_path) {
                Ok(pr) => {
                    let pl = build_track_plan(
                        &abs_path,
                        &pr,
                        &queue.recipe,
                        &CompatOverrides::default(),
                    );
                    (Some(pr), Some(pl), None)
                }
                Err(err) => {
                    unreadable_count += 1;
                    (None, None, Some(err))
                }
            },
            None => (
                None,
                None,
                Some("ffprobe not configured yet; locate FFmpeg tools to inspect.".to_string()),
            ),
        };

        let bit_rate = probe.as_ref().and_then(|p| p.format.bit_rate);
        let current_codec = probe.as_ref().and_then(|p| {
            p.streams
                .iter()
                .find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0)
                .map(|s| s.codec_name.clone())
        });
        let duration_secs = probe.as_ref().and_then(|p| p.format.duration_secs);

        let include = !duplicate_in_queue && unreadable_error.is_none();

        items.push(StagedImportItem {
            source_path: abs_path.to_string_lossy().to_string(),
            source_rel_path: rel_path.to_string_lossy().to_string(),
            source_root,
            destination_path: dest_path.to_string_lossy().to_string(),
            input_bytes,
            bit_rate,
            current_codec,
            duration_secs,
            duplicate_in_queue,
            destination_collision: would_collide,
            include,
            probe,
            plan,
            unreadable_error,
        });
    }

    ImportScanResult {
        scan_id,
        queue_id: queue.id.clone(),
        queue_name: queue.name.clone(),
        profile_name: queue.profile_name.clone(),
        destination_root: dest_root_str,
        recipe: queue.recipe.clone(),
        items,
        skipped_count,
        unreadable_count,
        canceled: cancel_flag.load(Ordering::Relaxed),
    }
}

fn walk_dir_incremental(
    base_dir: &Path,
    cur_dir: &Path,
    base_name: &Path,
    dest_root_canon: Option<&Path>,
    recursive: bool,
    cancel_flag: &Arc<AtomicBool>,
    visited_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, PathBuf, Option<String>)>,
    skipped_count: &mut usize,
) {
    if cancel_flag.load(Ordering::Relaxed) {
        return;
    }
    let Ok(canon) = cur_dir.canonicalize() else {
        *skipped_count += 1;
        return;
    };
    if !visited_dirs.insert(canon.clone()) {
        return;
    }
    if let Some(dc) = dest_root_canon {
        if canon.starts_with(dc) {
            return;
        }
    }
    if is_app_staging_or_partial(cur_dir) {
        return;
    }

    let Ok(entries) = std::fs::read_dir(cur_dir) else {
        *skipped_count += 1;
        return;
    };
    let mut sorted_entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    sorted_entries.sort();

    for path in sorted_entries {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            *skipped_count += 1;
            continue;
        };
        // Skip symlink directories and symlink files
        if meta.file_type().is_symlink() {
            *skipped_count += 1;
            continue;
        }
        if meta.is_dir() {
            if recursive {
                walk_dir_incremental(
                    base_dir,
                    &path,
                    base_name,
                    dest_root_canon,
                    recursive,
                    cancel_flag,
                    visited_dirs,
                    out,
                    skipped_count,
                );
            }
        } else if meta.is_file() {
            if is_app_staging_or_partial(&path) {
                continue;
            }
            if is_video_hint_ext(&path) {
                let rel_inner = path.strip_prefix(base_dir).unwrap_or(&path);
                let rel = base_name.join(rel_inner);
                out.push((
                    path,
                    rel,
                    Some(base_dir.to_string_lossy().to_string()),
                ));
            }
        }
    }
}

pub fn default_destination_root_for(input_paths: &[PathBuf]) -> String {
    if let Some(first) = input_paths.first() {
        let base = if first.is_dir() {
            first.parent().unwrap_or(first)
        } else {
            first.parent().unwrap_or_else(|| Path::new("."))
        };
        return base
            .join("FFmpeg Queue Output")
            .to_string_lossy()
            .to_string();
    }
    std::env::temp_dir()
        .join("FFmpeg Queue Output")
        .to_string_lossy()
        .to_string()
}
