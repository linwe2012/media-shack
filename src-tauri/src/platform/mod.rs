pub mod common;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::models::{ProcessPriority, SourceFingerprint};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published,
    DestinationExists,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct VolumeStats {
    pub volume_id: String,
    pub mount_point: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[cfg(target_os = "macos")]
pub use macos::{
    acquire_instance_lock, apply_process_priority, atomic_publish_no_replace,
    check_trash_support, destination_capabilities, disk_space_for_path,
    move_to_recoverable_trash_verified, run_supervisor_entrypoint, spawn_supervised_ffmpeg,
    InstanceLock, PowerAssertion, SupervisedChild,
};

#[cfg(target_os = "windows")]
pub use windows::{
    acquire_instance_lock, apply_process_priority, atomic_publish_no_replace,
    check_trash_support, destination_capabilities, disk_space_for_path,
    move_to_recoverable_trash_verified, run_supervisor_entrypoint, spawn_supervised_ffmpeg,
    InstanceLock, PowerAssertion, SupervisedChild,
};

pub fn capture_file_identity(path: &Path) -> Result<SourceFingerprint, String> {
    common::capture_file_identity_impl(path)
}

pub fn capture_file_identity_with_blake3(path: &Path) -> Result<SourceFingerprint, String> {
    common::capture_file_identity_with_blake3_impl(path)
}

pub fn verify_no_symlink_ancestors(path: &Path) -> Result<(), String> {
    common::verify_no_symlink_ancestors_impl(path)
}

pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to reveal file in Finder: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let arg = format!("/select,{}", path.display());
        std::process::Command::new("explorer")
            .arg(arg)
            .spawn()
            .map_err(|e| format!("Failed to reveal file in Explorer: {e}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Unsupported platform".to_string())
}

pub fn open_in_default_player(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {e}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Unsupported platform".to_string())
}

pub fn priority_label(p: ProcessPriority) -> &'static str {
    match p {
        ProcessPriority::Background => "Background",
        ProcessPriority::BelowNormal => "Below normal",
        ProcessPriority::Normal => "Normal",
    }
}

fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir_f) = std::fs::File::open(parent) {
            let _ = dir_f.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn path_exists_no_follow(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Cannot inspect destination {}: {e}", path.display())),
    }
}

fn copy_and_sync_file(src: &Path, dst: &Path, expected_bytes: u64) -> Result<(), String> {
    std::fs::copy(src, dst).map_err(|e| format!("Failed copying staged output to destination: {e}"))?;
    if let Ok(f) = std::fs::File::open(dst) {
        let _ = f.sync_all();
    }
    let copied_len = std::fs::metadata(dst)
        .map_err(|e| format!("Failed verifying copied output: {e}"))?
        .len();
    if copied_len != expected_bytes {
        let _ = std::fs::remove_file(dst);
        return Err(format!(
            "Copied output size mismatch ({copied_len} bytes != expected {expected_bytes} bytes)."
        ));
    }
    Ok(())
}

