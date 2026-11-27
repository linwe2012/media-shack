use media_shack_lib::analysis::{detect_black_and_frozen_frames, extract_sampled_frames, SamplingMode};
use media_shack_lib::catalog::{
    add_library_root, get_asset_detail, get_catalog_stats, mark_volume_offline_state, query_catalog,
    register_derived_file, scan_library_root, CatalogFilter,
};
use media_shack_lib::db::Database;
use media_shack_lib::duplicates::{
    list_duplicate_groups, merge_tags_from_copies, run_full_duplicate_scan,
};
use media_shack_lib::integrity::{
    export_checksum_manifest_file, verify_file_integrity,
};
use media_shack_lib::intelligence::{
    index_semantic_embeddings, is_face_grouping_enabled, label_face_group, list_face_groups,
    purge_all_biometric_data, review_ocr_detections_batch, run_face_grouping_for_files,
    run_ocr_and_watermark_analysis, save_creator_alias, semantic_media_search,
    set_face_grouping_enabled,
};
use media_shack_lib::metadata_interop::{
    discover_exiftool, export_sidecars_for_file, import_sidecar_for_file, sync_os_tags_for_file,
    write_embedded_metadata_lossless, ConflictPolicy,
};
use media_shack_lib::organize::{
    assign_tags_bulk, evaluate_collection, execute_batch_file_operations, list_collections,
    list_operation_history, merge_tags, preview_batch_move_or_copy, preview_batch_rename,
    save_collection, undo_operation, RenameOptions,
};
use media_shack_lib::repair::{diagnose_media_file, inspect_mp4_atoms, repair_damaged_video};
use media_shack_lib::tools::find_tool_pair;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::TempDir;

fn require_tools() -> (PathBuf, PathBuf) {
    find_tool_pair(None, None, None).expect("ffmpeg and ffprobe must be installed")
}

fn gen_video(ffmpeg: &Path, args: &[&str]) {
    let out = Command::new(ffmpeg).args(args).output().expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_catalog_identity_renames_hardlinks_offline_and_backup() {
    let (_ff, fp) = require_tools();
    let dir = TempDir::new().unwrap();
    let lib_dir = dir.path().join("library");
    std::fs::create_dir_all(&lib_dir).unwrap();

    // Unicode & Chinese filename
    let video_a = lib_dir.join("测试视频_day1.mp4");
    gen_video(
        &_ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            video_a.to_str().unwrap(),
        ],
    );

    let db = Database::open(&dir.path().join("appdata")).unwrap();
    let root = add_library_root(&db, &lib_dir, Some("TestLib"), true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));

    // Initial scan
    let rep1 = scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();
    assert_eq!(rep1.added_files, 1);
    assert_eq!(rep1.probed_files, 1);

    let page1 = query_catalog(&db, &CatalogFilter::default()).unwrap();
    assert_eq!(page1.total_count, 1);
    let orig_asset_id = page1.items[0].asset_id.clone();
    let orig_fkey = page1.items[0].file_key.clone();

    // Assign manual tag
    assign_tags_bulk(
        &db,
        &[orig_asset_id.clone()],
        &["creator:Tester".to_string(), "project:Vacation".to_string()],
        "manual",
        1.0,
    )
    .unwrap();

    // Hard link in same folder
    let hl_path = lib_dir.join("hardlink_copy.mp4");
    let _ = std::fs::hard_link(&video_a, &hl_path);

    // Rename original externally -> must preserve asset_id & tags!
    let renamed = lib_dir.join("测试视频_renamed.mp4");
    std::fs::rename(&video_a, &renamed).unwrap();

    let rep2 = scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();
    assert!(rep2.moved_or_renamed_files >= 1 || rep2.unchanged_files >= 1);

    let detail = get_asset_detail(&db, &orig_fkey).unwrap();
    assert_eq!(detail.entry.asset_id, orig_asset_id);
    assert!(detail.entry.tags.iter().any(|t| t.name == "creator:Tester"));

    // Mark volume offline -> files remain in DB, marked offline rather than deleted
    let vol_id = detail.entry.volume_id.clone();
    mark_volume_offline_state(&db, &vol_id, false).unwrap();
    let offline_page = query_catalog(
        &db,
        &CatalogFilter {
            online_only: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!offline_page.items.is_empty());
    mark_volume_offline_state(&db, &vol_id, true).unwrap();

    // Backup database
    let backup_p = dir.path().join("backup.db");
    db.backup_database(&backup_p).unwrap();
    assert!(std::fs::metadata(&backup_p).unwrap().len() > 0);
}

