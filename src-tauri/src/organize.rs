use crate::catalog::{
    normalize_display_path, query_catalog, refresh_fts_for_asset, to_long_fs_path, CatalogEntry,
    CatalogFilter, CatalogPage,
};
use crate::db::Database;
use crate::planner::sanitize_path_segment;
use crate::platform::{capture_file_identity, disk_space_for_path};
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
    Ok(FileOpPreviewResult {
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
        if target_str != entry.path && to_long_fs_path(&target).exists() {
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
    Ok(FileOpPreviewResult {
        items,
        valid_count,
        error_count,
        collision_count,
    })
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

fn copy_and_verify_blake3(src: &Path, dst: &Path) -> Result<String, String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(to_long_fs_path(parent))
            .map_err(|e| format!("Create directory failed: {e}"))?;
    }
    let tmp = dst.with_file_name(format!(".mediashack-tmp-{}", Uuid::new_v4()));
    let mut src_f = File::open(to_long_fs_path(src)).map_err(|e| format!("Open source failed: {e}"))?;
    let mut tmp_f = File::create(to_long_fs_path(&tmp)).map_err(|e| format!("Create temp failed: {e}"))?;
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
        return Err("Cross-volume copy verification failed: BLAKE3 mismatch.".to_string());
    }
    if to_long_fs_path(dst).exists() {
        let _ = std::fs::remove_file(to_long_fs_path(&tmp));
        return Err("Destination created concurrently before commit.".to_string());
    }
    std::fs::rename(to_long_fs_path(&tmp), to_long_fs_path(dst))
        .map_err(|e| format!("Commit rename failed: {e}"))?;
    Ok(src_hash)
}

