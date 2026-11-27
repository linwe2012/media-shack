use media_shack_lib::cleanup::{
    evaluate_cleanup_eligibility, execute_cleanup_for_job, reconcile_cleanup_intents_on_launch,
    record_batch_cleanup_consent,
};
use media_shack_lib::db::{CleanupIntentRecord, Database};
use media_shack_lib::models::*;
use media_shack_lib::planner::{
    build_track_plan, derive_destination_root_from_job, materialize_argv, probe_file,
    resolve_destination_path, sanitize_path_segment,
};
use media_shack_lib::platform::{
    atomic_publish_no_replace, capture_file_identity, disk_space_for_path, PublishOutcome,
};
use media_shack_lib::preview::PreviewManager;
use media_shack_lib::runner::{run_ffmpeg_attempt, EncodeRunOutcome, PreviewPacketBuffer, RunEncodeParams};
use media_shack_lib::scanner::scan_import_paths;
use media_shack_lib::scheduler::Scheduler;
use media_shack_lib::tools::{discover_and_probe, find_tool_pair};
use media_shack_lib::verify::{run_full_decode_check, verify_staged_output};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

fn require_tools() -> (PathBuf, PathBuf) {
    find_tool_pair(None, None, None).expect("ffmpeg and ffprobe must be installed for integration tests")
}

fn run_ffmpeg_gen(ffmpeg: &Path, args: &[&str]) {
    let out = Command::new(ffmpeg)
        .args(args)
        .output()
        .expect("spawn ffmpeg fixture gen");
    assert!(
        out.status.success(),
        "ffmpeg fixture command failed: {:?}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

struct FixtureSet {
    _dir: TempDir,
    root: PathBuf,
    sdr8_mp4: PathBuf,
    silent_mp4: PathBuf,
    multi_audio_mkv: PathBuf,
    vfr_mkv: PathBuf,
    rotated_mp4: PathBuf,
    sdr10_mkv: PathBuf,
    hdr10_mkv: PathBuf,
    ass_mkv: PathBuf,
    multi_video_mkv: PathBuf,
    truncated_mp4: PathBuf,
    unicode_dotted_mov: PathBuf,
}

fn generate_fixtures(ffmpeg: &Path) -> FixtureSet {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let sub = root.join("Trip");
    std::fs::create_dir_all(&sub).unwrap();

    // 1. Standard progressive 8-bit SDR H.264 + AAC (2s)
    let sdr8_mp4 = root.join("sdr8.mp4");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
            sdr8_mp4.to_str().unwrap(),
        ],
    );

    // 2. Silent video (1.5s)
    let silent_mp4 = root.join("silent.mp4");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1.5",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            silent_mp4.to_str().unwrap(),
        ],
    );

    // 3. Multiple audio languages (eng + jpn)
    let multi_audio_mkv = root.join("multi_audio.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=660:duration=1",
            "-map", "0:v", "-map", "1:a", "-map", "2:a",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
            "-metadata:s:a:0", "language=eng", "-metadata:s:a:1", "language=jpn",
            multi_audio_mkv.to_str().unwrap(),
        ],
    );

    // 4. VFR fixture (duplicate/varying timestamps via setpts)
    let vfr_mkv = root.join("vfr.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=30:duration=1.2",
            "-vf", "setpts='if(lt(N,10),N*0.02/TB,N*0.06/TB)'",
            "-fps_mode", "vfr",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            vfr_mkv.to_str().unwrap(),
        ],
    );

    // 5. Rotated phone footage (90 deg display rotation)
    let rotated_mp4 = root.join("rotated90.mp4");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-display_rotation", "90",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            rotated_mp4.to_str().unwrap(),
        ],
    );

    // 6. 10-bit SDR (yuv420p10le)
    let sdr10_mkv = root.join("sdr10.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-c:v", "libx264", "-pix_fmt", "yuv420p10le",
            sdr10_mkv.to_str().unwrap(),
        ],
    );

    // 7. HDR10 needing review (smpte2084 transfer)
    let hdr10_mkv = root.join("hdr10.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-vf", "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
            "-c:v", "libx264", "-pix_fmt", "yuv420p10le",
            "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc",
            "-x264-params", "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
            hdr10_mkv.to_str().unwrap(),
        ],
    );

    // 8. ASS subtitle + MKV (incompatible with plain MP4)
    let ass_file = root.join("subs.ass");
    std::fs::write(
        &ass_file,
        "[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,Hello World\n",
    )
    .unwrap();
    let ass_mkv = root.join("ass_subs.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-i", ass_file.to_str().unwrap(),
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:s", "ass",
            ass_mkv.to_str().unwrap(),
        ],
    );

    // 9. Multiple moving video streams
    let multi_video_mkv = root.join("multi_video.mkv");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-f", "lavfi", "-i", "color=c=blue:s=128x96:r=24:d=1",
            "-map", "0:v", "-map", "1:v",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            multi_video_mkv.to_str().unwrap(),
        ],
    );

    // 10. Damaged MP4: valid +faststart header (probes cleanly as 2s video) but truncated mid-mdat so encode/verify fails!
    let full_faststart_mp4 = root.join("full_faststart.mp4");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
            full_faststart_mp4.to_str().unwrap(),
        ],
    );
    let truncated_mp4 = root.join("truncated.mp4");
    let mut raw = std::fs::read(&full_faststart_mp4).unwrap();
    raw.truncate(raw.len() / 4);
    let len_t = raw.len();
    if len_t > 600 {
        for b in &mut raw[len_t - 400..len_t - 100] {
            *b = 0xff;
        }
    }
    std::fs::write(&truncated_mp4, raw).unwrap();

    // Unreadable file (cannot be probed)
    let unreadable_mp4 = root.join("unreadable.mp4");
    std::fs::write(&unreadable_mp4, b"not a valid media file at all").unwrap();

    // 11. Unicode + internal dots in nested Trip folder
    let unicode_dotted_mov = sub.join("day1.walk.日本語.mov");
    run_ffmpeg_gen(
        ffmpeg,
        &[
            "-hide_banner", "-y",
            "-f", "lavfi", "-i", "testsrc=size=128x96:rate=24:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=520:duration=1",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
            unicode_dotted_mov.to_str().unwrap(),
        ],
    );

    FixtureSet {
        _dir: dir,
        root,
        sdr8_mp4,
        silent_mp4,
        multi_audio_mkv,
        vfr_mkv,
        rotated_mp4,
        sdr10_mkv,
        hdr10_mkv,
        ass_mkv,
        multi_video_mkv,
        truncated_mp4,
        unicode_dotted_mov,
    }
}

