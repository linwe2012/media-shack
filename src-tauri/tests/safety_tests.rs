use media_shack_lib::catalog::{
    add_library_root, get_asset_detail, query_catalog, scan_library_root, CatalogFilter,
};
use media_shack_lib::cleanup::{
    execute_cleanup_for_job, execute_guarded_trash_plan, is_file_protected, preview_guarded_trash,
    record_batch_cleanup_consent, sync_protection_entry,
};
use media_shack_lib::db::Database;
use media_shack_lib::duplicates::run_full_duplicate_scan;
use media_shack_lib::models::*;
use media_shack_lib::organize::{
    execute_batch_plan_by_id, execute_tab_move_by_tag_plan, preview_batch_move_or_copy,
    preview_tab_move_by_tag, undo_operation,
};
use media_shack_lib::platform::{capture_file_identity, capture_file_identity_with_blake3};
use media_shack_lib::scheduler::Scheduler;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::TempDir;

fn make_queue(id: &str, name: &str, tab_type: TabType, order: i32) -> QueueTab {
    QueueTab {
        id: id.to_string(),
        name: name.to_string(),
        tab_order: order,
        tab_type,
        profile_id: None,
        profile_name: None,
        recipe: EncodeRecipe::archive_balanced_default(),
        priority: ProcessPriority::Background,
        recursive_import: true,
        destination_root: None,
        keep_awake: true,
        error_policy: ErrorPolicy::Continue,
        min_savings_enabled: false,
        min_savings_pct: 15.0,
        cleanup_policy: CleanupPolicy::KeepOriginals,
        full_decode_verify: false,
        is_running: false,
        is_paused: false,
        pause_after_current: false,
        waiting_reason: None,
    }
}

fn make_job(
    id: &str,
    queue_id: &str,
    src_path: &Path,
    rel_path: &str,
    dest_path: &Path,
    status: JobStatus,
) -> JobRecord {
    let fp = capture_file_identity(src_path).ok();
    let size = fp.as_ref().map(|f| f.size_bytes).unwrap_or(0);
    JobRecord {
        id: id.to_string(),
        queue_id: queue_id.to_string(),
        batch_id: format!("batch-{queue_id}"),
        position: 0,
        source_path: src_path.to_string_lossy().to_string(),
        source_rel_path: rel_path.to_string(),
        source_root: src_path.parent().map(|p| p.to_string_lossy().to_string()),
        source_fingerprint: fp,
        destination_path: dest_path.to_string_lossy().to_string(),
        dest_volume_id: None,
        recipe: EncodeRecipe::archive_balanced_default(),
        compat_overrides: CompatOverrides::default(),
        status,
        status_reason: None,
        probe: None,
        plan: None,
        input_bytes: size,
        output_bytes: None,
        output_fingerprint: None,
        error_stage: None,
        error_message: None,
        diagnostics: None,
        cleanup_policy: CleanupPolicy::KeepOriginals,
        cleanup_outcome: CleanupOutcome::None,
        cleanup_error: None,
        consent_id: None,
        full_decode_verified: false,
        min_savings_enabled: false,
        min_savings_pct: 15.0,
        tool_versions: None,
        current_attempt_id: None,
        created_at: 0,
        updated_at: 0,
        completed_at: None,
    }
}

