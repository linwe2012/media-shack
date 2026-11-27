use crate::models::*;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    pub app_data_dir: PathBuf,
}

impl Database {
    pub fn open(app_data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_data_dir)
            .map_err(|e| format!("Failed to create app data dir: {e}"))?;
        let db_path = app_data_dir.join("media_shack.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open SQLite database {}: {e}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = FULL;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| format!("Failed to set SQLite pragmas: {e}"))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            app_data_dir: app_data_dir.to_path_buf(),
        };
        db.migrate()?;
        db.seed_defaults()?;
        Ok(db)
    }

    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("SQLite in-memory error: {e}"))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| format!("Pragma error: {e}"))?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            app_data_dir: std::env::temp_dir().join("media-shack-test"),
        };
        db.migrate()?;
        db.seed_defaults()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );",
        )
        .map_err(|e| format!("Migration table error: {e}"))?;

        let current_ver: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Migration version query error: {e}"))?;

        if current_ver < 1 {
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(
                "CREATE TABLE app_settings (
                    key TEXT PRIMARY KEY,
                    value_json TEXT NOT NULL
                );
                CREATE TABLE tool_cache (
                    identity_key TEXT PRIMARY KEY,
                    capabilities_json TEXT NOT NULL,
                    checked_at INTEGER NOT NULL
                );
                CREATE TABLE named_presets (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE,
                    recipe_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE profiles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE,
                    profile_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE queues (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    tab_order INTEGER NOT NULL,
                    profile_id TEXT,
                    profile_name TEXT,
                    recipe_json TEXT NOT NULL,
                    priority TEXT NOT NULL,
                    recursive_import INTEGER NOT NULL,
                    destination_root TEXT,
                    keep_awake INTEGER NOT NULL,
                    error_policy TEXT NOT NULL,
                    min_savings_enabled INTEGER NOT NULL,
                    min_savings_pct REAL NOT NULL,
                    cleanup_policy TEXT NOT NULL,
                    full_decode_verify INTEGER NOT NULL,
                    is_running INTEGER NOT NULL DEFAULT 0,
                    is_paused INTEGER NOT NULL DEFAULT 0,
                    pause_after_current INTEGER NOT NULL DEFAULT 0,
                    waiting_reason TEXT
                );
                CREATE TABLE jobs (
                    id TEXT PRIMARY KEY,
                    queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                    batch_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    source_path TEXT NOT NULL,
                    source_rel_path TEXT NOT NULL,
                    source_root TEXT,
                    source_fingerprint_json TEXT,
                    destination_path TEXT NOT NULL,
                    dest_volume_id TEXT,
                    recipe_json TEXT NOT NULL,
                    compat_overrides_json TEXT NOT NULL,
                    status TEXT NOT NULL,
                    status_reason TEXT,
                    probe_json TEXT,
                    plan_json TEXT,
                    input_bytes INTEGER NOT NULL,
                    output_bytes INTEGER,
                    error_stage TEXT,
                    error_message TEXT,
                    diagnostics TEXT,
                    cleanup_policy TEXT NOT NULL,
                    cleanup_outcome TEXT NOT NULL,
                    cleanup_error TEXT,
                    consent_id TEXT,
                    full_decode_verified INTEGER NOT NULL DEFAULT 0,
                    min_savings_enabled INTEGER NOT NULL DEFAULT 0,
                    min_savings_pct REAL NOT NULL DEFAULT 15.0,
                    tool_versions TEXT,
                    current_attempt_id TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    completed_at INTEGER
                );
                CREATE INDEX idx_jobs_queue_pos ON jobs(queue_id, position);
                CREATE INDEX idx_jobs_status ON jobs(status);
                CREATE TABLE job_attempts (
                    attempt_id TEXT PRIMARY KEY,
                    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    queue_id TEXT NOT NULL,
                    staging_dir TEXT NOT NULL,
                    partial_path TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER,
                    exit_code INTEGER,
                    status TEXT NOT NULL
                );
                CREATE TABLE publish_intents (
                    intent_id TEXT PRIMARY KEY,
                    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL,
                    staged_path TEXT NOT NULL,
                    staged_volume_id TEXT NOT NULL,
                    staged_file_id TEXT NOT NULL,
                    staged_size INTEGER NOT NULL,
                    final_path TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE cleanup_consents (
                    consent_id TEXT PRIMARY KEY,
                    queue_id TEXT NOT NULL,
                    batch_id TEXT NOT NULL,
                    source_manifest_json TEXT NOT NULL,
                    total_bytes INTEGER NOT NULL,
                    approved_at INTEGER NOT NULL
                );
                CREATE TABLE cleanup_intents (
                    action_id TEXT PRIMARY KEY,
                    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    queue_id TEXT NOT NULL,
                    batch_id TEXT NOT NULL,
                    source_path TEXT NOT NULL,
                    source_volume_id TEXT NOT NULL,
                    source_file_id TEXT NOT NULL,
                    source_size INTEGER NOT NULL,
                    verified_output_path TEXT NOT NULL,
                    verified_output_file_id TEXT NOT NULL,
                    consent_id TEXT,
                    state TEXT NOT NULL,
                    error_message TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE volume_reserves (
                    volume_id TEXT PRIMARY KEY,
                    mount_hint TEXT NOT NULL,
                    reserve_bytes INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .map_err(|e| format!("Migration v1 failed: {e}"))?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (1, ?1)",
                params![Utc::now().timestamp()],
            )
            .map_err(|e| format!("Migration v1 version insert failed: {e}"))?;
            tx.commit().map_err(|e| e.to_string())?;
        }

        if current_ver < 2 {
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch("ALTER TABLE jobs ADD COLUMN output_fingerprint_json TEXT;")
                .map_err(|e| format!("Migration v2 failed: {e}"))?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (2, ?1)",
                params![Utc::now().timestamp()],
            )
            .map_err(|e| format!("Migration v2 version insert failed: {e}"))?;
            tx.commit().map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    fn seed_defaults(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let queue_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM queues", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if queue_count == 0 {
            let qid = Uuid::new_v4().to_string();
            let recipe_json = serde_json::to_string(&EncodeRecipe::archive_balanced_default())
                .map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO queues (
                    id, name, tab_order, profile_id, profile_name, recipe_json,
                    priority, recursive_import, destination_root, keep_awake,
                    error_policy, min_savings_enabled, min_savings_pct,
                    cleanup_policy, full_decode_verify, is_running, is_paused, pause_after_current
                ) VALUES (?1, ?2, 0, NULL, NULL, ?3, 'background', 1, NULL, 1, 'continue', 0, 15.0, 'keep_originals', 0, 0, 0, 0)",
                params![qid, "Queue 1", recipe_json],
            )
            .map_err(|e| e.to_string())?;
            let active_json = serde_json::to_string(&qid).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT OR REPLACE INTO app_settings(key, value_json) VALUES ('active_queue_id', ?1)",
                params![active_json],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn get_setting<T: for<'de> serde::Deserialize<'de>>(&self, key: &str) -> Result<Option<T>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let val: Option<String> = conn
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match val {
            Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }

    pub fn set_setting<T: serde::Serialize>(&self, key: &str, val: &T) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let s = serde_json::to_string(val).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO app_settings(key, value_json) VALUES (?1, ?2)",
            params![key, s],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_cached_tools(&self, identity_key: &str) -> Result<Option<ToolCapabilities>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT capabilities_json FROM tool_cache WHERE identity_key = ?1",
                params![identity_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match raw {
            Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }

    pub fn save_cached_tools(&self, caps: &ToolCapabilities) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let json = serde_json::to_string(caps).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO tool_cache(identity_key, capabilities_json, checked_at) VALUES (?1, ?2, ?3)",
            params![caps.identity_key, json, caps.checked_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_named_presets(&self) -> Result<Vec<NamedPreset>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, recipe_json, created_at FROM named_presets ORDER BY name ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let name: String = r.get(1)?;
                let rjson: String = r.get(2)?;
                let created_at: i64 = r.get(3)?;
                let recipe: EncodeRecipe =
                    serde_json::from_str(&rjson).unwrap_or_else(|_| EncodeRecipe::archive_balanced_default());
                Ok(NamedPreset {
                    id,
                    name,
                    recipe,
                    created_at,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn save_named_preset(&self, name: &str, recipe: &EncodeRecipe) -> Result<NamedPreset, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        let existing: Option<String> = conn
            .query_row("SELECT id FROM named_presets WHERE name = ?1", params![name], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut saved_recipe = recipe.clone();
        saved_recipe.preset_name = Some(name.to_string());
        let rjson = serde_json::to_string(&saved_recipe).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO named_presets(id, name, recipe_json, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, rjson, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(NamedPreset {
            id,
            name: name.to_string(),
            recipe: saved_recipe,
            created_at: now,
        })
    }

    pub fn delete_named_preset(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM named_presets WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT profile_json FROM profiles ORDER BY name ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let s: String = r.get(0)?;
                Ok(serde_json::from_str::<Profile>(&s))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for item in rows {
            let p = item.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
            out.push(p);
        }
        Ok(out)
    }

    pub fn save_profile(&self, profile: &Profile) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let pjson = serde_json::to_string(profile).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO profiles(id, name, profile_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![profile.id, profile.name, pjson, profile.created_at, profile.updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM profiles WHERE id = ?1", params![profile_id])
            .map_err(|e| e.to_string())?;
        // Queues keep their saved settings & jobs, only clearing profile_id link
        tx.execute(
            "UPDATE queues SET profile_id = NULL WHERE profile_id = ?1",
            params![profile_id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_queues(&self) -> Result<Vec<QueueTab>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, tab_order, profile_id, profile_name, recipe_json,
                        priority, recursive_import, destination_root, keep_awake,
                        error_policy, min_savings_enabled, min_savings_pct,
                        cleanup_policy, full_decode_verify, is_running, is_paused,
                        pause_after_current, waiting_reason
                 FROM queues ORDER BY tab_order ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let rjson: String = r.get(5)?;
                let prio_s: String = r.get(6)?;
                let err_s: String = r.get(10)?;
                let clean_s: String = r.get(13)?;
                Ok(QueueTab {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    tab_order: r.get(2)?,
                    profile_id: r.get(3)?,
                    profile_name: r.get(4)?,
                    recipe: serde_json::from_str(&rjson)
                        .unwrap_or_else(|_| EncodeRecipe::archive_balanced_default()),
                    priority: serde_json::from_str(&format!("\"{prio_s}\""))
                        .unwrap_or(ProcessPriority::Background),
                    recursive_import: r.get::<_, i64>(7)? != 0,
                    destination_root: r.get(8)?,
                    keep_awake: r.get::<_, i64>(9)? != 0,
                    error_policy: serde_json::from_str(&format!("\"{err_s}\""))
                        .unwrap_or(ErrorPolicy::Continue),
                    min_savings_enabled: r.get::<_, i64>(11)? != 0,
                    min_savings_pct: r.get(12)?,
                    cleanup_policy: serde_json::from_str(&format!("\"{clean_s}\""))
                        .unwrap_or(CleanupPolicy::KeepOriginals),
                    full_decode_verify: r.get::<_, i64>(14)? != 0,
                    is_running: r.get::<_, i64>(15)? != 0,
                    is_paused: r.get::<_, i64>(16)? != 0,
                    pause_after_current: r.get::<_, i64>(17)? != 0,
                    waiting_reason: r.get(18)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn get_queue(&self, queue_id: &str) -> Result<Option<QueueTab>, String> {
        Ok(self.list_queues()?.into_iter().find(|q| q.id == queue_id))
    }

    pub fn upsert_queue(&self, q: &QueueTab) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let rjson = serde_json::to_string(&q.recipe).map_err(|e| e.to_string())?;
        let prio_s = serde_json::to_string(&q.priority)
            .map_err(|e| e.to_string())?
            .trim_matches('"')
            .to_string();
        let err_s = serde_json::to_string(&q.error_policy)
            .map_err(|e| e.to_string())?
            .trim_matches('"')
            .to_string();
        let clean_s = serde_json::to_string(&q.cleanup_policy)
            .map_err(|e| e.to_string())?
            .trim_matches('"')
            .to_string();
        conn.execute(
            "INSERT INTO queues (
                id, name, tab_order, profile_id, profile_name, recipe_json,
                priority, recursive_import, destination_root, keep_awake,
                error_policy, min_savings_enabled, min_savings_pct,
                cleanup_policy, full_decode_verify, is_running, is_paused,
                pause_after_current, waiting_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                tab_order = excluded.tab_order,
                profile_id = excluded.profile_id,
                profile_name = excluded.profile_name,
                recipe_json = excluded.recipe_json,
                priority = excluded.priority,
                recursive_import = excluded.recursive_import,
                destination_root = excluded.destination_root,
                keep_awake = excluded.keep_awake,
                error_policy = excluded.error_policy,
                min_savings_enabled = excluded.min_savings_enabled,
                min_savings_pct = excluded.min_savings_pct,
                cleanup_policy = excluded.cleanup_policy,
                full_decode_verify = excluded.full_decode_verify,
                is_running = excluded.is_running,
                is_paused = excluded.is_paused,
                pause_after_current = excluded.pause_after_current,
                waiting_reason = excluded.waiting_reason",
            params![
                q.id,
                q.name,
                q.tab_order,
                q.profile_id,
                q.profile_name,
                rjson,
                prio_s,
                q.recursive_import as i64,
                q.destination_root,
                q.keep_awake as i64,
                err_s,
                q.min_savings_enabled as i64,
                q.min_savings_pct,
                clean_s,
                q.full_decode_verify as i64,
                q.is_running as i64,
                q.is_paused as i64,
                q.pause_after_current as i64,
                q.waiting_reason,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_queue(&self, queue_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM queues WHERE id = ?1", params![queue_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_jobs_for_queue(&self, queue_id: &str) -> Result<Vec<JobRecord>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, queue_id, batch_id, position, source_path, source_rel_path,
                        source_root, source_fingerprint_json, destination_path, dest_volume_id,
                        recipe_json, compat_overrides_json, status, status_reason,
                        probe_json, plan_json, input_bytes, output_bytes, error_stage,
                        error_message, diagnostics, cleanup_policy, cleanup_outcome,
                        cleanup_error, consent_id, full_decode_verified, min_savings_enabled,
                        min_savings_pct, tool_versions, current_attempt_id, created_at,
                        updated_at, completed_at, output_fingerprint_json
                 FROM jobs WHERE queue_id = ?1 ORDER BY position ASC",
            )
            .map_err(|e| e.to_string())?;
        Self::collect_jobs(&mut stmt, params![queue_id])
    }

    pub fn list_all_jobs(&self) -> Result<Vec<JobRecord>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, queue_id, batch_id, position, source_path, source_rel_path,
                        source_root, source_fingerprint_json, destination_path, dest_volume_id,
                        recipe_json, compat_overrides_json, status, status_reason,
                        probe_json, plan_json, input_bytes, output_bytes, error_stage,
                        error_message, diagnostics, cleanup_policy, cleanup_outcome,
                        cleanup_error, consent_id, full_decode_verified, min_savings_enabled,
                        min_savings_pct, tool_versions, current_attempt_id, created_at,
                        updated_at, completed_at, output_fingerprint_json
                 FROM jobs ORDER BY queue_id ASC, position ASC",
            )
            .map_err(|e| e.to_string())?;
        Self::collect_jobs(&mut stmt, [])
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<JobRecord>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, queue_id, batch_id, position, source_path, source_rel_path,
                        source_root, source_fingerprint_json, destination_path, dest_volume_id,
                        recipe_json, compat_overrides_json, status, status_reason,
                        probe_json, plan_json, input_bytes, output_bytes, error_stage,
                        error_message, diagnostics, cleanup_policy, cleanup_outcome,
                        cleanup_error, consent_id, full_decode_verified, min_savings_enabled,
                        min_savings_pct, tool_versions, current_attempt_id, created_at,
                        updated_at, completed_at, output_fingerprint_json
                 FROM jobs WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut items = Self::collect_jobs(&mut stmt, params![job_id])?;
        Ok(items.pop())
    }

    fn collect_jobs<P: rusqlite::Params>(stmt: &mut rusqlite::Statement<'_>, p: P) -> Result<Vec<JobRecord>, String> {
        let rows = stmt
            .query_map(p, |r| {
                let fp_s: Option<String> = r.get(7)?;
                let rec_s: String = r.get(10)?;
                let ov_s: String = r.get(11)?;
                let st_s: String = r.get(12)?;
                let probe_s: Option<String> = r.get(14)?;
                let plan_s: Option<String> = r.get(15)?;
                let clean_p: String = r.get(21)?;
                let clean_o: String = r.get(22)?;
                let out_fp_s: Option<String> = r.get(33)?;
                Ok(JobRecord {
                    id: r.get(0)?,
                    queue_id: r.get(1)?,
                    batch_id: r.get(2)?,
                    position: r.get(3)?,
                    source_path: r.get(4)?,
                    source_rel_path: r.get(5)?,
                    source_root: r.get(6)?,
                    source_fingerprint: fp_s.and_then(|s| serde_json::from_str(&s).ok()),
                    destination_path: r.get(8)?,
                    dest_volume_id: r.get(9)?,
                    recipe: serde_json::from_str(&rec_s)
                        .unwrap_or_else(|_| EncodeRecipe::archive_balanced_default()),
                    compat_overrides: serde_json::from_str(&ov_s).unwrap_or_default(),
                    status: JobStatus::from_str(&st_s),
                    status_reason: r.get(13)?,
                    probe: probe_s.and_then(|s| serde_json::from_str(&s).ok()),
                    plan: plan_s.and_then(|s| serde_json::from_str(&s).ok()),
                    input_bytes: r.get::<_, i64>(16)? as u64,
                    output_bytes: r.get::<_, Option<i64>>(17)?.map(|v| v as u64),
                    output_fingerprint: out_fp_s.and_then(|s| serde_json::from_str(&s).ok()),
                    error_stage: r.get(18)?,
                    error_message: r.get(19)?,
                    diagnostics: r.get(20)?,
                    cleanup_policy: serde_json::from_str(&format!("\"{clean_p}\""))
                        .unwrap_or(CleanupPolicy::KeepOriginals),
                    cleanup_outcome: CleanupOutcome::from_str(&clean_o),
                    cleanup_error: r.get(23)?,
                    consent_id: r.get(24)?,
                    full_decode_verified: r.get::<_, i64>(25)? != 0,
                    min_savings_enabled: r.get::<_, i64>(26)? != 0,
                    min_savings_pct: r.get(27)?,
                    tool_versions: r.get(28)?,
                    current_attempt_id: r.get(29)?,
                    created_at: r.get(30)?,
                    updated_at: r.get(31)?,
                    completed_at: r.get(32)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn upsert_job(&self, j: &JobRecord) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let fp_s = j
            .source_fingerprint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let out_fp_s = j
            .output_fingerprint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let rec_s = serde_json::to_string(&j.recipe).map_err(|e| e.to_string())?;
        let ov_s = serde_json::to_string(&j.compat_overrides).map_err(|e| e.to_string())?;
        let probe_s = j
            .probe
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let plan_s = j
            .plan
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let clean_p = serde_json::to_string(&j.cleanup_policy)
            .map_err(|e| e.to_string())?
            .trim_matches('"')
            .to_string();

        conn.execute(
            "INSERT INTO jobs (
                id, queue_id, batch_id, position, source_path, source_rel_path,
                source_root, source_fingerprint_json, destination_path, dest_volume_id,
                recipe_json, compat_overrides_json, status, status_reason,
                probe_json, plan_json, input_bytes, output_bytes, error_stage,
                error_message, diagnostics, cleanup_policy, cleanup_outcome,
                cleanup_error, consent_id, full_decode_verified, min_savings_enabled,
                min_savings_pct, tool_versions, current_attempt_id, created_at,
                updated_at, completed_at, output_fingerprint_json
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                ?31, ?32, ?33, ?34
            )
            ON CONFLICT(id) DO UPDATE SET
                queue_id = excluded.queue_id,
                batch_id = excluded.batch_id,
                position = excluded.position,
                source_path = excluded.source_path,
                source_rel_path = excluded.source_rel_path,
                source_root = excluded.source_root,
                source_fingerprint_json = excluded.source_fingerprint_json,
                destination_path = excluded.destination_path,
                dest_volume_id = excluded.dest_volume_id,
                recipe_json = excluded.recipe_json,
                compat_overrides_json = excluded.compat_overrides_json,
                status = excluded.status,
                status_reason = excluded.status_reason,
                probe_json = excluded.probe_json,
                plan_json = excluded.plan_json,
                input_bytes = excluded.input_bytes,
                output_bytes = excluded.output_bytes,
                error_stage = excluded.error_stage,
                error_message = excluded.error_message,
                diagnostics = excluded.diagnostics,
                cleanup_policy = excluded.cleanup_policy,
                cleanup_outcome = excluded.cleanup_outcome,
                cleanup_error = excluded.cleanup_error,
                consent_id = excluded.consent_id,
                full_decode_verified = excluded.full_decode_verified,
                min_savings_enabled = excluded.min_savings_enabled,
                min_savings_pct = excluded.min_savings_pct,
                tool_versions = excluded.tool_versions,
                current_attempt_id = excluded.current_attempt_id,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at,
                output_fingerprint_json = excluded.output_fingerprint_json",
            params![
                j.id,
                j.queue_id,
                j.batch_id,
                j.position,
                j.source_path,
                j.source_rel_path,
                j.source_root,
                fp_s,
                j.destination_path,
                j.dest_volume_id,
                rec_s,
                ov_s,
                j.status.as_str(),
                j.status_reason,
                probe_s,
                plan_s,
                j.input_bytes as i64,
                j.output_bytes.map(|v| v as i64),
                j.error_stage,
                j.error_message,
                j.diagnostics,
                clean_p,
                j.cleanup_outcome.as_str(),
                j.cleanup_error,
                j.consent_id,
                j.full_decode_verified as i64,
                j.min_savings_enabled as i64,
                j.min_savings_pct,
                j.tool_versions,
                j.current_attempt_id,
                j.created_at,
                j.updated_at,
                j.completed_at,
                out_fp_s,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_job(&self, job_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM jobs WHERE id = ?1", params![job_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn reorder_jobs(&self, queue_id: &str, ordered_job_ids: &[String]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (idx, job_id) in ordered_job_ids.iter().enumerate() {
            tx.execute(
                "UPDATE jobs SET position = ?1 WHERE id = ?2 AND queue_id = ?3",
                params![idx as i64, job_id, queue_id],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn next_job_position(&self, queue_id: &str) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let max_pos: Option<i64> = conn
            .query_row(
                "SELECT MAX(position) FROM jobs WHERE queue_id = ?1",
                params![queue_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(max_pos.map(|p| p + 1).unwrap_or(0))
    }

    pub fn record_attempt_start(
        &self,
        attempt_id: &str,
        job_id: &str,
        queue_id: &str,
        staging_dir: &str,
        partial_path: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO job_attempts (attempt_id, job_id, queue_id, staging_dir, partial_path, started_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
            params![attempt_id, job_id, queue_id, staging_dir, partial_path, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_attempt_finish(
        &self,
        attempt_id: &str,
        exit_code: Option<i32>,
        status: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE job_attempts SET finished_at = ?1, exit_code = ?2, status = ?3 WHERE attempt_id = ?4",
            params![now, exit_code, status, attempt_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_stale_attempts(&self) -> Result<Vec<(String, String, String, String)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT attempt_id, job_id, staging_dir, partial_path FROM job_attempts WHERE status = 'running'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn save_publish_intent(
        &self,
        intent_id: &str,
        job_id: &str,
        attempt_id: &str,
        staged_path: &str,
        staged_volume_id: &str,
        staged_file_id: &str,
        staged_size: u64,
        final_path: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO publish_intents (
                intent_id, job_id, attempt_id, staged_path, staged_volume_id,
                staged_file_id, staged_size, final_path, state, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?9)",
            params![
                intent_id,
                job_id,
                attempt_id,
                staged_path,
                staged_volume_id,
                staged_file_id,
                staged_size as i64,
                final_path,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_publish_intent_state(
        &self,
        intent_id: &str,
        state: &str,
        final_path: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        if let Some(fp) = final_path {
            conn.execute(
                "UPDATE publish_intents SET state = ?1, final_path = ?2, updated_at = ?3 WHERE intent_id = ?4",
                params![state, fp, now, intent_id],
            )
            .map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "UPDATE publish_intents SET state = ?1, updated_at = ?2 WHERE intent_id = ?3",
                params![state, now, intent_id],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn list_pending_publish_intents(&self) -> Result<Vec<PublishIntentRecord>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT intent_id, job_id, attempt_id, staged_path, staged_volume_id,
                        staged_file_id, staged_size, final_path, state
                 FROM publish_intents WHERE state = 'pending'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PublishIntentRecord {
                    intent_id: r.get(0)?,
                    job_id: r.get(1)?,
                    attempt_id: r.get(2)?,
                    staged_path: r.get(3)?,
                    staged_volume_id: r.get(4)?,
                    staged_file_id: r.get(5)?,
                    staged_size: r.get::<_, i64>(6)? as u64,
                    final_path: r.get(7)?,
                    state: r.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn save_cleanup_consent(
        &self,
        consent_id: &str,
        queue_id: &str,
        batch_id: &str,
        source_manifest_json: &str,
        total_bytes: u64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO cleanup_consents (consent_id, queue_id, batch_id, source_manifest_json, total_bytes, approved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![consent_id, queue_id, batch_id, source_manifest_json, total_bytes as i64, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_cleanup_consent(&self, consent_id: &str) -> Result<Option<(String, String, String)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT queue_id, batch_id, source_manifest_json FROM cleanup_consents WHERE consent_id = ?1",
            params![consent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn save_cleanup_intent(&self, intent: &CleanupIntentRecord) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO cleanup_intents (
                action_id, job_id, queue_id, batch_id, source_path, source_volume_id,
                source_file_id, source_size, verified_output_path, verified_output_file_id,
                consent_id, state, error_message, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                intent.action_id,
                intent.job_id,
                intent.queue_id,
                intent.batch_id,
                intent.source_path,
                intent.source_volume_id,
                intent.source_file_id,
                intent.source_size as i64,
                intent.verified_output_path,
                intent.verified_output_file_id,
                intent.consent_id,
                intent.state,
                intent.error_message,
                now,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_cleanup_intent(
        &self,
        action_id: &str,
        state: &str,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE cleanup_intents SET state = ?1, error_message = ?2, updated_at = ?3 WHERE action_id = ?4",
            params![state, error_message, now, action_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_pending_cleanup_intents(&self) -> Result<Vec<CleanupIntentRecord>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT action_id, job_id, queue_id, batch_id, source_path, source_volume_id,
                        source_file_id, source_size, verified_output_path, verified_output_file_id,
                        consent_id, state, error_message
                 FROM cleanup_intents WHERE state IN ('intent_created', 'executing')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CleanupIntentRecord {
                    action_id: r.get(0)?,
                    job_id: r.get(1)?,
                    queue_id: r.get(2)?,
                    batch_id: r.get(3)?,
                    source_path: r.get(4)?,
                    source_volume_id: r.get(5)?,
                    source_file_id: r.get(6)?,
                    source_size: r.get::<_, i64>(7)? as u64,
                    verified_output_path: r.get(8)?,
                    verified_output_file_id: r.get(9)?,
                    consent_id: r.get(10)?,
                    state: r.get(11)?,
                    error_message: r.get(12)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn get_volume_reserve(&self, volume_id: &str) -> Result<Option<u64>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let v: Option<i64> = conn
            .query_row(
                "SELECT reserve_bytes FROM volume_reserves WHERE volume_id = ?1",
                params![volume_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(v.map(|x| x.max(0) as u64))
    }

    pub fn set_volume_reserve(&self, volume_id: &str, mount_hint: &str, reserve_bytes: u64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO volume_reserves (volume_id, mount_hint, reserve_bytes, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![volume_id, mount_hint, reserve_bytes as i64, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_volume_reserves(&self) -> Result<Vec<(String, String, u64)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT volume_id, mount_hint, reserve_bytes FROM volume_reserves")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct PublishIntentRecord {
    pub intent_id: String,
    pub job_id: String,
    pub attempt_id: String,
    pub staged_path: String,
    pub staged_volume_id: String,
    pub staged_file_id: String,
    pub staged_size: u64,
    pub final_path: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct CleanupIntentRecord {
    pub action_id: String,
    pub job_id: String,
    pub queue_id: String,
    pub batch_id: String,
    pub source_path: String,
    pub source_volume_id: String,
    pub source_file_id: String,
    pub source_size: u64,
    pub verified_output_path: String,
    pub verified_output_file_id: String,
    pub consent_id: Option<String>,
    pub state: String,
    pub error_message: Option<String>,
}
