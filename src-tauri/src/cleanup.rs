use crate::db::{CleanupIntentRecord, Database};
use crate::models::*;
use crate::platform::{
    capture_file_identity, capture_file_identity_with_blake3, check_trash_support,
    move_to_recoverable_trash_verified,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConsentSourceItem {
    pub job_id: String,
    pub source_path: String,
    pub file_id: String,
    pub size_bytes: u64,
    pub recipe_json: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewedTrashPlanPayload {
    pub queue_id: Option<String>,
    pub queue_name: Option<String>,
    pub removal_reason: RemovalReason,
    pub items: Vec<TrashPreviewItem>,
}

/// Fail-closed check of Protected status across path, `(volume_id, file_id)`, and `ws_job_decisions`.
/// Any database/JSON read error returns `Err` so callers never treat an unreadable protection table as unprotected.
pub fn is_file_protected(
    db: &Database,
    path: Option<&Path>,
    fp: Option<&SourceFingerprint>,
) -> Result<bool, String> {
    let entries: Vec<ProtectedSourceRecord> = db.get_workspace_json_strict("ws_protected_entries")?;
    let legacy_paths: Vec<String> = db.get_workspace_json_strict("ws_protected_sources")?;
    let dec_map: HashMap<String, String> = db.get_workspace_json_strict("ws_job_decisions")?;

    let path_str = path.map(|p| crate::catalog::normalize_display_path(p));
    let raw_path_str = path.map(|p| p.to_string_lossy().to_string());

    if let Some(f) = fp {
        if entries
            .iter()
            .any(|e| e.file_id == f.file_id && e.volume_id == f.volume_id)
        {
            return Ok(true);
        }
    }
    if let Some(ref norm) = path_str {
        if entries.iter().any(|e| &e.path == norm) || legacy_paths.iter().any(|p| p == norm) {
            return Ok(true);
        }
    }
    if let Some(ref raw) = raw_path_str {
        if entries.iter().any(|e| &e.path == raw) || legacy_paths.iter().any(|p| p == raw) {
            return Ok(true);
        }
    }

    // Check job-level `protected` decisions across all queues too (fail closed on DB read error)
    let all_jobs = db.list_all_jobs()?;
    for j in all_jobs {
        if dec_map.get(&j.id).map(|s| s.as_str()) != Some("protected") {
            continue;
        }
        if let (Some(f), Some(ref jfp)) = (fp, &j.source_fingerprint) {
            if jfp.file_id == f.file_id && jfp.volume_id == f.volume_id {
                return Ok(true);
            }
        }
        if let Some(ref norm) = path_str {
            if crate::catalog::normalize_display_path(Path::new(&j.source_path)) == *norm {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

pub fn update_protected_entries_after_move(
    db: &Database,
    old_path: &str,
    new_path: &str,
    new_fp: Option<&SourceFingerprint>,
) -> Result<(), String> {
    let mut entries: Vec<ProtectedSourceRecord> = db.get_workspace_json_strict("ws_protected_entries")?;
    let mut legacy_paths: Vec<String> = db.get_workspace_json_strict("ws_protected_sources")?;
    let mut changed = false;

    for e in &mut entries {
        let matched = e.path == old_path
            || new_fp
                .map(|f| e.file_id == f.file_id && e.volume_id == f.volume_id)
                .unwrap_or(false);
        if matched {
            e.path = new_path.to_string();
            if let Some(f) = new_fp {
                e.volume_id = f.volume_id.clone();
                e.file_id = f.file_id.clone();
            }
            changed = true;
        }
    }
    for p in &mut legacy_paths {
        if p == old_path {
            *p = new_path.to_string();
            changed = true;
        }
    }
    if changed {
        db.set_workspace_json("ws_protected_entries", &entries)?;
        db.set_workspace_json("ws_protected_sources", &legacy_paths)?;
    }
    Ok(())
}

pub fn sync_protection_entry(
    db: &Database,
    path: &Path,
    protect: bool,
    exclude_job_id: Option<&str>,
) -> Result<(), String> {
    let mut entries: Vec<ProtectedSourceRecord> = db.get_workspace_json_strict("ws_protected_entries")?;
    let mut legacy_paths: Vec<String> = db.get_workspace_json_strict("ws_protected_sources")?;
    let dec_map: HashMap<String, String> = db.get_workspace_json_strict("ws_job_decisions")?;
    let path_str = path.to_string_lossy().to_string();
    let norm_path = crate::catalog::normalize_display_path(path);
    let fp = capture_file_identity(path).ok();

    if protect {
        if !legacy_paths.contains(&path_str) {
            legacy_paths.push(path_str.clone());
        }
        if let Some(ref f) = fp {
            if !entries
                .iter()
                .any(|e| e.file_id == f.file_id && e.volume_id == f.volume_id)
            {
                entries.push(ProtectedSourceRecord {
                    volume_id: f.volume_id.clone(),
                    file_id: f.file_id.clone(),
                    path: norm_path,
                });
            }
        } else if !entries.iter().any(|e| e.path == norm_path || e.path == path_str) {
            entries.push(ProtectedSourceRecord {
                volume_id: String::new(),
                file_id: String::new(),
                path: norm_path,
            });
        }
    } else {
        let all_jobs = db.list_all_jobs()?;
        let still_protected = all_jobs.iter().any(|j| {
            if exclude_job_id == Some(j.id.as_str()) {
                return false;
            }
            if dec_map.get(&j.id).map(|s| s.as_str()) != Some("protected") {
                return false;
            }
            if j.source_path == path_str {
                return true;
            }
            match (&fp, &j.source_fingerprint) {
                (Some(f), Some(jf)) => jf.file_id == f.file_id && jf.volume_id == f.volume_id,
                _ => false,
            }
        });
        if !still_protected {
            legacy_paths.retain(|p| p != &path_str && p != &norm_path);
            entries.retain(|e| {
                if e.path == path_str || e.path == norm_path {
                    return false;
                }
                if let Some(ref f) = fp {
                    if !e.file_id.is_empty() && e.file_id == f.file_id && e.volume_id == f.volume_id {
                        return false;
                    }
                }
                true
            });
        }
    }

    db.set_workspace_json("ws_protected_entries", &entries)?;
    db.set_workspace_json("ws_protected_sources", &legacy_paths)?;
    Ok(())
}

fn find_blocking_job_reference(
    all_jobs: &[JobRecord],
    exclude_job_id: Option<&str>,
    exclude_job_ids: Option<&HashSet<String>>,
    path: &Path,
    fp: Option<&SourceFingerprint>,
    is_unattended: bool,
    owner_job_cleanup_policy: Option<CleanupPolicy>,
) -> Option<String> {
    let norm = crate::catalog::normalize_display_path(path);
    for other in all_jobs {
        if exclude_job_id == Some(other.id.as_str()) {
            continue;
        }
        if exclude_job_ids.map(|s| s.contains(&other.id)).unwrap_or(false) {
            continue;
        }
        let same_source = match (&other.source_fingerprint, fp) {
            (Some(ofp), Some(target_fp)) => {
                ofp.file_id == target_fp.file_id && ofp.volume_id == target_fp.volume_id
            }
            _ => crate::catalog::normalize_display_path(Path::new(&other.source_path)) == norm,
        };
        if !same_source {
            continue;
        }
        // Hold source while any referencing job still needs it (including retryable failures/pending items)
        let other_needs_source = matches!(
            other.status,
            JobStatus::Listed
                | JobStatus::Queued
                | JobStatus::Probing
                | JobStatus::Ready
                | JobStatus::NeedsAttention
                | JobStatus::Encoding
                | JobStatus::Verifying
                | JobStatus::Publishing
                | JobStatus::Failed
                | JobStatus::Canceled
                | JobStatus::Interrupted
        );
        if other_needs_source {
            return Some(format!(
                "Source is still referenced by job '{}' ({:?}).",
                other.source_rel_path, other.status
            ));
        }
        if is_unattended {
            if let Some(pol) = owner_job_cleanup_policy {
                if other.cleanup_policy != pol {
                    return Some("Multiple jobs share this source with conflicting cleanup policies; manual review required.".to_string());
                }
            }
        }
    }
    None
}

pub fn evaluate_cleanup_eligibility(
    job: &JobRecord,
    all_jobs: &[JobRecord],
    is_unattended: bool,
) -> (bool, Option<String>) {
    if job.status != JobStatus::Completed {
        return (
            false,
            Some(format!(
                "Only Completed jobs are eligible for cleanup (current status: {:?}).",
                job.status
            )),
        );
    }
    if !job.full_decode_verified {
        return (
            false,
            Some("Full A/V decode verification of the output is required before source cleanup.".to_string()),
        );
    }
    if job.output_bytes.unwrap_or(0) == 0 {
        return (
            false,
            Some("Verified output size is missing or zero.".to_string()),
        );
    }
    if job.output_fingerprint.is_none() {
        return (
            false,
            Some("Verified output identity fingerprint is missing (older or incomplete record); keeping original.".to_string()),
        );
    }
    let Some(ref src_fp) = job.source_fingerprint else {
        return (
            false,
            Some("Source file identity fingerprint was not recorded.".to_string()),
        );
    };
    if src_fp.link_count > 1 {
        return (
            false,
            Some("File has multiple hard links (ambiguous identity).".to_string()),
        );
    }

    if is_unattended {
        if job.cleanup_policy != CleanupPolicy::TrashAfterEachFile {
            return (
                false,
                Some("Job policy does not authorize unattended cleanup.".to_string()),
            );
        }
        if let Some(ref plan) = job.plan {
            if plan.has_omitted_tracks || plan.has_approved_conversions {
                return (
                    false,
                    Some("Jobs with omitted tracks or lossy track conversions are excluded from automatic cleanup.".to_string()),
                );
            }
        }
        if job.consent_id.is_none() {
            return (
                false,
                Some("Missing explicit batch cleanup consent for this job.".to_string()),
            );
        }
    }

    if let Some(reason) = find_blocking_job_reference(
        all_jobs,
        Some(&job.id),
        None,
        Path::new(&job.source_path),
        Some(src_fp),
        is_unattended,
        Some(job.cleanup_policy),
    ) {
        return (false, Some(reason));
    }

    (true, None)
}

pub fn verify_batch_consent(db: &Database, job: &JobRecord) -> Result<(), String> {
    let cid = job
        .consent_id
        .as_deref()
        .ok_or_else(|| "No cleanup consent bound to this job.".to_string())?;
    let (c_qid, c_bid, manifest_json) = db
        .get_cleanup_consent(cid)?
        .ok_or_else(|| "Cleanup consent record not found.".to_string())?;
    if c_qid != job.queue_id || c_bid != job.batch_id {
        return Err("Consent does not match this queue and batch.".to_string());
    }
    let items: Vec<ConsentSourceItem> =
        serde_json::from_str(&manifest_json).map_err(|e| format!("Invalid consent manifest: {e}"))?;
    let recipe_json = serde_json::to_string(&job.recipe).unwrap_or_default();
    let src_file_id = job
        .source_fingerprint
        .as_ref()
        .map(|f| f.file_id.as_str())
        .unwrap_or("");

    let matched = items.iter().any(|it| {
        it.job_id == job.id && it.file_id == src_file_id && it.recipe_json == recipe_json
    });
    if !matched {
        return Err("Source file or recipe was added/modified after batch consent.".to_string());
    }
    Ok(())
}

pub fn build_cleanup_candidates(db: &Database, queue_id: &str) -> Result<Vec<CleanupCandidate>, String> {
    let all_jobs = db.list_all_jobs()?;
    let mut out = Vec::new();
    for j in all_jobs.iter().filter(|j| j.queue_id == queue_id && j.status == JobStatus::Completed) {
        let (mut eligible, mut reason) = evaluate_cleanup_eligibility(j, &all_jobs, false);
        if eligible {
            match is_file_protected(db, Some(Path::new(&j.source_path)), j.source_fingerprint.as_ref()) {
                Ok(true) => {
                    eligible = false;
                    reason = Some("Protected source ★ — original is locked against cleanup.".to_string());
                }
                Ok(false) => {}
                Err(e) => {
                    eligible = false;
                    reason = Some(format!("Cannot verify protection state safely: {e}"));
                }
            }
        }
        if eligible {
            if let Err(e) = check_trash_support(Path::new(&j.source_path)) {
                eligible = false;
                reason = Some(e);
            }
        }
        let vol_id = j
            .source_fingerprint
            .as_ref()
            .map(|f| f.volume_id.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let codec_label = if j.recipe.family == PresetFamily::Remux {
            format!("Remux ({})", j.recipe.container.ext().to_uppercase())
        } else {
            format!(
                "{} / {} (CRF {})",
                j.recipe.codec.display_name(),
                j.recipe.container.ext().to_uppercase(),
                j.recipe.crf
            )
        };
        let has_approved_conversions = j
            .plan
            .as_ref()
            .map(|p| p.has_approved_conversions || p.has_omitted_tracks)
            .unwrap_or(false);

        out.push(CleanupCandidate {
            job_id: j.id.clone(),
            queue_id: j.queue_id.clone(),
            source_path: j.source_path.clone(),
            output_path: j.destination_path.clone(),
            input_bytes: j.input_bytes,
            output_bytes: j.output_bytes.unwrap_or(0),
            source_volume_id: vol_id,
            codec_label,
            full_decode_verified: j.full_decode_verified,
            eligible,
            ineligibility_reason: reason,
            has_approved_conversions,
            cleanup_outcome: j.cleanup_outcome,
        });
    }
    Ok(out)
}

pub fn execute_cleanup_for_job(
    db: &Database,
    ffmpeg_path: Option<&Path>,
    job_id: &str,
    is_unattended: bool,
) -> Result<JobRecord, String> {
    let all_jobs = db.list_all_jobs()?;
    let mut job = all_jobs
        .iter()
        .find(|j| j.id == job_id)
        .cloned()
        .ok_or_else(|| "Job not found".to_string())?;

    if job.cleanup_outcome == CleanupOutcome::Trashed {
        return Ok(job);
    }

    // Fail-closed Protected ★ verification (applies to BOTH manual and unattended cleanup)
    match is_file_protected(db, Some(Path::new(&job.source_path)), job.source_fingerprint.as_ref()) {
        Ok(true) => {
            let msg = "Protected source ★ — marked protected after consent or across tabs; keeping original.".to_string();
            job.cleanup_outcome = CleanupOutcome::Skipped;
            job.cleanup_error = Some(msg.clone());
            let _ = db.upsert_job(&job);
            return Err(msg);
        }
        Ok(false) => {}
        Err(e) => {
            let msg = format!("Protection check failed ({e}); keeping original for safety.");
            job.cleanup_outcome = CleanupOutcome::Skipped;
            job.cleanup_error = Some(msg.clone());
            let _ = db.upsert_job(&job);
            return Err(msg);
        }
    }

    // If manual review is requested on a job that didn't have full_decode_verified yet, run full decode first!
    if !is_unattended && !job.full_decode_verified && job.status == JobStatus::Completed {
        if let Some(ff) = ffmpeg_path {
            let out_p = Path::new(&job.destination_path);
            if let Err(e) = crate::verify::run_full_decode_check(ff, out_p) {
                job.cleanup_outcome = CleanupOutcome::Failed;
                job.cleanup_error = Some(format!("Full output decode check failed: {e}"));
                let _ = db.upsert_job(&job);
                return Err(job.cleanup_error.clone().unwrap());
            }
            job.full_decode_verified = true;
            let _ = db.upsert_job(&job);
        }
    }

    let (eligible, reason) = evaluate_cleanup_eligibility(&job, &all_jobs, is_unattended);
    if !eligible {
        let msg = reason.unwrap_or_else(|| "Job is ineligible for source cleanup.".to_string());
        job.cleanup_outcome = CleanupOutcome::Skipped;
        job.cleanup_error = Some(msg.clone());
        let _ = db.upsert_job(&job);
        return Err(msg);
    }

    if is_unattended {
        if let Err(e) = verify_batch_consent(db, &job) {
            job.cleanup_outcome = CleanupOutcome::Skipped;
            job.cleanup_error = Some(e.clone());
            let _ = db.upsert_job(&job);
            return Err(e);
        }
    }

    let out_path = std::path::PathBuf::from(&job.destination_path);
    let src_path = std::path::PathBuf::from(&job.source_path);
    let src_fp = job
        .source_fingerprint
        .clone()
        .ok_or_else(|| "Missing source fingerprint".to_string())?;
    let recorded_out_fp = job
        .output_fingerprint
        .clone()
        .ok_or_else(|| "Missing verified output fingerprint".to_string())?;

    // Recheck verified output presence & full identity/content fingerprint
    let out_fp = if recorded_out_fp.full_blake3.is_some() {
        capture_file_identity_with_blake3(&out_path)
    } else {
        capture_file_identity(&out_path)
    }
    .map_err(|e| {
        let msg = format!("Verified output missing or unreadable before cleanup: {e}");
        job.cleanup_outcome = CleanupOutcome::Failed;
        job.cleanup_error = Some(msg.clone());
        let _ = db.upsert_job(&job);
        msg
    })?;

    if Some(out_fp.size_bytes) != job.output_bytes {
        let msg = format!(
            "Verified output size changed since publication (expected {:?}, got {}). Keeping original.",
            job.output_bytes, out_fp.size_bytes
        );
        job.cleanup_outcome = CleanupOutcome::Failed;
        job.cleanup_error = Some(msg.clone());
        let _ = db.upsert_job(&job);
        return Err(msg);
    }
    if out_fp.file_id != recorded_out_fp.file_id
        || out_fp.volume_id != recorded_out_fp.volume_id
        || out_fp.mtime_ns != recorded_out_fp.mtime_ns
        || out_fp.head_hash != recorded_out_fp.head_hash
        || (recorded_out_fp.full_blake3.is_some() && out_fp.full_blake3 != recorded_out_fp.full_blake3)
    {
        let msg = "Verified output file identity or content changed since publication. Keeping original.".to_string();
        job.cleanup_outcome = CleanupOutcome::Failed;
        job.cleanup_error = Some(msg.clone());
        let _ = db.upsert_job(&job);
        return Err(msg);
    }

    // Never allow source and output to alias each other
    if out_fp.file_id == src_fp.file_id || out_path == src_path {
        let msg = "Source and verified output share the same file identity (alias). Keeping original.".to_string();
        job.cleanup_outcome = CleanupOutcome::Failed;
        job.cleanup_error = Some(msg.clone());
        let _ = db.upsert_job(&job);
        return Err(msg);
    }

    // Verify Trash support on source volume
    if let Err(e) = check_trash_support(&src_path) {
        job.cleanup_outcome = CleanupOutcome::Failed;
        job.cleanup_error = Some(e.clone());
        let _ = db.upsert_job(&job);
        return Err(e);
    }

    // Persist intent before calling OS Trash
    let action_id = Uuid::new_v4().to_string();
    let intent = CleanupIntentRecord {
        action_id: action_id.clone(),
        job_id: job.id.clone(),
        queue_id: job.queue_id.clone(),
        batch_id: job.batch_id.clone(),
        source_path: job.source_path.clone(),
        source_volume_id: src_fp.volume_id.clone(),
        source_file_id: src_fp.file_id.clone(),
        source_size: src_fp.size_bytes,
        verified_output_path: job.destination_path.clone(),
        verified_output_file_id: out_fp.file_id.clone(),
        consent_id: job.consent_id.clone(),
        state: "intent_created".to_string(),
        error_message: None,
    };
    db.save_cleanup_intent(&intent)?;
    db.update_cleanup_intent(&action_id, "executing", None)?;

    match move_to_recoverable_trash_verified(&src_path, &src_fp) {
        Ok(()) => {
            db.update_cleanup_intent(&action_id, "trashed", None)?;
            job.cleanup_outcome = CleanupOutcome::Trashed;
            job.cleanup_error = None;
            db.upsert_job(&job)?;
            Ok(job)
        }
        Err(e) => {
            let _ = db.update_cleanup_intent(&action_id, "failed", Some(&e));
            job.cleanup_outcome = CleanupOutcome::Failed;
            job.cleanup_error = Some(e.clone());
            let _ = db.upsert_job(&job);
            Err(e)
        }
    }
}

pub fn reconcile_cleanup_intents_on_launch(db: &Database) -> Result<(), String> {
    let pending = db.list_pending_cleanup_intents()?;
    for intent in pending {
        let src_path = Path::new(&intent.source_path);
        let msg = match capture_file_identity(src_path) {
            Ok(fp) if fp.file_id == intent.source_file_id => {
                "App exited during cleanup; original file is still present. Review manually (will not replay automatically)."
            }
            Ok(_) => {
                "App exited during cleanup and file at source path has a different identity. Review manually."
            }
            Err(_) => {
                "App exited during cleanup and source path is no longer present (likely moved to Trash). Review manually."
            }
        };
        let _ = db.update_cleanup_intent(&intent.action_id, "uncertain", Some(msg));
        if let Ok(Some(mut job)) = db.get_job(&intent.job_id) {
            job.cleanup_outcome = CleanupOutcome::Uncertain;
            job.cleanup_error = Some(msg.to_string());
            let _ = db.upsert_job(&job);
        }
    }
    Ok(())
}

pub fn record_batch_cleanup_consent(
    db: &Database,
    queue_id: &str,
    batch_id: &str,
    job_ids: &[String],
) -> Result<String, String> {
    let all_jobs = db.list_all_jobs()?;
    let id_set: HashSet<&String> = job_ids.iter().collect();
    let mut manifest = Vec::new();
    let mut total_bytes = 0u64;

    for j in all_jobs {
        if j.queue_id == queue_id && id_set.contains(&j.id) {
            if let Some(ref fp) = j.source_fingerprint {
                total_bytes += j.input_bytes;
                manifest.push(ConsentSourceItem {
                    job_id: j.id.clone(),
                    source_path: j.source_path.clone(),
                    file_id: fp.file_id.clone(),
                    size_bytes: j.input_bytes,
                    recipe_json: serde_json::to_string(&j.recipe).unwrap_or_default(),
                });
            }
        }
    }

    let consent_id = Uuid::new_v4().to_string();
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    db.save_cleanup_consent(&consent_id, queue_id, batch_id, &manifest_json, total_bytes)?;

    for item in &manifest {
        if let Ok(Some(mut j)) = db.get_job(&item.job_id) {
            j.batch_id = batch_id.to_string();
            j.consent_id = Some(consent_id.clone());
            j.cleanup_policy = CleanupPolicy::TrashAfterEachFile;
            let _ = db.upsert_job(&j);
        }
    }

    Ok(consent_id)
}

/// Shared strict Trash preview & execution gate across Queue Items, Queue Duplicates, and Library Duplicates.
/// - Enforces queue scope upfront (rejects entire request if any ID is outside `queue_id`).
/// - Checks Protected ★ by file_id + path (fail-closed on DB errors).
/// - Checks all queues for pending/active job references to the same physical file.
/// - For exact duplicate groups, guarantees at least one distinct physical accessible matching copy survives.
/// - Never authorizes visually similar / perceptual matches for automatic bulk removal without explicit review.
pub fn preview_guarded_trash(
    db: &Database,
    queue_id: Option<&str>,
    item_ids: &[String],
    reason: RemovalReason,
) -> Result<TrashPreviewPlan, String> {
    if item_ids.is_empty() {
        return Err("No items selected to move to Trash.".to_string());
    }

    let queue_name = if let Some(qid) = queue_id {
        Some(
            db.get_queue(qid)?
                .ok_or_else(|| format!("Queue '{qid}' not found."))?
                .name,
        )
    } else {
        None
    };

    let all_jobs = db.list_all_jobs()?;
    let target_job_set: HashSet<String> = item_ids.iter().cloned().collect();
    let dedupe_map: HashMap<String, Vec<TabDedupeGroup>> = db.get_workspace_json_strict("ws_tab_dedupe_groups")?;

    let mut preview_items = Vec::new();
    let mut total_bytes = 0u64;
    let mut eligible_count = 0usize;
    let mut blocked_count = 0usize;
    let mut trash_supported = true;
    let mut trash_support_error: Option<String> = None;

    match reason {
        RemovalReason::LibraryDuplicateCopy => {
            let entries = crate::catalog::load_entries_by_file_keys(db, item_ids)?;
            let entry_map: HashMap<String, _> = entries.into_iter().map(|e| (e.file_key.clone(), e)).collect();
            let groups = crate::duplicates::list_duplicate_groups(db, None)?;

            for fk in item_ids {
                let Some(entry) = entry_map.get(fk) else {
                    return Err(format!("Library item '{fk}' not found."));
                };
                let p = Path::new(&entry.path);
                let parent_dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
                let mut elig = true;
                let mut block_reason: Option<String> = None;

                let fp = match capture_file_identity(p) {
                    Ok(f) => f,
                    Err(e) => {
                        elig = false;
                        block_reason = Some(format!("Source file inaccessible or missing: {e}"));
                        SourceFingerprint {
                            volume_id: entry.volume_id.clone(),
                            file_id: entry.os_file_id.clone(),
                            size_bytes: entry.size_bytes,
                            mtime_ns: 0,
                            head_hash: String::new(),
                            link_count: 1,
                            full_blake3: None,
                        }
                    }
                };
                if elig && (fp.file_id != entry.os_file_id || fp.volume_id != entry.volume_id) {
                    elig = false;
                    block_reason = Some("File identity at path changed since last scan.".to_string());
                }
                if elig && fp.link_count > 1 {
                    elig = false;
                    block_reason = Some("Hard-linked file (link count > 1); excluded for safety.".to_string());
                }
                if elig {
                    match is_file_protected(db, Some(p), Some(&fp)) {
                        Ok(true) => {
                            elig = false;
                            block_reason = Some("Protected source ★ — locked against deletion.".to_string());
                        }
                        Ok(false) => {}
                        Err(e) => {
                            elig = false;
                            block_reason = Some(format!("Cannot verify protection status: {e}"));
                        }
                    }
                }
                if elig {
                    if let Some(msg) = find_blocking_job_reference(&all_jobs, None, None, p, Some(&fp), false, None) {
                        elig = false;
                        block_reason = Some(msg);
                    }
                }
                if elig {
                    // Check duplicate group survival invariant
                    let containing = groups.iter().find(|g| g.members.iter().any(|m| &m.entry.file_key == fk));
                    if let Some(grp) = containing {
                        if grp.match_kind == "exact" {
                            let target_keys: HashSet<&str> = item_ids.iter().map(|s| s.as_str()).collect();
                            let has_distinct_survivor = grp.members.iter().any(|m| {
                                if target_keys.contains(m.entry.file_key.as_str()) {
                                    return false;
                                }
                                let mp = Path::new(&m.entry.path);
                                match capture_file_identity_with_blake3(mp) {
                                    Ok(sfp) => {
                                        (sfp.volume_id != fp.volume_id || sfp.file_id != fp.file_id)
                                            && sfp.full_blake3.as_deref() == entry.blake3_hash.as_deref()
                                    }
                                    Err(_) => false,
                                }
                            });
                            if !has_distinct_survivor {
                                elig = false;
                                block_reason = Some("Cannot remove: no distinct accessible exact copy would survive outside selection.".to_string());
                            }
                        }
                    }
                }
                if elig {
                    if let Err(e) = check_trash_support(p) {
                        elig = false;
                        trash_supported = false;
                        if trash_support_error.is_none() {
                            trash_support_error = Some(e.clone());
                        }
                        block_reason = Some(e);
                    }
                }

                if elig {
                    eligible_count += 1;
                    total_bytes += fp.size_bytes;
                } else {
                    blocked_count += 1;
                }
                preview_items.push(TrashPreviewItem {
                    item_id: fk.clone(),
                    queue_id: None,
                    filename: entry.filename.clone(),
                    parent_dir,
                    path: entry.path.clone(),
                    size_bytes: fp.size_bytes,
                    volume_id: fp.volume_id,
                    file_id: fp.file_id,
                    head_hash: fp.head_hash,
                    group_id: None,
                    eligible: elig,
                    reason: block_reason,
                });
            }
        }
        RemovalReason::QueueSpecificItems | RemovalReason::QueueDuplicateGroup => {
            let qid = queue_id.ok_or_else(|| "queue_id is required for tab Trash.".to_string())?;
            let queue_groups = dedupe_map.get(qid).cloned().unwrap_or_default();

            // Strict queue ownership validation BEFORE touching anything
            let mut target_jobs = Vec::with_capacity(item_ids.len());
            for jid in item_ids {
                let job = all_jobs
                    .iter()
                    .find(|j| &j.id == jid)
                    .ok_or_else(|| format!("Selected item '{jid}' does not exist."))?;
                if job.queue_id != qid {
                    return Err(format!(
                        "Scope violation: item '{}' belongs to another tab, not the current tab. Rejected entire request.",
                        job.source_rel_path
                    ));
                }
                target_jobs.push(job.clone());
            }

            // Deduplicate identical physical files referenced multiple times in the selection
            let mut seen_phys: HashSet<(String, String)> = HashSet::new();

            for job in target_jobs {
                let p = Path::new(&job.source_path);
                let filename = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| job.source_rel_path.clone());
                let parent_dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
                let mut elig = true;
                let mut block_reason: Option<String> = None;

                let fp = match capture_file_identity(p) {
                    Ok(f) => f,
                    Err(e) => {
                        elig = false;
                        block_reason = Some(format!("File missing or unreadable: {e}"));
                        job.source_fingerprint.clone().unwrap_or(SourceFingerprint {
                            volume_id: String::new(),
                            file_id: String::new(),
                            size_bytes: job.input_bytes,
                            mtime_ns: 0,
                            head_hash: String::new(),
                            link_count: 1,
                            full_blake3: None,
                        })
                    }
                };
                if elig && fp.file_id != String::new() && !seen_phys.insert((fp.volume_id.clone(), fp.file_id.clone())) {
                    elig = false;
                    block_reason = Some("Already queued in this selection (same physical file).".to_string());
                }
                if elig {
                    if let Some(ref stored) = job.source_fingerprint {
                        if stored.file_id != fp.file_id || stored.volume_id != fp.volume_id {
                            elig = false;
                            block_reason = Some("File identity at path changed since attachment.".to_string());
                        }
                    }
                }
                if elig && fp.link_count > 1 {
                    elig = false;
                    block_reason = Some("File has multiple hard links; excluded for safety.".to_string());
                }
                if elig {
                    match is_file_protected(db, Some(p), Some(&fp)) {
                        Ok(true) => {
                            elig = false;
                            block_reason = Some("Protected source ★ — locked against deletion.".to_string());
                        }
                        Ok(false) => {}
                        Err(e) => {
                            elig = false;
                            block_reason = Some(format!("Cannot verify protection state: {e}"));
                        }
                    }
                }
                if elig {
                    if let Some(msg) = find_blocking_job_reference(
                        &all_jobs,
                        Some(&job.id),
                        Some(&target_job_set),
                        p,
                        Some(&fp),
                        false,
                        None,
                    ) {
                        elig = false;
                        block_reason = Some(msg);
                    }
                }
                let grp_opt = queue_groups
                    .iter()
                    .find(|g| g.items.iter().any(|it| it.job_id == job.id));
                if elig && reason == RemovalReason::QueueDuplicateGroup {
                    if let Some(grp) = grp_opt {
                        if grp.match_type.starts_with("Exact") {
                            let has_distinct_survivor = grp.items.iter().any(|it| {
                                if it.trashed || target_job_set.contains(&it.job_id) {
                                    return false;
                                }
                                let sp = Path::new(&it.path);
                                if let Ok(sfp) = capture_file_identity(sp) {
                                    (sfp.volume_id != fp.volume_id || sfp.file_id != fp.file_id)
                                        && sfp.size_bytes == fp.size_bytes
                                } else {
                                    false
                                }
                            });
                            if !has_distinct_survivor {
                                elig = false;
                                block_reason = Some("Cannot remove: no distinct accessible exact copy would survive.".to_string());
                            }
                        }
                    }
                }
                if elig {
                    if let Err(e) = check_trash_support(p) {
                        elig = false;
                        trash_supported = false;
                        if trash_support_error.is_none() {
                            trash_support_error = Some(e.clone());
                        }
                        block_reason = Some(e);
                    }
                }

                if elig {
                    eligible_count += 1;
                    total_bytes += fp.size_bytes;
                } else {
                    blocked_count += 1;
                }
                preview_items.push(TrashPreviewItem {
                    item_id: job.id.clone(),
                    queue_id: Some(qid.to_string()),
                    filename,
                    parent_dir,
                    path: job.source_path.clone(),
                    size_bytes: fp.size_bytes,
                    volume_id: fp.volume_id,
                    file_id: fp.file_id,
                    head_hash: fp.head_hash,
                    group_id: grp_opt.map(|g| g.id.clone()),
                    eligible: elig,
                    reason: block_reason,
                });
            }
        }
        _ => return Err("Unsupported removal reason for interactive Trash preview.".to_string()),
    }

    let plan_id = Uuid::new_v4().to_string();
    let payload = ReviewedTrashPlanPayload {
        queue_id: queue_id.map(|s| s.to_string()),
        queue_name: queue_name.clone(),
        removal_reason: reason,
        items: preview_items.clone(),
    };
    let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    db.save_reviewed_plan(&plan_id, "trash", queue_id, &payload_json)?;

    Ok(TrashPreviewPlan {
        plan_id,
        queue_id: queue_id.map(|s| s.to_string()),
        queue_name,
        removal_reason: reason,
        items: preview_items,
        eligible_count,
        total_bytes,
        blocked_count,
        trash_supported,
        trash_support_error,
    })
}

pub fn execute_guarded_trash_plan(
    db: &Database,
    plan_id: &str,
) -> Result<TrashBatchResult, String> {
    let (_qid, payload_json) = db.take_reviewed_plan(plan_id, "trash")?;
    let plan: ReviewedTrashPlanPayload = serde_json::from_str(&payload_json)
        .map_err(|e| format!("Invalid reviewed Trash plan: {e}"))?;

    let all_jobs = db.list_all_jobs()?;
    let eligible_ids: HashSet<String> = plan
        .items
        .iter()
        .filter(|i| i.eligible)
        .map(|i| i.item_id.clone())
        .collect();
    let mut outcomes = Vec::new();
    let mut moved_count = 0usize;
    let mut failed_count = 0usize;

    // Validate scope once more upfront before touching any file
    if let Some(ref qid) = plan.queue_id {
        for it in &plan.items {
            let job = all_jobs
                .iter()
                .find(|j| j.id == it.item_id)
                .ok_or_else(|| format!("Item '{}' no longer exists in tab.", it.filename))?;
            if &job.queue_id != qid {
                return Err("Scope mismatch detected in Trash plan; aborted safely.".to_string());
            }
        }
    }

    let mut dedupe_map: HashMap<String, Vec<TabDedupeGroup>> = db.get_workspace_json_strict("ws_tab_dedupe_groups")?;
    let lib_dup_groups = if plan.removal_reason == RemovalReason::LibraryDuplicateCopy {
        crate::duplicates::list_duplicate_groups(db, Some("exact")).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut trashed_job_ids = HashSet::new();
    let mut trashed_lib_keys: HashSet<String> = HashSet::new();

    for it in &plan.items {
        if !it.eligible {
            failed_count += 1;
            outcomes.push(TrashItemOutcome {
                item_id: it.item_id.clone(),
                filename: it.filename.clone(),
                path: it.path.clone(),
                moved: false,
                error: it.reason.clone().or_else(|| Some("Blocked by safety check.".to_string())),
            });
            continue;
        }

        let p = Path::new(&it.path);
        let live_fp = match capture_file_identity(p) {
            Ok(fp) => fp,
            Err(e) => {
                failed_count += 1;
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: false,
                    error: Some(format!("File missing or unreadable: {e}")),
                });
                continue;
            }
        };

        if live_fp.file_id != it.file_id
            || live_fp.volume_id != it.volume_id
            || live_fp.size_bytes != it.size_bytes
            || live_fp.head_hash != it.head_hash
        {
            failed_count += 1;
            outcomes.push(TrashItemOutcome {
                item_id: it.item_id.clone(),
                filename: it.filename.clone(),
                path: it.path.clone(),
                moved: false,
                error: Some("File identity or content changed after preview; left untouched.".to_string()),
            });
            continue;
        }

        if live_fp.link_count > 1 {
            failed_count += 1;
            outcomes.push(TrashItemOutcome {
                item_id: it.item_id.clone(),
                filename: it.filename.clone(),
                path: it.path.clone(),
                moved: false,
                error: Some("File has multiple hard links; left untouched.".to_string()),
            });
            continue;
        }

        match is_file_protected(db, Some(p), Some(&live_fp)) {
            Ok(true) => {
                failed_count += 1;
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: false,
                    error: Some("Protected ★ (updated after preview); left untouched.".to_string()),
                });
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                failed_count += 1;
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: false,
                    error: Some(format!("Protection verification failed ({e}); left untouched.")),
                });
                continue;
            }
        }

        let current_jobs = match db.list_all_jobs() {
            Ok(j) => j,
            Err(e) => {
                failed_count += 1;
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: false,
                    error: Some(format!("Queue reference check failed ({e}); left untouched.")),
                });
                continue;
            }
        };
        if let Some(msg) = find_blocking_job_reference(
            &current_jobs,
            Some(&it.item_id),
            Some(&eligible_ids),
            p,
            Some(&live_fp),
            false,
            None,
        ) {
            failed_count += 1;
            outcomes.push(TrashItemOutcome {
                item_id: it.item_id.clone(),
                filename: it.filename.clone(),
                path: it.path.clone(),
                moved: false,
                error: Some(msg),
            });
            continue;
        }

        // Re-verify last exact duplicate copy immediately before Trash in case survivor changed mid-batch
        if plan.removal_reason == RemovalReason::LibraryDuplicateCopy {
            if let Some(grp) = lib_dup_groups
                .iter()
                .find(|g| g.match_kind == "exact" && g.members.iter().any(|m| m.entry.file_key == it.item_id))
            {
                let my_blake3 = grp
                    .members
                    .iter()
                    .find(|m| m.entry.file_key == it.item_id)
                    .and_then(|m| m.entry.blake3_hash.as_deref());
                let has_survivor = grp.members.iter().any(|m| {
                    if m.entry.file_key == it.item_id
                        || trashed_lib_keys.contains(&m.entry.file_key)
                        || eligible_ids.contains(&m.entry.file_key)
                    {
                        return false;
                    }
                    let sp = Path::new(&m.entry.path);
                    capture_file_identity_with_blake3(sp)
                        .map(|sfp| {
                            (sfp.volume_id != live_fp.volume_id || sfp.file_id != live_fp.file_id)
                                && sfp.full_blake3.as_deref() == my_blake3
                        })
                        .unwrap_or(false)
                });
                if !has_survivor {
                    failed_count += 1;
                    outcomes.push(TrashItemOutcome {
                        item_id: it.item_id.clone(),
                        filename: it.filename.clone(),
                        path: it.path.clone(),
                        moved: false,
                        error: Some(
                            "Exact duplicate survivor disappeared before Trash; keeping this copy."
                                .to_string(),
                        ),
                    });
                    continue;
                }
            }
        }
        if matches!(plan.removal_reason, RemovalReason::QueueDuplicateGroup) {
            if let (Some(qid), Some(gid)) = (&plan.queue_id, &it.group_id) {
                if let Some(grp) = dedupe_map
                    .get(qid)
                    .and_then(|gs| gs.iter().find(|g| &g.id == gid))
                {
                    if grp.match_type.starts_with("Exact") {
                        let has_survivor = grp.items.iter().any(|m| {
                            if m.trashed || m.job_id == it.item_id || trashed_job_ids.contains(&m.job_id) {
                                return false;
                            }
                            let sp = Path::new(&m.path);
                            capture_file_identity(sp)
                                .map(|sfp| {
                                    (sfp.volume_id != live_fp.volume_id || sfp.file_id != live_fp.file_id)
                                        && sfp.size_bytes == live_fp.size_bytes
                                })
                                .unwrap_or(false)
                        });
                        if !has_survivor {
                            failed_count += 1;
                            outcomes.push(TrashItemOutcome {
                                item_id: it.item_id.clone(),
                                filename: it.filename.clone(),
                                path: it.path.clone(),
                                moved: false,
                                error: Some("Survivor disappeared before Trash; keeping this copy.".to_string()),
                            });
                            continue;
                        }
                    }
                }
            }
        }

        match move_to_recoverable_trash_verified(p, &live_fp) {
            Ok(()) => {
                moved_count += 1;
                if plan.removal_reason == RemovalReason::LibraryDuplicateCopy {
                    trashed_lib_keys.insert(it.item_id.clone());
                    if let Ok(conn) = db.lock_conn() {
                        let _ = conn.execute(
                            "UPDATE file_locations SET is_online = 0, is_current = 0 WHERE file_key = ?1",
                            rusqlite::params![it.item_id],
                        );
                        let _ = conn.execute(
                            "UPDATE media_files SET is_online = 0 WHERE file_key = ?1",
                            rusqlite::params![it.item_id],
                        );
                        let _ = conn.execute(
                            "DELETE FROM similarity_edges WHERE file_key_a = ?1 OR file_key_b = ?1",
                            rusqlite::params![it.item_id],
                        );
                    }
                } else {
                    trashed_job_ids.insert(it.item_id.clone());
                    let _ = db.delete_job(&it.item_id);
                    if let Some(qid) = &plan.queue_id {
                        if let Some(groups) = dedupe_map.get_mut(qid) {
                            for g in groups.iter_mut() {
                                for m in g.items.iter_mut() {
                                    if m.job_id == it.item_id {
                                        m.trashed = true;
                                    }
                                }
                            }
                        }
                    }
                }
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: true,
                    error: None,
                });
            }
            Err(e) => {
                failed_count += 1;
                outcomes.push(TrashItemOutcome {
                    item_id: it.item_id.clone(),
                    filename: it.filename.clone(),
                    path: it.path.clone(),
                    moved: false,
                    error: Some(e),
                });
            }
        }
    }

    if !trashed_job_ids.is_empty() {
        let _ = db.set_workspace_json("ws_tab_dedupe_groups", &dedupe_map);
    }

    Ok(TrashBatchResult {
        moved_count,
        failed_count,
        outcomes,
    })
}