#[test]
fn test_scope_isolation_and_fail_closed_protection_reader() {
    let dir = TempDir::new().unwrap();
    let app_data = dir.path().join("appdata");
    let media_dir = dir.path().join("media");
    std::fs::create_dir_all(&media_dir).unwrap();

    let f1 = media_dir.join("tab1_clip.mp4");
    let f2 = media_dir.join("tab2_clip.mp4");
    std::fs::write(&f1, vec![b'A'; 4096]).unwrap();
    std::fs::write(&f2, vec![b'B'; 4096]).unwrap();

    let db = Database::open(&app_data).unwrap();
    db.upsert_queue(&make_queue("q1", "Organize 1", TabType::Organize, 0))
        .unwrap();
    db.upsert_queue(&make_queue("q2", "Organize 2", TabType::Organize, 1))
        .unwrap();

    let j1 = make_job("j1", "q1", &f1, "tab1_clip.mp4", &f1, JobStatus::Queued);
    let j2 = make_job("j2", "q2", &f2, "tab2_clip.mp4", &f2, JobStatus::Queued);
    db.upsert_job(&j1).unwrap();
    db.upsert_job(&j2).unwrap();

    // Mixed-tab IDs must reject the entire request before touching any file
    let err = preview_guarded_trash(
        &db,
        Some("q1"),
        &["j1".to_string(), "j2".to_string()],
        RemovalReason::QueueSpecificItems,
    )
    .unwrap_err();
    assert!(
        err.contains("Scope violation") || err.contains("another tab"),
        "Expected scope violation, got: {err}"
    );
    assert!(f1.exists() && f2.exists());

    // Corrupting ws_protected_sources JSON must fail closed (never silently behave as unprotected)
    db.set_setting("ws_protected_sources", &"{corrupt-json".to_string())
        .unwrap();
    let prot_err = is_file_protected(&db, Some(&f1), j1.source_fingerprint.as_ref()).unwrap_err();
    assert!(
        prot_err.contains("Corrupted safety state"),
        "Expected fail-closed error, got: {prot_err}"
    );
}

#[test]
fn test_late_protection_missing_and_swapped_output_block_cleanup() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    let src = work.join("original.mp4");
    let out = work.join("encoded.mp4");
    std::fs::write(&src, vec![b'S'; 8192]).unwrap();
    std::fs::write(&out, vec![b'O'; 4096]).unwrap();

    db.upsert_queue(&make_queue("q1", "Queue", TabType::Encode, 0))
        .unwrap();
    let mut job = make_job("j-clean", "q1", &src, "original.mp4", &out, JobStatus::Completed);
    job.cleanup_policy = CleanupPolicy::TrashAfterEachFile;
    job.cleanup_outcome = CleanupOutcome::Pending;
    job.full_decode_verified = true;
    job.output_bytes = Some(4096);
    job.output_fingerprint = Some(capture_file_identity_with_blake3(&out).unwrap());
    db.upsert_job(&job).unwrap();

    // Grant batch consent first
    let _cid = record_batch_cleanup_consent(&db, "q1", &job.batch_id, &[job.id.clone()]).unwrap();
    let job = db.get_job(&job.id).unwrap().unwrap();

    // 1. Mark source Protected ★ AFTER consent was given -> unattended cleanup must refuse!
    sync_protection_entry(&db, &src, true, None).unwrap();
    let err = execute_cleanup_for_job(&db, None, &job.id, true).unwrap_err();
    assert!(
        err.contains("Protected"),
        "Late protection must block unattended cleanup: {err}"
    );
    assert!(src.exists());

    // Remove protection so we test output verification gates
    sync_protection_entry(&db, &src, false, None).unwrap();

    // 2. Missing output_fingerprint must refuse cleanup
    let mut job_no_fp = db.get_job(&job.id).unwrap().unwrap();
    job_no_fp.output_fingerprint = None;
    db.upsert_job(&job_no_fp).unwrap();
    let err_fp = execute_cleanup_for_job(&db, None, &job.id, true).unwrap_err();
    assert!(
        err_fp.contains("identity fingerprint is missing") || err_fp.contains("output_fingerprint"),
        "Missing output_fingerprint must block cleanup: {err_fp}"
    );
    assert!(src.exists());

    // 3. Equal-size content-swapped output must be caught by full BLAKE3 and block cleanup
    let mut job_swapped = db.get_job(&job.id).unwrap().unwrap();
    job_swapped.output_fingerprint = Some(capture_file_identity_with_blake3(&out).unwrap());
    db.upsert_job(&job_swapped).unwrap();
    // Overwrite output with DIFFERENT content of the EXACT SAME byte length
    std::fs::write(&out, vec![b'X'; 4096]).unwrap();
    let err_swap = execute_cleanup_for_job(&db, None, &job.id, true).unwrap_err();
    assert!(
        err_swap.contains("content changed") || err_swap.contains("fingerprint"),
        "Equal-size output swap must block cleanup: {err_swap}"
    );
    assert!(src.exists());
}