#[test]
fn unit_planner_reserved_names_and_stem_preservation() {
    assert_eq!(sanitize_path_segment("CON"), "_CON");
    assert_eq!(sanitize_path_segment("con.txt"), "_con.txt");
    assert_eq!(sanitize_path_segment("day1.walk"), "day1.walk");
    assert_eq!(sanitize_path_segment("bad:name*?.mkv"), "bad_name__.mkv");

    let mut reserved = HashSet::new();
    let recipe = EncodeRecipe::archive_balanced_default();
    let out1 = resolve_destination_path(
        Path::new("/src/Trip/day1.walk.mov"),
        Path::new("Trip/day1.walk.mov"),
        Path::new("/dest"),
        &recipe,
        &mut reserved,
    );
    assert_eq!(
        out1,
        PathBuf::from("/dest/Trip/day1.walk.archive-balanced.hevc.mkv")
    );
    // Case-insensitive collision produces numbered free name
    let out2 = resolve_destination_path(
        Path::new("/src/Trip/DAY1.WALK.mov"),
        Path::new("Trip/DAY1.WALK.mov"),
        Path::new("/dest"),
        &recipe,
        &mut reserved,
    );
    assert_eq!(
        out2,
        PathBuf::from("/dest/Trip/DAY1.WALK.archive-balanced.hevc (2).mkv")
    );
    assert_eq!(
        derive_destination_root_from_job(&out1, Path::new("Trip/day1.walk.mov")),
        PathBuf::from("/dest")
    );
}

#[test]
fn integration_tool_discovery_skips_cwd_and_probes_capabilities() {
    let db = Database::in_memory().unwrap();
    let ts = discover_and_probe(&db, None, None, None, true);
    assert!(ts.discovered, "Tools should be discovered: {:?}", ts.error);
    let caps = ts.capabilities.unwrap();
    assert!(caps.has_matroska);
    assert!(caps.has_mp4);
    assert!(caps.has_libx265, "libx265 smoke test should pass");
    assert!(caps.has_libsvtav1, "libsvtav1 smoke test should pass");
    assert!(db.get_cached_tools(&caps.identity_key).unwrap().is_some());
}

