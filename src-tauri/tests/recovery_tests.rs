use media_shack_lib::recovery::{diagnose_file, recover_file, ContainerKind, RecoveryOptions};
use media_shack_lib::tools::find_tool_pair;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gen_video(
    ffmpeg: &Path,
    out: &Path,
    codec: &str,
    fps: u32,
    duration_secs: u32,
    size: &str,
    b_frames: u32,
    audio: bool,
    vfr: bool,
    stereo_lr: bool,
) {
    let mut cmd = Command::new(ffmpeg);
    let src_filter = if codec == "hevc" {
        format!("testsrc2=size={size}:rate={fps}:duration={duration_secs}")
    } else {
        format!("testsrc=size={size}:rate={fps}:duration={duration_secs}")
    };
    cmd.args([
        "-hide_banner",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &src_filter,
    ]);
    if audio {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=520:sample_rate=44100:duration={duration_secs}"),
        ]);
    }
    let vf_expr = if vfr {
        Some("setpts=PTS*(1+0.15*sin(N))")
    } else {
        None
    };
    if let Some(vf) = vf_expr {
        cmd.args(["-vf", vf]);
    }
    match codec {
        "hevc" => {
            cmd.args([
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "16",
                "-x265-params",
                &format!("bframes={b_frames}:keyint={fps}:min-keyint={fps}:log-level=error"),
                "-tag:v",
                "hvc1",
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        _ => {
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                &fps.to_string(),
                "-bf",
                &b_frames.to_string(),
                "-pix_fmt",
                "yuv420p",
            ]);
        }
    }
    if audio {
        cmd.args(["-c:a", "aac", "-b:a", "96k"]);
    }
    if stereo_lr {
        cmd.args(["-metadata:s:v:0", "stereo_mode=left_right"]);
    }
    // Leave moov at tail (do NOT use faststart) so truncations strip moov cleanly!
    cmd.arg(out);
    let out_res = cmd.output().expect("Failed running ffmpeg test fixture generator");
    assert!(
        out_res.status.success(),
        "ffmpeg fixture gen failed: {}",
        String::from_utf8_lossy(&out_res.stderr)
    );
}

fn find_atom_bounds(path: &Path, target_tag: &[u8; 4]) -> Option<(u64, u64)> {
    let bytes = std::fs::read(path).ok()?;
    let mut off = 0usize;
    while off + 8 <= bytes.len() {
        let s32 = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let tag = &bytes[off + 4..off + 8];
        if s32 < 8 || off + s32 > bytes.len() {
            break;
        }
        if tag == target_tag {
            return Some((off as u64, (off + s32) as u64));
        }
        off += s32;
    }
    None
}

fn truncate_file_pct(src: &Path, dst: &Path, keep_pct: f64) {
    let bytes = std::fs::read(src).unwrap();
    let keep = ((bytes.len() as f64) * keep_pct) as usize;
    std::fs::write(dst, &bytes[..keep.max(64)]).unwrap();
}

