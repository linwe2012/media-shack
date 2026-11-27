use crate::db::Database;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceBudgets {
    pub cpu_workers: usize,
    pub gpu_concurrency: usize,
    pub memory_limit_mb: usize,
    pub disk_io_concurrency: usize,
    pub max_ffmpeg_processes: usize,
    pub cache_max_mb: usize,
    pub throttle_analysis_while_transcoding: bool,
}

impl Default for ResourceBudgets {
    fn default() -> Self {
        Self {
            cpu_workers: 4,
            gpu_concurrency: 1,
            memory_limit_mb: 4096,
            disk_io_concurrency: 2,
            max_ffmpeg_processes: 2,
            cache_max_mb: 2048,
            throttle_analysis_while_transcoding: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub task_type: String,
    pub category: String,
    pub target_file_keys: Vec<String>,
    pub priority: i32,
    pub state: String, // queued | running | paused | completed | failed | canceled
    pub progress_pct: f64,
    pub throughput_label: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
    pub resource_kind: String,
    pub payload_json: String,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Clone)]
pub struct TaskManager {
    pub db: Database,
    pub cancel_flags: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    pub pause_flags: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    pub active_hdd_volumes: Arc<Mutex<HashSet<String>>>,
}

impl TaskManager {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            cancel_flags: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pause_flags: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_hdd_volumes: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn get_budgets(&self) -> Result<ResourceBudgets, String> {
        Ok(self
            .db
            .get_setting::<ResourceBudgets>("resource_budgets")?
            .unwrap_or_default())
    }

    pub fn save_budgets(&self, mut b: ResourceBudgets) -> Result<ResourceBudgets, String> {
        b.cpu_workers = b.cpu_workers.clamp(1, 32);
        b.gpu_concurrency = b.gpu_concurrency.clamp(1, 8);
        b.memory_limit_mb = b.memory_limit_mb.clamp(512, 65536);
        b.disk_io_concurrency = b.disk_io_concurrency.clamp(1, 8);
        b.max_ffmpeg_processes = b.max_ffmpeg_processes.clamp(1, 8);
        b.cache_max_mb = b.cache_max_mb.clamp(128, 102400);
        self.db.set_setting("resource_budgets", &b)?;
        self.db
            .set_setting("analysis_cache_budget_bytes", &((b.cache_max_mb as u64) * 1024 * 1024))?;
        Ok(b)
    }

    pub fn enqueue_task(
        &self,
        task_type: &str,
        category: &str,
        target_file_keys: Vec<String>,
        priority: i32,
        resource_kind: &str,
        payload: serde_json::Value,
    ) -> Result<TaskRecord, String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let targets_json = serde_json::to_string(&target_file_keys).map_err(|e| e.to_string())?;
        let payload_json = payload.to_string();
        let conn = self.db.lock_conn()?;
        conn.execute(
            "INSERT INTO analysis_jobs (
                id, task_type, category, target_file_keys_json, priority, state,
                progress_pct, retry_count, max_retries, resource_kind, payload_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', 0.0, 0, 2, ?6, ?7, ?8, ?8)",
            params![id, task_type, category, targets_json, priority, resource_kind, payload_json, now],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        Ok(TaskRecord {
            id,
            task_type: task_type.to_string(),
            category: category.to_string(),
            target_file_keys,
            priority,
            state: "queued".to_string(),
            progress_pct: 0.0,
            throughput_label: None,
            retry_count: 0,
            max_retries: 2,
            resource_kind: resource_kind.to_string(),
            payload_json,
            error_message: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        })
    }