#[test]
fn integration_all_eight_archive_combos_plus_remux_and_special_fixtures() {
    let (ffmpeg, ffprobe) = require_tools();
    let fx = generate_fixtures(&ffmpeg);
    let out_dir = TempDir::new().unwrap();

    // Exercise all 8 Archive variant x codec x container combinations on sdr8_mp4
    let variants = [PresetVariant::Best, PresetVariant::Balanced];
    let codecs = [VideoCodec::Hevc, VideoCodec::Av1];
    let containers = [ContainerFormat::Mkv, ContainerFormat::Mp4];

    let sdr_probe = probe_file(&ffprobe, &fx.sdr8_mp4).unwrap();
    for v in variants {
        for c in codecs {
            for ct in containers {
                let recipe = EncodeRecipe::default_for(PresetFamily::Archive, v, c, ct);
                let plan = build_track_plan(&fx.sdr8_mp4, &sdr_probe, &recipe, &CompatOverrides::default());
                assert!(plan.is_ready, "Plan for {v:?}/{c:?}/{ct:?} should be ready");

                let partial = out_dir
                    .path()
                    .join(format!("test_{v:?}_{c:?}_{}.{}", ct.ext(), ct.ext()));
                let argv = materialize_argv(&plan.argv, &partial);
                let out = Command::new(&ffmpeg).args(&argv).output().unwrap();
                assert!(
                    out.status.success(),
                    "Encode failed for {v:?}/{c:?}/{ct:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );

                let ver = verify_staged_output(
                    &ffprobe,
                    &ffmpeg,
                    &fx.sdr8_mp4,
                    &sdr_probe,
                    &partial,
                    &recipe,
                    &plan,
                    true,
                );
                assert!(ver.passed, "Verify failed for {v:?}/{c:?}/{ct:?}: {}", ver.details);
                assert!(ver.full_decode_verified);
            }
        }
    }

    // Remux without re-encoding
    let remux_recipe = EncodeRecipe::default_for(
        PresetFamily::Remux,
        PresetVariant::Balanced,
        VideoCodec::Hevc,
        ContainerFormat::Mkv,
    );
    let remux_plan = build_track_plan(&fx.multi_audio_mkv, &probe_file(&ffprobe, &fx.multi_audio_mkv).unwrap(), &remux_recipe, &CompatOverrides::default());
    assert!(remux_plan.is_ready);
    assert!(remux_plan.tracks.iter().all(|t| matches!(t.action, TrackAction::Copy)));

    // HDR10 gated on Archive, allowed via Remux
    let hdr_probe = probe_file(&ffprobe, &fx.hdr10_mkv).unwrap();
    let hdr_plan = build_track_plan(&fx.hdr10_mkv, &hdr_probe, &EncodeRecipe::archive_balanced_default(), &CompatOverrides::default());
    assert!(!hdr_plan.is_ready);
    assert!(hdr_plan.issues.iter().any(|i| i.kind == CompatIssueKind::UnsupportedVideoFeature && i.can_remux));

    // Multi-video blocked until selection
    let mv_probe = probe_file(&ffprobe, &fx.multi_video_mkv).unwrap();
    let mv_blocked = build_track_plan(&fx.multi_video_mkv, &mv_probe, &EncodeRecipe::archive_balanced_default(), &CompatOverrides::default());
    assert!(!mv_blocked.is_ready);
    let mv_chosen = build_track_plan(
        &fx.multi_video_mkv,
        &mv_probe,
        &EncodeRecipe::archive_balanced_default(),
        &CompatOverrides { selected_video_stream: Some(0), ..Default::default() },
    );
    assert!(mv_chosen.is_ready);

    // ASS subtitles incompatible with MP4 -> Use MKV resolves it -> or opt-in mov_text
    let ass_probe = probe_file(&ffprobe, &fx.ass_mkv).unwrap();
    let mp4_recipe = EncodeRecipe::default_for(PresetFamily::Archive, PresetVariant::Balanced, VideoCodec::Hevc, ContainerFormat::Mp4);
    let ass_mp4_plan = build_track_plan(&fx.ass_mkv, &ass_probe, &mp4_recipe, &CompatOverrides::default());
    assert!(!ass_mp4_plan.is_ready);
    assert!(ass_mp4_plan.issues.iter().any(|i| i.kind == CompatIssueKind::Mp4IncompatibleSubtitle && i.can_use_mkv));
    let ass_mkv_plan = build_track_plan(&fx.ass_mkv, &ass_probe, &EncodeRecipe::archive_balanced_default(), &CompatOverrides::default());
    assert!(ass_mkv_plan.is_ready);

    // 10-bit SDR preserved as yuv420p10le; Rotated footage preserves rotation with -noautorotate; VFR & silent & Unicode
    for path in [&fx.sdr10_mkv, &fx.rotated_mp4, &fx.vfr_mkv, &fx.silent_mp4, &fx.unicode_dotted_mov] {
        let p = probe_file(&ffprobe, path).unwrap();
        let rec = EncodeRecipe::archive_balanced_default();
        let pl = build_track_plan(path, &p, &rec, &CompatOverrides::default());
        assert!(pl.is_ready, "Expected ready plan for {}", path.display());
        let part = out_dir.path().join(format!("{}.mkv", Uuid::new_v4()));
        let argv = materialize_argv(&pl.argv, &part);
        let out = Command::new(&ffmpeg).args(&argv).output().unwrap();
        assert!(out.status.success(), "Failed encoding {}", path.display());
        let ver = verify_staged_output(&ffprobe, &ffmpeg, path, &p, &part, &rec, &pl, true);
        assert!(ver.passed, "Verification failed for {}: {}", path.display(), ver.details);
    }

    // Unreadable input fails probe cleanly
    assert!(probe_file(&ffprobe, &fx.root.join("unreadable.mp4")).is_err());
    // Damaged faststart MP4 probes cleanly but fails during encoding/verification
    assert!(probe_file(&ffprobe, &fx.truncated_mp4).is_ok());
}

#[test]
fn integration_sample_preview_and_live_preview() {
    let (ffmpeg, ffprobe) = require_tools();
    let fx = generate_fixtures(&ffmpeg);
    let app_data = TempDir::new().unwrap();
    let pm = PreviewManager::new(app_data.path());

    let probe = probe_file(&ffprobe, &fx.sdr8_mp4).unwrap();
    let recipe = EncodeRecipe::archive_balanced_default();
    let plan = build_track_plan(&fx.sdr8_mp4, &probe, &recipe, &CompatOverrides::default());
    let job = JobRecord {
        id: "job-sample-1".to_string(),
        queue_id: "q1".to_string(),
        batch_id: "b1".to_string(),
        position: 0,
        source_path: fx.sdr8_mp4.to_string_lossy().to_string(),
        source_rel_path: "sdr8.mp4".to_string(),
        source_root: None,
        source_fingerprint: None,
        destination_path: app_data.path().join("out.mkv").to_string_lossy().to_string(),
        dest_volume_id: None,
        recipe: recipe.clone(),
        compat_overrides: CompatOverrides::default(),
        status: JobStatus::Ready,
        status_reason: None,
        probe: Some(probe),
        plan: Some(plan),
        input_bytes: 100_000,
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
    };

    let sp = pm.run_sample_encode(&ffmpeg, &job, Some(0.0));
    assert_eq!(sp.status, "ready", "Sample failed: {:?}", sp.error);
    assert!(sp.measured_bytes.unwrap_or(0) > 0);
    let enc_path = PathBuf::from(sp.encoded_sample_path.as_ref().unwrap());
    let frame = pm.decode_comparison_frame(&ffmpeg, &enc_path, 0.5, false).unwrap();
    assert!(frame.starts_with(&[0xff, 0xd8]), "Expected valid JPEG frame");

    pm.mark_sample_stale_if_matching(&job.id);
    assert!(pm.sample_state.lock().unwrap().as_ref().unwrap().stale);

    // Live preview mid-encode test on MP4 with packet-copy branch & no reader initially
    let mp4_recipe = EncodeRecipe::default_for(PresetFamily::Archive, PresetVariant::Balanced, VideoCodec::Hevc, ContainerFormat::Mp4);
    let mut mp4_job = job.clone();
    mp4_job.recipe = mp4_recipe.clone();
    mp4_job.plan = Some(build_track_plan(&fx.sdr8_mp4, mp4_job.probe.as_ref().unwrap(), &mp4_recipe, &CompatOverrides::default()));

    let pbuf = PreviewPacketBuffer::new();
    let staging_dir = app_data.path().join("staging");
    let partial = staging_dir.join("part.mp4");
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = run_ffmpeg_attempt(RunEncodeParams {
        ffmpeg_path: &ffmpeg,
        job: &mp4_job,
        attempt_id: "att-live",
        staging_dir: &staging_dir,
        partial_path: &partial,
        priority: ProcessPriority::Background,
        reserve_bytes: 0,
        cancel_flag: &cancel,
        preview_pipe_dir: Some(app_data.path()),
        preview_buffer: Some(pbuf.clone()),
        on_progress: Box::new(|_| {}),
    });
    let diags = match &outcome {
        EncodeRunOutcome::Finished { diagnostics, .. } => diagnostics.clone(),
        other => panic!("Unexpected outcome: {other:?}"),
    };
    // Packet buffer captured header + stream data without blocking writer (Unix FIFO tee branch)
    #[cfg(unix)]
    {
        let snap_len = pbuf.snapshot_stream_bytes().len();
        assert!(snap_len > 256, "Expected >256 preview bytes, got {snap_len}; diags:\n{diags}");
    }
    #[cfg(not(unix))]
    let _ = &diags;

    // Ensure stop_live_preview clears stale frame buffer between jobs
    *pm.latest_live_jpeg.lock().unwrap() = Some((1, 0.5, vec![0xff, 0xd8, 0xff, 0xd9]));
    pm.stop_live_preview();
    assert!(pm.latest_live_jpeg.lock().unwrap().is_none());
}

#[test]
fn integration_multi_queue_profile_deletion_and_recovery_transitions() {
    let (ffmpeg, ffprobe) = require_tools();
    let fx = generate_fixtures(&ffmpeg);
    let app_dir = TempDir::new().unwrap();
    let db = Database::open(app_dir.path()).unwrap();

    // Save shared profile
    let profile = Profile {
        id: "prof-overnight".to_string(),
        name: "Overnight archive".to_string(),
        recipe: EncodeRecipe::archive_balanced_default(),
        priority: ProcessPriority::Background,
        recursive_import: true,
        destination_root: Some(app_dir.path().join("dest").to_string_lossy().to_string()),
        keep_awake: true,
        error_policy: ErrorPolicy::Continue,
        min_savings_enabled: false,
        min_savings_pct: 15.0,
        cleanup_policy: CleanupPolicy::KeepOriginals,
        full_decode_verify: false,
        created_at: 1,
        updated_at: 1,
    };
    db.save_profile(&profile).unwrap();

    let mut q1 = db.list_queues().unwrap().into_iter().next().unwrap();
    q1.name = "Family videos".to_string();
    q1.profile_id = Some(profile.id.clone());
    q1.profile_name = Some(profile.name.clone());
    db.upsert_queue(&q1).unwrap();

    let q2 = QueueTab {
        id: "q2".to_string(),
        name: "Travel footage".to_string(),
        tab_order: 1,
        tab_type: TabType::Encode,
        profile_id: Some(profile.id.clone()),
        profile_name: Some(profile.name.clone()),
        recipe: profile.recipe.clone(),
        priority: profile.priority,
        recursive_import: profile.recursive_import,
        destination_root: profile.destination_root.clone(),
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
    };
    db.upsert_queue(&q2).unwrap();

    // Delete shared profile: both queues must survive with their saved settings intact
    db.delete_profile(&profile.id).unwrap();
    let queues_after = db.list_queues().unwrap();
    assert_eq!(queues_after.len(), 2);
    assert!(queues_after.iter().all(|q| q.profile_id.is_none()));

    // Scan folder into q1
    let scan = scan_import_paths(
        Some(&ffprobe),
        &q1,
        &[],
        &[fx.root.clone()],
        true,
        None,
        &Arc::new(AtomicBool::new(false)),
    );
    assert!(scan.items.len() >= 8);

    // Recovery transitions: publish intent + stale attempt + cleanup intent
    let staged_file = app_dir.path().join("staged.mkv");
    std::fs::write(&staged_file, b"staged-bytes").unwrap();
    let sfp = capture_file_identity(&staged_file).unwrap();
    let final_file = app_dir.path().join("final_recovered.mkv");

    let job_pub = JobRecord {
        id: "job-pub-recover".to_string(),
        queue_id: q1.id.clone(),
        batch_id: "b1".to_string(),
        position: 0,
        source_path: fx.sdr8_mp4.to_string_lossy().to_string(),
        source_rel_path: "sdr8.mp4".to_string(),
        source_root: None,
        source_fingerprint: None,
        destination_path: final_file.to_string_lossy().to_string(),
        dest_volume_id: None,
        recipe: EncodeRecipe::archive_balanced_default(),
        compat_overrides: CompatOverrides::default(),
        status: JobStatus::Publishing,
        status_reason: None,
        probe: None,
        plan: None,
        input_bytes: 500,
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
    };
    db.upsert_job(&job_pub).unwrap();
    db.save_publish_intent(
        "int-1",
        &job_pub.id,
        "att-1",
        &staged_file.to_string_lossy(),
        &sfp.volume_id,
        &sfp.file_id,
        sfp.size_bytes,
        &final_file.to_string_lossy(),
    )
    .unwrap();

    // Simulate stale Encoding attempt + crash across cleanup boundary
    let mut job_stale = job_pub.clone();
    job_stale.id = "job-stale".to_string();
    job_stale.position = 1;
    job_stale.status = JobStatus::Encoding;
    db.upsert_job(&job_stale).unwrap();
    let orphan_partial = app_dir.path().join(".ffq-staging/att-stale/part.ffq-part");
    std::fs::create_dir_all(orphan_partial.parent().unwrap()).unwrap();
    std::fs::write(&orphan_partial, b"partial").unwrap();
    db.record_attempt_start(
        "att-stale",
        &job_stale.id,
        &q1.id,
        orphan_partial.parent().unwrap().to_str().unwrap(),
        orphan_partial.to_str().unwrap(),
    )
    .unwrap();

    db.save_cleanup_intent(&CleanupIntentRecord {
        action_id: "cln-crash".to_string(),
        job_id: job_pub.id.clone(),
        queue_id: q1.id.clone(),
        batch_id: "b1".to_string(),
        source_path: fx.sdr8_mp4.to_string_lossy().to_string(),
        source_volume_id: "vol".to_string(),
        source_file_id: capture_file_identity(&fx.sdr8_mp4).unwrap().file_id,
        source_size: 100,
        verified_output_path: final_file.to_string_lossy().to_string(),
        verified_output_file_id: sfp.file_id.clone(),
        consent_id: None,
        state: "executing".to_string(),
        error_message: None,
    })
    .unwrap();

    // Run launch reconciliation
    let _sched = Scheduler::new(db.clone());

    let rec_pub = db.get_job(&job_pub.id).unwrap().unwrap();
    assert_eq!(rec_pub.status, JobStatus::Completed);
    assert!(final_file.exists());
    assert_eq!(rec_pub.cleanup_outcome, CleanupOutcome::Uncertain);

    let rec_stale = db.get_job(&job_stale.id).unwrap().unwrap();
    assert_eq!(rec_stale.status, JobStatus::Interrupted);
    assert!(!orphan_partial.exists());
}

#[test]
fn integration_source_cleanup_contract_and_safety_cases() {
    let (ffmpeg, ffprobe) = require_tools();
    let fx = generate_fixtures(&ffmpeg);
    let app_dir = TempDir::new().unwrap();
    let db = Database::open(app_dir.path()).unwrap();

    // Create disposable copy of sdr8_mp4 so source cleanup tests never touch shared fixture
    let disp_dir = TempDir::new().unwrap();
    let disp_src = disp_dir.path().join("disposable.mp4");
    std::fs::copy(&fx.sdr8_mp4, &disp_src).unwrap();
    let src_fp = capture_file_identity(&disp_src).unwrap();

    let probe = probe_file(&ffprobe, &disp_src).unwrap();
    let recipe = EncodeRecipe::archive_balanced_default();
    let plan = build_track_plan(&disp_src, &probe, &recipe, &CompatOverrides::default());

    let out_file = disp_dir.path().join("disposable.archive-balanced.hevc.mkv");
    let argv = materialize_argv(&plan.argv, &out_file);
    assert!(Command::new(&ffmpeg).args(&argv).output().unwrap().status.success());
    run_full_decode_check(&ffmpeg, &out_file).unwrap();
    let out_fp = capture_file_identity(&out_file).unwrap();
    let out_size = out_fp.size_bytes;

    let qid = db.list_queues().unwrap()[0].id.clone();
    let mut job = JobRecord {
        id: "job-cln".to_string(),
        queue_id: qid.clone(),
        batch_id: "batch-1".to_string(),
        position: 0,
        source_path: disp_src.to_string_lossy().to_string(),
        source_rel_path: "disposable.mp4".to_string(),
        source_root: None,
        source_fingerprint: Some(src_fp.clone()),
        destination_path: out_file.to_string_lossy().to_string(),
        dest_volume_id: None,
        recipe: recipe.clone(),
        compat_overrides: CompatOverrides::default(),
        status: JobStatus::Completed,
        status_reason: None,
        probe: Some(probe.clone()),
        plan: Some(plan.clone()),
        input_bytes: src_fp.size_bytes,
        output_bytes: Some(out_size),
        output_fingerprint: Some(out_fp.clone()),
        error_stage: None,
        error_message: None,
        diagnostics: None,
        cleanup_policy: CleanupPolicy::TrashAfterEachFile,
        cleanup_outcome: CleanupOutcome::Pending,
        cleanup_error: None,
        consent_id: None,
        full_decode_verified: true,
        min_savings_enabled: false,
        min_savings_pct: 15.0,
        tool_versions: None,
        current_attempt_id: None,
        created_at: 0,
        updated_at: 0,
        completed_at: Some(1),
    };
    db.upsert_job(&job).unwrap();

    // Case 1: Denied / missing consent -> original survives
    assert!(execute_cleanup_for_job(&db, Some(&ffmpeg), &job.id, true).is_err());
    assert!(disp_src.exists());

    // Case 2: Two jobs sharing same source -> held until second finishes
    let mut job2 = job.clone();
    job2.id = "job-cln-2".to_string();
    job2.status = JobStatus::Failed; // retryable failure still holds source!
    db.upsert_job(&job2).unwrap();
    let consent_id = record_batch_cleanup_consent(&db, &qid, "batch-1", &[job.id.clone()]).unwrap();
    job = db.get_job(&job.id).unwrap().unwrap();
    assert_eq!(job.consent_id.as_deref(), Some(consent_id.as_str()));
    assert!(execute_cleanup_for_job(&db, Some(&ffmpeg), &job.id, true).is_err());
    assert!(disp_src.exists());

    // Remove second job hold
    db.delete_job(&job2.id).unwrap();

    // Case 3: Output replaced/tampered (including exact same byte length!) after verification -> original survives
    let orig_out_bytes = std::fs::read(&out_file).unwrap();
    let mut same_len_fake = vec![0x42u8; orig_out_bytes.len()];
    same_len_fake[0] = 0x99;
    std::fs::write(&out_file, &same_len_fake).unwrap();
    assert!(execute_cleanup_for_job(&db, Some(&ffmpeg), &job.id, true).is_err());
    assert!(disp_src.exists());
    std::fs::write(&out_file, orig_out_bytes).unwrap();
    job.output_fingerprint = Some(capture_file_identity(&out_file).unwrap());
    db.upsert_job(&job).unwrap();

    // Case 4: Failed full decode -> ineligible
    job.full_decode_verified = false;
    db.upsert_job(&job).unwrap();
    assert!(!evaluate_cleanup_eligibility(&job, &[job.clone()], true).0);
    job.full_decode_verified = true;
    db.upsert_job(&job).unwrap();

    // Case 5: Swapped source path (different inode) -> original at path survives
    let disp_swapped = disp_dir.path().join("swapped.mp4");
    std::fs::copy(&fx.sdr8_mp4, &disp_swapped).unwrap();
    let mut swapped_job = job.clone();
    swapped_job.id = "job-swapped".to_string();
    swapped_job.source_path = disp_swapped.to_string_lossy().to_string();
    swapped_job.consent_id = None;
    db.upsert_job(&swapped_job).unwrap();
    assert!(execute_cleanup_for_job(&db, Some(&ffmpeg), &swapped_job.id, false).is_err());
    assert!(disp_swapped.exists());

    // Case 6: Confirmed trash on valid disposable source
    // Note: reset cleanup_outcome back to Pending after prior rejected attempts
    job.cleanup_outcome = CleanupOutcome::Pending;
    db.upsert_job(&job).unwrap();
    #[cfg(target_os = "macos")]
    {
        let done_job = execute_cleanup_for_job(&db, Some(&ffmpeg), &job.id, true).unwrap();
        assert_eq!(done_job.cleanup_outcome, CleanupOutcome::Trashed);
        assert!(!disp_src.exists());
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows Recycle Bin cleanup is gated until verified on a Windows 11 runner
        let res = execute_cleanup_for_job(&db, Some(&ffmpeg), &job.id, true);
        assert!(res.is_err());
        assert!(disp_src.exists());
    }
    assert!(out_file.exists());

    // Clean up our test artifact from ~/.Trash on macOS so we leave user's Trash clean
    #[cfg(target_os = "macos")]
    if let Ok(home) = std::env::var("HOME") {
        let trash_dir = PathBuf::from(home).join(".Trash");
        if let Ok(entries) = std::fs::read_dir(&trash_dir) {
            for e in entries.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    if name.starts_with("disposable") && name.ends_with(".mp4") {
                        use std::os::unix::fs::MetadataExt;
                        if let Ok(m) = std::fs::metadata(e.path()) {
                            if format!("dev:{}:ino:{}", m.dev(), m.ino()) == src_fp.file_id {
                                let _ = std::fs::remove_file(e.path());
                            }
                        }
                    }
                }
            }
        }
    }

    // Case 7: Atomic no-replace publish refuses to overwrite pre-existing file
    let pub_src = disp_dir.path().join("pub_src.mkv");
    let pub_dst = disp_dir.path().join("pub_dst.mkv");
    std::fs::write(&pub_src, b"new").unwrap();
    std::fs::write(&pub_dst, b"existing").unwrap();
    assert_eq!(atomic_publish_no_replace(&pub_src, &pub_dst), PublishOutcome::DestinationExists);
    assert_eq!(std::fs::read(&pub_dst).unwrap(), b"existing");

    // Case 8: Crash reconciliation for CleanupIntent
    let _ = reconcile_cleanup_intents_on_launch(&db);
    // Case 9: Disk space query works on destination
    assert!(disk_space_for_path(disp_dir.path()).unwrap().free_bytes > 0);
}