#[test]
fn recovery_truncation_percentages_moov_removal_and_leading_cut() {
    let (ffmpeg, ffprobe) = find_tool_pair(None, None, None).expect("FFmpeg required for recovery tests");
    let dir = tempfile::tempdir().unwrap();
    let healthy = dir.path().join("healthy_h264.mp4");
    let healthy_ref = dir.path().join("reference_h264.mp4");

    // Generate 8-second 24fps H.264 + B-frames + AAC MP4 with trailing moov
    gen_video(&ffmpeg, &healthy, "h264", 24, 8, "320x240", 2, true, false, false);
    // Reference is a distinct generated video with same codec parameters
    gen_video(&ffmpeg, &healthy_ref, "h264", 24, 3, "320x240", 2, true, false, false);

    // 1. Clean removal of `moov` atom + verify truncation percentages 10%, 25%, 50%, 75%, 90%
    let moov_bounds = find_atom_bounds(&healthy, b"moov").expect("Generated MP4 should have moov atom");
    let no_moov = dir.path().join("no_moov.mp4");
    {
        let raw = std::fs::read(&healthy).unwrap();
        std::fs::write(&no_moov, &raw[..moov_bounds.0 as usize]).unwrap();
    }
    let diag_no_moov = diagnose_file(&no_moov, None).unwrap();
    assert_eq!(diag_no_moov.container_kind, ContainerKind::ConventionalMp4);
    assert!(!diag_no_moov.has_moov);
    assert!(diag_no_moov.has_mdat);

    let out_no_moov = dir.path().join("recovered_no_moov.mp4");
    let rep_no_moov = recover_file(
        &RecoveryOptions {
            input_path: no_moov.clone(),
            reference_paths: vec![healthy_ref.clone()],
            output_path: out_no_moov.clone(),
            video_only: false,
            deep_scan: false,
            timeout_secs: 60,
            generate_contact_sheet: true,
        },
        &ffmpeg,
        &ffprobe,
        None,
    )
    .unwrap();
    assert!(rep_no_moov.input_untouched_verified);
    assert_ne!(rep_no_moov.overall_status, "unrecoverable", "Summary: {}", rep_no_moov.honest_summary);
    assert!(out_no_moov.is_file());
    assert!(rep_no_moov.contact_sheet_path.is_some());
    let best = rep_no_moov.candidates.iter().find(|c| c.succeeded).unwrap();
    assert!(best.decoded_video_frames >= 120, "Expected >=120 frames, got {}", best.decoded_video_frames);

    // Truncations at 25%, 50%, 75%, 90% (and 10% which might still contain >=1 GOP at 24fps=24 frames)
    for pct in [0.10, 0.25, 0.50, 0.75, 0.90] {
        let label = (pct * 100.0) as u32;
        let trunc = dir.path().join(format!("trunc_{label}.mp4"));
        truncate_file_pct(&healthy, &trunc, pct);
        let d = diagnose_file(&trunc, None).unwrap();
        assert!(!d.has_moov);
        assert!(d.atoms.iter().any(|a| a.tag == "mdat" && a.truncated));

        let out = dir.path().join(format!("rec_{label}.mp4"));
        let rep = recover_file(
            &RecoveryOptions {
                input_path: trunc,
                reference_paths: vec![healthy_ref.clone()],
                output_path: out.clone(),
                video_only: false,
                deep_scan: false,
                timeout_secs: 60,
                generate_contact_sheet: false,
            },
            &ffmpeg,
            &ffprobe,
            None,
        )
        .unwrap();
        assert!(rep.input_untouched_verified);
        assert_ne!(
            rep.overall_status,
            "unrecoverable",
            "Truncation {label}% failed to recover: {}",
            rep.honest_summary
        );
    }

    // 2. Cutting off FIRST half of file (missing ftyp, mdat header, and missing moov!)
    let second_half_no_moov = dir.path().join("second_half_headless.mp4");
    {
        let raw = std::fs::read(&no_moov).unwrap();
        let cut_start = raw.len() / 2;
        std::fs::write(&second_half_no_moov, &raw[cut_start..]).unwrap();
    }
    let diag_headless = diagnose_file(&second_half_no_moov, None).unwrap();
    assert!(!diag_headless.has_ftyp);
    assert!(!diag_headless.has_moov);
    assert!(!diag_headless.mdat_ranges.is_empty());

    let out_headless = dir.path().join("rec_headless.mp4");
    let rep_headless = recover_file(
        &RecoveryOptions {
            input_path: second_half_no_moov,
            reference_paths: vec![healthy_ref.clone()],
            output_path: out_headless.clone(),
            video_only: true,
            deep_scan: true,
            timeout_secs: 60,
            generate_contact_sheet: false,
        },
        &ffmpeg,
        &ffprobe,
        None,
    )
    .unwrap();
    assert_ne!(
        rep_headless.overall_status,
        "unrecoverable",
        "Headless second-half recovery failed: {}",
        rep_headless.honest_summary
    );
}

