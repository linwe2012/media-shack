use crate::db::{CleanupIntentRecord, Database};
use crate::models::*;
use crate::platform::{
    capture_file_identity, check_trash_support, move_to_recoverable_trash_verified,
};
use std::collections::HashSet;
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

    // Check cross-queue references to the same source
    for other in all_jobs {
        if other.id == job.id {
            continue;
        }
        let same_source = match (&other.source_fingerprint, &job.source_fingerprint) {
            (Some(ofp), Some(jfp)) => ofp.file_id == jfp.file_id,
            _ => other.source_path == job.source_path,
        };
        if !same_source {
            continue;
        }
        // Hold source while any other job referencing it still needs it (including retryable failures)
        let other_needs_source = matches!(
            other.status,
            JobStatus::Queued
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
            return (
                false,
                Some(format!(
                    "Source is still referenced by job {} in another/same queue ({:?}).",
                    other.source_rel_path, other.status
                )),
            );
        }
        if is_unattended && other.cleanup_policy != job.cleanup_policy {
            return (
                false,
                Some("Multiple jobs share this source with conflicting cleanup policies; manual review required.".to_string()),
            );
        }
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
        let (eligible, reason) = evaluate_cleanup_eligibility(j, &all_jobs, false);
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

    let src_fp = job
        .source_fingerprint
        .clone()
        .ok_or_else(|| "Missing source fingerprint".to_string())?;
    let src_path = Path::new(&job.source_path);
    let out_path = Path::new(&job.destination_path);

    // Recheck verified output presence & full identity/content fingerprint
    let out_fp = match capture_file_identity(out_path) {
        Ok(fp) => fp,
        Err(e) => {
            let msg = format!("Verified output missing or unreadable before cleanup: {e}");
            job.cleanup_outcome = CleanupOutcome::Failed;
            job.cleanup_error = Some(msg.clone());
            let _ = db.upsert_job(&job);
            return Err(msg);
        }
    };
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
    if let Some(ref recorded_out_fp) = job.output_fingerprint {
        if out_fp.file_id != recorded_out_fp.file_id
            || out_fp.volume_id != recorded_out_fp.volume_id
            || out_fp.mtime_ns != recorded_out_fp.mtime_ns
            || out_fp.head_hash != recorded_out_fp.head_hash
        {
            let msg = "Verified output file identity or content changed since publication. Keeping original.".to_string();
            job.cleanup_outcome = CleanupOutcome::Failed;
            job.cleanup_error = Some(msg.clone());
            let _ = db.upsert_job(&job);
            return Err(msg);
        }
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
    if let Err(e) = check_trash_support(src_path) {
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

    match move_to_recoverable_trash_verified(src_path, &src_fp) {
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