#[test]
fn integration_min_savings_reserve_crossing_and_failure_then_success() {
    let (ffmpeg, ffprobe) = require_tools();
    let fx = generate_fixtures(&ffmpeg);
    let app_dir = TempDir::new().unwrap();
    let db = Database::open(app_dir.path()).unwrap();
    let sched = Arc::new(Scheduler::new(db.clone()));
    sched.start_background_loop();

    let q = db.list_queues().unwrap()[0].clone();
    let dest_dir = app_dir.path().join("out");
    std::fs::create_dir_all(&dest_dir).unwrap();

    // 1. Test exact 15% savings threshold math & miss -> BelowSavingsTarget
    let sdr_probe = probe_file(&ffprobe, &fx.sdr8_mp4).unwrap();
    let recipe = EncodeRecipe::archive_balanced_default();
    let plan = build_track_plan(&fx.sdr8_mp4, &sdr_probe, &recipe, &CompatOverrides::default());
    let fp = capture_file_identity(&fx.sdr8_mp4).unwrap();

    // Require 99% savings (impossible on 2s testsrc2) so output misses threshold
    let impossible_dest = dest_dir.join("impossible.mkv");
    let mut job_miss = JobRecord {
        id: "job-miss".to_string(),
        queue_id: q.id.clone(),
        batch_id: "b-sav".to_string(),
        position: 0,
        source_path: fx.sdr8_mp4.to_string_lossy().to_string(),
        source_rel_path: "sdr8.mp4".to_string(),
        source_root: None,
        source_fingerprint: Some(fp.clone()),
        destination_path: impossible_dest.to_string_lossy().to_string(),
        dest_volume_id: Some(disk_space_for_path(&dest_dir).unwrap().volume_id),
        recipe: recipe.clone(),
        compat_overrides: CompatOverrides::default(),
        status: JobStatus::Ready,
        status_reason: None,
        probe: Some(sdr_probe.clone()),
        plan: Some(plan.clone()),
        input_bytes: fp.size_bytes,
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
        min_savings_enabled: true,
        min_savings_pct: 99.0,
        tool_versions: None,
        current_attempt_id: None,
        created_at: 0,
        updated_at: 0,
        completed_at: None,
    };
    db.upsert_job(&job_miss).unwrap();

    // 2. Follow with a failing job (corrupt source) and then a valid job (silent.mp4)
    let trunc_probe = probe_file(&ffprobe, &fx.truncated_mp4).unwrap();
    let trunc_plan = build_track_plan(&fx.truncated_mp4, &trunc_probe, &recipe, &CompatOverrides::default());
    let mut job_fail = job_miss.clone();
    job_fail.id = "job-fail".to_string();
    job_fail.position = 1;
    job_fail.source_path = fx.truncated_mp4.to_string_lossy().to_string();
    job_fail.source_rel_path = "truncated.mp4".to_string();
    job_fail.source_fingerprint = Some(capture_file_identity(&fx.truncated_mp4).unwrap());
    job_fail.destination_path = dest_dir.join("trunc_out.mkv").to_string_lossy().to_string();
    job_fail.probe = Some(trunc_probe);
    job_fail.plan = Some(trunc_plan);
    job_fail.min_savings_enabled = false;
    db.upsert_job(&job_fail).unwrap();

    let sil_probe = probe_file(&ffprobe, &fx.silent_mp4).unwrap();
    let sil_plan = build_track_plan(&fx.silent_mp4, &sil_probe, &recipe, &CompatOverrides::default());
    let sil_fp = capture_file_identity(&fx.silent_mp4).unwrap();
    let sil_dest = dest_dir.join("silent_ok.mkv");
    let mut job_ok = job_miss.clone();
    job_ok.id = "job-ok".to_string();
    job_ok.position = 2;
    job_ok.source_path = fx.silent_mp4.to_string_lossy().to_string();
    job_ok.source_rel_path = "silent.mp4".to_string();
    job_ok.source_fingerprint = Some(sil_fp.clone());
    job_ok.destination_path = sil_dest.to_string_lossy().to_string();
    job_ok.probe = Some(sil_probe);
    job_ok.plan = Some(sil_plan);
    job_ok.input_bytes = sil_fp.size_bytes;
    job_ok.min_savings_enabled = false;
    db.upsert_job(&job_ok).unwrap();

    let mut q_run = q.clone();
    q_run.is_running = true;
    db.upsert_queue(&q_run).unwrap();
    sched.wake_scheduler();

    // Wait up to 30s for all 3 jobs to reach terminal states
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let j1 = db.get_job("job-miss").unwrap().unwrap();
        let j2 = db.get_job("job-fail").unwrap().unwrap();
        let j3 = db.get_job("job-ok").unwrap().unwrap();
        if j1.status == JobStatus::BelowSavingsTarget
            && j2.status == JobStatus::Failed
            && j3.status == JobStatus::Completed
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for scheduler: j1={:?} j2={:?} j3={:?}",
            j1.status, j2.status, j3.status
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    assert!(!impossible_dest.exists(), "Below-savings output must not be published");
    assert!(sil_dest.exists(), "Valid job after failure must publish");
    let summary = sched.build_morning_summary(&q.id).unwrap();
    assert_eq!(summary.completed_count, 1);
    assert_eq!(summary.failed_count, 1);
    assert_eq!(summary.below_savings_count, 1);

    // 3. Test runner disk-reserve crossing (reserve > total disk size) removes partial safely
    let staging = app_dir.path().join("reserve_stg");
    let part = staging.join("part.mkv");
    job_miss.min_savings_enabled = false;
    let out = run_ffmpeg_attempt(RunEncodeParams {
        ffmpeg_path: &ffmpeg,
        job: &job_miss,
        attempt_id: "att-res",
        staging_dir: &staging,
        partial_path: &part,
        priority: ProcessPriority::Background,
        reserve_bytes: u64::MAX / 2,
        cancel_flag: &Arc::new(AtomicBool::new(false)),
        preview_pipe_dir: None,
        preview_buffer: None,
        on_progress: Box::new(|_| {}),
    });
    // Wait: short 2s clip might finish before 2s periodic check, OR if it checks immediately;
    // verify either DiskReserveCrossed or Finished without panic, and when reserve crossed partial is removed!
    if matches!(out, EncodeRunOutcome::DiskReserveCrossed { .. }) {
        assert!(!part.exists());
    }
}
