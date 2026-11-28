use crate::db::Database;
use crate::models::ProbeResult;
use crate::planner::{is_blocked_input_path, probe_file};
use crate::platform::{capture_file_identity, disk_space_for_path};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use uuid::Uuid;

pub const MEDIA_VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "avi", "webm", "mts", "m2ts", "ts", "mpg", "mpeg", "wmv",
    "flv", "3gp", "3g2", "vob", "ogv", "mxf", "dv", "divx", "asf", "rmvb",
];

pub const MEDIA_IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "heic", "heif", "bmp", "tif", "tiff", "gif",
];

pub const MEDIA_AUDIO_EXTS: &[&str] = &[
    "mp3", "aac", "m4a", "flac", "wav", "ogg", "opus", "wma", "alac", "aiff",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(i32)]
pub enum AnalysisLevel {
    Level0Filesystem = 0,
    Level1Ffprobe = 1,
    Level2ThumbnailFingerprint = 2,
    Level3HashesPerceptual = 3,
    Level4OcrSemantic = 4,
    Level5DeepIntegrity = 5,
}

impl AnalysisLevel {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Level0Filesystem,
            1 => Self::Level1Ffprobe,
            2 => Self::Level2ThumbnailFingerprint,
            3 => Self::Level3HashesPerceptual,
            4 => Self::Level4OcrSemantic,
            _ => Self::Level5DeepIntegrity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub volume_id: String,
    pub mount_point: String,
    pub label: Option<String>,
    pub fs_type: Option<String>,
    pub storage_kind: String,
    pub is_online: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryRoot {
    pub id: String,
    pub path: String,
    pub root_type: String,
    pub volume_id: Option<String>,
    pub label: Option<String>,
    pub recursive: bool,
    pub exclusions: Vec<String>,
    pub include_hidden: bool,
    pub auto_analysis_level: i32,
    pub is_enabled: bool,
    pub last_scan_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogTagBadge {
    pub id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub color: Option<String>,
    pub source: String,
    pub confidence: f64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub asset_id: String,
    pub file_key: String,
    pub location_id: String,
    pub root_id: Option<String>,
    pub path: String,
    pub filename: String,
    pub parent_dir: String,
    pub extension: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub recorded_at: Option<String>,
    pub media_type: String,
    pub volume_id: String,
    pub os_file_id: String,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub head_hash: String,
    pub blake3_hash: Option<String>,
    pub sha256_hash: Option<String>,
    pub link_count: u64,
    pub analysis_level: i32,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bitrate: Option<u64>,
    pub fps: Option<f64>,
    pub pix_fmt: Option<String>,
    pub vr_projection: Option<String>,
    pub thumbnail_path: Option<String>,
    pub integrity_status: String,
    pub is_online: bool,
    pub tags: Vec<CatalogTagBadge>,
    pub duplicate_group_count: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRelationshipRecord {
    pub id: String,
    pub source_asset_id: String,
    pub target_asset_id: String,
    pub source_file_key: Option<String>,
    pub target_file_key: Option<String>,
    pub relationship_type: String,
    pub details_json: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDetail {
    pub entry: CatalogEntry,
    pub probe: Option<ProbeResult>,
    pub locations: Vec<FileLocationRecord>,
    pub relationships: Vec<MediaRelationshipRecord>,
    pub ocr_snippets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLocationRecord {
    pub location_id: String,
    pub file_key: String,
    pub root_id: Option<String>,
    pub path: String,
    pub filename: String,
    pub parent_dir: String,
    pub extension: String,
    pub is_current: bool,
    pub is_online: bool,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogFilter {
    pub file_keys: Option<Vec<String>>,
    pub search_text: Option<String>,
    pub root_id: Option<String>,
    pub directory_prefix: Option<String>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub tag_match_mode: Option<String>, // "and" | "or"
    pub media_type: Option<String>,
    pub codec: Option<String>,
    pub min_width: Option<u32>,
    pub max_width: Option<u32>,
    pub min_height: Option<u32>,
    pub min_bitrate: Option<u64>,
    pub max_bitrate: Option<u64>,
    pub min_duration: Option<f64>,
    pub max_duration: Option<f64>,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub mtime_from: Option<i64>,
    pub mtime_to: Option<i64>,
    pub integrity_status: Option<String>,
    pub only_duplicates: bool,
    pub only_untagged: bool,
    pub only_watermarked: bool,
    pub creator: Option<String>,
    pub ocr_query: Option<String>,
    pub vr_only: bool,
    pub online_only: Option<bool>, // Some(true) => online, Some(false) => offline, None => all
    pub sort_by: Option<String>,
    pub sort_dir: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogPage {
    pub items: Vec<CatalogEntry>,
    pub total_count: usize,
    pub total_bytes: u64,
    pub online_count: usize,
    pub offline_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanLibraryReport {
    pub root_id: String,
    pub discovered_files: usize,
    pub added_files: usize,
    pub unchanged_files: usize,
    pub moved_or_renamed_files: usize,
    pub metadata_only_updated: usize,
    pub content_changed_files: usize,
    pub marked_offline: usize,
    pub skipped_files: usize,
    pub probed_files: usize,
    pub canceled: bool,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogStats {
    pub total_assets: usize,
    pub total_files: usize,
    pub online_files: usize,
    pub offline_files: usize,
    pub total_bytes: u64,
    pub total_tags: usize,
    pub duplicate_edges: usize,
    pub corrupted_count: usize,
}

pub fn classify_media_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if MEDIA_VIDEO_EXTS.contains(&ext.as_str()) {
        "video"
    } else if MEDIA_IMAGE_EXTS.contains(&ext.as_str()) {
        "image"
    } else if MEDIA_AUDIO_EXTS.contains(&ext.as_str()) {
        "audio"
    } else {
        "other"
    }
}

pub fn is_supported_catalog_media(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    MEDIA_VIDEO_EXTS.contains(&ext.as_str())
        || MEDIA_IMAGE_EXTS.contains(&ext.as_str())
        || MEDIA_AUDIO_EXTS.contains(&ext.as_str())
}

/// Convert long Windows paths to extended-length `\\?\` paths safely for std::fs calls.
pub fn to_long_fs_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let s = path.to_string_lossy();
        if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
            return path.to_path_buf();
        }
        if path.is_absolute() && s.len() >= 240 {
            if let Some(unc) = s.strip_prefix(r"\\") {
                return PathBuf::from(format!(r"\\?\UNC\{unc}"));
            }
            return PathBuf::from(format!(r"\\?\{s}"));
        }
    }
    path.to_path_buf()
}

pub fn normalize_display_path(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    s
}

pub fn detect_storage_kind(path: &Path) -> String {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        let s = normalize_display_path(path);
        if s.starts_with(r"\\") {
            return "network".to_string();
        }
        if let Ok(vs) = disk_space_for_path(path) {
            let wide: Vec<u16> = std::ffi::OsStr::new(&vs.mount_point)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let dt = unsafe { GetDriveTypeW(wide.as_ptr()) };
            return match dt {
                2 => "removable".to_string(),
                3 => "fixed_disk".to_string(),
                4 => "network".to_string(),
                _ => "unknown".to_string(),
            };
        }
        return "unknown".to_string();
    }
    #[cfg(target_os = "macos")]
    {
        let s = path.to_string_lossy();
        if s.starts_with("/Volumes/") {
            return "removable".to_string();
        }
        return "ssd".to_string();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "unknown".to_string()
    }
}

fn is_hidden_entry(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.starts_with('.') {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        if let Ok(m) = std::fs::symlink_metadata(to_long_fs_path(path)) {
            let attrs = m.file_attributes();
            if (attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)) != 0 {
                return true;
            }
        }
    }
    false
}

fn matches_exclusions(path: &Path, exclusions: &[String]) -> bool {
    if exclusions.is_empty() {
        return false;
    }
    let full_lower = normalize_display_path(path).to_ascii_lowercase();
    let name_lower = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    for pat in exclusions {
        let p = pat.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        if let Some(ext_pat) = p.strip_prefix("*.") {
            if name_lower.ends_with(&format!(".{ext_pat}")) {
                return true;
            }
        } else if name_lower == p || full_lower.contains(&p) {
            return true;
        }
    }
    false
}

pub fn add_library_root(
    db: &Database,
    raw_path: &Path,
    label: Option<&str>,
    recursive: bool,
    exclusions: Vec<String>,
    include_hidden: bool,
    auto_analysis_level: i32,
) -> Result<LibraryRoot, String> {
    let fs_path = to_long_fs_path(raw_path);
    let meta = std::fs::symlink_metadata(&fs_path)
        .map_err(|e| format!("Cannot access path {}: {e}", raw_path.display()))?;
    if meta.file_type().is_symlink() {
        return Err("Symlink roots are blocked for safety.".to_string());
    }
    let root_type = if meta.is_file() {
        "file".to_string()
    } else if normalize_display_path(raw_path).starts_with(r"\\") {
        "network".to_string()
    } else if raw_path.parent().is_none() {
        "drive".to_string()
    } else {
        "directory".to_string()
    };

    let canon = fs_path.canonicalize().unwrap_or(fs_path);
    let norm_path = normalize_display_path(&canon);
    let now = Utc::now().timestamp();
    let vol_stats = disk_space_for_path(&canon).ok();
    let volume_id = vol_stats.as_ref().map(|v| v.volume_id.clone());

    if let Some(ref vs) = vol_stats {
        upsert_volume_record(
            db,
            &VolumeRecord {
                volume_id: vs.volume_id.clone(),
                mount_point: vs.mount_point.clone(),
                label: None,
                fs_type: None,
                storage_kind: detect_storage_kind(&canon),
                is_online: true,
                total_bytes: vs.total_bytes,
                free_bytes: vs.free_bytes,
                last_seen_at: now,
            },
        )?;
    }

    let conn = db.lock_conn()?;
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM library_roots WHERE path = ?1",
            params![norm_path],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let excl_json = serde_json::to_string(&exclusions).map_err(|e| e.to_string())?;
    let lbl = label.map(str::to_string).or_else(|| {
        canon
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(norm_path.clone()))
    });

    conn.execute(
        "INSERT INTO library_roots (
            id, path, root_type, volume_id, label, recursive, exclusions_json,
            include_hidden, auto_analysis_level, is_enabled, last_scan_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, NULL, ?10)
         ON CONFLICT(id) DO UPDATE SET
            root_type = excluded.root_type,
            volume_id = excluded.volume_id,
            label = COALESCE(excluded.label, library_roots.label),
            recursive = excluded.recursive,
            exclusions_json = excluded.exclusions_json,
            include_hidden = excluded.include_hidden,
            auto_analysis_level = excluded.auto_analysis_level,
            is_enabled = 1",
        params![
            id,
            norm_path,
            root_type,
            volume_id,
            lbl,
            recursive as i64,
            excl_json,
            include_hidden as i64,
            auto_analysis_level.clamp(0, 5),
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    drop(conn);

    get_library_root(db, &id)?.ok_or_else(|| "Failed to load library root".to_string())
}

pub fn get_library_root(db: &Database, root_id: &str) -> Result<Option<LibraryRoot>, String> {
    let list = list_library_roots(db)?;
    Ok(list.into_iter().find(|r| r.id == root_id))
}

pub fn list_library_roots(db: &Database) -> Result<Vec<LibraryRoot>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, path, root_type, volume_id, label, recursive, exclusions_json,
                    include_hidden, auto_analysis_level, is_enabled, last_scan_at, created_at
             FROM library_roots ORDER BY created_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let excl_s: String = r.get(6)?;
            Ok(LibraryRoot {
                id: r.get(0)?,
                path: r.get(1)?,
                root_type: r.get(2)?,
                volume_id: r.get(3)?,
                label: r.get(4)?,
                recursive: r.get::<_, i64>(5)? != 0,
                exclusions: serde_json::from_str(&excl_s).unwrap_or_default(),
                include_hidden: r.get::<_, i64>(7)? != 0,
                auto_analysis_level: r.get(8)?,
                is_enabled: r.get::<_, i64>(9)? != 0,
                last_scan_at: r.get(10)?,
                created_at: r.get(11)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn remove_library_root(db: &Database, root_id: &str, remove_entries: bool) -> Result<(), String> {
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if remove_entries {
        let file_keys: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT DISTINCT file_key FROM file_locations WHERE root_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![root_id], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };
        tx.execute("DELETE FROM file_locations WHERE root_id = ?1", params![root_id])
            .map_err(|e| e.to_string())?;
        for fk in file_keys {
            let rem: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM file_locations WHERE file_key = ?1",
                    params![fk],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if rem == 0 {
                tx.execute("DELETE FROM media_files WHERE file_key = ?1", params![fk])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.execute(
            "DELETE FROM media_assets WHERE asset_id NOT IN (SELECT DISTINCT asset_id FROM media_files)",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute("DELETE FROM library_roots WHERE id = ?1", params![root_id])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_volume_record(db: &Database, vol: &VolumeRecord) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO volumes (
            volume_id, mount_point, label, fs_type, storage_kind, is_online,
            total_bytes, free_bytes, last_seen_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(volume_id) DO UPDATE SET
            mount_point = excluded.mount_point,
            label = COALESCE(excluded.label, volumes.label),
            fs_type = COALESCE(excluded.fs_type, volumes.fs_type),
            storage_kind = excluded.storage_kind,
            is_online = excluded.is_online,
            total_bytes = excluded.total_bytes,
            free_bytes = excluded.free_bytes,
            last_seen_at = excluded.last_seen_at",
        params![
            vol.volume_id,
            vol.mount_point,
            vol.label,
            vol.fs_type,
            vol.storage_kind,
            vol.is_online as i64,
            vol.total_bytes as i64,
            vol.free_bytes as i64,
            vol.last_seen_at
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn list_volumes(db: &Database) -> Result<Vec<VolumeRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT volume_id, mount_point, label, fs_type, storage_kind, is_online,
                    total_bytes, free_bytes, last_seen_at
             FROM volumes ORDER BY mount_point ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(VolumeRecord {
                volume_id: r.get(0)?,
                mount_point: r.get(1)?,
                label: r.get(2)?,
                fs_type: r.get(3)?,
                storage_kind: r.get(4)?,
                is_online: r.get::<_, i64>(5)? != 0,
                total_bytes: r.get::<_, i64>(6)?.max(0) as u64,
                free_bytes: r.get::<_, i64>(7)?.max(0) as u64,
                last_seen_at: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Reconcile online/offline state of all registered volumes and their cataloged media.
/// Disconnected volumes stay in the catalog and are marked `is_online = 0` rather than deleted.
pub fn refresh_catalog_volumes(db: &Database) -> Result<Vec<VolumeRecord>, String> {
    let roots = list_library_roots(db)?;
    let existing = list_volumes(db)?;
    let now = Utc::now().timestamp();

    let mut checked_vols: HashMap<String, VolumeRecord> = HashMap::new();
    for v in existing {
        let mp = Path::new(&v.mount_point);
        let online = mp.exists()
            && disk_space_for_path(mp)
                .map(|cur| cur.volume_id == v.volume_id)
                .unwrap_or(false);
        let mut updated = v.clone();
        updated.is_online = online;
        if online {
            if let Ok(cur) = disk_space_for_path(mp) {
                updated.total_bytes = cur.total_bytes;
                updated.free_bytes = cur.free_bytes;
                updated.last_seen_at = now;
            }
        }
        checked_vols.insert(updated.volume_id.clone(), updated);
    }

    for r in roots {
        let rp = Path::new(&r.path);
        if rp.exists() {
            if let Ok(cur) = disk_space_for_path(rp) {
                checked_vols.insert(
                    cur.volume_id.clone(),
                    VolumeRecord {
                        volume_id: cur.volume_id,
                        mount_point: cur.mount_point,
                        label: None,
                        fs_type: None,
                        storage_kind: detect_storage_kind(rp),
                        is_online: true,
                        total_bytes: cur.total_bytes,
                        free_bytes: cur.free_bytes,
                        last_seen_at: now,
                    },
                );
            }
        }
    }

    for vol in checked_vols.values() {
        upsert_volume_record(db, vol)?;
        let conn = db.lock_conn()?;
        conn.execute(
            "UPDATE media_files SET is_online = ?1 WHERE volume_id = ?2",
            params![vol.is_online as i64, vol.volume_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE file_locations SET is_online = ?1
             WHERE file_key IN (SELECT file_key FROM media_files WHERE volume_id = ?2)",
            params![vol.is_online as i64, vol.volume_id],
        )
        .map_err(|e| e.to_string())?;
    }

    list_volumes(db)
}

pub fn mark_volume_offline_state(db: &Database, volume_id: &str, is_online: bool) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "UPDATE volumes SET is_online = ?1 WHERE volume_id = ?2",
        params![is_online as i64, volume_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE media_files SET is_online = ?1 WHERE volume_id = ?2",
        params![is_online as i64, volume_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE file_locations SET is_online = ?1
         WHERE file_key IN (SELECT file_key FROM media_files WHERE volume_id = ?2)",
        params![is_online as i64, volume_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Scan a registered library root incrementally.
/// Preserves identities across renames/moves/hard-links, distinguishes metadata-only vs content changes,
/// and marks vanished paths offline without deleting catalog metadata.
pub fn scan_library_root(
    db: &Database,
    root_id: &str,
    ffprobe_path: Option<&Path>,
    target_level: Option<i32>,
    cancel_flag: &Arc<AtomicBool>,
    pause_flag: Option<&Arc<AtomicBool>>,
) -> Result<ScanLibraryReport, String> {
    let started = std::time::Instant::now();
    let root = get_library_root(db, root_id)?
        .ok_or_else(|| format!("Library root not found: {root_id}"))?;
    let root_path = to_long_fs_path(Path::new(&root.path));
    if !root_path.exists() {
        if let Some(ref vid) = root.volume_id {
            let _ = mark_volume_offline_state(db, vid, false);
        }
        return Err(format!("Library root is currently offline or missing: {}", root.path));
    }

    if let Ok(vs) = disk_space_for_path(&root_path) {
        let now = Utc::now().timestamp();
        upsert_volume_record(
            db,
            &VolumeRecord {
                volume_id: vs.volume_id.clone(),
                mount_point: vs.mount_point,
                label: None,
                fs_type: None,
                storage_kind: detect_storage_kind(&root_path),
                is_online: true,
                total_bytes: vs.total_bytes,
                free_bytes: vs.free_bytes,
                last_seen_at: now,
            },
        )?;
        let conn = db.lock_conn()?;
        conn.execute(
            "UPDATE library_roots SET volume_id = ?1 WHERE id = ?2",
            params![vs.volume_id, root_id],
        )
        .map_err(|e| e.to_string())?;
    }

    let analysis_goal = target_level.unwrap_or(root.auto_analysis_level).clamp(0, 5);
    let mut discovered_paths: Vec<PathBuf> = Vec::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let mut skipped_files = 0usize;

    let meta = std::fs::symlink_metadata(&root_path)
        .map_err(|e| format!("Cannot access root {}: {e}", root.path))?;
    if meta.is_file() {
        if is_supported_catalog_media(&root_path) && is_blocked_input_path(&root_path).is_none() {
            discovered_paths.push(root_path.clone());
        }
    } else if meta.is_dir() {
        walk_catalog_dir(
            &root_path,
            root.recursive,
            root.include_hidden,
            &root.exclusions,
            cancel_flag,
            pause_flag,
            &mut visited_dirs,
            &mut discovered_paths,
            &mut skipped_files,
        );
    }

    let mut added_files = 0usize;
    let mut unchanged_files = 0usize;
    let mut moved_or_renamed_files = 0usize;
    let mut metadata_only_updated = 0usize;
    let mut content_changed_files = 0usize;
    let mut probed_files = 0usize;

    let now = Utc::now().timestamp();
    let mut seen_current_paths: HashSet<String> = HashSet::with_capacity(discovered_paths.len());
    let mut newly_needing_probe: Vec<(String, PathBuf)> = Vec::new();

    for abs_path in &discovered_paths {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        wait_while_paused(pause_flag, cancel_flag);
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }

        let norm_path = normalize_display_path(abs_path);
        seen_current_paths.insert(norm_path.clone());

        let fp = match capture_file_identity(abs_path) {
            Ok(f) => f,
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };

        let filename = abs_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "media".to_string());
        let parent_dir = abs_path
            .parent()
            .map(normalize_display_path)
            .unwrap_or_default();
        let ext = abs_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let media_type = classify_media_type(abs_path).to_string();

        let (file_key, needs_probe) = upsert_scanned_file(
            db,
            Some(root_id),
            &norm_path,
            &filename,
            &parent_dir,
            &ext,
            &media_type,
            &fp,
            now,
            &mut added_files,
            &mut unchanged_files,
            &mut moved_or_renamed_files,
            &mut metadata_only_updated,
            &mut content_changed_files,
        )?;

        if needs_probe && analysis_goal >= 1 {
            newly_needing_probe.push((file_key, abs_path.clone()));
        }
    }

    // Reconcile vanished locations under this root (only if scan was not canceled)
    let mut marked_offline = 0usize;
    if !cancel_flag.load(Ordering::Relaxed) {
        let conn = db.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT location_id, file_key, path FROM file_locations
                 WHERE root_id = ?1 AND is_current = 1 AND is_online = 1",
            )
            .map_err(|e| e.to_string())?;
        let existing_locs: Vec<(String, String, String)> = stmt
            .query_map(params![root_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);

        for (loc_id, fkey, old_path) in existing_locs {
            if !seen_current_paths.contains(&old_path) {
                conn.execute(
                    "UPDATE file_locations SET is_online = 0 WHERE location_id = ?1",
                    params![loc_id],
                )
                .map_err(|e| e.to_string())?;
                let any_online: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM file_locations WHERE file_key = ?1 AND is_current = 1 AND is_online = 1",
                        params![fkey],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if any_online == 0 {
                    conn.execute(
                        "UPDATE media_files SET is_online = 0 WHERE file_key = ?1",
                        params![fkey],
                    )
                    .map_err(|e| e.to_string())?;
                }
                marked_offline += 1;
            }
        }
        conn.execute(
            "UPDATE library_roots SET last_scan_at = ?1 WHERE id = ?2",
            params![Utc::now().timestamp(), root_id],
        )
        .map_err(|e| e.to_string())?;
    }

    // Level 1..5 lazy analysis execution
    let probed_targets: Vec<(String, PathBuf)> = newly_needing_probe.clone();
    if analysis_goal >= 1 {
        if let Some(fp_bin) = ffprobe_path {
            for (file_key, path) in &newly_needing_probe {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                wait_while_paused(pause_flag, cancel_flag);
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if enrich_file_with_ffprobe(db, fp_bin, file_key, path).is_ok() {
                    probed_files += 1;
                }
            }
        }
    }

    if analysis_goal >= 2 && !cancel_flag.load(Ordering::Relaxed) {
        if let Some(ff_bin) = ffprobe_path.and_then(|fp| {
            let dir = fp.parent()?;
            let name = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };
            let cand = dir.join(name);
            if cand.is_file() { Some(cand) } else { None }
        }) {
            for (file_key, _) in &probed_targets {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                wait_while_paused(pause_flag, cancel_flag);
                if analysis_goal >= 2 {
                    let _ = crate::analysis::extract_sampled_frames(
                        db,
                        &ff_bin,
                        file_key,
                        &crate::analysis::SamplingMode::Percentage { count: 8 },
                    );
                }
                if analysis_goal >= 3 {
                    let _ = crate::duplicates::extract_or_load_perceptual_sequence(db, &ff_bin, file_key);
                    if let Ok(Some(p)) = get_current_path_for_file(db, file_key) {
                        if let Ok((b3, s2)) = crate::duplicates::compute_file_hashes(Path::new(&p), true) {
                            let conn = db.lock_conn()?;
                            let _ = conn.execute(
                                "UPDATE media_files SET blake3_hash = ?1, sha256_hash = ?2, analysis_level = MAX(analysis_level, 3) WHERE file_key = ?3",
                                params![b3, s2, file_key],
                            );
                        }
                    }
                }
                if analysis_goal >= 4 {
                    let _ = crate::intelligence::run_ocr_and_watermark_analysis(db, &ff_bin, file_key);
                    let _ = crate::intelligence::index_semantic_embeddings(db, &ff_bin, file_key);
                }
                if analysis_goal >= 5 {
                    let _ = crate::integrity::verify_file_integrity(db, Some(&ff_bin), ffprobe_path, file_key, false);
                }
            }
        }
    }

    Ok(ScanLibraryReport {
        root_id: root_id.to_string(),
        discovered_files: discovered_paths.len(),
        added_files,
        unchanged_files,
        moved_or_renamed_files,
        metadata_only_updated,
        content_changed_files,
        marked_offline,
        skipped_files,
        probed_files,
        canceled: cancel_flag.load(Ordering::Relaxed),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn wait_while_paused(pause_flag: Option<&Arc<AtomicBool>>, cancel_flag: &Arc<AtomicBool>) {
    if let Some(pf) = pause_flag {
        while pf.load(Ordering::Relaxed) && !cancel_flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn walk_catalog_dir(
    dir: &Path,
    recursive: bool,
    include_hidden: bool,
    exclusions: &[String],
    cancel_flag: &Arc<AtomicBool>,
    pause_flag: Option<&Arc<AtomicBool>>,
    visited_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    skipped: &mut usize,
) {
    if cancel_flag.load(Ordering::Relaxed) {
        return;
    }
    wait_while_paused(pause_flag, cancel_flag);
    let Ok(canon) = dir.canonicalize() else {
        *skipped += 1;
        return;
    };
    if !visited_dirs.insert(canon) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        *skipped += 1;
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for p in paths {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        if !include_hidden && is_hidden_entry(&p) {
            continue;
        }
        if matches_exclusions(&p, exclusions) {
            continue;
        }
        if crate::scanner::is_app_staging_or_partial(&p) {
            continue;
        }
        let long_p = to_long_fs_path(&p);
        let Ok(meta) = std::fs::symlink_metadata(&long_p) else {
            *skipped += 1;
            continue;
        };
        if meta.file_type().is_symlink() {
            *skipped += 1;
            continue;
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                *skipped += 1;
                continue;
            }
        }
        if meta.is_dir() {
            if recursive {
                walk_catalog_dir(
                    &long_p,
                    recursive,
                    include_hidden,
                    exclusions,
                    cancel_flag,
                    pause_flag,
                    visited_dirs,
                    out,
                    skipped,
                );
            }
        } else if meta.is_file() && is_supported_catalog_media(&long_p) && is_blocked_input_path(&long_p).is_none() {
            out.push(long_p);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn upsert_scanned_file(
    db: &Database,
    root_id: Option<&str>,
    norm_path: &str,
    filename: &str,
    parent_dir: &str,
    ext: &str,
    media_type: &str,
    fp: &crate::models::SourceFingerprint,
    now: i64,
    added: &mut usize,
    unchanged: &mut usize,
    moved_or_renamed: &mut usize,
    metadata_only: &mut usize,
    content_changed: &mut usize,
) -> Result<(String, bool), String> {
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    // 1. Check if this exact path is already known
    let existing_by_path: Option<(String, String, String, String, i64, i64, String, i32)> = tx
        .query_row(
            "SELECT fl.location_id, mf.file_key, mf.asset_id, mf.os_file_id, mf.size_bytes, mf.mtime_ns, mf.head_hash, mf.analysis_level
             FROM file_locations fl
             JOIN media_files mf ON mf.file_key = fl.file_key
             WHERE fl.path = ?1 AND fl.is_current = 1",
            params![norm_path],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((loc_id, fkey, asset_id, old_fid, old_size, old_mtime, old_head, level)) = existing_by_path {
        if old_fid == fp.file_id && old_size as u64 == fp.size_bytes && old_mtime == fp.mtime_ns {
            // Completely unchanged
            tx.execute(
                "UPDATE file_locations SET is_online = 1, root_id = COALESCE(?1, root_id), last_seen_at = ?2 WHERE location_id = ?3",
                params![root_id, now, loc_id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE media_files SET is_online = 1, link_count = ?1, last_seen_at = ?2 WHERE file_key = ?3",
                params![fp.link_count as i64, now, fkey],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            *unchanged += 1;
            return Ok((fkey, level < 1));
        }

        if old_fid == fp.file_id && old_size as u64 == fp.size_bytes && old_head == fp.head_hash {
            // Metadata-only change (mtime touched, same file_id & content head/tail): keep analysis cache!
            tx.execute(
                "UPDATE media_files SET mtime_ns = ?1, link_count = ?2, is_online = 1, last_seen_at = ?3, updated_at = ?3 WHERE file_key = ?4",
                params![fp.mtime_ns, fp.link_count as i64, now, fkey],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE file_locations SET is_online = 1, last_seen_at = ?1 WHERE location_id = ?2",
                params![now, loc_id],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            *metadata_only += 1;
            return Ok((fkey, level < 1));
        }

        if old_fid == fp.file_id {
            // In-place content change: invalidate derived hashes/analysis
            tx.execute("DELETE FROM fingerprints WHERE file_key = ?1", params![fkey])
                .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM analysis_results WHERE file_key = ?1", params![fkey])
                .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM media_embeddings WHERE file_key = ?1", params![fkey])
                .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM ocr_detections WHERE file_key = ?1", params![fkey])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE media_files SET size_bytes = ?1, mtime_ns = ?2, head_hash = ?3,
                        blake3_hash = NULL, sha256_hash = NULL, analysis_level = 0,
                        integrity_status = 'unverified', is_online = 1, last_seen_at = ?4, updated_at = ?4
                 WHERE file_key = ?5",
                params![fp.size_bytes as i64, fp.mtime_ns, fp.head_hash, now, fkey],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE file_locations SET is_online = 1, last_seen_at = ?1 WHERE location_id = ?2",
                params![now, loc_id],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            *content_changed += 1;
            return Ok((fkey, true));
        }

        // Different os_file_id at same path: replaced by an entirely different file!
        // Mark old location non-current and fall through to identity lookup / insert.
        let _ = asset_id;
        tx.execute(
            "UPDATE file_locations SET is_current = 0, is_online = 0 WHERE location_id = ?1",
            params![loc_id],
        )
        .map_err(|e| e.to_string())?;
    }

    // 2. Match by OS file identity (volume_id + os_file_id) -> detects renames, same-volume moves, and hard links!
    let existing_by_id: Option<(String, String, i64, String, i32)> = tx
        .query_row(
            "SELECT file_key, asset_id, size_bytes, head_hash, analysis_level
             FROM media_files WHERE volume_id = ?1 AND os_file_id = ?2",
            params![fp.volume_id, fp.file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((fkey, asset_id, old_size, old_head, level)) = existing_by_id {
        if old_size as u64 == fp.size_bytes && old_head == fp.head_hash {
            // Check existing current locations for this file_key
            let mut stmt = tx
                .prepare("SELECT location_id, path FROM file_locations WHERE file_key = ?1 AND is_current = 1")
                .map_err(|e| e.to_string())?;
            let old_locs: Vec<(String, String)> = stmt
                .query_map(params![fkey], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(stmt);

            let mut is_hard_link_extra = false;
            for (old_loc_id, old_p) in old_locs {
                if old_p != norm_path {
                    if to_long_fs_path(Path::new(&old_p)).exists() {
                        // Old path still exists on disk => hard link!
                        is_hard_link_extra = true;
                    } else {
                        // Old path gone => file was moved or renamed!
                        tx.execute(
                            "UPDATE file_locations SET is_current = 0, is_online = 0 WHERE location_id = ?1",
                            params![old_loc_id],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
            }

            let new_loc_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO file_locations (
                    location_id, file_key, root_id, path, filename, parent_dir, extension,
                    is_current, is_online, first_seen_at, last_seen_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, ?8, ?8)",
                params![new_loc_id, fkey, root_id, norm_path, filename, parent_dir, ext, now],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE media_files SET is_online = 1, link_count = ?1, mtime_ns = ?2, last_seen_at = ?3, updated_at = ?3 WHERE file_key = ?4",
                params![fp.link_count as i64, fp.mtime_ns, now, fkey],
            )
            .map_err(|e| e.to_string())?;
            refresh_fts_row_tx(&tx, &asset_id)?;
            tx.commit().map_err(|e| e.to_string())?;
            if is_hard_link_extra {
                *unchanged += 1;
            } else {
                *moved_or_renamed += 1;
            }
            return Ok((fkey, level < 1));
        }
    }

    // 3. Fallback identity match for offline/moved files with identical size + head_tail fingerprint
    let moved_offline: Option<(String, String, i32)> = tx
        .query_row(
            "SELECT mf.file_key, mf.asset_id, mf.analysis_level
             FROM media_files mf
             WHERE mf.size_bytes = ?1 AND mf.head_hash = ?2 AND mf.is_online = 0
             LIMIT 1",
            params![fp.size_bytes as i64, fp.head_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((fkey, asset_id, level)) = moved_offline {
        // Verify old path truly no longer exists before rebinding
        let old_exists = {
            let mut stmt = tx
                .prepare("SELECT path FROM file_locations WHERE file_key = ?1 AND is_current = 1")
                .map_err(|e| e.to_string())?;
            let paths: Vec<String> = stmt
                .query_map(params![fkey], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            paths.into_iter().any(|p| to_long_fs_path(Path::new(&p)).exists())
        };
        if !old_exists {
            tx.execute(
                "UPDATE file_locations SET is_current = 0, is_online = 0 WHERE file_key = ?1",
                params![fkey],
            )
            .map_err(|e| e.to_string())?;
            let new_loc_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO file_locations (
                    location_id, file_key, root_id, path, filename, parent_dir, extension,
                    is_current, is_online, first_seen_at, last_seen_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, ?8, ?8)",
                params![new_loc_id, fkey, root_id, norm_path, filename, parent_dir, ext, now],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE media_files SET volume_id = ?1, os_file_id = ?2, mtime_ns = ?3,
                        link_count = ?4, is_online = 1, last_seen_at = ?5, updated_at = ?5
                 WHERE file_key = ?6",
                params![fp.volume_id, fp.file_id, fp.mtime_ns, fp.link_count as i64, now, fkey],
            )
            .map_err(|e| e.to_string())?;
            refresh_fts_row_tx(&tx, &asset_id)?;
            tx.commit().map_err(|e| e.to_string())?;
            *moved_or_renamed += 1;
            return Ok((fkey, level < 1));
        }
    }

    // 4. New logical media asset + physical media file + current location
    let asset_id = Uuid::new_v4().to_string();
    let file_key = Uuid::new_v4().to_string();
    let loc_id = Uuid::new_v4().to_string();
    let stem = Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.to_string());

    tx.execute(
        "INSERT INTO media_assets (asset_id, title, media_type, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![asset_id, stem, media_type, now],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO media_files (
            file_key, asset_id, volume_id, os_file_id, size_bytes, mtime_ns, head_hash,
            link_count, analysis_level, integrity_status, is_online, last_seen_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 'unverified', 1, ?9, ?9, ?9)",
        params![
            file_key,
            asset_id,
            fp.volume_id,
            fp.file_id,
            fp.size_bytes as i64,
            fp.mtime_ns,
            fp.head_hash,
            fp.link_count as i64,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO file_locations (
            location_id, file_key, root_id, path, filename, parent_dir, extension,
            is_current, is_online, first_seen_at, last_seen_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, ?8, ?8)",
        params![loc_id, file_key, root_id, norm_path, filename, parent_dir, ext, now],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO fingerprints (id, file_key, fp_type, algo_version, value_text, created_at)
         VALUES (?1, ?2, 'head_tail', 'fnv64_v1', ?3, ?4)",
        params![Uuid::new_v4().to_string(), file_key, fp.head_hash, now],
    )
    .map_err(|e| e.to_string())?;

    refresh_fts_row_tx(&tx, &asset_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    *added += 1;
    Ok((file_key, true))
}

/// Enrich a cataloged file with Level 1 FFprobe technical metadata and update FTS.
pub fn enrich_file_with_ffprobe(
    db: &Database,
    ffprobe_path: &Path,
    file_key: &str,
    path: &Path,
) -> Result<(), String> {
    let media_type = classify_media_type(path);
    if media_type == "other" {
        return Ok(());
    }
    let probe = probe_file(ffprobe_path, path)?;
    let probe_json = serde_json::to_string(&probe).map_err(|e| e.to_string())?;

    let vstream = probe
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0);
    let astream = probe.streams.iter().find(|s| s.codec_type == "audio");

    let width = vstream.and_then(|v| v.width);
    let height = vstream.and_then(|v| v.height);
    let video_codec = vstream.map(|v| v.codec_name.clone());
    let audio_codec = astream.map(|a| a.codec_name.clone());
    let pix_fmt = vstream.and_then(|v| v.pix_fmt.clone());
    let bitrate = probe
        .format
        .bit_rate
        .or_else(|| vstream.and_then(|v| v.bit_rate));
    let duration_secs = probe.format.duration_secs;
    let fps = vstream
        .and_then(|v| v.avg_frame_rate.as_deref().or(v.r_frame_rate.as_deref()))
        .and_then(crate::planner::parse_rational);
    let color_info_json = vstream.map(|v| {
        serde_json::json!({
            "color_range": v.color_range,
            "color_space": v.color_space,
            "color_transfer": v.color_transfer,
            "color_primaries": v.color_primaries,
            "bit_depth": v.bit_depth,
            "rotation": v.rotation,
            "side_data_types": v.side_data_types,
        })
        .to_string()
    });
    let vr_projection = detect_vr_projection(&probe, path);
    let creator_hint = probe
        .format
        .tags
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("artist") || k.eq_ignore_ascii_case("author"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty());
    let recorded_at = probe
        .format
        .tags
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("creation_time") || k.eq_ignore_ascii_case("date"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    let asset_id: String = tx
        .query_row(
            "SELECT asset_id FROM media_files WHERE file_key = ?1",
            params![file_key],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    tx.execute(
        "UPDATE media_files SET
            analysis_level = MAX(analysis_level, 1),
            duration_secs = ?1,
            width = ?2,
            height = ?3,
            video_codec = ?4,
            audio_codec = ?5,
            bitrate = ?6,
            fps = ?7,
            pix_fmt = ?8,
            color_info_json = ?9,
            vr_projection = ?10,
            probe_json = ?11,
            updated_at = ?12
         WHERE file_key = ?13",
        params![
            duration_secs,
            width.map(|w| w as i64),
            height.map(|h| h as i64),
            video_codec,
            audio_codec,
            bitrate.map(|b| b as i64),
            fps,
            pix_fmt,
            color_info_json,
            vr_projection,
            probe_json,
            now,
            file_key
        ],
    )
    .map_err(|e| e.to_string())?;

    if creator_hint.is_some() || recorded_at.is_some() {
        tx.execute(
            "UPDATE media_assets SET
                creator = COALESCE(creator, ?1),
                recorded_at = COALESCE(recorded_at, ?2),
                updated_at = ?3
             WHERE asset_id = ?4",
            params![creator_hint, recorded_at, now, asset_id],
        )
        .map_err(|e| e.to_string())?;
    }

    refresh_fts_row_tx(&tx, &asset_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn detect_vr_projection(probe: &ProbeResult, path: &Path) -> Option<String> {
    let fname = path.to_string_lossy().to_ascii_lowercase();
    for s in &probe.streams {
        for sd in &s.side_data_types {
            let l = sd.to_ascii_lowercase();
            if l.contains("spherical") || l.contains("stereo 3d") || l.contains("stereo3d") {
                if fname.contains("360") {
                    return Some("VR360".to_string());
                }
                return Some("VR180".to_string());
            }
        }
        if let Some(proj) = s.tags.get("projection").or_else(|| s.tags.get("stereo_mode")) {
            let p = proj.to_ascii_lowercase();
            if p.contains("equirect") || p.contains("360") {
                return Some("VR360".to_string());
            }
            if p.contains("180") || p.contains("sbs") || p.contains("tb") {
                return Some("VR180".to_string());
            }
        }
    }
    if fname.contains("vr180") || fname.contains("_180_") || fname.contains("_sbs") {
        return Some("VR180".to_string());
    }
    if fname.contains("vr360") || fname.contains("_360_") || fname.contains("equirect") {
        return Some("VR360".to_string());
    }
    None
}

pub fn refresh_fts_row_tx(tx: &rusqlite::Transaction<'_>, asset_id: &str) -> Result<(), String> {
    tx.execute("DELETE FROM media_fts WHERE asset_id = ?1", params![asset_id])
        .map_err(|e| e.to_string())?;
    let row: Option<(String, String, String, String, String)> = tx
        .query_row(
            "SELECT
                COALESCE(fl.filename, ''),
                COALESCE(fl.path, ''),
                COALESCE(ma.title, ''),
                COALESCE(ma.creator, ''),
                COALESCE(mf.video_codec, '')
             FROM media_assets ma
             LEFT JOIN media_files mf ON mf.asset_id = ma.asset_id
             LEFT JOIN file_locations fl ON fl.file_key = mf.file_key AND fl.is_current = 1
             WHERE ma.asset_id = ?1
             LIMIT 1",
            params![asset_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let Some((filename, path, title, creator, codec)) = row else {
        return Ok(());
    };

    let mut stmt = tx
        .prepare(
            "SELECT t.name FROM media_tags mt
             JOIN tags t ON t.id = mt.tag_id
             WHERE mt.asset_id = ?1 AND mt.status = 'active'",
        )
        .map_err(|e| e.to_string())?;
    let tags_vec: Vec<String> = stmt
        .query_map(params![asset_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let tags_text = tags_vec.join(" ");

    let mut ocr_stmt = tx
        .prepare(
            "SELECT DISTINCT o.normalized_text FROM ocr_detections o
             JOIN media_files mf ON mf.file_key = o.file_key
             WHERE mf.asset_id = ?1 AND o.review_status != 'rejected'",
        )
        .map_err(|e| e.to_string())?;
    let ocr_vec: Vec<String> = ocr_stmt
        .query_map(params![asset_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(ocr_stmt);
    let ocr_text = ocr_vec.join(" ");

    tx.execute(
        "INSERT INTO media_fts (asset_id, filename, path, title, creator, tags_text, ocr_text, codec)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![asset_id, filename, path, title, creator, tags_text, ocr_text, codec],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn refresh_fts_for_asset(db: &Database, asset_id: &str) -> Result<(), String> {
    let mut conn = db.lock_conn()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    refresh_fts_row_tx(&tx, asset_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn ensure_path_in_catalog(
    db: &Database,
    path: &Path,
    ffprobe_path: Option<&Path>,
) -> Result<(String, String), String> {
    let norm_path = normalize_display_path(path);
    {
        let conn = db.lock_conn()?;
        if let Some((fk, aid)) = conn
            .query_row(
                "SELECT fl.file_key, mf.asset_id FROM file_locations fl
                 JOIN media_files mf ON mf.file_key = fl.file_key
                 WHERE fl.path = ?1 AND fl.is_current = 1
                 LIMIT 1",
                params![norm_path],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?
        {
            return Ok((fk, aid));
        }
    }

    let fp = capture_file_identity(path)?;
    let now = Utc::now().timestamp();
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    let parent_dir = path
        .parent()
        .map(normalize_display_path)
        .unwrap_or_default();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let media_type = classify_media_type(path).to_string();
    let (mut a, mut u, mut m, mut mo, mut c) = (0, 0, 0, 0, 0);
    let (file_key, needs_probe) = upsert_scanned_file(
        db,
        None,
        &norm_path,
        &filename,
        &parent_dir,
        &ext,
        &media_type,
        &fp,
        now,
        &mut a,
        &mut u,
        &mut m,
        &mut mo,
        &mut c,
    )?;
    if needs_probe {
        if let Some(fp_bin) = ffprobe_path {
            let _ = enrich_file_with_ffprobe(db, fp_bin, &file_key, path);
        }
    }
    let conn = db.lock_conn()?;
    let asset_id: String = conn
        .query_row(
            "SELECT asset_id FROM media_files WHERE file_key = ?1",
            params![file_key],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok((file_key, asset_id))
}

/// Register a newly produced derivative file (transcoded, remuxed, or recovered) into the catalog
/// and link it to its original asset with `media_relationships`.
pub fn register_derived_file(
    db: &Database,
    source_file_key_or_path: &str,
    derived_path: &Path,
    relationship_type: &str,
    details: serde_json::Value,
    inherit_tags: bool,
    ffprobe_path: Option<&Path>,
) -> Result<String, String> {
    let fp = capture_file_identity(derived_path)?;
    let now = Utc::now().timestamp();
    let norm = normalize_display_path(derived_path);
    let filename = derived_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let parent_dir = derived_path
        .parent()
        .map(normalize_display_path)
        .unwrap_or_default();
    let ext = derived_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let media_type = classify_media_type(derived_path).to_string();

    let (source_asset_id, source_file_key) = {
        let conn = db.lock_conn()?;
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT mf.asset_id, mf.file_key FROM media_files mf
                 LEFT JOIN file_locations fl ON fl.file_key = mf.file_key AND fl.is_current = 1
                 WHERE mf.file_key = ?1 OR fl.path = ?1
                 LIMIT 1",
                params![source_file_key_or_path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match found {
            Some((a, f)) => (Some(a), Some(f)),
            None => (None, None),
        }
    };

    let mut a = 0;
    let mut u = 0;
    let mut m = 0;
    let mut md = 0;
    let mut cc = 0;
    let (derived_fkey, _) = upsert_scanned_file(
        db,
        None,
        &norm,
        &filename,
        &parent_dir,
        &ext,
        &media_type,
        &fp,
        now,
        &mut a,
        &mut u,
        &mut m,
        &mut md,
        &mut cc,
    )?;

    if let Some(fp_bin) = ffprobe_path {
        let _ = enrich_file_with_ffprobe(db, fp_bin, &derived_fkey, derived_path);
    }

    let derived_asset_id: String = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT asset_id FROM media_files WHERE file_key = ?1",
            params![derived_fkey],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?
    };

    if let Some(src_asset) = source_asset_id {
        let conn = db.lock_conn()?;
        conn.execute(
            "INSERT INTO media_relationships (
                id, source_asset_id, target_asset_id, source_file_key, target_file_key,
                relationship_type, details_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                src_asset,
                derived_asset_id,
                source_file_key,
                derived_fkey,
                relationship_type,
                details.to_string(),
                now
            ],
        )
        .map_err(|e| e.to_string())?;

        if inherit_tags {
            conn.execute(
                "INSERT OR IGNORE INTO media_tags (asset_id, tag_id, source, confidence, status, created_at, updated_at)
                 SELECT ?1, tag_id, source, confidence, status, ?2, ?2
                 FROM media_tags WHERE asset_id = ?3 AND status = 'active'",
                params![derived_asset_id, now, src_asset],
            )
            .map_err(|e| e.to_string())?;
        }
        drop(conn);
        let _ = refresh_fts_for_asset(db, &derived_asset_id);
    }

    Ok(derived_fkey)
}

pub fn query_catalog(db: &Database, filter: &CatalogFilter) -> Result<CatalogPage, String> {
    let conn = db.lock_conn()?;
    let mut clauses: Vec<String> = vec!["fl.is_current = 1".to_string()];
    let mut sql_params: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(ref fkeys) = filter.file_keys {
        if fkeys.is_empty() {
            return Ok(CatalogPage {
                items: Vec::new(),
                total_count: 0,
                total_bytes: 0,
                online_count: 0,
                offline_count: 0,
            });
        }
        let placeholders = vec!["?"; fkeys.len()].join(",");
        clauses.push(format!("mf.file_key IN ({placeholders})"));
        for k in fkeys {
            sql_params.push(k.clone().into());
        }
    }

    if let Some(ref st) = filter.search_text {
        let trimmed = st.trim();
        if !trimmed.is_empty() {
            let like = format!("%{}%", trimmed.to_ascii_lowercase());
            let fts_tokens: Vec<String> = trimmed
                .split(|c: char| !c.is_alphanumeric() && c != '_' && !('\u{4e00}'..='\u{9fff}').contains(&c))
                .filter(|w| !w.is_empty())
                .map(|w| format!("\"{w}\"*"))
                .collect();
            if fts_tokens.is_empty() {
                clauses.push(
                    "(LOWER(fl.filename) LIKE ? OR LOWER(fl.path) LIKE ? OR LOWER(COALESCE(ma.title,'')) LIKE ? OR LOWER(COALESCE(ma.creator,'')) LIKE ?)".to_string(),
                );
                sql_params.push(like.clone().into());
                sql_params.push(like.clone().into());
                sql_params.push(like.clone().into());
                sql_params.push(like.into());
            } else {
                clauses.push(
                    "(LOWER(fl.filename) LIKE ? OR LOWER(fl.path) LIKE ? OR LOWER(COALESCE(ma.title,'')) LIKE ? OR LOWER(COALESCE(ma.creator,'')) LIKE ? OR ma.asset_id IN (SELECT asset_id FROM media_fts WHERE media_fts MATCH ?))".to_string(),
                );
                sql_params.push(like.clone().into());
                sql_params.push(like.clone().into());
                sql_params.push(like.clone().into());
                sql_params.push(like.into());
                sql_params.push(fts_tokens.join(" ").into());
            }
        }
    }

    if let Some(ref rid) = filter.root_id {
        if !rid.is_empty() {
            clauses.push("fl.root_id = ?".to_string());
            sql_params.push(rid.clone().into());
        }
    }
    if let Some(ref dir) = filter.directory_prefix {
        if !dir.trim().is_empty() {
            clauses.push("LOWER(fl.parent_dir) LIKE ?".to_string());
            sql_params.push(format!("{}%", dir.trim().to_ascii_lowercase()).into());
        }
    }
    if let Some(ref mt) = filter.media_type {
        if !mt.is_empty() && mt != "all" {
            clauses.push("ma.media_type = ?".to_string());
            sql_params.push(mt.clone().into());
        }
    }
    if let Some(ref codec) = filter.codec {
        if !codec.trim().is_empty() {
            clauses.push("LOWER(COALESCE(mf.video_codec,'')) = ?".to_string());
            sql_params.push(codec.trim().to_ascii_lowercase().into());
        }
    }
    if let Some(w) = filter.min_width {
        clauses.push("COALESCE(mf.width, 0) >= ?".to_string());
        sql_params.push((w as i64).into());
    }
    if let Some(w) = filter.max_width {
        clauses.push("COALESCE(mf.width, 0) <= ?".to_string());
        sql_params.push((w as i64).into());
    }
    if let Some(h) = filter.min_height {
        clauses.push("COALESCE(mf.height, 0) >= ?".to_string());
        sql_params.push((h as i64).into());
    }
    if let Some(b) = filter.min_bitrate {
        clauses.push("COALESCE(mf.bitrate, 0) >= ?".to_string());
        sql_params.push((b as i64).into());
    }
    if let Some(b) = filter.max_bitrate {
        clauses.push("COALESCE(mf.bitrate, 0) <= ?".to_string());
        sql_params.push((b as i64).into());
    }
    if let Some(d) = filter.min_duration {
        clauses.push("COALESCE(mf.duration_secs, 0) >= ?".to_string());
        sql_params.push(d.into());
    }
    if let Some(d) = filter.max_duration {
        clauses.push("COALESCE(mf.duration_secs, 0) <= ?".to_string());
        sql_params.push(d.into());
    }
    if let Some(sz) = filter.min_size_bytes {
        clauses.push("mf.size_bytes >= ?".to_string());
        sql_params.push((sz as i64).into());
    }
    if let Some(sz) = filter.max_size_bytes {
        clauses.push("mf.size_bytes <= ?".to_string());
        sql_params.push((sz as i64).into());
    }
    if let Some(ref df) = filter.date_from {
        if !df.is_empty() {
            clauses.push("COALESCE(ma.recorded_at, '') >= ?".to_string());
            sql_params.push(df.clone().into());
        }
    }
    if let Some(ref dt) = filter.date_to {
        if !dt.is_empty() {
            clauses.push("COALESCE(ma.recorded_at, '9999') <= ?".to_string());
            sql_params.push(dt.clone().into());
        }
    }
    if let Some(mf) = filter.mtime_from {
        clauses.push("mf.mtime_ns >= ?".to_string());
        sql_params.push(mf.into());
    }
    if let Some(mt) = filter.mtime_to {
        clauses.push("mf.mtime_ns <= ?".to_string());
        sql_params.push(mt.into());
    }
    if let Some(ref ist) = filter.integrity_status {
        if !ist.is_empty() && ist != "all" {
            clauses.push("mf.integrity_status = ?".to_string());
            sql_params.push(ist.clone().into());
        }
    }
    if let Some(ref cr) = filter.creator {
        if !cr.trim().is_empty() {
            clauses.push("LOWER(COALESCE(ma.creator,'')) LIKE ?".to_string());
            sql_params.push(format!("%{}%", cr.trim().to_ascii_lowercase()).into());
        }
    }
    if let Some(ref oq) = filter.ocr_query {
        if !oq.trim().is_empty() {
            clauses.push(
                "mf.file_key IN (SELECT file_key FROM ocr_detections WHERE LOWER(normalized_text) LIKE ? AND review_status != 'rejected')".to_string(),
            );
            sql_params.push(format!("%{}%", oq.trim().to_ascii_lowercase()).into());
        }
    }
    if filter.vr_only {
        clauses.push("mf.vr_projection IS NOT NULL".to_string());
    }
    if let Some(online) = filter.online_only {
        clauses.push("mf.is_online = ?".to_string());
        sql_params.push((if online { 1i64 } else { 0i64 }).into());
    }
    if filter.only_duplicates {
        clauses.push(
            "(mf.file_key IN (SELECT file_key_a FROM similarity_edges UNION SELECT file_key_b FROM similarity_edges))".to_string(),
        );
    }
    if filter.only_untagged {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM media_tags mt WHERE mt.asset_id = ma.asset_id AND mt.status = 'active')".to_string(),
        );
    }
    if filter.only_watermarked {
        clauses.push(
            "EXISTS (SELECT 1 FROM ocr_detections o WHERE o.file_key = mf.file_key AND o.classification = 'watermark' AND o.review_status != 'rejected')".to_string(),
        );
    }

    if !filter.include_tags.is_empty() {
        let is_or = filter.tag_match_mode.as_deref() == Some("or");
        if is_or {
            let placeholders = vec!["?"; filter.include_tags.len()].join(",");
            clauses.push(format!(
                "ma.asset_id IN (SELECT mt.asset_id FROM media_tags mt JOIN tags t ON t.id = mt.tag_id WHERE mt.status = 'active' AND LOWER(t.name) IN ({placeholders}))"
            ));
            for t in &filter.include_tags {
                sql_params.push(t.trim().to_ascii_lowercase().into());
            }
        } else {
            for t in &filter.include_tags {
                clauses.push(
                    "ma.asset_id IN (SELECT mt.asset_id FROM media_tags mt JOIN tags t ON t.id = mt.tag_id WHERE mt.status = 'active' AND LOWER(t.name) = ?)".to_string(),
                );
                sql_params.push(t.trim().to_ascii_lowercase().into());
            }
        }
    }

    for t in &filter.exclude_tags {
        clauses.push(
            "ma.asset_id NOT IN (SELECT mt.asset_id FROM media_tags mt JOIN tags t ON t.id = mt.tag_id WHERE mt.status = 'active' AND LOWER(t.name) = ?)".to_string(),
        );
        sql_params.push(t.trim().to_ascii_lowercase().into());
    }

    let where_sql = clauses.join(" AND ");
    let base_from = "FROM file_locations fl
        JOIN media_files mf ON mf.file_key = fl.file_key
        JOIN media_assets ma ON ma.asset_id = mf.asset_id";

    let count_sql = format!(
        "SELECT COUNT(*), COALESCE(SUM(mf.size_bytes), 0),
                COALESCE(SUM(CASE WHEN mf.is_online = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN mf.is_online = 0 THEN 1 ELSE 0 END), 0)
         {base_from} WHERE {where_sql}"
    );
    let (total_count, total_bytes, online_count, offline_count): (usize, u64, usize, usize) = {
        let mut stmt = conn.prepare(&count_sql).map_err(|e| e.to_string())?;
        stmt.query_row(rusqlite::params_from_iter(sql_params.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)? as usize,
                r.get::<_, i64>(1)?.max(0) as u64,
                r.get::<_, i64>(2)?.max(0) as usize,
                r.get::<_, i64>(3)?.max(0) as usize,
            ))
        })
        .map_err(|e| e.to_string())?
    };

    let sort_col = match filter.sort_by.as_deref() {
        Some("size") => "mf.size_bytes",
        Some("duration") => "COALESCE(mf.duration_secs, 0)",
        Some("resolution") => "(COALESCE(mf.width, 0) * COALESCE(mf.height, 0))",
        Some("codec") => "COALESCE(mf.video_codec, '')",
        Some("bitrate") => "COALESCE(mf.bitrate, 0)",
        Some("mtime") => "mf.mtime_ns",
        Some("updated") => "mf.updated_at",
        _ => "fl.filename",
    };
    let sort_dir = match filter.sort_dir.as_deref() {
        Some("desc") | Some("DESC") => "DESC",
        _ => "ASC",
    };

    let limit = filter.limit.unwrap_or(500).min(500_000) as i64;
    let offset = filter.offset.unwrap_or(0) as i64;

    let query_sql = format!(
        "SELECT
            ma.asset_id, mf.file_key, fl.location_id, fl.root_id, fl.path, fl.filename,
            fl.parent_dir, fl.extension, ma.title, ma.description, ma.creator, ma.recorded_at,
            ma.media_type, mf.volume_id, mf.os_file_id, mf.size_bytes, mf.mtime_ns, mf.head_hash,
            mf.blake3_hash, mf.sha256_hash, mf.link_count, mf.analysis_level, mf.duration_secs,
            mf.width, mf.height, mf.video_codec, mf.audio_codec, mf.bitrate, mf.fps, mf.pix_fmt,
            mf.vr_projection, mf.thumbnail_path, mf.integrity_status, mf.is_online, mf.updated_at,
            (SELECT COUNT(*) FROM similarity_edges se WHERE se.file_key_a = mf.file_key OR se.file_key_b = mf.file_key) AS dup_cnt
         {base_from}
         WHERE {where_sql}
         ORDER BY {sort_col} {sort_dir}, fl.filename ASC
         LIMIT ? OFFSET ?"
    );

    let mut q_params = sql_params;
    q_params.push(limit.into());
    q_params.push(offset.into());

    let mut stmt = conn.prepare(&query_sql).map_err(|e| e.to_string())?;
    let mut items: Vec<CatalogEntry> = stmt
        .query_map(rusqlite::params_from_iter(q_params.iter()), |r| {
            Ok(CatalogEntry {
                asset_id: r.get(0)?,
                file_key: r.get(1)?,
                location_id: r.get(2)?,
                root_id: r.get(3)?,
                path: r.get(4)?,
                filename: r.get(5)?,
                parent_dir: r.get(6)?,
                extension: r.get(7)?,
                title: r.get(8)?,
                description: r.get(9)?,
                creator: r.get(10)?,
                recorded_at: r.get(11)?,
                media_type: r.get(12)?,
                volume_id: r.get(13)?,
                os_file_id: r.get(14)?,
                size_bytes: r.get::<_, i64>(15)?.max(0) as u64,
                mtime_ns: r.get(16)?,
                head_hash: r.get(17)?,
                blake3_hash: r.get(18)?,
                sha256_hash: r.get(19)?,
                link_count: r.get::<_, i64>(20)?.max(1) as u64,
                analysis_level: r.get(21)?,
                duration_secs: r.get(22)?,
                width: r.get::<_, Option<i64>>(23)?.map(|v| v as u32),
                height: r.get::<_, Option<i64>>(24)?.map(|v| v as u32),
                video_codec: r.get(25)?,
                audio_codec: r.get(26)?,
                bitrate: r.get::<_, Option<i64>>(27)?.map(|v| v.max(0) as u64),
                fps: r.get(28)?,
                pix_fmt: r.get(29)?,
                vr_projection: r.get(30)?,
                thumbnail_path: r.get(31)?,
                integrity_status: r.get(32)?,
                is_online: r.get::<_, i64>(33)? != 0,
                tags: Vec::new(),
                duplicate_group_count: r.get(35)?,
                updated_at: r.get(34)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    if !items.is_empty() {
        let mut tag_stmt = conn
            .prepare(
                "SELECT t.id, t.name, t.namespace, t.color, mt.source, mt.confidence, mt.status
                 FROM media_tags mt
                 JOIN tags t ON t.id = mt.tag_id
                 WHERE mt.asset_id = ?1 AND mt.status != 'rejected'
                 ORDER BY t.name ASC",
            )
            .map_err(|e| e.to_string())?;
        for item in &mut items {
            let badges: Vec<CatalogTagBadge> = tag_stmt
                .query_map(params![item.asset_id], |r| {
                    Ok(CatalogTagBadge {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        namespace: r.get(2)?,
                        color: r.get(3)?,
                        source: r.get(4)?,
                        confidence: r.get(5)?,
                        status: r.get(6)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            item.tags = badges;
        }
    }

    Ok(CatalogPage {
        items,
        total_count,
        total_bytes,
        online_count,
        offline_count,
    })
}

pub fn get_current_path_for_file(db: &Database, file_key: &str) -> Result<Option<String>, String> {
    let conn = db.lock_conn()?;
    conn.query_row(
        "SELECT path FROM file_locations WHERE file_key = ?1 AND is_current = 1 LIMIT 1",
        params![file_key],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn load_entries_by_file_keys(db: &Database, file_keys: &[String]) -> Result<Vec<CatalogEntry>, String> {
    if file_keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut all = Vec::new();
    for chunk in file_keys.chunks(400) {
        let mut p = query_catalog(
            db,
            &CatalogFilter {
                file_keys: Some(chunk.to_vec()),
                limit: Some(chunk.len()),
                ..Default::default()
            },
        )?;
        all.append(&mut p.items);
    }
    Ok(all)
}

/// Periodic background reconciliation loop (reconciles volume availability and enabled library roots).
pub fn start_catalog_reconciler(db: Database, ffprobe_provider: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>) {
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        loop {
            std::thread::sleep(std::time::Duration::from_secs(90));
            let _ = refresh_catalog_volumes(&db);
            let fp = ffprobe_provider();
            if let Ok(roots) = list_library_roots(&db) {
                for r in roots {
                    if r.is_enabled && Path::new(&r.path).exists() {
                        let _ = scan_library_root(&db, &r.id, fp.as_deref(), Some(1), &cancel, None);
                    }
                }
            }
        }
    });
}

pub fn get_asset_detail(db: &Database, file_key: &str) -> Result<AssetDetail, String> {
    let mut items = load_entries_by_file_keys(db, &[file_key.to_string()])?;
    let entry = items
        .pop()
        .ok_or_else(|| format!("Catalog file not found: {file_key}"))?;

    let conn = db.lock_conn()?;
    let probe_s: Option<String> = conn
        .query_row(
            "SELECT probe_json FROM media_files WHERE file_key = ?1",
            params![file_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let probe = probe_s.and_then(|s| serde_json::from_str(&s).ok());

    let mut loc_stmt = conn
        .prepare(
            "SELECT location_id, file_key, root_id, path, filename, parent_dir, extension,
                    is_current, is_online, first_seen_at, last_seen_at
             FROM file_locations WHERE file_key = ?1 ORDER BY is_current DESC, last_seen_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let locations = loc_stmt
        .query_map(params![file_key], |r| {
            Ok(FileLocationRecord {
                location_id: r.get(0)?,
                file_key: r.get(1)?,
                root_id: r.get(2)?,
                path: r.get(3)?,
                filename: r.get(4)?,
                parent_dir: r.get(5)?,
                extension: r.get(6)?,
                is_current: r.get::<_, i64>(7)? != 0,
                is_online: r.get::<_, i64>(8)? != 0,
                first_seen_at: r.get(9)?,
                last_seen_at: r.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut rel_stmt = conn
        .prepare(
            "SELECT id, source_asset_id, target_asset_id, source_file_key, target_file_key,
                    relationship_type, details_json, created_at
             FROM media_relationships
             WHERE source_asset_id = ?1 OR target_asset_id = ?1
             ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let relationships = rel_stmt
        .query_map(params![entry.asset_id], |r| {
            Ok(MediaRelationshipRecord {
                id: r.get(0)?,
                source_asset_id: r.get(1)?,
                target_asset_id: r.get(2)?,
                source_file_key: r.get(3)?,
                target_file_key: r.get(4)?,
                relationship_type: r.get(5)?,
                details_json: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut ocr_stmt = conn
        .prepare(
            "SELECT DISTINCT normalized_text FROM ocr_detections
             WHERE file_key = ?1 AND review_status != 'rejected'
             ORDER BY frequency DESC LIMIT 20",
        )
        .map_err(|e| e.to_string())?;
    let ocr_snippets = ocr_stmt
        .query_map(params![file_key], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(AssetDetail {
        entry,
        probe,
        locations,
        relationships,
        ocr_snippets,
    })
}

pub fn get_catalog_stats(db: &Database) -> Result<CatalogStats, String> {
    let conn = db.lock_conn()?;
    let total_assets: usize = conn
        .query_row("SELECT COUNT(*) FROM media_assets", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? as usize;
    let (total_files, online_files, offline_files, total_bytes, corrupted_count): (usize, usize, usize, u64, usize) = conn
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN is_online = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN is_online = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(CASE WHEN integrity_status = 'corrupted' THEN 1 ELSE 0 END), 0)
             FROM media_files",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as usize,
                    r.get::<_, i64>(1)?.max(0) as usize,
                    r.get::<_, i64>(2)?.max(0) as usize,
                    r.get::<_, i64>(3)?.max(0) as u64,
                    r.get::<_, i64>(4)?.max(0) as usize,
                ))
            },
        )
        .map_err(|e| e.to_string())?;
    let total_tags: usize = conn
        .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? as usize;
    let duplicate_edges: usize = conn
        .query_row("SELECT COUNT(*) FROM similarity_edges", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? as usize;

    Ok(CatalogStats {
        total_assets,
        total_files,
        online_files,
        offline_files,
        total_bytes,
        total_tags,
        duplicate_edges,
        corrupted_count,
    })
}

pub fn update_asset_metadata_fields(
    db: &Database,
    asset_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    creator: Option<&str>,
    recorded_at: Option<&str>,
) -> Result<(), String> {
    let conn = db.lock_conn()?;
    let now = Utc::now().timestamp();
    conn.execute(
        "UPDATE media_assets SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            creator = COALESCE(?3, creator),
            recorded_at = COALESCE(?4, recorded_at),
            updated_at = ?5
         WHERE asset_id = ?6",
        params![title, description, creator, recorded_at, now, asset_id],
    )
    .map_err(|e| e.to_string())?;
    drop(conn);
    refresh_fts_for_asset(db, asset_id)
}
