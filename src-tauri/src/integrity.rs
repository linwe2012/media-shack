use crate::analysis::apply_no_window;
use crate::catalog::{query_catalog, to_long_fs_path, CatalogFilter};
use crate::db::Database;
use crate::duplicates::compute_file_hashes;
use crate::planner::probe_file;
use crate::verify::run_full_decode_check;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityCheckRecord {
    pub id: String,
    pub file_key: String,
    pub path: String,
    pub check_type: String,
    pub status: String, // valid | changed | missing | offline | corrupted | intentional_modification | unverified
    pub blake3_hash: Option<String>,
    pub sha256_hash: Option<String>,
    pub backup_copy_count: usize,
    pub backup_warning: Option<String>,
    pub details: Option<String>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksumManifestEntry {
    pub file_key: String,
    pub path: String,
    pub size_bytes: u64,
    pub blake3: String,
    pub sha256: String,
    pub integrity_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksumManifest {
    pub generated_at: i64,
    pub entry_count: usize,
    pub entries: Vec<ChecksumManifestEntry>,
    pub single_copy_warning: String,
}

pub fn discover_par2() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["par2.exe", "par2j64.exe", "par2j.exe"]
    } else {
        &["par2", "par2create"]
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for n in names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

pub fn verify_file_integrity(
    db: &Database,
    ffmpeg_path: Option<&Path>,
    ffprobe_path: Option<&Path>,
    file_key: &str,
    full_decode: bool,
) -> Result<IntegrityCheckRecord, String> {
    let mut items = crate::catalog::load_entries_by_file_keys(db, &[file_key.to_string()])?;
    let entry = items
        .pop()
        .ok_or_else(|| "Catalog file not found.".to_string())?;

    let now = Utc::now().timestamp();
    let p = Path::new(&entry.path);
    let long_p = to_long_fs_path(p);

    // Check volume availability first
    let vol_online: bool = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT is_online FROM volumes WHERE volume_id = ?1",
            params![entry.volume_id],
            |r| r.get::<_, i64>(0).map(|v| v != 0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(true)
    };

    if !vol_online || !entry.is_online {
        return record_integrity_outcome(
            db,
            &entry.file_key,
            &entry.path,
            "availability",
            "offline",
            entry.blake3_hash.clone(),
            entry.sha256_hash.clone(),
            "Storage volume is currently offline/disconnected.",
            now,
        );
    }

    if !long_p.exists() {
        return record_integrity_outcome(
            db,
            &entry.file_key,
            &entry.path,
            "availability",
            "missing",
            entry.blake3_hash.clone(),
            entry.sha256_hash.clone(),
            "Volume is online, but file is missing from expected path.",
            now,
        );
    }

    // Compute BLAKE3 + SHA-256
    let (b3, s2_opt) = compute_file_hashes(p, true)?;
    let s2 = s2_opt.unwrap_or_default();

    // Detect unexpected checksum changes vs intentional metadata edits
    if let Some(ref prev_b3) = entry.blake3_hash {
        if prev_b3 != &b3 {
            let synced_recently: bool = {
                let conn = db.lock_conn()?;
                conn.query_row(
                    "SELECT COUNT(*) FROM metadata_sync_state WHERE file_key = ?1 AND sync_status = 'synced' AND updated_at >= ?2",
                    params![file_key, entry.updated_at],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                    > 0
            };
            let status = if synced_recently {
                "intentional_modification"
            } else {
                "changed"
            };
            let msg = if synced_recently {
                format!("Checksum updated after intentional metadata modification (new BLAKE3 {b3}).")
            } else {
                format!("Unexpected checksum change! Expected BLAKE3 {prev_b3}, got {b3}.")
            };
            return record_integrity_outcome(
                db,
                &entry.file_key,
                &entry.path,
                "checksum",
                status,
                Some(b3),
                Some(s2),
                &msg,
                now,
            );
        }
    }

    // Store fresh hashes if missing
    {
        let conn = db.lock_conn()?;
        conn.execute(
            "UPDATE media_files SET blake3_hash = ?1, sha256_hash = ?2, updated_at = ?3 WHERE file_key = ?4",
            params![b3, s2, now, file_key],
        )
        .map_err(|e| e.to_string())?;
    }

    // Structural FFprobe + optional full A/V decode check for video/audio
    if entry.media_type == "video" || entry.media_type == "audio" {
        if let Some(fp) = ffprobe_path {
            if let Err(e) = probe_file(fp, p) {
                return record_integrity_outcome(
                    db,
                    &entry.file_key,
                    &entry.path,
                    "ffprobe",
                    "corrupted",
                    Some(b3),
                    Some(s2),
                    &format!("FFprobe failed: {e}"),
                    now,
                );
            }
        }
        if full_decode {
            if let Some(ff) = ffmpeg_path {
                if let Err(e) = run_full_decode_check(ff, p) {
                    return record_integrity_outcome(
                        db,
                        &entry.file_key,
                        &entry.path,
                        "full_decode",
                        "corrupted",
                        Some(b3),
                        Some(s2),
                        &format!("Full A/V decode failed: {e}"),
                        now,
                    );
                }
            }
        }
    }

    record_integrity_outcome(
        db,
        &entry.file_key,
        &entry.path,
        if full_decode { "full_decode_and_checksum" } else { "checksum_and_probe" },
        "valid",
        Some(b3),
        Some(s2),
        "Checksum and container integrity verified.",
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_integrity_outcome(
    db: &Database,
    file_key: &str,
    path: &str,
    check_type: &str,
    status: &str,
    blake3_hash: Option<String>,
    sha256_hash: Option<String>,
    details: &str,
    now: i64,
) -> Result<IntegrityCheckRecord, String> {
    let id = Uuid::new_v4().to_string();
    let backup_copy_count = count_distinct_volume_copies(db, file_key, blake3_hash.as_deref())?;
    let backup_warning = if backup_copy_count <= 1 {
        Some("Single archived copy only — checksums detect corruption but cannot protect against disk failure.".to_string())
    } else {
        None
    };

    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO integrity_results (id, file_key, check_type, status, blake3_hash, sha256_hash, details, checked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, file_key, check_type, status, blake3_hash, sha256_hash, details, now],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE media_files SET integrity_status = ?1, analysis_level = MAX(analysis_level, 5), updated_at = ?2 WHERE file_key = ?3",
        params![status, now, file_key],
    )
    .map_err(|e| e.to_string())?;

    Ok(IntegrityCheckRecord {
        id,
        file_key: file_key.to_string(),
        path: path.to_string(),
        check_type: check_type.to_string(),
        status: status.to_string(),
        blake3_hash,
        sha256_hash,
        backup_copy_count,
        backup_warning,
        details: Some(details.to_string()),
        checked_at: now,
    })
}

fn count_distinct_volume_copies(
    db: &Database,
    file_key: &str,
    blake3_opt: Option<&str>,
) -> Result<usize, String> {
    let conn = db.lock_conn()?;
    if let Some(b3) = blake3_opt {
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT volume_id) FROM media_files WHERE blake3_hash = ?1",
                params![b3],
                |r| r.get(0),
            )
            .unwrap_or(1);
        return Ok(cnt.max(1) as usize);
    }
    let _ = file_key;
    Ok(1)
}

pub fn generate_checksum_manifest(
    db: &Database,
    file_keys: Option<&[String]>,
) -> Result<ChecksumManifest, String> {
    let page = query_catalog(
        db,
        &CatalogFilter {
            online_only: Some(true),
            limit: Some(250_000),
            ..Default::default()
        },
    )?;
    let filter_set: Option<std::collections::HashSet<&str>> =
        file_keys.map(|keys| keys.iter().map(|s| s.as_str()).collect());
    let mut entries = Vec::new();
    let now = Utc::now().timestamp();

    for e in page.items {
        if let Some(ref set) = filter_set {
            if !set.contains(e.file_key.as_str()) {
                continue;
            }
        }
        let (b3, s2) = if let (Some(b), Some(s)) = (e.blake3_hash.clone(), e.sha256_hash.clone()) {
            (b, s)
        } else {
            let Ok((b, s_opt)) = compute_file_hashes(Path::new(&e.path), true) else {
                continue;
            };
            let s = s_opt.unwrap_or_default();
            let conn = db.lock_conn()?;
            conn.execute(
                "UPDATE media_files SET blake3_hash = ?1, sha256_hash = ?2, updated_at = ?3 WHERE file_key = ?4",
                params![b, s, now, e.file_key],
            )
            .map_err(|err| err.to_string())?;
            (b, s)
        };
        entries.push(ChecksumManifestEntry {
            file_key: e.file_key,
            path: e.path,
            size_bytes: e.size_bytes,
            blake3: b3,
            sha256: s2,
            integrity_status: e.integrity_status,
        });
    }

    Ok(ChecksumManifest {
        generated_at: now,
        entry_count: entries.len(),
        entries,
        single_copy_warning: "Checksums detect corruption and silent bit-rot, but do not repair damaged blocks without parity (PAR2) or a separate backup volume.".to_string(),
    })
}

pub fn export_checksum_manifest_file(
    db: &Database,
    dest_path: &Path,
    format: &str, // "json" | "sha256sum"
    file_keys: Option<&[String]>,
) -> Result<usize, String> {
    let manifest = generate_checksum_manifest(db, file_keys)?;
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if format == "sha256sum" {
        let mut lines = Vec::with_capacity(manifest.entries.len());
        for e in &manifest.entries {
            lines.push(format!("{} *{}", e.sha256, e.path));
        }
        std::fs::write(dest_path, lines.join("\n")).map_err(|e| e.to_string())?;
    } else {
        let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(dest_path, json).map_err(|e| e.to_string())?;
    }
    Ok(manifest.entry_count)
}

pub fn list_integrity_history(db: &Database, limit: usize) -> Result<Vec<IntegrityCheckRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT ir.id, ir.file_key, COALESCE(fl.path, ''), ir.check_type, ir.status,
                    ir.blake3_hash, ir.sha256_hash, ir.details, ir.checked_at
             FROM integrity_results ir
             LEFT JOIN file_locations fl ON fl.file_key = ir.file_key AND fl.is_current = 1
             ORDER BY ir.checked_at DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit.min(500) as i64], |r| {
            let b3: Option<String> = r.get(5)?;
            Ok(IntegrityCheckRecord {
                id: r.get(0)?,
                file_key: r.get(1)?,
                path: r.get(2)?,
                check_type: r.get(3)?,
                status: r.get(4)?,
                blake3_hash: b3,
                sha256_hash: r.get(6)?,
                backup_copy_count: 1,
                backup_warning: None,
                details: r.get(7)?,
                checked_at: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut list: Vec<IntegrityCheckRecord> = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    drop(stmt);

    let mut b3_vols: HashMap<String, usize> = HashMap::new();
    let mut vstmt = conn
        .prepare("SELECT blake3_hash, COUNT(DISTINCT volume_id) FROM media_files WHERE blake3_hash IS NOT NULL GROUP BY blake3_hash")
        .map_err(|e| e.to_string())?;
    for row in vstmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))
        .map_err(|e| e.to_string())?
        .flatten()
    {
        b3_vols.insert(row.0, row.1);
    }
    for rec in &mut list {
        if let Some(ref b3) = rec.blake3_hash {
            rec.backup_copy_count = *b3_vols.get(b3).unwrap_or(&1);
        }
        if rec.backup_copy_count <= 1 {
            rec.backup_warning = Some("Single copy only (no redundant volume backup detected).".to_string());
        }
    }
    Ok(list)
}

pub fn create_par2_parity_if_available(target_path: &Path, redundancy_pct: u32) -> Result<String, String> {
    let Some(par2) = discover_par2() else {
        return Err("PAR2 parity tool (`par2`) is not installed on PATH.".to_string());
    };
    let mut cmd = Command::new(par2);
    cmd.args(["c", &format!("-r{}", redundancy_pct.clamp(5, 50))])
        .arg(to_long_fs_path(target_path));
    apply_no_window(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Failed launching par2: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok("Created PAR2 parity recovery files.".to_string())
}