#[test]
fn test_exact_duplicate_survivor_hardlink_and_missing_count_guards() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();

    let copy1 = lib.join("copy1.mp4");
    let copy2 = lib.join("copy2.mp4");
    let hl_copy = lib.join("hardlink.mp4");
    let payload: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    std::fs::write(&copy1, &payload).unwrap();
    std::fs::write(&copy2, &payload).unwrap();
    std::fs::hard_link(&copy1, &hl_copy).unwrap();

    let root = add_library_root(&db, &lib, Some("Lib"), true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, None, None, &cancel, None).unwrap();
    run_full_duplicate_scan(&db, None, true, false, &cancel).unwrap();

    let page = query_catalog(&db, &CatalogFilter::default()).unwrap();
    let fk1 = page
        .items
        .iter()
        .find(|e| e.filename == "copy1.mp4")
        .unwrap()
        .file_key
        .clone();
    let fk2 = page
        .items
        .iter()
        .find(|e| e.filename == "copy2.mp4")
        .unwrap()
        .file_key
        .clone();
    let fk_hl = page
        .items
        .iter()
        .find(|e| e.filename == "hardlink.mp4")
        .unwrap()
        .file_key
        .clone();

    // Hardlinked copy & copy1 (linked together) must be blocked; copy2 blocked if all exact copies selected
    let all_plan = preview_guarded_trash(
        &db,
        None,
        &[fk1.clone(), fk2.clone(), fk_hl.clone()],
        RemovalReason::LibraryDuplicateCopy,
    )
    .unwrap();
    assert_eq!(all_plan.eligible_count, 0);
    assert_eq!(all_plan.blocked_count, 3);
    assert!(all_plan
        .items
        .iter()
        .any(|i| i.reason.as_deref().unwrap_or("").contains("Hard-linked")));
    assert!(all_plan
        .items
        .iter()
        .any(|i| i.reason.as_deref().unwrap_or("").contains("survive outside selection")));

    // Missing file inside queue selection must report moved_count = 0, failed_count = 1
    db.upsert_queue(&make_queue("q1", "Org", TabType::Organize, 0))
        .unwrap();
    let ghost = lib.join("vanished.mp4");
    std::fs::write(&ghost, b"temp").unwrap();
    let ghost_job = make_job("jg", "q1", &ghost, "vanished.mp4", &ghost, JobStatus::Queued);
    db.upsert_job(&ghost_job).unwrap();
    std::fs::remove_file(&ghost).unwrap();

    let ghost_plan = preview_guarded_trash(
        &db,
        Some("q1"),
        &["jg".to_string()],
        RemovalReason::QueueSpecificItems,
    )
    .unwrap();
    assert_eq!(ghost_plan.eligible_count, 0);
    let ghost_res = execute_guarded_trash_plan(&db, &ghost_plan.plan_id).unwrap();
    assert_eq!(ghost_res.moved_count, 0);
    assert_eq!(ghost_res.failed_count, 1);
}