/// Execute validated batch rename, move, or copy with cycle-safe temp intermediates,
/// verified cross-volume copy-then-remove, and durable `operation_history`.
pub fn execute_batch_file_operations(
    db: &Database,
    items: &[FileOpPreviewItem],
    op_summary: &str,
) -> Result<BatchOpExecResult, String> {
    if let Some(bad) = items.iter().find(|i| i.validation_error.is_some()) {
        return Err(bad
            .validation_error
            .clone()
            .unwrap_or_else(|| "Validation error in batch preview".to_string()));
    }

    let op_id = Uuid::new_v4().to_string();
    let mut journal: Vec<JournalEntry> = Vec::new();
    let mut succeeded = 0usize;
    let mut errors = Vec::new();

    // Two-phase handling for renames/same-volume moves to handle cycles (A->B, B->A) & case-only renames safely
    let mut move_items: Vec<&FileOpPreviewItem> = Vec::new();
    let mut copy_items: Vec<&FileOpPreviewItem> = Vec::new();
    for it in items {
        if it.old_path == it.new_path {
            continue;
        }
        if it.op_kind == "copy" {
            copy_items.push(it);
        } else {
            move_items.push(it);
        }
    }

    // Separate same-volume moves (staged via temp names to break cycles) vs cross-volume moves
    let mut staged_same_vol: Vec<(&FileOpPreviewItem, PathBuf)> = Vec::new();
    for it in &move_items {
        let src = Path::new(&it.old_path);
        let dst = Path::new(&it.new_path);
        if !to_long_fs_path(src).exists() {
            errors.push(format!("Missing source: {}", it.old_path));
            continue;
        }
        if it.cross_volume {
            match copy_and_verify_blake3(src, dst) {
                Ok(hash) => {
                    if let Err(e) = std::fs::remove_file(to_long_fs_path(src)) {
                        errors.push(format!("Copied & verified {}, but failed removing source: {e}", it.new_path));
                        continue;
                    }
                    update_catalog_location_after_move(db, &it.file_key, &it.old_path, dst)?;
                    journal.push(JournalEntry {
                        file_key: it.file_key.clone(),
                        op_kind: it.op_kind.clone(),
                        from_path: it.old_path.clone(),
                        to_path: it.new_path.clone(),
                        blake3_hash: Some(hash),
                        cross_volume: true,
                    });
                    succeeded += 1;
                }
                Err(e) => errors.push(format!("{}: {e}", it.old_path)),
            }
        } else {
            let tmp = src.with_file_name(format!(".mediashack-cycle-{}", Uuid::new_v4()));
            if let Err(e) = std::fs::rename(to_long_fs_path(src), to_long_fs_path(&tmp)) {
                errors.push(format!("Stage rename failed for {}: {e}", it.old_path));
                continue;
            }
            staged_same_vol.push((it, tmp));
        }
    }

    for (it, tmp) in staged_same_vol {
        let dst = Path::new(&it.new_path);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(to_long_fs_path(parent));
        }
        match std::fs::rename(to_long_fs_path(&tmp), to_long_fs_path(dst)) {
            Ok(()) => {
                update_catalog_location_after_move(db, &it.file_key, &it.old_path, dst)?;
                journal.push(JournalEntry {
                    file_key: it.file_key.clone(),
                    op_kind: it.op_kind.clone(),
                    from_path: it.old_path.clone(),
                    to_path: it.new_path.clone(),
                    blake3_hash: None,
                    cross_volume: false,
                });
                succeeded += 1;
            }
            Err(e) => {
                // Rollback staged temp back to original path
                let _ = std::fs::rename(to_long_fs_path(&tmp), to_long_fs_path(Path::new(&it.old_path)));
                errors.push(format!("Commit rename failed for {}: {e}", it.new_path));
            }
        }
    }

    for it in copy_items {
        let src = Path::new(&it.old_path);
        let dst = Path::new(&it.new_path);
        match copy_and_verify_blake3(src, dst) {
            Ok(hash) => {
                journal.push(JournalEntry {
                    file_key: it.file_key.clone(),
                    op_kind: "copy".to_string(),
                    from_path: it.old_path.clone(),
                    to_path: it.new_path.clone(),
                    blake3_hash: Some(hash),
                    cross_volume: it.cross_volume,
                });
                succeeded += 1;
            }
            Err(e) => errors.push(format!("Copy {} failed: {e}", it.old_path)),
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
                "SELECT root_id FROM file_locations WHERE file_key = ?1 AND path = ?2 LIMIT 1",
                params![file_key, old_path],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();

        tx.execute(
            "UPDATE file_locations SET is_current = 0, is_online = 0 WHERE file_key = ?1 AND path = ?2",
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
            // Undo copy: verify hash matches before removing copied file
            if let Some(ref expected) = entry.blake3_hash {
                match blake3_file(to_p) {
                    Ok(actual) if &actual == expected => {
                        if let Err(e) = std::fs::remove_file(to_long_fs_path(to_p)) {
                            errors.push(format!("Failed removing copy {}: {e}", entry.to_path));
                        } else {
                            succeeded += 1;
                        }
                    }
                    _ => errors.push(format!("Copy modified after operation, refusing undo: {}", entry.to_path)),
                }
            } else {
                errors.push(format!("Missing verification hash for copy undo: {}", entry.to_path));
            }
            continue;
        }

        // Rename / move undo
        if to_long_fs_path(from_p).exists()
            && !rec.entries.iter().any(|other| other.to_path.eq_ignore_ascii_case(&entry.from_path))
        {
            errors.push(format!("Undo conflict: original path {} is now occupied.", entry.from_path));
            continue;
        }
    }

    if !errors.is_empty() && succeeded == 0 && rec.entries.iter().any(|e| e.op_kind != "copy") {
        return Err(errors.join("\n"));
    }

    // Reverse non-copy moves safely via temps (handles rename cycles)
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
        if entry.cross_volume {
            match copy_and_verify_blake3(to_p, from_p) {
                Ok(_) => {
                    let _ = std::fs::remove_file(to_long_fs_path(to_p));
                    let _ = update_catalog_location_after_move(db, &entry.file_key, &entry.to_path, from_p);
                    succeeded += 1;
                }
                Err(e) => errors.push(format!("Undo cross-volume move failed: {e}")),
            }
        } else {
            let tmp = to_p.with_file_name(format!(".mediashack-undo-{}", Uuid::new_v4()));
            if let Err(e) = std::fs::rename(to_long_fs_path(to_p), to_long_fs_path(&tmp)) {
                errors.push(format!("Undo stage failed for {}: {e}", entry.to_path));
            } else {
                staged_undo.push((entry, tmp));
            }
        }
    }

    for (entry, tmp) in staged_undo {
        let from_p = Path::new(&entry.from_path);
        if let Some(parent) = from_p.parent() {
            let _ = std::fs::create_dir_all(to_long_fs_path(parent));
        }
        match std::fs::rename(to_long_fs_path(&tmp), to_long_fs_path(from_p)) {
            Ok(()) => {
                let _ = update_catalog_location_after_move(db, &entry.file_key, &entry.to_path, from_p);
                succeeded += 1;
            }
            Err(e) => {
                let _ = std::fs::rename(to_long_fs_path(&tmp), to_long_fs_path(Path::new(&entry.to_path)));
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