#[test]
fn recovery_hevc_zero_fill_random_noise_corrupt_mdat_and_wrong_reference() {
    let (ffmpeg, ffprobe) = find_tool_pair(None, None, None).expect("FFmpeg required for recovery tests");
    let dir = tempfile::tempdir().unwrap();

    // 6-second HEVC no-audio VFR video + healthy HEVC reference + WRONG H264 reference + SBS VR metadata
    let healthy_hevc = dir.path().join("healthy_hevc.mp4");
    let ref_hevc = dir.path().join("ref_hevc.mp4");
    let wrong_ref_h264 = dir.path().join("wrong_h264.mp4");

    gen_video(&ffmpeg, &healthy_hevc, "hevc", 24, 6, "640x320", 1, false, true, true);
    gen_video(&ffmpeg, &ref_hevc, "hevc", 24, 3, "640x320", 1, false, true, true);
    media_shack_lib::recovery::isobmff::inject_st3d_box_into_mp4(&ref_hevc, "side_by_side").unwrap();
    gen_video(&ffmpeg, &wrong_ref_h264, "h264", 30, 3, "192x144", 0, true, false, false);

    let mdat_bounds = find_atom_bounds(&healthy_hevc, b"mdat").unwrap();
    let moov_bounds = find_atom_bounds(&healthy_hevc, b"moov").unwrap();

    // Damaged HEVC: strip moov, corrupt mdat size header to huge 0x7FFF_FFFF, zero-fill >= 16KB hole,
    // and overwrite >= 8KB with non-zero repetitive corruption
    let damaged_hevc = dir.path().join("damaged_hevc.mp4");
    {
        let mut raw = std::fs::read(&healthy_hevc).unwrap();
        raw.truncate(moov_bounds.0 as usize);
        // Corrupt mdat size
        raw[mdat_bounds.0 as usize..mdat_bounds.0 as usize + 4]
            .copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
        // Zero-fill 16KB hole in middle
        let mid = raw.len() / 2;
        let zend = (mid + 16_384).min(raw.len().saturating_sub(4096));
        assert!(zend > mid + 4096, "Expected large enough HEVC fixture (len={})", raw.len());
        raw[mid..zend].fill(0x00);
        // Repetitive 8KB 0xAA corruption near 25%
        let q1 = (raw.len() / 4).max(mdat_bounds.0 as usize + 128);
        if q1 + 8192 < mid {
            raw[q1..q1 + 8192].fill(0xAA);
        }
        std::fs::write(&damaged_hevc, &raw).unwrap();
    }

    let diag = diagnose_file(&damaged_hevc, None).unwrap();
    assert!(!diag.corruption_regions.is_empty(), "Expected zero/repetitive regions detected");
    assert!(diag.atoms.iter().any(|a| a.tag == "mdat" && a.truncated));

    let out_hevc = dir.path().join("recovered_hevc.mp4");
    // Pass BOTH wrong H.264 ref (tiny 192x144) and matching HEVC ref (640x320 SBS) — tool should rank HEVC higher or succeed!
    let rep = recover_file(
        &RecoveryOptions {
            input_path: damaged_hevc,
            reference_paths: vec![wrong_ref_h264, ref_hevc],
            output_path: out_hevc.clone(),
            video_only: true,
            deep_scan: true,
            timeout_secs: 90,
            generate_contact_sheet: false,
        },
        &ffmpeg,
        &ffprobe,
        None,
    )
    .unwrap();
    assert!(rep.input_untouched_verified);
    assert_ne!(
        rep.overall_status,
        "unrecoverable",
        "HEVC damaged recovery failed: {}",
        rep.honest_summary
    );
    let best = rep.candidates.iter().find(|c| c.succeeded).unwrap();
    assert_eq!(best.width, Some(640));
    assert_eq!(best.height, Some(320));
    // Verify stereo/VR metadata preserved from reference
    assert!(rep.vr_status.is_stereoscopic);
}

#[test]
fn video_recover_cli_smoke_diagnose_and_batch() {
    let (ffmpeg, _ffprobe) = find_tool_pair(None, None, None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let healthy = dir.path().join("ref.mp4");
    let broken = dir.path().join("broken_dir/clip.mp4");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    let out_dir = dir.path().join("out_dir");

    gen_video(&ffmpeg, &healthy, "h264", 24, 4, "256x192", 0, true, false, false);
    truncate_file_pct(&healthy, &broken, 0.55);

    // Run `video-recover` binary via CARGO_BIN_EXE_video-recover (provided automatically by Cargo!)
    let bin: PathBuf = env!("CARGO_BIN_EXE_video-recover").into();

    let diag_out = Command::new(&bin)
        .args(["diagnose", broken.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(diag_out.status.success(), "CLI diagnose failed: {}", String::from_utf8_lossy(&diag_out.stderr));
    let diag_json: serde_json::Value = serde_json::from_slice(&diag_out.stdout).unwrap();
    assert_eq!(diag_json["has_moov"].as_bool(), Some(false));

    let batch_out = Command::new(&bin)
        .args([
            "batch",
            broken.parent().unwrap().to_str().unwrap(),
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--reference",
            healthy.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        batch_out.status.success(),
        "CLI batch failed: {}\nstderr: {}",
        String::from_utf8_lossy(&batch_out.stdout),
        String::from_utf8_lossy(&batch_out.stderr)
    );
    let rec_file = out_dir.join("clip_recovered.mp4");
    assert!(rec_file.is_file(), "Expected batch output file {}", rec_file.display());
}

#[test]
fn db_typed_tabs_and_repair_session_persistence() {
    use media_shack_lib::db::Database;
    use media_shack_lib::models::{RepairJobRecord, TabType};

    let db = Database::in_memory().unwrap();
    let mut queues = db.list_queues().unwrap();
    assert_eq!(queues.len(), 1);
    assert_eq!(queues[0].tab_type, TabType::Encode);

    queues[0].tab_type = TabType::Repair;
    db.upsert_queue(&queues[0]).unwrap();
    let reloaded = db.get_queue(&queues[0].id).unwrap().unwrap();
    assert_eq!(reloaded.tab_type, TabType::Repair);

    let rec = RepairJobRecord {
        queue_id: reloaded.id.clone(),
        input_path: "/tmp/damaged.mp4".into(),
        reference_paths: vec!["/tmp/ref.mp4".into()],
        output_path: None,
        video_only: true,
        deep_scan: true,
        timeout_secs: 120,
        status: "idle".into(),
        diagnosis: None,
        report: None,
        progress: None,
        error: None,
        preview_sheet_filename: None,
        updated_at: 12345,
    };
    db.upsert_repair_session(&rec).unwrap();
    let got = db.get_repair_session(&reloaded.id).unwrap().unwrap();
    assert_eq!(got.input_path, "/tmp/damaged.mp4");
    assert!(got.video_only);
}