#[test]
fn test_unified_move_subfolders_collision_cross_sync_and_safe_undo() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let src_root = dir.path().join("src_root");
    let src_sub = src_root.join("Day1");
    let dst_root = dir.path().join("dest_root");
    std::fs::create_dir_all(&src_sub).unwrap();
    std::fs::create_dir_all(&dst_root).unwrap();

    let clip = src_sub.join("clip.mp4");
    let data: Vec<u8> = (0..8192).map(|i| (i % 199) as u8).collect();
    std::fs::write(&clip, &data).unwrap();

    // Add to Catalog
    let root = add_library_root(&db, &src_root, Some("Src"), true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, None, None, &cancel, None).unwrap();
    let fkey = query_catalog(&db, &CatalogFilter::default()).unwrap().items[0]
        .file_key
        .clone();

    // Add same file to q_org (Organize), q_pending (Ready with consent_id), q_done (Completed)
    db.upsert_queue(&make_queue("q_org", "Organize", TabType::Organize, 0))
        .unwrap();
    db.upsert_queue(&make_queue("q_pend", "Pending", TabType::Encode, 1))
        .unwrap();
    db.upsert_queue(&make_queue("q_done", "Done", TabType::Encode, 2))
        .unwrap();

    let j_org = make_job("j_org", "q_org", &clip, "Day1/clip.mp4", &clip, JobStatus::Queued);
    let mut j_pend = make_job(
        "j_pend",
        "q_pend",
        &clip,
        "Day1/clip.mp4",
        &dst_root.join("out.mkv"),
        JobStatus::Ready,
    );
    j_pend.consent_id = Some("old-consent-id".to_string());
    let j_done = make_job(
        "j_done",
        "q_done",
        &clip,
        "Day1/clip.mp4",
        &dst_root.join("done.mkv"),
        JobStatus::Completed,
    );
    db.upsert_job(&j_org).unwrap();
    db.upsert_job(&j_pend).unwrap();
    db.upsert_job(&j_done).unwrap();

    // Protect source ★ before moving
    sync_protection_entry(&db, &clip, true, None).unwrap();

    let mut job_tags = HashMap::new();
    job_tags.insert("j_org".to_string(), vec!["trip".to_string()]);

    // 1. Pre-existing collision at dest_root/Day1/clip.mp4 -> default keep_existing blocks overwrite
    let expected_dest = dst_root.join("Day1").join("clip.mp4");
    std::fs::create_dir_all(expected_dest.parent().unwrap()).unwrap();
    std::fs::write(&expected_dest, b"existing-do-not-overwrite").unwrap();

    let coll_prev = preview_tab_move_by_tag(
        &db,
        "q_org",
        "trip",
        &dst_root.to_string_lossy(),
        false,
        true,
        "keep_existing",
        &job_tags,
    )
    .unwrap();
    assert_eq!(coll_prev.items.len(), 1);
    assert!(!coll_prev.items[0].valid);
    assert!(coll_prev.items[0].collision);

    // Clear colliding placeholder and preview + execute cleanly
    std::fs::remove_file(&expected_dest).unwrap();
    let prev = preview_tab_move_by_tag(
        &db,
        "q_org",
        "trip",
        &dst_root.to_string_lossy(),
        false,
        true,
        "keep_existing",
        &job_tags,
    )
    .unwrap();
    assert!(prev.items[0].valid);
    assert_eq!(PathBuf::from(&prev.items[0].new_path), expected_dest);

    let exec = execute_tab_move_by_tag_plan(&db, prev.plan_id.as_deref().unwrap()).unwrap();
    assert_eq!(exec.moved_count, 1);
    assert!(!clip.exists());
    assert!(expected_dest.exists());

    // Verify Catalog, Pending job (+ revoked consent!), Historical job, and Protected ★ follow correctly
    let detail = get_asset_detail(&db, &fkey).unwrap();
    assert_eq!(
        std::fs::canonicalize(&detail.entry.path).unwrap(),
        std::fs::canonicalize(&expected_dest).unwrap()
    );
    let j_pend_after = db.get_job("j_pend").unwrap().unwrap();
    assert_eq!(
        std::fs::canonicalize(&j_pend_after.source_path).unwrap(),
        std::fs::canonicalize(&expected_dest).unwrap()
    );
    assert!(j_pend_after.consent_id.is_none(), "Consent must be revoked after source move");
    let j_done_after = db.get_job("j_done").unwrap().unwrap();
    assert_eq!(PathBuf::from(&j_done_after.source_path), clip, "Completed history must stay immutable");
    assert!(is_file_protected(&db, Some(&expected_dest), None).unwrap());

    // 2. Undo Move when original path is occupied must refuse overwrite and keep moved file intact
    std::fs::write(&clip, b"newly-created-blocker").unwrap();
    let undo_err = undo_operation(&db, exec.operation_id.as_deref().unwrap()).unwrap_err();
    assert!(
        undo_err.contains("Undo conflict") && undo_err.contains("keeping moved file"),
        "Expected Undo conflict error, got: {undo_err}"
    );
    assert!(expected_dest.exists());
    assert_eq!(std::fs::read(&clip).unwrap(), b"newly-created-blocker");
}