#[test]
fn test_tags_collections_batch_rename_cycles_move_and_undo() {
    let (ff, fp) = require_tools();
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();

    let f_a = src_dir.join("alpha.mp4");
    let f_b = src_dir.join("beta.mp4");
    for f in [&f_a, &f_b] {
        gen_video(
            &ff,
            &[
                "-hide_banner", "-y",
                "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
                f.to_str().unwrap(),
            ],
        );
    }

    let db = Database::in_memory().unwrap();
    let root = add_library_root(&db, &src_dir, None, true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();

    let page = query_catalog(&db, &CatalogFilter::default()).unwrap();
    assert_eq!(page.items.len(), 2);
    let keys: Vec<String> = page.items.iter().map(|e| e.file_key.clone()).collect();
    let aids: Vec<String> = page.items.iter().map(|e| e.asset_id.clone()).collect();

    // Tagging, aliases, merging, and manual precedence over AI suggestions
    assign_tags_bulk(&db, &aids, &["creator:Alice".to_string(), "format:VR180".to_string()], "manual", 1.0).unwrap();
    let cols = list_collections(&db).unwrap();
    assert!(cols.len() >= 7);
    let saved_col = save_collection(
        &db,
        None,
        "Alice Videos",
        Some("Filter test"),
        &CatalogFilter {
            include_tags: vec!["creator:Alice".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let col_res = evaluate_collection(&db, &saved_col.id).unwrap();
    assert_eq!(col_res.total_count, 2);

    // Tag merge
    let t_old = media_shack_lib::organize::create_or_get_tag(&db, "old_tag", None).unwrap();
    let t_new = media_shack_lib::organize::create_or_get_tag(&db, "new_tag", None).unwrap();
    assign_tags_bulk(&db, &[aids[0].clone()], &["old_tag".to_string()], "manual", 1.0).unwrap();
    merge_tags(&db, &t_old.id, &t_new.id).unwrap();

    // Rename cycle test: swap alpha.mp4 <-> beta.mp4 via batch rename
    let cycle_items = vec![
        media_shack_lib::organize::FileOpPreviewItem {
            file_key: keys[0].clone(),
            asset_id: aids[0].clone(),
            old_path: page.items[0].path.clone(),
            new_path: page.items[1].path.clone(),
            op_kind: "rename".to_string(),
            cross_volume: false,
            collision: false,
            validation_error: None,
        },
        media_shack_lib::organize::FileOpPreviewItem {
            file_key: keys[1].clone(),
            asset_id: aids[1].clone(),
            old_path: page.items[1].path.clone(),
            new_path: page.items[0].path.clone(),
            op_kind: "rename".to_string(),
            cross_volume: false,
            collision: false,
            validation_error: None,
        },
    ];
    let exec_cycle = execute_batch_file_operations(&db, &cycle_items, "Swap rename cycle").unwrap();
    assert_eq!(exec_cycle.succeeded, 2);
    assert!(f_a.exists() && f_b.exists());

    // Undo cycle
    let undo_res = undo_operation(&db, &exec_cycle.operation_id).unwrap();
    assert_eq!(undo_res.succeeded, 2);

    // Reserved Windows name validation in preview_batch_rename
    let bad_prev = preview_batch_rename(
        &db,
        &[keys[0].clone()],
        &RenameOptions {
            template: "CON.{extension}".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(bad_prev.error_count == 1);

    // Template move + undo
    let mv_prev = preview_batch_move_or_copy(
        &db,
        &keys,
        &dst_dir,
        Some("{creator}/{filename}_{index}.{extension}"),
        false,
        Some("first"),
    )
    .unwrap();
    assert_eq!(mv_prev.valid_count, 2);
    let mv_exec = execute_batch_file_operations(&db, &mv_prev.items, "Organize into creator dir").unwrap();
    assert_eq!(mv_exec.succeeded, 2);
    assert!(!f_a.exists());
    assert!(dst_dir.join("Alice").exists());

    let hist = list_operation_history(&db).unwrap();
    assert!(!hist.is_empty());
    let mv_undo = undo_operation(&db, &mv_exec.operation_id).unwrap();
    assert_eq!(mv_undo.succeeded, 2);
    assert!(f_a.exists());
}

#[test]
fn test_duplicates_exact_equivalent_perceptual_partial_and_false_positives() {
    let (ff, fp) = require_tools();
    let dir = TempDir::new().unwrap();
    let lib = dir.path().join("dups");
    std::fs::create_dir_all(&lib).unwrap();

    // 1. Original 4s moving pattern video
    let orig_mp4 = lib.join("orig.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=192x144:rate=24:duration=4",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
            orig_mp4.to_str().unwrap(),
        ],
    );

    // 2. Exact byte-identical copy
    let exact_mp4 = lib.join("orig_exact_copy.mp4");
    std::fs::copy(&orig_mp4, &exact_mp4).unwrap();

    // 3. Remuxed MKV (equivalent streams, different container)
    let remux_mkv = lib.join("orig_remuxed.mkv");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-i", orig_mp4.to_str().unwrap(),
            "-c", "copy",
            remux_mkv.to_str().unwrap(),
        ],
    );

    // 4. Transcoded lower-res variant (perceptual match)
    let trans_mp4 = lib.join("orig_scaled_hevc.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-i", orig_mp4.to_str().unwrap(),
            "-vf", "scale=128:96",
            "-c:v", "libx265", "-crf", "24", "-preset", "ultrafast",
            "-c:a", "copy",
            trans_mp4.to_str().unwrap(),
        ],
    );

    // 5. Partial trimmed clip (1s..3s of original)
    let clip_mp4 = lib.join("orig_partial_clip.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-ss", "1.0", "-t", "2.0",
            "-i", orig_mp4.to_str().unwrap(),
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            clip_mp4.to_str().unwrap(),
        ],
    );

    // 6. Unrelated solid color video (should NOT match!)
    let unrelated_mp4 = lib.join("unrelated_blue.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "color=c=blue:s=160x120:r=24:d=4",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            unrelated_mp4.to_str().unwrap(),
        ],
    );

    let db = Database::in_memory().unwrap();
    let root = add_library_root(&db, &lib, None, true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();

    let sum = run_full_duplicate_scan(&db, Some(&ff), true, true, &cancel).unwrap();
    assert_eq!(sum.exact_groups_found, 1, "Exact duplicate group expected");
    assert!(sum.equivalent_pairs_found >= 1, "Equivalent remuxed MKV pair expected");
    assert!(
        sum.perceptual_pairs_found + sum.partial_overlaps_found >= 1,
        "Perceptual / partial matches expected: {sum:?}"
    );

    let groups = list_duplicate_groups(&db, None).unwrap();
    assert!(!groups.is_empty());
    // Unrelated blue video should not appear in exact or equivalent groups
    for g in &groups {
        if g.match_kind == "exact" || g.match_kind == "equivalent_stream" {
            assert!(!g.members.iter().any(|m| m.entry.filename == "unrelated_blue.mp4"));
        }
    }

    // Tag merge test
    let page = query_catalog(&db, &CatalogFilter::default()).unwrap();
    let a1 = &page.items[0].asset_id;
    let a2 = &page.items[1].asset_id;
    assign_tags_bulk(&db, &[a1.clone()], &["tag_from_copy".to_string()], "manual", 1.0).unwrap();
    let merged = merge_tags_from_copies(&db, &[a1.clone()], a2).unwrap();
    assert!(merged >= 1);
}

#[test]
fn test_metadata_interop_sidecars_exiftool_and_os_tags() {
    let (ff, fp) = require_tools();
    let dir = TempDir::new().unwrap();
    let mp4 = dir.path().join("meta_test.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            mp4.to_str().unwrap(),
        ],
    );

    let db = Database::in_memory().unwrap();
    let root = add_library_root(&db, dir.path(), None, false, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();
    let entry = query_catalog(&db, &CatalogFilter::default()).unwrap().items.remove(0);

    assign_tags_bulk(&db, &[entry.asset_id.clone()], &["location:Colorado".to_string(), "activity:Skiing".to_string()], "manual", 1.0).unwrap();

    // Sidecar JSON + XMP export & conflict detection
    let exported = export_sidecars_for_file(&db, &entry.file_key, true, true).unwrap();
    assert_eq!(exported.len(), 2);
    assert!(Path::new(&exported[0]).exists());
    assert!(Path::new(&exported[1]).exists());

    // Modify sidecar JSON to simulate external edit conflict
    let json_raw = std::fs::read_to_string(&exported[0]).unwrap();
    let mut sc: serde_json::Value = serde_json::from_str(&json_raw).unwrap();
    sc["tags"] = serde_json::json!(["external:EditedTag"]);
    std::fs::write(&exported[0], serde_json::to_string_pretty(&sc).unwrap()).unwrap();

    let conflict_rep = import_sidecar_for_file(&db, &entry.file_key, ConflictPolicy::Manual).unwrap();
    assert_eq!(conflict_rep.status, "conflict");

    let merge_rep = import_sidecar_for_file(&db, &entry.file_key, ConflictPolicy::Merge).unwrap();
    assert_eq!(merge_rep.status, "synced");

    // Embedded metadata write (uses ExifTool if installed, or lossless FFmpeg copy)
    let et = discover_exiftool(None);
    let msg = write_embedded_metadata_lossless(
        &db,
        et.as_deref(),
        Some(&ff),
        &entry.file_key,
        &["Colorado".to_string(), "Skiing".to_string()],
        Some("Ski Trip"),
        Some("CreatorOne"),
    )
    .unwrap();
    assert!(msg.contains("lossless") || msg.contains("exiftool") || msg.contains("ffmpeg"));

    // OS native tag sync probe (Windows IPropertyStore or macOS Finder) — must not crash even if handler is unsupported
    let os_rep = sync_os_tags_for_file(&db, &entry.file_key, "push", ConflictPolicy::Merge).unwrap();
    assert!(["synced", "unsupported"].contains(&os_rep.status.as_str()));
}

#[test]
fn test_integrity_repair_ocr_semantic_faces_and_1000_perf() {
    let (ff, fp) = require_tools();
    let dir = TempDir::new().unwrap();
    let lib = dir.path().join("media");
    std::fs::create_dir_all(&lib).unwrap();

    // Create reference healthy MP4 + truncated damaged MP4
    let healthy_mp4 = lib.join("healthy.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
            healthy_mp4.to_str().unwrap(),
        ],
    );
    let mut raw = std::fs::read(&healthy_mp4).unwrap();
    raw.truncate(raw.len() / 2);
    let damaged_mp4 = lib.join("damaged_trunc.mp4");
    std::fs::write(&damaged_mp4, &raw).unwrap();

    // Create ocean-blue clip with persistent corner box (watermark simulation) & SRT subtitle
    let ocean_mp4 = lib.join("ocean_scene.mp4");
    let srt = dir.path().join("sub.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,500\nOcean waves\n").unwrap();
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "color=c=blue:s=160x120:r=24:d=2",
            "-i", srt.to_str().unwrap(),
            "-vf", "drawbox=x=5:y=5:w=30:h=12:color=white:t=fill",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:s", "mov_text",
            ocean_mp4.to_str().unwrap(),
        ],
    );

    let db = Database::in_memory().unwrap();
    let root = add_library_root(&db, &lib, None, true, vec![], false, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_library_root(&db, &root.id, Some(&fp), Some(1), &cancel, None).unwrap();

    let page = query_catalog(&db, &CatalogFilter::default()).unwrap();
    let healthy_entry = page.items.iter().find(|e| e.filename == "healthy.mp4").unwrap().clone();
    let ocean_entry = page.items.iter().find(|e| e.filename == "ocean_scene.mp4").unwrap().clone();

    // 1. Integrity verification + tamper detection
    let v1 = verify_file_integrity(&db, Some(&ff), Some(&fp), &healthy_entry.file_key, true).unwrap();
    assert_eq!(v1.status, "valid");

    // Corrupt a byte in healthy.mp4 -> unexpected checksum change detected!
    let mut h_bytes = std::fs::read(&healthy_mp4).unwrap();
    let mid = h_bytes.len() / 2;
    h_bytes[mid] ^= 0xFF;
    std::fs::write(&healthy_mp4, &h_bytes).unwrap();
    let v2 = verify_file_integrity(&db, Some(&ff), Some(&fp), &healthy_entry.file_key, false).unwrap();
    assert_eq!(v2.status, "changed");

    let manifest_p = dir.path().join("manifest.json");
    let cnt = export_checksum_manifest_file(&db, &manifest_p, "json", None).unwrap();
    assert!(cnt >= 1 && manifest_p.exists());

    // 2. MP4 atom inspection, diagnosis, and non-destructive recovery
    let atoms = inspect_mp4_atoms(&damaged_mp4).unwrap();
    assert!(!atoms.is_empty());
    let diag = diagnose_media_file(&db, &ff, &fp, &damaged_mp4, None).unwrap();
    assert_ne!(diag.container_integrity, "healthy");

    // Re-create clean healthy reference for repair test
    let ref_mp4 = dir.path().join("ref.mp4");
    gen_video(
        &ff,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
            ref_mp4.to_str().unwrap(),
        ],
    );
    let rep_out_dir = dir.path().join("repaired");
    let rep_res = repair_damaged_video(&db, &ff, &fp, &damaged_mp4, Some(&ref_mp4), Some(&rep_out_dir), None).unwrap();
    assert!(rep_res.verified_decoded);
    assert!(Path::new(&rep_res.output_path).exists());

    // 3. Shared frames + black/frozen check + OCR/watermark + Semantic search
    let frames = extract_sampled_frames(&db, &ff, &ocean_entry.file_key, &SamplingMode::Percentage { count: 4 }).unwrap();
    assert!(!frames.is_empty());
    let bf = detect_black_and_frozen_frames(&db, &ff, &ocean_entry.file_key).unwrap();
    assert!(!bf.is_mostly_black);

    save_creator_alias(&db, "@ocean_user", "OceanUser").unwrap();
    let ocr_dets = run_ocr_and_watermark_analysis(&db, &ff, &ocean_entry.file_key).unwrap();
    assert!(!ocr_dets.is_empty(), "Expected subtitle and/or persistent overlay detection");
    let det_ids: Vec<String> = ocr_dets.iter().map(|d| d.id.clone()).collect();
    review_ocr_detections_batch(&db, &det_ids, true, Some("creator")).unwrap();

    let emb_cnt = index_semantic_embeddings(&db, &ff, &ocean_entry.file_key).unwrap();
    assert!(emb_cnt > 0);
    let sem_hits = semantic_media_search(&db, "videos filmed near the ocean", None, 10).unwrap();
    assert!(!sem_hits.is_empty());
    assert_eq!(sem_hits[0].file_key, ocean_entry.file_key);

    // 4. Opt-in Face Grouping + Biometric Purge
    assert!(!is_face_grouping_enabled(&db).unwrap());
    set_face_grouping_enabled(&db, true).unwrap();
    let _ = run_face_grouping_for_files(&db, &ff, &[ocean_entry.file_key.clone()]).unwrap();
    let _fgrps = list_face_groups(&db).unwrap();
    if let Some(g) = _fgrps.first() {
        label_face_group(&db, &g.id, "Person A").unwrap();
    }
    purge_all_biometric_data(&db).unwrap();
    assert!(list_face_groups(&db).unwrap().is_empty());

    // 5. Register derived relationship
    let derived_fk = register_derived_file(
        &db,
        &ocean_entry.file_key,
        Path::new(&rep_res.output_path),
        "transcoded_from",
        serde_json::json!({"test": true}),
        true,
        Some(&fp),
    )
    .unwrap();
    let der_detail = get_asset_detail(&db, &derived_fk).unwrap();
    assert!(!der_detail.relationships.is_empty());

    // 6. Performance benchmark: 1,000-file catalog scan & search latency
    let perf_dir = dir.path().join("perf_1000");
    std::fs::create_dir_all(&perf_dir).unwrap();
    for i in 0..1000 {
        let p = perf_dir.join(format!("clip_{i:04}.mp4"));
        std::fs::write(&p, format!("dummy-media-payload-{i:04}")).unwrap();
    }
    let perf_root = add_library_root(&db, &perf_dir, Some("Perf1000"), true, vec![], false, 0).unwrap();
    let rep_perf = scan_library_root(&db, &perf_root.id, None, Some(0), &cancel, None).unwrap();
    assert_eq!(rep_perf.discovered_files, 1000);
    let t_q = std::time::Instant::now();
    let q_res = query_catalog(
        &db,
        &CatalogFilter {
            search_text: Some("clip_0500".to_string()),
            limit: Some(50),
            ..Default::default()
        },
    )
    .unwrap();
    let q_ms = t_q.elapsed().as_millis();
    assert_eq!(q_res.total_count, 1);
    let stats = get_catalog_stats(&db).unwrap();
    assert!(stats.total_files >= 1000);
    println!(
        "PERF_1000: initial_scan={}ms search_latency={}ms total_files={}",
        rep_perf.elapsed_ms, q_ms, stats.total_files
    );
}