/// Publish a verified staged file to `final_path` without overwriting an existing destination.
/// First tries strict OS kernel no-replace (`renameatx_np(RENAME_EXCL)` / `MoveFileExW`).
/// If `allow_fault_tolerant_fallback` is true and the filesystem returns ENOTSUP (e.g. exFAT/SMB on macOS)
/// or cross-volume move (`EXDEV`), uses a verified non-overwriting fallback while always preserving `staged_path` on failure.
pub fn publish_staged_file(
    staged_path: &Path,
    final_path: &Path,
    allow_fault_tolerant_fallback: bool,
) -> PublishOutcome {
    match atomic_publish_no_replace(staged_path, final_path) {
        PublishOutcome::Published => PublishOutcome::Published,
        PublishOutcome::DestinationExists => PublishOutcome::DestinationExists,
        PublishOutcome::Error(orig_err) => {
            if !allow_fault_tolerant_fallback {
                return PublishOutcome::Error(orig_err);
            }
            if let Some(parent) = final_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return PublishOutcome::Error(format!(
                        "Failed to create destination directory: {e}"
                    ));
                }
            }
            match path_exists_no_follow(final_path) {
                Ok(true) => return PublishOutcome::DestinationExists,
                Ok(false) => {}
                Err(e) => return PublishOutcome::Error(e),
            }
            let staged_size = match std::fs::metadata(staged_path) {
                Ok(m) => m.len(),
                Err(e) => {
                    return PublishOutcome::Error(format!(
                        "Cannot inspect staged output before publish: {e}"
                    ))
                }
            };
            if let Ok(f) = std::fs::File::open(staged_path) {
                let _ = f.sync_all();
            }

            // Re-check immediately before fallback rename
            if let Ok(true) = path_exists_no_follow(final_path) {
                return PublishOutcome::DestinationExists;
            }
            if std::fs::rename(staged_path, final_path).is_ok() {
                sync_parent_dir(final_path);
                if std::fs::metadata(final_path).map(|m| m.len()).ok() == Some(staged_size) {
                    return PublishOutcome::Published;
                }
                return PublishOutcome::Error(format!(
                    "Published output verification failed after move to {}.",
                    final_path.display()
                ));
            }

            // Fallback for cross-volume moves (EXDEV) or filesystems rejecting direct rename:
            // Copy into hidden temp on target directory, fsync + verify size, then rename into final_path.
            let Some(parent) = final_path.parent() else {
                return PublishOutcome::Error("Destination has no parent directory.".to_string());
            };
            let temp_target = parent.join(format!(".ffq-pub-{}.tmp", uuid::Uuid::new_v4()));
            if let Err(e) = copy_and_sync_file(staged_path, &temp_target, staged_size) {
                let _ = std::fs::remove_file(&temp_target);
                return PublishOutcome::Error(e);
            }
            if let Ok(true) = path_exists_no_follow(final_path) {
                let _ = std::fs::remove_file(&temp_target);
                return PublishOutcome::DestinationExists;
            }
            if let Err(e) = std::fs::rename(&temp_target, final_path) {
                let _ = std::fs::remove_file(&temp_target);
                return PublishOutcome::Error(format!(
                    "Fault-tolerant publish failed moving temp file into destination ({orig_err}): {e}"
                ));
            }
            sync_parent_dir(final_path);
            if std::fs::metadata(final_path).map(|m| m.len()).ok() != Some(staged_size) {
                return PublishOutcome::Error(
                    "Destination file size verification failed after copy.".to_string(),
                );
            }
            let _ = std::fs::remove_file(staged_path);
            PublishOutcome::Published
        }
    }
}