#[test]
fn test_undo_copy_blocks_when_original_offline_or_modified() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();

    let src = src_dir.join("master.mp4");
    std::fs::write(&src, vec![b'M'; 6144]).unwrap();

    let root = add_library_root(&db, &src_dir, Some("Src"), true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, None, None, &cancel, None).unwrap();
    let fkey = query_catalog(&db, &CatalogFilter::default()).unwrap().items[0]
        .file_key
        .clone();

    let prev = preview_batch_move_or_copy(
        &db,
        &[fkey],
        &dst_dir,
        Some("{filename}.{extension}"),
        true,
        None,
    )
    .unwrap();
    let copied_path = PathBuf::from(&prev.items[0].new_path);
    let exec = execute_batch_plan_by_id(&db, prev.plan_id.as_deref().unwrap(), "Test Copy").unwrap();
    assert_eq!(exec.succeeded, 1);
    assert!(copied_path.exists());

    // Delete original (offline / gone) -> Undo Copy must REFUSE to delete the copy!
    std::fs::remove_file(&src).unwrap();
    let res_offline = undo_operation(&db, &exec.operation_id).unwrap();
    assert_eq!(res_offline.succeeded, 0);
    assert_eq!(res_offline.failed, 1);
    assert!(
        res_offline.errors.iter().any(|e| e.contains("offline/missing") || e.contains("retaining")),
        "Expected offline original protection, got: {:?}",
        res_offline.errors
    );
    assert!(copied_path.exists(), "Copy must survive when original is offline");

    // Put DIFFERENT content of the SAME size at src -> BLAKE3 mismatch must also retain the copy!
    std::fs::write(&src, vec![b'Z'; 6144]).unwrap();
    let res_diff = undo_operation(&db, &exec.operation_id).unwrap();
    assert_eq!(res_diff.succeeded, 0);
    assert_eq!(res_diff.failed, 1);
    assert!(
        res_diff.errors.iter().any(|e| e.contains("BLAKE3") || e.contains("changed") || e.contains("retaining")),
        "Expected BLAKE3 mismatch protection, got: {:?}",
        res_diff.errors
    );
    assert!(copied_path.exists(), "Copy must survive when original content changed");
}

#[test]
fn test_non_destructive_restart_reconciliation_preserves_staging_files() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let stg_dir = dir.path().join(".ffq-staging").join("attempt-1");
    std::fs::create_dir_all(&stg_dir).unwrap();

    let partial = stg_dir.join("partial.mkv");
    let unexpected = stg_dir.join("unrelated_user_file.mov");
    std::fs::write(&partial, b"partial-encode").unwrap();
    std::fs::write(&unexpected, b"important-unrelated-data").unwrap();

    db.upsert_queue(&make_queue("q1", "Q1", TabType::Encode, 0))
        .unwrap();
    db.upsert_job(&make_job(
        "job-1",
        "q1",
        dir.path(),
        "job-1.mp4",
        &partial,
        JobStatus::Encoding,
    ))
    .unwrap();
    db.record_attempt_start(
        "attempt-1",
        "job-1",
        "q1",
        &stg_dir.to_string_lossy(),
        &partial.to_string_lossy(),
    )
    .unwrap();

    // Launch reconciliation via Scheduler::new
    let _sched = Scheduler::new(db);

    assert!(!partial.exists(), "Recorded partial file should be cleaned up");
    assert!(
        unexpected.exists(),
        "Unexpected file inside .ffq-staging MUST NOT be wiped by recursive remove_dir_all!"
    );
}