    pub fn update_task_progress(
        &self,
        task_id: &str,
        state: &str,
        progress_pct: f64,
        throughput_label: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().timestamp();
        let completed_at = if matches!(state, "completed" | "failed" | "canceled") {
            Some(now)
        } else {
            None
        };
        let conn = self.db.lock_conn()?;
        conn.execute(
            "UPDATE analysis_jobs SET
                state = ?1,
                progress_pct = ?2,
                throughput_label = COALESCE(?3, throughput_label),
                error_message = ?4,
                updated_at = ?5,
                completed_at = COALESCE(?6, completed_at)
             WHERE id = ?7",
            params![state, progress_pct.clamp(0.0, 100.0), throughput_label, error_message, now, completed_at, task_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>, String> {
        let conn = self.db.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_type, category, target_file_keys_json, priority, state,
                        progress_pct, throughput_label, retry_count, max_retries, resource_kind,
                        payload_json, error_message, created_at, updated_at, completed_at
                 FROM analysis_jobs
                 ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1 WHEN 'paused' THEN 2 ELSE 3 END,
                          priority ASC, created_at DESC
                 LIMIT 200",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let tjson: String = r.get(3)?;
                Ok(TaskRecord {
                    id: r.get(0)?,
                    task_type: r.get(1)?,
                    category: r.get(2)?,
                    target_file_keys: serde_json::from_str(&tjson).unwrap_or_default(),
                    priority: r.get(4)?,
                    state: r.get(5)?,
                    progress_pct: r.get(6)?,
                    throughput_label: r.get(7)?,
                    retry_count: r.get(8)?,
                    max_retries: r.get(9)?,
                    resource_kind: r.get(10)?,
                    payload_json: r.get(11)?,
                    error_message: r.get(12)?,
                    created_at: r.get(13)?,
                    updated_at: r.get(14)?,
                    completed_at: r.get(15)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn register_control_flags(&self, task_id: &str) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        if let Ok(mut cf) = self.cancel_flags.lock() {
            cf.insert(task_id.to_string(), cancel.clone());
        }
        if let Ok(mut pf) = self.pause_flags.lock() {
            pf.insert(task_id.to_string(), pause.clone());
        }
        (cancel, pause)
    }

    pub fn pause_task(&self, task_id: &str) -> Result<(), String> {
        if let Ok(pf) = self.pause_flags.lock() {
            if let Some(flag) = pf.get(task_id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
        self.update_task_progress(task_id, "paused", 0.0, None, None)
    }

    pub fn resume_task(&self, task_id: &str) -> Result<(), String> {
        if let Ok(pf) = self.pause_flags.lock() {
            if let Some(flag) = pf.get(task_id) {
                flag.store(false, Ordering::Relaxed);
            }
        }
        self.update_task_progress(task_id, "running", 0.0, None, None)
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        if let Ok(cf) = self.cancel_flags.lock() {
            if let Some(flag) = cf.get(task_id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
        self.update_task_progress(task_id, "canceled", 0.0, None, None)
    }

    pub fn set_task_priority(&self, task_id: &str, priority: i32) -> Result<(), String> {
        let conn = self.db.lock_conn()?;
        conn.execute(
            "UPDATE analysis_jobs SET priority = ?1, updated_at = ?2 WHERE id = ?3",
            params![priority.clamp(1, 10), Utc::now().timestamp(), task_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Reconcile any background analysis tasks that were left `running` on unexpected exit.
    pub fn reconcile_interrupted_tasks_on_launch(&self) -> Result<usize, String> {
        let conn = self.db.lock_conn()?;
        let updated = conn
            .execute(
                "UPDATE analysis_jobs SET state = 'paused', error_message = 'Interrupted by previous exit — click Resume to continue', updated_at = ?1
                 WHERE state = 'running'",
                params![Utc::now().timestamp()],
            )
            .map_err(|e| e.to_string())?;
        Ok(updated)
    }

    /// Acquire a per-volume HDD I/O lock if storage_kind is mechanical/unknown, serializing heavy random-access ops on HDDs.
    pub fn acquire_volume_io_guard(&self, volume_id: &str, storage_kind: &str) {
        if storage_kind == "ssd" {
            return;
        }
        loop {
            if let Ok(mut set) = self.active_hdd_volumes.lock() {
                if !set.contains(volume_id) {
                    set.insert(volume_id.to_string());
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }

    pub fn release_volume_io_guard(&self, volume_id: &str) {
        if let Ok(mut set) = self.active_hdd_volumes.lock() {
            set.remove(volume_id);
        }
    }

    /// Wait while transcoding is active and `throttle_analysis_while_transcoding` is enabled, or task is paused.
    pub fn wait_for_budget_or_pause(
        &self,
        scheduler: &crate::scheduler::Scheduler,
        pause_flag: Option<&Arc<AtomicBool>>,
        cancel_flag: Option<&Arc<AtomicBool>>,
    ) {
        loop {
            if cancel_flag.map(|f| f.load(Ordering::Relaxed)).unwrap_or(false) {
                return;
            }
            let paused = pause_flag.map(|f| f.load(Ordering::Relaxed)).unwrap_or(false);
            let transcode_active = scheduler
                .active_cancel_flag
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false);
            let throttle = self.should_throttle_for_transcode(transcode_active);
            if !paused && !throttle {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    /// Check if heavy background analysis should throttle because transcoding is active
    pub fn should_throttle_for_transcode(&self, has_active_transcode: bool) -> bool {
        if !has_active_transcode {
            return false;
        }
        self.get_budgets()
            .map(|b| b.throttle_analysis_while_transcoding)
            .unwrap_or(true)
    }
}