/// Fault-tolerant in-place replacement:
/// 1. Stage verified output onto the target directory (`tmp_new`) first so cross-volume copies never risk originals.
/// 2. Move original (`source_path`) and any colliding `final_path` to safe `.ffq-orig` / `.ffq-prev` backups.
/// 3. Rename `tmp_new -> final_path` and verify exact size; on any error, roll back backups & restore staged file.
/// 4. After `final_path` is verified in place, either keep readable `.orig` backup or remove/Trash original backup.
pub fn fault_tolerant_replace_file(
    staged_path: &Path,
    source_path: &Path,
    final_path: &Path,
    expected_staged_bytes: u64,
    expected_source_fp: Option<&SourceFingerprint>,
    prefer_trash: bool,
    keep_backup: bool,
) -> Result<Option<PathBuf>, String> {
    let dest_parent = final_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "Destination has no parent directory.".to_string())?;
    std::fs::create_dir_all(dest_parent)
        .map_err(|e| format!("Failed creating destination folder: {e}"))?;

    let staged_len = std::fs::metadata(staged_path)
        .map_err(|e| format!("Cannot stat staged output: {e}"))?
        .len();
    if expected_staged_bytes > 0 && staged_len != expected_staged_bytes {
        return Err(format!(
            "Staged file size mismatch ({staged_len} != expected {expected_staged_bytes}); aborted safely."
        ));
    }
    if let Ok(f) = std::fs::File::open(staged_path) {
        let _ = f.sync_all();
    }

    let tmp_new = dest_parent.join(format!(".ffq-new-{}.tmp", uuid::Uuid::new_v4()));
    let moved_from_staged = if std::fs::rename(staged_path, &tmp_new).is_ok() {
        true
    } else {
        copy_and_sync_file(staged_path, &tmp_new, staged_len)?;
        false
    };

    let same_path = source_path == final_path;
    let mut orig_backup: Option<PathBuf> = None;
    let mut prev_backup: Option<PathBuf> = None;

    // Move original out of the way to a same-volume backup if replacing same path or if backup requested / non-Trash volume
    if same_path && path_exists_no_follow(source_path).unwrap_or(false) {
        let src_dir = source_path.parent().unwrap_or(dest_parent);
        let bak = src_dir.join(format!(".ffq-orig-{}.bak", uuid::Uuid::new_v4()));
        if let Err(e) = std::fs::rename(source_path, &bak) {
            if moved_from_staged {
                let _ = std::fs::rename(&tmp_new, staged_path);
            } else {
                let _ = std::fs::remove_file(&tmp_new);
            }
            return Err(format!(
                "Cannot move original aside before replacing: {e}. Staged output preserved."
            ));
        }
        orig_backup = Some(bak);
    } else if !same_path && path_exists_no_follow(final_path).unwrap_or(false) {
        let bak = dest_parent.join(format!(".ffq-prev-{}.bak", uuid::Uuid::new_v4()));
        if let Err(e) = std::fs::rename(final_path, &bak) {
            if moved_from_staged {
                let _ = std::fs::rename(&tmp_new, staged_path);
            } else {
                let _ = std::fs::remove_file(&tmp_new);
            }
            return Err(format!(
                "Cannot move existing destination file aside: {e}. Staged output preserved."
            ));
        }
        prev_backup = Some(bak);
    }

    // Move staged tmp_new into final_path
    if let Err(e) = std::fs::rename(&tmp_new, final_path) {
        if let Some(ref b) = orig_backup {
            let _ = std::fs::rename(b, source_path);
        }
        if let Some(ref pb) = prev_backup {
            let _ = std::fs::rename(pb, final_path);
        }
        if moved_from_staged {
            let _ = std::fs::rename(&tmp_new, staged_path);
        } else {
            let _ = std::fs::remove_file(&tmp_new);
        }
        return Err(format!(
            "Failed moving new file into {}: {e}. Original restored and staged file preserved.",
            final_path.display()
        ));
    }
    sync_parent_dir(final_path);

    let final_ok = std::fs::metadata(final_path).map(|m| m.len()).ok() == Some(staged_len);
    if !final_ok {
        let failed_tmp = dest_parent.join(format!(".ffq-failed-{}.tmp", uuid::Uuid::new_v4()));
        let _ = std::fs::rename(final_path, &failed_tmp);
        if let Some(ref b) = orig_backup {
            let _ = std::fs::rename(b, source_path);
        }
        if let Some(ref pb) = prev_backup {
            let _ = std::fs::rename(pb, final_path);
        }
        if moved_from_staged {
            let _ = std::fs::rename(&failed_tmp, staged_path);
        }
        return Err("Replaced output failed verification check; original restored.".to_string());
    }

    // Replacement verified on disk! Clean up staged source (if copied) and previous target backup.
    if !moved_from_staged {
        let _ = std::fs::remove_file(staged_path);
    }
    if let Some(pb) = prev_backup {
        let _ = std::fs::remove_file(pb);
    }

    if keep_backup {
        if let Some(bak) = orig_backup {
            let src_dir = source_path.parent().unwrap_or(dest_parent);
            let stem = source_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "video".to_string());
            let ext = source_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp4");
            let mut chosen = src_dir.join(format!("{stem}.orig.{ext}"));
            for n in 2u32..1000 {
                if !path_exists_no_follow(&chosen).unwrap_or(true) {
                    break;
                }
                chosen = src_dir.join(format!("{stem}.orig ({n}).{ext}"));
            }
            if std::fs::rename(&bak, &chosen).is_ok() {
                return Ok(Some(chosen));
            }
            return Ok(Some(bak));
        }
        return Ok(Some(source_path.to_path_buf()));
    }

    // Delete or Trash original after verified replacement
    if let Some(bak) = orig_backup {
        let _ = std::fs::remove_file(bak);
    } else if !same_path && path_exists_no_follow(source_path).unwrap_or(false) {
        let trashed = if prefer_trash {
            if let Some(fp) = expected_source_fp {
                move_to_recoverable_trash_verified(source_path, fp).is_ok()
            } else {
                false
            }
        } else {
            false
        };
        if !trashed {
            let _ = std::fs::remove_file(source_path);
        }
    }

    Ok(None)
}
