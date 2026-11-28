pub mod common;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::models::{ProcessPriority, SourceFingerprint};
use std::path::Path;

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
    check_trash_support, disk_space_for_path, move_to_recoverable_trash_verified,
    run_supervisor_entrypoint, spawn_supervised_ffmpeg, InstanceLock, PowerAssertion,
    SupervisedChild,
};

#[cfg(target_os = "windows")]
pub use windows::{
    acquire_instance_lock, apply_process_priority, atomic_publish_no_replace,
    check_trash_support, disk_space_for_path, move_to_recoverable_trash_verified,
    run_supervisor_entrypoint, spawn_supervised_ffmpeg, InstanceLock, PowerAssertion,
    SupervisedChild,
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
