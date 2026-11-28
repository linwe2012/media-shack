use crate::catalog::{
    normalize_display_path, query_catalog, refresh_fts_for_asset, to_long_fs_path, CatalogEntry,
    CatalogFilter, CatalogPage,
};
use crate::db::{Database, FileOpJournalRecord};
use crate::models::{JobStatus, MoveByTagItem, MoveByTagResult};
use crate::planner::{build_track_plan, sanitize_path_segment};
use crate::platform::{
    atomic_publish_no_replace, capture_file_identity, capture_file_identity_with_blake3,
    disk_space_for_path, move_to_recoverable_trash_verified, PublishOutcome,
};
use chrono::Utc;
use regex::Regex;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagRecord {
    pub id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub leaf_name: String,
    pub color: Option<String>,
    pub aliases: Vec<String>,
    pub usage_count: usize,
    pub manual_count: usize,
    pub ai_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub filter: CatalogFilter,
    pub is_builtin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RenameOptions {
    pub template: String,
    pub find_text: Option<String>,
    pub replace_text: Option<String>,
    pub use_regex: bool,
    pub case_mode: Option<String>, // "preserve" | "lower" | "upper" | "title"
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub remove_text: Option<String>,
    pub normalize_unicode: bool,
    pub start_index: Option<usize>,
    pub index_pad: Option<usize>,
    pub multi_value_policy: Option<String>, // "first" | "join"
    pub dest_dir_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOpPreviewItem {
    pub file_key: String,
    pub asset_id: String,
    pub old_path: String,
    pub new_path: String,
    pub op_kind: String, // "rename" | "move" | "copy" | "move_rename"
    pub cross_volume: bool,
    pub collision: bool,
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOpPreviewResult {
    #[serde(default)]
    pub plan_id: Option<String>,
    pub items: Vec<FileOpPreviewItem>,
    pub valid_count: usize,
    pub error_count: usize,
    pub collision_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub file_key: String,
    pub op_kind: String,
    pub from_path: String,
    pub to_path: String,
    pub blake3_hash: Option<String>,
    pub cross_volume: bool,
    #[serde(default)]
    pub src_volume_id: Option<String>,
    #[serde(default)]
    pub src_file_id: Option<String>,
    #[serde(default)]
    pub dst_file_id: Option<String>,
    #[serde(default)]
    pub status_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationHistoryRecord {
    pub id: String,
    pub op_type: String,
    pub summary: String,
    pub entries: Vec<JournalEntry>,
    pub state: String,
    pub created_at: i64,
    pub undone_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOpExecResult {
    pub operation_id: String,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

pub fn parse_tag_parts(raw: &str) -> (String, Option<String>, String) {
    let trimmed = raw.trim();
    if let Some((ns, leaf)) = trimmed.split_once(':') {
        let ns_clean = ns.trim().to_string();
        let leaf_clean = leaf.trim().to_string();
        if !ns_clean.is_empty() && !leaf_clean.is_empty() {
            return (format!("{ns_clean}:{leaf_clean}"), Some(ns_clean), leaf_clean);
        }
    }
    (trimmed.to_string(), None, trimmed.to_string())
}

pub fn create_or_get_tag(
    db: &Database,
    name: &str,
    color: Option<&str>,
) -> Result<TagRecord, String> {
    let (norm_name, namespace, leaf_name) = parse_tag_parts(name);
    if norm_name.is_empty() {
        return Err("Tag name cannot be empty.".to_string());
    }
    let conn = db.lock_conn()?;
    let now = Utc::now().timestamp();

    // Check exact tag or alias match
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM tags WHERE LOWER(name) = LOWER(?1)",
            params![norm_name],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let id = if let Some(eid) = existing_id {
        if let Some(c) = color {
            conn.execute("UPDATE tags SET color = ?1 WHERE id = ?2", params![c, eid])
                .map_err(|e| e.to_string())?;
        }
        eid
    } else {
        // Also resolve alias if a tag already owns this alias
        let mut stmt = conn
            .prepare("SELECT id, aliases_json FROM tags")
            .map_err(|e| e.to_string())?;
        let mut alias_hit: Option<String> = None;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let aliases: Vec<String> = serde_json::from_str(&row.1).unwrap_or_default();
            if aliases.iter().any(|a| a.eq_ignore_ascii_case(&norm_name)) {
                alias_hit = Some(row.0);
                break;
            }
        }
        drop(stmt);

        if let Some(hit) = alias_hit {
            hit
        } else {
            let nid = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO tags (id, name, namespace, leaf_name, color, aliases_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '[]', ?6)",
                params![nid, norm_name, namespace, leaf_name, color, now],
            )
            .map_err(|e| e.to_string())?;
            nid
        }
    };
    drop(conn);
    list_tags(db)?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| "Failed to read created tag".to_string())
}

pub fn list_tags(db: &Database) -> Result<Vec<TagRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT t.id, t.name, t.namespace, t.leaf_name, t.color, t.aliases_json, t.created_at,
                    COALESCE(SUM(CASE WHEN mt.status = 'active' THEN 1 ELSE 0 END), 0) AS active_cnt,
                    COALESCE(SUM(CASE WHEN mt.status = 'active' AND mt.source = 'manual' THEN 1 ELSE 0 END), 0) AS manual_cnt,
                    COALESCE(SUM(CASE WHEN mt.status = 'active' AND mt.source = 'ai_suggested' THEN 1 ELSE 0 END), 0) AS ai_cnt
             FROM tags t
             LEFT JOIN media_tags mt ON mt.tag_id = t.id
             GROUP BY t.id
             ORDER BY active_cnt DESC, t.name ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let aliases_s: String = r.get(5)?;
            Ok(TagRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                namespace: r.get(2)?,
                leaf_name: r.get(3)?,
                color: r.get(4)?,
                aliases: serde_json::from_str(&aliases_s).unwrap_or_default(),
                created_at: r.get(6)?,
                usage_count: r.get::<_, i64>(7)?.max(0) as usize,
                manual_count: r.get::<_, i64>(8)?.max(0) as usize,
                ai_count: r.get::<_, i64>(9)?.max(0) as usize,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn suggest_tags(db: &Database, prefix: &str) -> Result<Vec<TagRecord>, String> {
    let p = prefix.trim().to_ascii_lowercase();
    let all = list_tags(db)?;
    if p.is_empty() {
        return Ok(all.into_iter().take(25).collect());
    }
    Ok(all
        .into_iter()
        .filter(|t| {
            t.name.to_ascii_lowercase().contains(&p)
                || t.aliases.iter().any(|a| a.to_ascii_lowercase().contains(&p))
        })
        .take(25)
        .collect())
}

pub fn add_tag_alias(db: &Database, tag_id: &str, alias: &str) -> Result<(), String> {
    let clean = alias.trim();
    if clean.is_empty() {
        return Err("Alias cannot be empty.".to_string());
    }
    let conn = db.lock_conn()?;
    let raw: String = conn
        .query_row("SELECT aliases_json FROM tags WHERE id = ?1", params![tag_id], |r| r.get(0))
        .map_err(|e| format!("Tag not found: {e}"))?;
    let mut aliases: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    if !aliases.iter().any(|a| a.eq_ignore_ascii_case(clean)) {
        aliases.push(clean.to_string());
    }
    let updated = serde_json::to_string(&aliases).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE tags SET aliases_json = ?1 WHERE id = ?2",
        params![updated, tag_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn merge_tags(db: &Database, source_tag_id: &str, target_tag_id: &str) -> Result<usize, String> {
    if source_tag_id == target_tag_id {
        return Err("Source and target tags must be different.".to_string());
    }
    let affected_assets: Vec<String>;
    {
        let mut conn = db.lock_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let src_name: String = tx
            .query_row("SELECT name FROM tags WHERE id = ?1", params![source_tag_id], |r| r.get(0))
            .map_err(|e| format!("Source tag not found: {e}"))?;
        let tgt_aliases_s: String = tx
            .query_row("SELECT aliases_json FROM tags WHERE id = ?1", params![target_tag_id], |r| r.get(0))
            .map_err(|e| format!("Target tag not found: {e}"))?;

        let mut stmt = tx
            .prepare("SELECT asset_id FROM media_tags WHERE tag_id = ?1")
            .map_err(|e| e.to_string())?;
        affected_assets = stmt
            .query_map(params![source_tag_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);

        tx.execute(
            "INSERT OR IGNORE INTO media_tags (asset_id, tag_id, source, confidence, status, created_at, updated_at)
             SELECT asset_id, ?1, source, confidence, status, created_at, updated_at
             FROM media_tags WHERE tag_id = ?2",
            params![target_tag_id, source_tag_id],
        )
        .map_err(|e| e.to_string())?;

        let mut aliases: Vec<String> = serde_json::from_str(&tgt_aliases_s).unwrap_or_default();
        if !aliases.iter().any(|a| a.eq_ignore_ascii_case(&src_name)) {
            aliases.push(src_name);
        }
        let aliases_json = serde_json::to_string(&aliases).map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE tags SET aliases_json = ?1 WHERE id = ?2",
            params![aliases_json, target_tag_id],
        )
        .map_err(|e| e.to_string())?;

        tx.execute("DELETE FROM tags WHERE id = ?1", params![source_tag_id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    for aid in &affected_assets {
        let _ = refresh_fts_for_asset(db, aid);
    }
    Ok(affected_assets.len())
}

/// Assign tag to multiple assets.
/// Enforces manual override precedence: automated (`ai_suggested` / `imported`) assignments
/// never overwrite an existing `manual` tag or resurrect a user-`rejected` tag.
pub fn assign_tags_bulk(
    db: &Database,
    asset_ids: &[String],
    tag_names: &[String],
    source: &str,
    confidence: f64,
) -> Result<usize, String> {
    let mut tag_ids = Vec::new();
    for name in tag_names {
        if name.trim().is_empty() {
            continue;
        }
        let tr = create_or_get_tag(db, name, None)?;
        tag_ids.push(tr.id);
    }
    if tag_ids.is_empty() || asset_ids.is_empty() {
        return Ok(0);
    }

    let now = Utc::now().timestamp();
    let mut assigned = 0usize;
    {
        let mut conn = db.lock_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for aid in asset_ids {
            for tid in &tag_ids {
                let existing: Option<(String, String)> = tx
                    .query_row(
                        "SELECT source, status FROM media_tags WHERE asset_id = ?1 AND tag_id = ?2",
                        params![aid, tid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;

                if let Some((cur_src, cur_status)) = existing {
                    if source != "manual" && (cur_src == "manual" || cur_status == "rejected") {
                        // Manual user corrections take precedence over later automated analysis
                        continue;
                    }
                }

                tx.execute(
                    "INSERT INTO media_tags (asset_id, tag_id, source, confidence, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)
                     ON CONFLICT(asset_id, tag_id) DO UPDATE SET
                        source = excluded.source,
                        confidence = excluded.confidence,
                        status = 'active',
                        updated_at = excluded.updated_at",
                    params![aid, tid, source, confidence, now],
                )
                .map_err(|e| e.to_string())?;
                assigned += 1;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    for aid in asset_ids {
        let _ = refresh_fts_for_asset(db, aid);
    }
    Ok(assigned)
}

pub fn remove_tags_bulk(
    db: &Database,
    asset_ids: &[String],
    tag_ids: &[String],
    mark_rejected: bool,
) -> Result<usize, String> {
    if asset_ids.is_empty() || tag_ids.is_empty() {
        return Ok(0);
    }
    let now = Utc::now().timestamp();
    let mut removed = 0usize;
    {
        let mut conn = db.lock_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for aid in asset_ids {
            for tid in tag_ids {
                let changed = if mark_rejected {
                    tx.execute(
                        "UPDATE media_tags SET status = 'rejected', source = 'manual', updated_at = ?1
                         WHERE asset_id = ?2 AND tag_id = ?3",
                        params![now, aid, tid],
                    )
                    .map_err(|e| e.to_string())?
                } else {
                    tx.execute(
                        "DELETE FROM media_tags WHERE asset_id = ?1 AND tag_id = ?2",
                        params![aid, tid],
                    )
                    .map_err(|e| e.to_string())?
                };
                removed += changed;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    for aid in asset_ids {
        let _ = refresh_fts_for_asset(db, aid);
    }
    Ok(removed)
}

pub fn seed_builtin_collections(db: &Database) -> Result<(), String> {
    let builtins: Vec<(&str, &str, CatalogFilter)> = vec![
        (
            "All VR180 / VR360 Videos",
            "Stereoscopic and spherical VR footage",
            CatalogFilter {
                vr_only: true,
                ..Default::default()
            },
        ),
        (
            "Videos Larger Than 5 GB",
            "Large media candidates for archival compression",
            CatalogFilter {
                media_type: Some("video".to_string()),
                min_size_bytes: Some(5 * 1024 * 1024 * 1024),
                ..Default::default()
            },
        ),
        (
            "H.264 Awaiting Archival",
            "H.264 videos ready to queue for HEVC/AV1 archival",
            CatalogFilter {
                media_type: Some("video".to_string()),
                codec: Some("h264".to_string()),
                ..Default::default()
            },
        ),
        (
            "Videos With Watermarks",
            "Media containing detected persistent watermarks",
            CatalogFilter {
                only_watermarked: true,
                ..Default::default()
            },
        ),
        (
            "Untagged Media",
            "Cataloged items without active tags",
            CatalogFilter {
                only_untagged: true,
                ..Default::default()
            },
        ),
        (
            "Suspected Duplicates",
            "Files belonging to exact, equivalent, or perceptual duplicate groups",
            CatalogFilter {
                only_duplicates: true,
                ..Default::default()
            },
        ),
        (
            "Corrupted / Needs Repair",
            "Media with failed structural or decoding integrity checks",
            CatalogFilter {
                integrity_status: Some("corrupted".to_string()),
                ..Default::default()
            },
        ),
    ];

    let conn = db.lock_conn()?;
    let now = Utc::now().timestamp();
    for (name, desc, filter) in builtins {
        let qjson = serde_json::to_string(&filter).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO collections (id, name, description, query_json, is_builtin, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
            params![Uuid::new_v4().to_string(), name, desc, qjson, now],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn save_collection(
    db: &Database,
    id: Option<&str>,
    name: &str,
    description: Option<&str>,
    filter: &CatalogFilter,
) -> Result<CollectionRecord, String> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err("Collection name cannot be empty.".to_string());
    }
    let cid = id
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().timestamp();
    let qjson = serde_json::to_string(filter).map_err(|e| e.to_string())?;
    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO collections (id, name, description, query_json, is_builtin, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            query_json = excluded.query_json,
            updated_at = excluded.updated_at",
        params![cid, clean, description, qjson, now],
    )
    .map_err(|e| e.to_string())?;
    drop(conn);
    list_collections(db)?
        .into_iter()
        .find(|c| c.id == cid)
        .ok_or_else(|| "Failed to load saved collection".to_string())
}

pub fn list_collections(db: &Database) -> Result<Vec<CollectionRecord>, String> {
    seed_builtin_collections(db)?;
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, query_json, is_builtin, created_at, updated_at
             FROM collections ORDER BY is_builtin DESC, name ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let qjson: String = r.get(3)?;
            Ok(CollectionRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                filter: serde_json::from_str(&qjson).unwrap_or_default(),
                is_builtin: r.get::<_, i64>(4)? != 0,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn delete_collection(db: &Database, id: &str) -> Result<(), String> {
    let conn = db.lock_conn()?;
    conn.execute(
        "DELETE FROM collections WHERE id = ?1 AND is_builtin = 0",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn evaluate_collection(db: &Database, collection_id: &str) -> Result<CatalogPage, String> {
    let col = list_collections(db)?
        .into_iter()
        .find(|c| c.id == collection_id)
        .ok_or_else(|| "Collection not found".to_string())?;
    query_catalog(db, &col.filter)
}

fn load_entries_for_file_keys(db: &Database, file_keys: &[String]) -> Result<Vec<CatalogEntry>, String> {
    let loaded = crate::catalog::load_entries_by_file_keys(db, file_keys)?;
    let mut map: HashMap<String, CatalogEntry> = loaded
        .into_iter()
        .map(|e| (e.file_key.clone(), e))
        .collect();
    let mut ordered = Vec::new();
    for fk in file_keys {
        if let Some(entry) = map.remove(fk) {
            ordered.push(entry);
        }
    }
    Ok(ordered)
}

fn render_template_for_entry(
    template: &str,
    entry: &CatalogEntry,
    index_val: usize,
    pad_width: usize,
    multi_policy: &str,
    sanitize_segments: bool,
) -> String {
    let orig_stem = Path::new(&entry.filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| entry.filename.clone());
    let title = entry
        .title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| orig_stem.clone());
    let creator = entry
        .creator
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            entry
                .tags
                .iter()
                .find(|t| t.namespace.as_deref() == Some("creator"))
                .map(|t| t.name.split_once(':').map(|(_, l)| l.to_string()).unwrap_or_else(|| t.name.clone()))
        })
        .unwrap_or_else(|| "Unknown".to_string());

    let date = entry
        .recorded_at
        .as_deref()
        .and_then(|s| s.get(0..10))
        .map(|s| s.replace(':', "-"))
        .unwrap_or_else(|| {
            chrono::DateTime::from_timestamp(entry.mtime_ns / 1_000_000_000, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "0000-00-00".to_string())
        });
    let year = date.get(0..4).unwrap_or("0000").to_string();
    let parent = Path::new(&entry.parent_dir)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let resolution = match (entry.width, entry.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "unknown".to_string(),
    };
    let codec = entry
        .video_codec
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let duration = entry
        .duration_secs
        .map(|d| format!("{}s", d.round() as u64))
        .unwrap_or_else(|| "0s".to_string());
    let idx_str = if pad_width > 0 {
        format!("{:0width$}", index_val, width = pad_width)
    } else {
        index_val.to_string()
    };

    let active_tag_names: Vec<String> = entry
        .tags
        .iter()
        .filter(|t| t.status == "active")
        .map(|t| t.name.clone())
        .collect();
    let tags_val = if active_tag_names.is_empty() {
        "untagged".to_string()
    } else if multi_policy == "join" {
        active_tag_names.join("_")
    } else {
        active_tag_names[0].clone()
    };

    let clean = |val: String| -> String {
        if sanitize_segments {
            sanitize_path_segment(&val)
        } else {
            val
        }
    };

    let mut out = template.to_string();
    let pairs = [
        ("{filename}", clean(orig_stem)),
        ("{title}", clean(title)),
        ("{creator}", clean(creator)),
        ("{date}", clean(date)),
        ("{year}", clean(year)),
        ("{parent}", clean(parent)),
        ("{resolution}", clean(resolution)),
        ("{codec}", clean(codec)),
        ("{duration}", clean(duration)),
        ("{index}", idx_str),
        ("{tags}", clean(tags_val)),
        ("{extension}", entry.extension.clone()),
    ];
    for (k, v) in pairs {
        out = out.replace(k, &v);
    }
    out
}

fn apply_rename_transforms(stem: &str, opts: &RenameOptions) -> Result<String, String> {
    let mut s = stem.to_string();
    if let Some(ref rem) = opts.remove_text {
        if !rem.is_empty() {
            s = s.replace(rem, "");
        }
    }
    if let Some(ref find) = opts.find_text {
        if !find.is_empty() {
            let rep = opts.replace_text.as_deref().unwrap_or("");
            if opts.use_regex {
                let re = Regex::new(find).map_err(|e| format!("Invalid regex: {e}"))?;
                s = re.replace_all(&s, rep).to_string();
            } else {
                s = s.replace(find, rep);
            }
        }
    }
    if let Some(ref pfx) = opts.prefix {
        s = format!("{pfx}{s}");
    }
    if let Some(ref sfx) = opts.suffix {
        s = format!("{s}{sfx}");
    }
    match opts.case_mode.as_deref() {
        Some("lower") => s = s.to_lowercase(),
        Some("upper") => s = s.to_uppercase(),
        Some("title") => {
            s = s
                .split_whitespace()
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
        _ => {}
    }
    if opts.normalize_unicode {
        // Normalize common compatibility whitespace/punctuation cleanly
        s = s
            .replace('\u{3000}', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    Ok(s)
}

fn validate_target_path(path: &Path) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.trim().is_empty() {
        return Some("Resulting filename is empty.".to_string());
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Some("Filename cannot end with a space or dot on Windows.".to_string());
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Some(format!("Reserved Windows device name: {stem}"));
    }
    if name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')) {
        return Some("Filename contains invalid Windows characters.".to_string());
    }
    if normalize_display_path(path).len() > 255 {
        return Some("Path exceeds 255 characters.".to_string());
    }
    None
}

pub fn preview_batch_rename(
    db: &Database,
    file_keys: &[String],
    opts: &RenameOptions,
) -> Result<FileOpPreviewResult, String> {
    let entries = load_entries_for_file_keys(db, file_keys)?;
    let start_idx = opts.start_index.unwrap_or(1);
    let pad = opts.index_pad.unwrap_or(3);
    let multi = opts.multi_value_policy.as_deref().unwrap_or("first");
    let tmpl = if opts.template.trim().is_empty() {
        "{filename}.{extension}".to_string()
    } else {
        opts.template.clone()
    };

    let source_set: HashSet<String> = entries
        .iter()
        .map(|e| e.path.to_ascii_lowercase())
        .collect();
    let mut seen_targets: HashMap<String, usize> = HashMap::new();
    let mut items = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        let rendered = render_template_for_entry(&tmpl, entry, start_idx + i, pad, multi, true);
        let p_rendered = Path::new(&rendered);
        let raw_stem = p_rendered
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(rendered.clone());
        let raw_ext = p_rendered
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| entry.extension.clone());

        let transformed_stem = apply_rename_transforms(&raw_stem, opts)?;
        let raw_upper = transformed_stem.trim().to_ascii_uppercase();
        const RESERVED_NAMES: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let reserved_err = if RESERVED_NAMES.contains(&raw_upper.as_str()) {
            Some(format!("Reserved Windows device name: {raw_upper}"))
        } else {
            None
        };
        let safe_stem = sanitize_path_segment(&transformed_stem);
        let new_filename = if raw_ext.is_empty() {
            safe_stem
        } else {
            format!("{safe_stem}.{raw_ext}")
        };
        let new_path = Path::new(&entry.parent_dir).join(&new_filename);
        let new_path_str = normalize_display_path(&new_path);
        let lower = new_path_str.to_ascii_lowercase();
        *seen_targets.entry(lower.clone()).or_insert(0) += 1;

        let mut validation_error = reserved_err.or_else(|| validate_target_path(&new_path));
        let mut collision = false;
        if new_path_str != entry.path {
            let exists_on_disk = to_long_fs_path(&new_path).exists();
            // Case-only rename on Windows (`a.mp4` -> `A.mp4`) or cycle (`A->B, B->A`) is safe if part of source_set
            if exists_on_disk && !source_set.contains(&lower) {
                collision = true;
                if validation_error.is_none() {
                    validation_error = Some("Destination file already exists on disk.".to_string());
                }
            }
        }

        items.push(FileOpPreviewItem {
            file_key: entry.file_key.clone(),
            asset_id: entry.asset_id.clone(),
            old_path: entry.path.clone(),
            new_path: new_path_str,
            op_kind: "rename".to_string(),
            cross_volume: false,
            collision,
            validation_error,
        });
    }

    for it in &mut items {
        let lower = it.new_path.to_ascii_lowercase();
        if seen_targets.get(&lower).copied().unwrap_or(0) > 1 {
            it.collision = true;
            if it.validation_error.is_none() {
                it.validation_error = Some("Multiple items resolve to the same target filename.".to_string());
            }
        }
    }

    let valid_count = items.iter().filter(|i| i.validation_error.is_none()).count();
    let collision_count = items.iter().filter(|i| i.collision).count();
    let error_count = items.len() - valid_count;
    let plan_id = save_batch_op_plan(db, "library_batch_op", None, &items).ok();
    Ok(FileOpPreviewResult {
        plan_id,
        items,
        valid_count,
        error_count,
        collision_count,
    })
}

pub fn preview_batch_move_or_copy(
    db: &Database,
    file_keys: &[String],
    dest_root: &Path,
    folder_template: Option<&str>,
    is_copy: bool,
    multi_value_policy: Option<&str>,
) -> Result<FileOpPreviewResult, String> {
    let entries = load_entries_for_file_keys(db, file_keys)?;
    let multi = multi_value_policy.unwrap_or("first");
    let dest_vol = disk_space_for_path(dest_root).ok().map(|v| v.volume_id);
    let mut seen_targets: HashMap<String, usize> = HashMap::new();
    let mut items = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        let rel_str = match folder_template {
            Some(t) if !t.trim().is_empty() => {
                render_template_for_entry(t, entry, i + 1, 3, multi, false)
            }
            _ => entry.filename.clone(),
        };
        let mut target = dest_root.to_path_buf();
        for comp in Path::new(&rel_str).components() {
            if let std::path::Component::Normal(os) = comp {
                target.push(sanitize_path_segment(&os.to_string_lossy()));
            }
        }
        let target_str = normalize_display_path(&target);
        let lower = target_str.to_ascii_lowercase();
        *seen_targets.entry(lower.clone()).or_insert(0) += 1;

        let cross_volume = dest_vol
            .as_deref()
            .map(|dv| dv != entry.volume_id)
            .unwrap_or(true);
        let mut validation_error = validate_target_path(&target);
        let mut collision = false;
        if target_str != entry.path && path_exists_case_aware(&target) {
            collision = true;
            if validation_error.is_none() {
                validation_error = Some("Destination path already exists.".to_string());
            }
        }
        let op_kind = if is_copy {
            "copy".to_string()
        } else if folder_template.is_some() {
            "move_rename".to_string()
        } else {
            "move".to_string()
        };

        items.push(FileOpPreviewItem {
            file_key: entry.file_key.clone(),
            asset_id: entry.asset_id.clone(),
            old_path: entry.path.clone(),
            new_path: target_str,
            op_kind,
            cross_volume,
            collision,
            validation_error,
        });
    }

    for it in &mut items {
        let lower = it.new_path.to_ascii_lowercase();
        if seen_targets.get(&lower).copied().unwrap_or(0) > 1 {
            it.collision = true;
            if it.validation_error.is_none() {
                it.validation_error = Some("Collision: multiple files target the same destination.".to_string());
            }
        }
    }

    let valid_count = items.iter().filter(|i| i.validation_error.is_none()).count();
    let collision_count = items.iter().filter(|i| i.collision).count();
    let error_count = items.len() - valid_count;
    let plan_id = save_batch_op_plan(db, "library_batch_op", None, &items).ok();
    Ok(FileOpPreviewResult {
        plan_id,
        items,
        valid_count,
        error_count,
        collision_count,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlannedBatchItemBound {
    pub item: FileOpPreviewItem,
    pub src_volume_id: String,
    pub src_file_id: String,
    pub src_size: u64,
    pub src_head_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewedBatchOpPlan {
    pub summary: Option<String>,
    pub bound_items: Vec<PlannedBatchItemBound>,
}

fn path_exists_case_aware(target: &Path) -> bool {
    let long_p = to_long_fs_path(target);
    if long_p.exists() {
        return true;
    }
    let Some(parent) = target.parent() else {
        return false;
    };
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !parent.exists() {
        return false;
    }
    let target_lower = name.to_ascii_lowercase();
    if let Ok(rd) = std::fs::read_dir(to_long_fs_path(parent)) {
        for ent in rd.flatten() {
            if ent.file_name().to_string_lossy().to_ascii_lowercase() == target_lower {
                return true;
            }
        }
    }
    false
}

fn save_batch_op_plan(
    db: &Database,
    kind: &str,
    queue_id: Option<&str>,
    items: &[FileOpPreviewItem],
) -> Result<String, String> {
    let mut bound_items = Vec::with_capacity(items.len());
    for it in items {
        let (vol, fid, sz, hh) = match capture_file_identity(Path::new(&it.old_path)) {
            Ok(fp) => (fp.volume_id, fp.file_id, fp.size_bytes, fp.head_hash),
            Err(_) => (String::new(), String::new(), 0, String::new()),
        };
        bound_items.push(PlannedBatchItemBound {
            item: it.clone(),
            src_volume_id: vol,
            src_file_id: fid,
            src_size: sz,
            src_head_hash: hh,
        });
    }
    let plan_id = Uuid::new_v4().to_string();
    let payload = ReviewedBatchOpPlan {
        summary: None,
        bound_items,
    };
    let json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    db.save_reviewed_plan(&plan_id, kind, queue_id, &json)?;
    Ok(plan_id)
}

fn blake3_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(to_long_fs_path(path)).map_err(|e| format!("Open failed: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("Read failed: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn copy_and_verify_blake3(
    src: &Path,
    dst: &Path,
    expected_fp: Option<&crate::models::SourceFingerprint>,
) -> Result<String, String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(to_long_fs_path(parent))
            .map_err(|e| format!("Create directory failed: {e}"))?;
    }
    // Never follow symlinks on source and recheck identity before copying
    let pre_fp = capture_file_identity(src)?;
    if let Some(exp) = expected_fp {
        if !exp.file_id.is_empty()
            && (pre_fp.file_id != exp.file_id
                || pre_fp.volume_id != exp.volume_id
                || pre_fp.size_bytes != exp.size_bytes
                || pre_fp.head_hash != exp.head_hash)
        {
            return Err("Source file identity changed before copy.".to_string());
        }
    }

    let tmp = dst.with_file_name(format!(".mediashack-tmp-{}", Uuid::new_v4()));
    let mut src_opts = std::fs::OpenOptions::new();
    src_opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        src_opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut src_f = src_opts
        .open(to_long_fs_path(src))
        .map_err(|e| format!("Open source failed: {e}"))?;

    // Private exclusive staging file creation (O_CREAT | O_EXCL)
    let mut tmp_opts = std::fs::OpenOptions::new();
    tmp_opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        tmp_opts.mode(0o600);
    }
    let mut tmp_f = tmp_opts
        .open(to_long_fs_path(&tmp))
        .map_err(|e| format!("Create private staging temp failed: {e}"))?;

    let mut src_hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = src_f.read(&mut buf).map_err(|e| format!("Read failed: {e}"))?;
        if n == 0 {
            break;
        }
        src_hasher.update(&buf[..n]);
        tmp_f.write_all(&buf[..n]).map_err(|e| format!("Write failed: {e}"))?;
    }
    tmp_f.sync_all().map_err(|e| format!("Sync failed: {e}"))?;
    drop(tmp_f);

    let src_hash = src_hasher.finalize().to_hex().to_string();
    let dst_hash = blake3_file(&tmp)?;
    if src_hash != dst_hash {
        let _ = std::fs::remove_file(to_long_fs_path(&tmp));
        return Err("Copy verification failed: BLAKE3 mismatch.".to_string());
    }
    if path_exists_case_aware(dst) {
        let _ = std::fs::remove_file(to_long_fs_path(&tmp));
        return Err("Destination already exists before commit (existing file preserved).".to_string());
    }

    match atomic_publish_no_replace(&tmp, dst) {
        PublishOutcome::Published => Ok(src_hash),
        PublishOutcome::DestinationExists => {
            let _ = std::fs::remove_file(to_long_fs_path(&tmp));
            Err("Destination created concurrently before commit (existing file preserved).".to_string())
        }
        PublishOutcome::Error(e) => {
            let _ = std::fs::remove_file(to_long_fs_path(&tmp));
            Err(format!("Commit no-replace rename failed: {e}"))
        }
    }
}

fn check_active_job_using_file(
    db: &Database,
    src_path: &Path,
    src_fp: Option<&crate::models::SourceFingerprint>,
) -> Result<(), String> {
    let all_jobs = db.list_all_jobs()?;
    let norm = normalize_display_path(src_path);
    for j in all_jobs {
        let active = matches!(
            j.status,
            JobStatus::Probing | JobStatus::Encoding | JobStatus::Verifying | JobStatus::Publishing
        );
        if !active {
            continue;
        }
        let same = match (src_fp, &j.source_fingerprint) {
            (Some(fp), Some(jfp)) => !fp.file_id.is_empty() && fp.file_id == jfp.file_id && fp.volume_id == jfp.volume_id,
            _ => normalize_display_path(Path::new(&j.source_path)) == norm,
        };
        if same {
            return Err(format!(
                "Deferred: file is currently held by active job '{}'.",
                j.source_rel_path
            ));
        }
    }
    Ok(())
}

fn sync_jobs_after_file_move(
    db: &Database,
    old_path: &str,
    new_path: &Path,
    new_fp: Option<&crate::models::SourceFingerprint>,
) -> Result<(), String> {
    let all_jobs = db.list_all_jobs()?;
    let old_norm = normalize_display_path(Path::new(old_path));
    let new_str = normalize_display_path(new_path);
    let new_name = new_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    let new_parent = new_path.parent().map(normalize_display_path);
    let now = Utc::now().timestamp();

    for mut j in all_jobs {
        // Keep historical Completed records untouched as accurate audit of paths used at time
        if j.status == JobStatus::Completed {
            continue;
        }
        let same = match (new_fp, &j.source_fingerprint) {
            (Some(fp), Some(jfp)) => !fp.file_id.is_empty() && fp.file_id == jfp.file_id && fp.volume_id == jfp.volume_id,
            _ => false,
        } || normalize_display_path(Path::new(&j.source_path)) == old_norm
            || j.source_path == old_path;

        if !same {
            continue;
        }

        j.source_path = new_str.clone();
        j.source_rel_path = new_name.clone();
        if let Some(ref p) = new_parent {
            j.source_root = Some(p.clone());
        }
        if let Some(fp) = new_fp {
            j.source_fingerprint = Some(fp.clone());
        }
        // Revoke stale cleanup consent when path changes
        j.consent_id = None;
        if let Some(ref probe) = j.probe {
            let plan = build_track_plan(new_path, probe, &j.recipe, &j.compat_overrides);
            if matches!(j.status, JobStatus::Ready | JobStatus::NeedsAttention) {
                j.status = if plan.is_ready { JobStatus::Ready } else { JobStatus::NeedsAttention };
            }
            j.plan = Some(plan);
        }
        j.updated_at = now;
        db.upsert_job(&j)?;
    }

    crate::cleanup::update_protected_entries_after_move(db, old_path, &new_str, new_fp)?;
    Ok(())
}

/// Execute a stored reviewed Batch Organize plan (`plan_id`) enforcing identity & freshness checks.
pub fn execute_batch_plan_by_id(
    db: &Database,
    plan_id: &str,
    op_summary: &str,
) -> Result<BatchOpExecResult, String> {
    let (_qid, payload_json) = db.take_reviewed_plan(plan_id, "library_batch_op")?;
    let plan: ReviewedBatchOpPlan = serde_json::from_str(&payload_json)
        .map_err(|e| format!("Invalid reviewed organize plan: {e}"))?;
    execute_batch_file_operations_bound(db, &plan.bound_items, op_summary)
}

/// Execute validated batch rename, move, or copy with per-item phase journaling (`file_op_journal`),
/// atomic no-replace primitives, verified cross-volume copy-then-remove, and queue/catalog sync.
pub fn execute_batch_file_operations(
    db: &Database,
    items: &[FileOpPreviewItem],
    op_summary: &str,
) -> Result<BatchOpExecResult, String> {
    let mut bound = Vec::with_capacity(items.len());
    for it in items {
        let (vol, fid, sz, hh) = match capture_file_identity(Path::new(&it.old_path)) {
            Ok(fp) => (fp.volume_id, fp.file_id, fp.size_bytes, fp.head_hash),
            Err(_) => (String::new(), String::new(), 0, String::new()),
        };
        bound.push(PlannedBatchItemBound {
            item: it.clone(),
            src_volume_id: vol,
            src_file_id: fid,
            src_size: sz,
            src_head_hash: hh,
        });
    }
    execute_batch_file_operations_bound(db, &bound, op_summary)
}

fn execute_batch_file_operations_bound(
    db: &Database,
    bound_items: &[PlannedBatchItemBound],
    op_summary: &str,
) -> Result<BatchOpExecResult, String> {
    if let Some(bad) = bound_items.iter().find(|b| b.item.validation_error.is_some()) {
        return Err(bad
            .item
            .validation_error
            .clone()
            .unwrap_or_else(|| "Validation error in batch preview".to_string()));
    }

    let op_id = Uuid::new_v4().to_string();
    let active_bounds: Vec<(usize, &PlannedBatchItemBound)> = bound_items
        .iter()
        .enumerate()
        .filter(|(_, b)| b.item.old_path != b.item.new_path)
        .collect();

    // Persist complete per-item plan journal BEFORE any filesystem mutation; abort if journaling fails.
    if !active_bounds.is_empty() {
        let journal_rows: Vec<FileOpJournalRecord> = active_bounds
            .iter()
            .map(|(idx, b)| FileOpJournalRecord {
                op_id: op_id.clone(),
                item_idx: *idx,
                item_id: b.item.file_key.clone(),
                op_kind: b.item.op_kind.clone(),
                phase: "planned".to_string(),
                from_path: b.item.old_path.clone(),
                staging_path: None,
                to_path: b.item.new_path.clone(),
                src_volume_id: Some(b.src_volume_id.clone()).filter(|s| !s.is_empty()),
                src_file_id: Some(b.src_file_id.clone()).filter(|s| !s.is_empty()),
                src_size: Some(b.src_size),
                src_head_hash: Some(b.src_head_hash.clone()).filter(|s| !s.is_empty()),
                dst_file_id: None,
                blake3_hash: None,
                cross_volume: b.item.cross_volume,
                error_message: None,
            })
            .collect();
        db.insert_file_op_journal_batch(&journal_rows)?;
    }

    let mut journal: Vec<JournalEntry> = Vec::new();
    let mut succeeded = 0usize;
    let mut errors = Vec::new();

    // Separate same-volume moves (staged via private temp names to break cycles) vs cross-volume moves vs copies
    let mut staged_same_vol: Vec<(usize, &PlannedBatchItemBound, PathBuf)> = Vec::new();
    let mut copy_items: Vec<(usize, &PlannedBatchItemBound)> = Vec::new();

    let source_lower_set: HashSet<String> = active_bounds
        .iter()
        .map(|(_, b)| b.item.old_path.to_ascii_lowercase())
        .collect();

    for &(idx, b) in &active_bounds {
        let it = &b.item;
        if it.op_kind == "copy" {
            copy_items.push((idx, b));
            continue;
        }
        let src = Path::new(&it.old_path);
        let dst = Path::new(&it.new_path);

        let src_fp = match capture_file_identity(src) {
            Ok(fp) => fp,
            Err(e) => {
                let msg = format!("Missing or unreadable source {}: {e}", it.old_path);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
                errors.push(msg);
                continue;
            }
        };
        if !b.src_file_id.is_empty() && (src_fp.file_id != b.src_file_id || src_fp.head_hash != b.src_head_hash) {
            let msg = format!("Source file changed since preview ({}); left untouched.", it.old_path);
            let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
            errors.push(msg);
            continue;
        }
        if let Err(msg) = check_active_job_using_file(db, src, Some(&src_fp)) {
            let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
            errors.push(msg);
            continue;
        }

        // Confirm whether move is actually cross-volume using volume IDs
        let src_vol = disk_space_for_path(src).ok().map(|v| v.volume_id);
        let dst_vol = disk_space_for_path(dst).ok().map(|v| v.volume_id);
        let is_confirmed_cross_vol = match (src_vol, dst_vol) {
            (Some(sv), Some(dv)) => sv != dv,
            _ => it.cross_volume,
        };

        if is_confirmed_cross_vol {
            match copy_and_verify_blake3(src, dst, Some(&src_fp)) {
                Ok(hash) => {
                    let dst_fp = capture_file_identity(dst).ok();
                    let dst_fid = dst_fp.as_ref().map(|f| f.file_id.clone());
                    let _ = db.update_file_op_journal_phase(
                        &op_id,
                        idx,
                        "copied_verified",
                        None,
                        dst_fid.as_deref(),
                        Some(&hash),
                        None,
                    );

                    // Recheck source identity & protected status before source removal
                    let re_fp = capture_file_identity(src);
                    let prot = crate::cleanup::is_file_protected(db, Some(src), re_fp.as_ref().ok());
                    let remove_res = match (re_fp, prot) {
                        (_, Ok(true)) => Err("Source is Protected ★; Copied; original retained.".to_string()),
                        (_, Err(e)) => Err(format!("Protection check error ({e}); Copied; original retained.")),
                        (Ok(cur), Ok(false)) if cur.file_id == src_fp.file_id && cur.head_hash == src_fp.head_hash => {
                            std::fs::remove_file(to_long_fs_path(src))
                                .map_err(|e| format!("Failed removing source: {e}; Copied; original retained."))
                        }
                        _ => Err("Source identity changed after copy; Copied; original retained.".to_string()),
                    };

                    if let Err(reason) = remove_res {
                        let _ = db.update_file_op_journal_phase(
                            &op_id,
                            idx,
                            "copied_original_retained",
                            None,
                            dst_fid.as_deref(),
                            Some(&hash),
                            Some(&reason),
                        );
                        journal.push(JournalEntry {
                            file_key: it.file_key.clone(),
                            op_kind: "copy".to_string(),
                            from_path: it.old_path.clone(),
                            to_path: it.new_path.clone(),
                            blake3_hash: Some(hash),
                            cross_volume: true,
                            src_volume_id: Some(src_fp.volume_id.clone()),
                            src_file_id: Some(src_fp.file_id.clone()),
                            dst_file_id: dst_fid,
                            status_note: Some("Copied; original retained".to_string()),
                        });
                        errors.push(format!("{}: {reason}", it.new_path));
                        continue;
                    }

                    let _ = update_catalog_location_after_move(db, &it.file_key, &it.old_path, dst);
                    let _ = sync_jobs_after_file_move(db, &it.old_path, dst, dst_fp.as_ref());
                    let _ = db.update_file_op_journal_phase(
                        &op_id,
                        idx,
                        "committed",
                        None,
                        dst_fid.as_deref(),
                        Some(&hash),
                        None,
                    );
                    journal.push(JournalEntry {
                        file_key: it.file_key.clone(),
                        op_kind: it.op_kind.clone(),
                        from_path: it.old_path.clone(),
                        to_path: it.new_path.clone(),
                        blake3_hash: Some(hash),
                        cross_volume: true,
                        src_volume_id: Some(src_fp.volume_id.clone()),
                        src_file_id: Some(src_fp.file_id.clone()),
                        dst_file_id: dst_fid,
                        status_note: None,
                    });
                    succeeded += 1;
                }
                Err(e) => {
                    let msg = format!("{}: {e}", it.old_path);
                    let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
                    errors.push(msg);
                }
            }
        } else {
            // Check case-aware destination conflict before staging unless target is one of the batch sources
            let dst_lower = it.new_path.to_ascii_lowercase();
            if path_exists_case_aware(dst) && !source_lower_set.contains(&dst_lower) {
                let msg = format!("Destination already exists ({}); source kept intact.", it.new_path);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
                errors.push(msg);
                continue;
            }
            let tmp = src.with_file_name(format!(".mediashack-cycle-{}", Uuid::new_v4()));
            let tmp_str = tmp.to_string_lossy().to_string();
            // Record intermediate staging name BEFORE moving!
            db.update_file_op_journal_phase(&op_id, idx, "staged", Some(&tmp_str), None, None, None)?;
            match atomic_publish_no_replace(src, &tmp) {
                PublishOutcome::Published => staged_same_vol.push((idx, b, tmp)),
                PublishOutcome::DestinationExists | PublishOutcome::Error(_) => {
                    let msg = format!("No-replace stage failed for {}", it.old_path);
                    let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
                    errors.push(msg);
                }
            }
        }
    }

    for (idx, b, tmp) in staged_same_vol {
        let it = &b.item;
        let dst = Path::new(&it.new_path);
        let old_p = Path::new(&it.old_path);
        match atomic_publish_no_replace(&tmp, dst) {
            PublishOutcome::Published => {
                let dst_fp = capture_file_identity(dst).ok();
                let dst_fid = dst_fp.as_ref().map(|f| f.file_id.clone());
                let _ = update_catalog_location_after_move(db, &it.file_key, &it.old_path, dst);
                let _ = sync_jobs_after_file_move(db, &it.old_path, dst, dst_fp.as_ref());
                let _ = db.update_file_op_journal_phase(&op_id, idx, "committed", None, dst_fid.as_deref(), None, None);
                journal.push(JournalEntry {
                    file_key: it.file_key.clone(),
                    op_kind: it.op_kind.clone(),
                    from_path: it.old_path.clone(),
                    to_path: it.new_path.clone(),
                    blake3_hash: None,
                    cross_volume: false,
                    src_volume_id: Some(b.src_volume_id.clone()).filter(|s| !s.is_empty()),
                    src_file_id: Some(b.src_file_id.clone()).filter(|s| !s.is_empty()),
                    dst_file_id: dst_fid,
                    status_note: None,
                });
                succeeded += 1;
            }
            PublishOutcome::DestinationExists => {
                let _ = atomic_publish_no_replace(&tmp, old_p);
                let msg = format!("Destination appeared concurrently at {}; original restored.", it.new_path);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "rolled_back", None, None, None, Some(&msg));
                errors.push(msg);
            }
            PublishOutcome::Error(e) => {
                let _ = atomic_publish_no_replace(&tmp, old_p);
                let msg = format!("Commit no-replace rename failed for {}: {e}", it.new_path);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "rolled_back", None, None, None, Some(&msg));
                errors.push(msg);
            }
        }
    }

    for (idx, b) in copy_items {
        let it = &b.item;
        let src = Path::new(&it.old_path);
        let dst = Path::new(&it.new_path);
        let src_fp = capture_file_identity(src).ok();
        match copy_and_verify_blake3(src, dst, src_fp.as_ref()) {
            Ok(hash) => {
                let dst_fid = capture_file_identity(dst).ok().map(|f| f.file_id);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "committed", None, dst_fid.as_deref(), Some(&hash), None);
                journal.push(JournalEntry {
                    file_key: it.file_key.clone(),
                    op_kind: "copy".to_string(),
                    from_path: it.old_path.clone(),
                    to_path: it.new_path.clone(),
                    blake3_hash: Some(hash),
                    cross_volume: it.cross_volume,
                    src_volume_id: src_fp.as_ref().map(|f| f.volume_id.clone()),
                    src_file_id: src_fp.as_ref().map(|f| f.file_id.clone()),
                    dst_file_id: dst_fid,
                    status_note: None,
                });
                succeeded += 1;
            }
            Err(e) => {
                let msg = format!("Copy {} failed: {e}", it.old_path);
                let _ = db.update_file_op_journal_phase(&op_id, idx, "failed", None, None, None, Some(&msg));
                errors.push(msg);
            }
        }
    }

    let state = if errors.is_empty() {
        "committed"
    } else if succeeded > 0 {
        "partial"
    } else {
        "failed"
    };
    let conn = db.lock_conn()?;
    let entries_json = serde_json::to_string(&journal).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO operation_history (id, op_type, summary, entries_json, state, created_at, undone_at)
         VALUES (?1, 'batch_file_op', ?2, ?3, ?4, ?5, NULL)",
        params![op_id, op_summary, entries_json, state, Utc::now().timestamp()],
    )
    .map_err(|e| e.to_string())?;

    Ok(BatchOpExecResult {
        operation_id: op_id,
        succeeded,
        failed: errors.len(),
        errors,
    })
}

fn update_catalog_location_after_move(
    db: &Database,
    file_key: &str,
    old_path: &str,
    new_path: &Path,
) -> Result<(), String> {
    let norm_new = normalize_display_path(new_path);
    let filename = new_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    let parent_dir = new_path
        .parent()
        .map(normalize_display_path)
        .unwrap_or_default();
    let ext = new_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let now = Utc::now().timestamp();
    let fp = capture_file_identity(new_path).ok();

    let asset_id: String = {
        let mut conn = db.lock_conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let root_id: Option<String> = tx
            .query_row(
                "SELECT root_id FROM file_locations WHERE file_key = ?1 AND (path = ?2 OR is_current = 1) ORDER BY CASE WHEN path = ?2 THEN 0 ELSE 1 END LIMIT 1",
                params![file_key, old_path],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();

        tx.execute(
            "UPDATE file_locations SET is_current = 0, is_online = 0 WHERE file_key = ?1 AND (path = ?2 OR is_current = 1)",
            params![file_key, old_path],
        )
        .map_err(|e| e.to_string())?;

        tx.execute(
            "INSERT INTO file_locations (
                location_id, file_key, root_id, path, filename, parent_dir, extension,
                is_current, is_online, first_seen_at, last_seen_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, ?8, ?8)",
            params![Uuid::new_v4().to_string(), file_key, root_id, norm_new, filename, parent_dir, ext, now],
        )
        .map_err(|e| e.to_string())?;

        if let Some(ref f) = fp {
            tx.execute(
                "UPDATE media_files SET volume_id = ?1, os_file_id = ?2, updated_at = ?3 WHERE file_key = ?4",
                params![f.volume_id, f.file_id, now, file_key],
            )
            .map_err(|e| e.to_string())?;
        }

        let aid: String = tx
            .query_row(
                "SELECT asset_id FROM media_files WHERE file_key = ?1",
                params![file_key],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        aid
    };

    refresh_fts_for_asset(db, &asset_id)?;
    Ok(())
}

pub fn list_operation_history(db: &Database) -> Result<Vec<OperationHistoryRecord>, String> {
    let conn = db.lock_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, op_type, summary, entries_json, state, created_at, undone_at
             FROM operation_history ORDER BY created_at DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let ejson: String = r.get(3)?;
            Ok(OperationHistoryRecord {
                id: r.get(0)?,
                op_type: r.get(1)?,
                summary: r.get(2)?,
                entries: serde_json::from_str(&ejson).unwrap_or_default(),
                state: r.get(4)?,
                created_at: r.get(5)?,
                undone_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn undo_operation(db: &Database, operation_id: &str) -> Result<BatchOpExecResult, String> {
    let rec = list_operation_history(db)?
        .into_iter()
        .find(|o| o.id == operation_id)
        .ok_or_else(|| "Operation history record not found.".to_string())?;
    if rec.state == "undone" {
        return Err("Operation was already undone.".to_string());
    }

    let mut succeeded = 0usize;
    let mut errors = Vec::new();

    // Check conflicts before mutating
    for entry in rec.entries.iter().rev() {
        let to_p = Path::new(&entry.to_path);
        let from_p = Path::new(&entry.from_path);
        if !to_long_fs_path(to_p).exists() {
            errors.push(format!("Target missing for undo: {}", entry.to_path));
            continue;
        }
        if entry.op_kind == "copy" {
            // Undo Copy: MUST verify (1) copy at to_path matches blake3_hash, AND
            // (2) original at from_path still exists as a distinct readable file with matching content!
            // If original is gone or changed, retain the copy and explain why.
            let Some(ref expected) = entry.blake3_hash else {
                errors.push(format!("Missing verification hash for copy undo: {}; keeping copy.", entry.to_path));
                continue;
            };
            let dst_fp = match capture_file_identity_with_blake3(to_p) {
                Ok(fp) if fp.full_blake3.as_deref() == Some(expected.as_str()) => fp,
                _ => {
                    errors.push(format!("Copy at {} modified or unreadable; retaining copy.", entry.to_path));
                    continue;
                }
            };
            let orig_fp = match capture_file_identity_with_blake3(from_p) {
                Ok(fp) => fp,
                Err(e) => {
                    errors.push(format!("Original source {} is offline/missing ({e}); retaining verified copy at {}.", entry.from_path, entry.to_path));
                    continue;
                }
            };
            if orig_fp.file_id == dst_fp.file_id && orig_fp.volume_id == dst_fp.volume_id {
                errors.push(format!("Original and copy share the same physical identity; retaining {}.", entry.to_path));
                continue;
            }
            if orig_fp.full_blake3.as_deref() != Some(expected.as_str()) {
                errors.push(format!("Original source {} content changed since copy; retaining verified copy at {}.", entry.from_path, entry.to_path));
                continue;
            }
            // Dispose of the eligible copy via recoverable OS Trash only
            match move_to_recoverable_trash_verified(to_p, &dst_fp) {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(format!("Recoverable Trash unavailable for copy {} ({e}); copy kept.", entry.to_path)),
            }
            continue;
        }

        // Rename / move undo conflict check (case-aware & excluding items in same move cycle)
        let in_cycle_src = rec.entries.iter().any(|other| other.to_path.eq_ignore_ascii_case(&entry.from_path));
        if path_exists_case_aware(from_p) && !in_cycle_src {
            errors.push(format!("Undo conflict: original path {} is now occupied; keeping moved file at {}.", entry.from_path, entry.to_path));
            continue;
        }
    }

    if !errors.is_empty() && succeeded == 0 && rec.entries.iter().any(|e| e.op_kind != "copy") {
        return Err(errors.join("\n"));
    }

    // Reverse non-copy moves safely via no-replace staging (handles rename cycles)
    let mut staged_undo: Vec<(&JournalEntry, PathBuf)> = Vec::new();
    for entry in rec.entries.iter().rev() {
        if entry.op_kind == "copy" {
            continue;
        }
        let to_p = Path::new(&entry.to_path);
        let from_p = Path::new(&entry.from_path);
        if !to_long_fs_path(to_p).exists() {
            continue;
        }
        let in_cycle_src = rec.entries.iter().any(|other| other.to_path.eq_ignore_ascii_case(&entry.from_path));
        if path_exists_case_aware(from_p) && !in_cycle_src {
            continue;
        }
        let Ok(cur_fp) = capture_file_identity(to_p) else {
            continue;
        };
        if let Some(ref exp_fid) = entry.dst_file_id {
            if &cur_fp.file_id != exp_fid {
                errors.push(format!("File identity at {} changed; refusing Undo.", entry.to_path));
                continue;
            }
        }
        if entry.cross_volume {
            match copy_and_verify_blake3(to_p, from_p, Some(&cur_fp)) {
                Ok(_) => {
                    let from_fp = capture_file_identity(from_p).ok();
                    let _ = std::fs::remove_file(to_long_fs_path(to_p));
                    let _ = update_catalog_location_after_move(db, &entry.file_key, &entry.to_path, from_p);
                    let _ = sync_jobs_after_file_move(db, &entry.to_path, from_p, from_fp.as_ref());
                    succeeded += 1;
                }
                Err(e) => errors.push(format!("Undo cross-volume move failed: {e}")),
            }
        } else {
            let tmp = to_p.with_file_name(format!(".mediashack-undo-{}", Uuid::new_v4()));
            match atomic_publish_no_replace(to_p, &tmp) {
                PublishOutcome::Published => staged_undo.push((entry, tmp)),
                PublishOutcome::DestinationExists | PublishOutcome::Error(_) => {
                    errors.push(format!("Undo stage failed for {}", entry.to_path));
                }
            }
        }
    }

    for (entry, tmp) in staged_undo {
        let from_p = Path::new(&entry.from_path);
        match atomic_publish_no_replace(&tmp, from_p) {
            PublishOutcome::Published => {
                let from_fp = capture_file_identity(from_p).ok();
                let _ = update_catalog_location_after_move(db, &entry.file_key, &entry.to_path, from_p);
                let _ = sync_jobs_after_file_move(db, &entry.to_path, from_p, from_fp.as_ref());
                succeeded += 1;
            }
            PublishOutcome::DestinationExists => {
                let _ = atomic_publish_no_replace(&tmp, Path::new(&entry.to_path));
                errors.push(format!("Undo conflict: {} appeared before restore; moved file retained.", entry.from_path));
            }
            PublishOutcome::Error(e) => {
                let _ = atomic_publish_no_replace(&tmp, Path::new(&entry.to_path));
                errors.push(format!("Undo restore failed for {}: {e}", entry.from_path));
            }
        }
    }

    let conn = db.lock_conn()?;
    let new_state = if errors.is_empty() { "undone" } else { "partial" };
    conn.execute(
        "UPDATE operation_history SET state = ?1, undone_at = ?2 WHERE id = ?3",
        params![new_state, Utc::now().timestamp(), operation_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(BatchOpExecResult {
        operation_id: operation_id.to_string(),
        succeeded,
        failed: errors.len(),
        errors,
    })
}

/// Reconcile any interrupted `file_op_journal` records on launch without overwriting newly arrived files.
pub fn reconcile_organizer_on_launch(db: &Database) -> Result<(), String> {
    let pending = db.list_pending_file_op_journal()?;
    for row in pending {
        let from_p = Path::new(&row.from_path);
        let to_p = Path::new(&row.to_path);
        let stg_p = row.staging_path.as_deref().map(Path::new);

        let from_exists = to_long_fs_path(from_p).exists();
        let to_exists = to_long_fs_path(to_p).exists();
        let stg_exists = stg_p.map(|p| to_long_fs_path(p).exists()).unwrap_or(false);

        let msg = if stg_exists && (from_exists || to_exists) {
            format!(
                "Interrupted move/rename: unexpected file at {} or {}; staged item preserved at {}",
                row.from_path,
                row.to_path,
                row.staging_path.as_deref().unwrap_or("")
            )
        } else if row.phase == "copied_verified" && to_exists && from_exists {
            format!(
                "Interrupted cross-volume move: verified copy at {} and original at {} both retained.",
                row.to_path, row.from_path
            )
        } else {
            format!(
                "Interrupted file operation ({}) reconciled for review: {} -> {}",
                row.phase, row.from_path, row.to_path
            )
        };
        let _ = db.update_file_op_journal_phase(&row.op_id, row.item_idx, "uncertain", None, None, None, Some(&msg));
    }
    Ok(())
}

/// Unified Sort-to-Folder-by-Tag preview & execution powered by the same safe primitive engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TabMovePlanPayload {
    pub queue_id: String,
    pub tag_name: String,
    pub dest_dir: String,
    pub copy_only: bool,
    pub preserve_subfolders: bool,
    pub collision_policy: String,
    pub bound_items: Vec<(String, MoveByTagItem, String, String, u64, String)>, // (job_id, item, vol, fid, size, head_hash)
}

pub fn preview_tab_move_by_tag(
    db: &Database,
    queue_id: &str,
    tag_name: &str,
    dest_dir: &str,
    copy_only: bool,
    preserve_subfolders: bool,
    collision_policy: &str,
    job_tags: &HashMap<String, Vec<String>>,
) -> Result<MoveByTagResult, String> {
    let target_tag_lower = tag_name.trim().to_ascii_lowercase();
    let jobs = db.list_jobs_for_queue(queue_id)?;
    let dest_root = PathBuf::from(dest_dir.trim());
    let dest_vol = disk_space_for_path(&dest_root).ok().map(|v| v.volume_id);
    let policy = match collision_policy {
        "number" | "skip" => collision_policy.to_string(),
        _ => "keep_existing".to_string(),
    };

    let mut reserved_lower: HashSet<String> = HashSet::new();
    let mut raw_candidates = Vec::new();

    for j in jobs {
        let tags = job_tags.get(&j.id).cloned().unwrap_or_default();
        if !tags.iter().any(|t| t.to_ascii_lowercase() == target_tag_lower) {
            continue;
        }
        let src = PathBuf::from(&j.source_path);
        let fname = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| j.source_rel_path.clone());

        let mut target = dest_root.clone();
        if preserve_subfolders {
            let rel_p = Path::new(&j.source_rel_path);
            for comp in rel_p.components() {
                if let std::path::Component::Normal(os) = comp {
                    target.push(sanitize_path_segment(&os.to_string_lossy()));
                }
            }
        } else {
            target.push(&fname);
        }

        raw_candidates.push((j, src, fname, target));
    }

    // Count duplicate target paths (case-insensitive) inside this batch upfront
    let mut batch_counts: HashMap<String, usize> = HashMap::new();
    for (_, _, _, target) in &raw_candidates {
        let low = normalize_display_path(target).to_ascii_lowercase();
        *batch_counts.entry(low).or_insert(0) += 1;
    }

    let mut preview_items = Vec::new();
    let mut bound_items = Vec::new();
    let mut errors = Vec::new();

    for (j, src, fname, initial_target) in raw_candidates {
        let fp = capture_file_identity(&src);
        let src_vol = disk_space_for_path(&src).ok().map(|v| v.volume_id);
        let cross_volume = match (&src_vol, &dest_vol) {
            (Some(sv), Some(dv)) => sv != dv,
            _ => true,
        };

        let mut final_target = initial_target.clone();
        let init_lower = normalize_display_path(&initial_target).to_ascii_lowercase();
        let batch_dup = batch_counts.get(&init_lower).copied().unwrap_or(0) > 1;
        let disk_exists = final_target != src && path_exists_case_aware(&final_target);
        let mut collision = batch_dup || disk_exists;

        if collision && policy == "number" && src.exists() {
            let stem = final_target.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
            let ext = final_target.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let parent = final_target.parent().unwrap_or(&dest_root).to_path_buf();
            for n in 1usize..=999 {
                let cand_name = if ext.is_empty() {
                    format!("{stem} ({n})")
                } else {
                    format!("{stem} ({n}).{ext}")
                };
                let cand = parent.join(cand_name);
                let cand_low = normalize_display_path(&cand).to_ascii_lowercase();
                if !path_exists_case_aware(&cand) && !reserved_lower.contains(&cand_low) {
                    final_target = cand;
                    collision = false;
                    break;
                }
            }
        }

        let target_str = normalize_display_path(&final_target);
        let target_low = target_str.to_ascii_lowercase();
        let mut valid = true;
        let mut err_msg = validate_target_path(&final_target);
        if err_msg.is_some() {
            valid = false;
        } else if !src.exists() {
            valid = false;
            err_msg = Some("Source file no longer exists on disk.".to_string());
        } else if final_target == src {
            valid = false;
            err_msg = Some("Already at target location.".to_string());
        } else if !copy_only && check_active_job_using_file(db, &src, fp.as_ref().ok()).is_err() {
            valid = false;
            err_msg = Some("File is currently in use by an active job; move deferred.".to_string());
        } else if collision || reserved_lower.contains(&target_low) {
            valid = false;
            err_msg = Some(if disk_exists {
                "Destination already exists (default Keep Existing).".to_string()
            } else {
                "Collision: multiple tagged files map to this filename.".to_string()
            });
        }

        if valid {
            reserved_lower.insert(target_low);
        }
        if let Some(ref e) = err_msg {
            errors.push(format!("{fname}: {e}"));
        }

        let (vol_s, fid_s, sz_s, hh_s) = match &fp {
            Ok(f) => (f.volume_id.clone(), f.file_id.clone(), f.size_bytes, f.head_hash.clone()),
            Err(_) => (String::new(), String::new(), j.input_bytes, String::new()),
        };

        let item = MoveByTagItem {
            job_id: j.id.clone(),
            filename: fname,
            old_path: j.source_path.clone(),
            new_path: target_str,
            size_bytes: sz_s,
            valid,
            collision,
            cross_volume,
            copied_original_retained: false,
            error: err_msg,
        };
        bound_items.push((j.id, item.clone(), vol_s, fid_s, sz_s, hh_s));
        preview_items.push(item);
    }

    let plan_id = Uuid::new_v4().to_string();
    let payload = TabMovePlanPayload {
        queue_id: queue_id.to_string(),
        tag_name: tag_name.to_string(),
        dest_dir: dest_dir.to_string(),
        copy_only,
        preserve_subfolders,
        collision_policy: policy.clone(),
        bound_items,
    };
    let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    db.save_reviewed_plan(&plan_id, "tab_move_by_tag", Some(queue_id), &payload_json)?;

    Ok(MoveByTagResult {
        plan_id: Some(plan_id),
        tag: tag_name.to_string(),
        dest_dir: dest_dir.to_string(),
        copy_only,
        preserve_subfolders,
        collision_policy: policy,
        dry_run: true,
        items: preview_items,
        moved_count: 0,
        retained_count: 0,
        operation_id: None,
        errors,
    })
}

pub fn execute_tab_move_by_tag_plan(
    db: &Database,
    plan_id: &str,
) -> Result<MoveByTagResult, String> {
    let (_qid, payload_json) = db.take_reviewed_plan(plan_id, "tab_move_by_tag")?;
    let plan: TabMovePlanPayload = serde_json::from_str(&payload_json)
        .map_err(|e| format!("Invalid reviewed Sort-to-Folder plan: {e}"))?;

    let mut bound_for_exec = Vec::new();
    let mut skipped_items = Vec::new();
    let mut skipped_errors = Vec::new();

    for (jid, item, vol, fid, sz, hh) in &plan.bound_items {
        if !item.valid {
            if plan.collision_policy == "skip" {
                skipped_items.push(item.clone());
                continue;
            }
            if let Some(ref e) = item.error {
                skipped_errors.push(format!("{}: {e}", item.filename));
            }
            skipped_items.push(item.clone());
            continue;
        }
        // Resolve catalog file_key if present (by path OR physical volume/file_id to survive /var vs /private/var aliases)
        let fkey = db
            .lock_conn()
            .ok()
            .and_then(|c| {
                c.query_row(
                    "SELECT fl.file_key
                     FROM file_locations fl
                     JOIN media_files mf ON mf.file_key = fl.file_key
                     WHERE fl.is_current = 1
                       AND (fl.path = ?1 OR (mf.volume_id = ?2 AND mf.os_file_id = ?3))
                     LIMIT 1",
                    params![item.old_path, vol, fid],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .unwrap_or_else(|| jid.clone());

        bound_for_exec.push(PlannedBatchItemBound {
            item: FileOpPreviewItem {
                file_key: fkey,
                asset_id: jid.clone(),
                old_path: item.old_path.clone(),
                new_path: item.new_path.clone(),
                op_kind: if plan.copy_only { "copy".to_string() } else { "move".to_string() },
                cross_volume: item.cross_volume,
                collision: false,
                validation_error: None,
            },
            src_volume_id: vol.clone(),
            src_file_id: fid.clone(),
            src_size: *sz,
            src_head_hash: hh.clone(),
        });
    }

    let exec_res = if bound_for_exec.is_empty() {
        BatchOpExecResult {
            operation_id: Uuid::new_v4().to_string(),
            succeeded: 0,
            failed: skipped_errors.len(),
            errors: skipped_errors.clone(),
        }
    } else {
        execute_batch_file_operations_bound(
            db,
            &bound_for_exec,
            &format!("Sort by tag '{}'", plan.tag_name),
        )?
    };

    let mut retained_count = 0usize;
    let mut final_items = Vec::new();
    for it in &plan.bound_items {
        let mut item = it.1.clone();
        if item.valid {
            if let Some(err_match) = exec_res.errors.iter().find(|e| e.contains(&item.new_path) || e.contains(&item.old_path)) {
                item.valid = false;
                item.error = Some(err_match.clone());
                if err_match.contains("Copied; original retained") {
                    item.copied_original_retained = true;
                    retained_count += 1;
                }
            }
        }
        final_items.push(item);
    }

    let mut all_errors = skipped_errors;
    all_errors.extend(exec_res.errors.clone());

    Ok(MoveByTagResult {
        plan_id: Some(plan_id.to_string()),
        tag: plan.tag_name,
        dest_dir: plan.dest_dir,
        copy_only: plan.copy_only,
        preserve_subfolders: plan.preserve_subfolders,
        collision_policy: plan.collision_policy,
        dry_run: false,
        items: final_items,
        moved_count: exec_res.succeeded,
        retained_count,
        operation_id: Some(exec_res.operation_id),
        errors: all_errors,
    })
}