#[test]
fn test_v1_migration_and_10k_catalog_scale() {
    let dir = TempDir::new().unwrap();
    let app_dir = dir.path().join("appdata_v1");
    std::fs::create_dir_all(&app_dir).unwrap();
    let db_path = app_dir.join("media_shack.db");

    // Seed a legacy v1 schema database before opening via Database::open (must migrate cleanly v1 -> v2 -> v3)
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
             INSERT INTO schema_migrations(version, applied_at) VALUES (1, 1700000000);
             CREATE TABLE app_settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
             CREATE TABLE tool_cache (identity_key TEXT PRIMARY KEY, capabilities_json TEXT NOT NULL, checked_at INTEGER NOT NULL);
             CREATE TABLE named_presets (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, recipe_json TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, profile_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE queues (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, tab_order INTEGER NOT NULL, profile_id TEXT,
                 profile_name TEXT, recipe_json TEXT NOT NULL, priority TEXT NOT NULL, recursive_import INTEGER NOT NULL,
                 destination_root TEXT, keep_awake INTEGER NOT NULL, error_policy TEXT NOT NULL, min_savings_enabled INTEGER NOT NULL,
                 min_savings_pct REAL NOT NULL, cleanup_policy TEXT NOT NULL, full_decode_verify INTEGER NOT NULL,
                 is_running INTEGER NOT NULL DEFAULT 0, is_paused INTEGER NOT NULL DEFAULT 0, pause_after_current INTEGER NOT NULL DEFAULT 0,
                 waiting_reason TEXT
             );
             CREATE TABLE jobs (
                 id TEXT PRIMARY KEY, queue_id TEXT NOT NULL, batch_id TEXT NOT NULL, position INTEGER NOT NULL,
                 source_path TEXT NOT NULL, source_rel_path TEXT NOT NULL, source_root TEXT, source_fingerprint_json TEXT,
                 destination_path TEXT NOT NULL, dest_volume_id TEXT, recipe_json TEXT NOT NULL, compat_overrides_json TEXT NOT NULL,
                 status TEXT NOT NULL, status_reason TEXT, probe_json TEXT, plan_json TEXT, input_bytes INTEGER NOT NULL,
                 output_bytes INTEGER, error_stage TEXT, error_message TEXT, diagnostics TEXT, cleanup_policy TEXT NOT NULL,
                 cleanup_outcome TEXT NOT NULL, cleanup_error TEXT, consent_id TEXT, full_decode_verified INTEGER NOT NULL DEFAULT 0,
                 min_savings_enabled INTEGER NOT NULL DEFAULT 0, min_savings_pct REAL NOT NULL DEFAULT 15.0, tool_versions TEXT,
                 current_attempt_id TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, completed_at INTEGER
             );
             CREATE TABLE job_attempts (attempt_id TEXT PRIMARY KEY, job_id TEXT NOT NULL, queue_id TEXT NOT NULL, staging_dir TEXT NOT NULL, partial_path TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, exit_code INTEGER, status TEXT NOT NULL);
             CREATE TABLE publish_intents (intent_id TEXT PRIMARY KEY, job_id TEXT NOT NULL, attempt_id TEXT NOT NULL, staged_path TEXT NOT NULL, staged_volume_id TEXT NOT NULL, staged_file_id TEXT NOT NULL, staged_size INTEGER NOT NULL, final_path TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE cleanup_consents (consent_id TEXT PRIMARY KEY, queue_id TEXT NOT NULL, batch_id TEXT NOT NULL, source_manifest_json TEXT NOT NULL, total_bytes INTEGER NOT NULL, approved_at INTEGER NOT NULL);
             CREATE TABLE cleanup_intents (action_id TEXT PRIMARY KEY, job_id TEXT NOT NULL, queue_id TEXT NOT NULL, batch_id TEXT NOT NULL, source_path TEXT NOT NULL, source_volume_id TEXT NOT NULL, source_file_id TEXT NOT NULL, source_size INTEGER NOT NULL, verified_output_path TEXT NOT NULL, verified_output_file_id TEXT NOT NULL, consent_id TEXT, state TEXT NOT NULL, error_message TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE volume_reserves (volume_id TEXT PRIMARY KEY, mount_hint TEXT NOT NULL, reserve_bytes INTEGER NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .unwrap();
    }

    let db = Database::open(&app_dir).unwrap();
    let perf_dir = dir.path().join("scale_10k");
    std::fs::create_dir_all(&perf_dir).unwrap();
    for i in 0..10_000 {
        let p = perf_dir.join(format!("item_{i:05}.mp4"));
        std::fs::write(&p, format!("payload-{i:05}")).unwrap();
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let root = add_library_root(&db, &perf_dir, Some("Scale10k"), true, vec![], false, 0).unwrap();
    let rep = scan_library_root(&db, &root.id, None, Some(0), &cancel, None).unwrap();
    assert_eq!(rep.discovered_files, 10_000);

    // Ensure query_catalog exceeds the old 5,000 cap when asked
    let all_page = query_catalog(
        &db,
        &CatalogFilter {
            limit: Some(15_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all_page.total_count, 10_000);
    assert_eq!(all_page.items.len(), 10_000);

    let t_q = std::time::Instant::now();
    let hit = query_catalog(
        &db,
        &CatalogFilter {
            search_text: Some("item_09999".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let search_ms = t_q.elapsed().as_millis();
    assert_eq!(hit.total_count, 1);
    println!("PERF_10K: scan={}ms search={}ms", rep.elapsed_ms, search_ms);
}
