use media_shack_lib::recovery::{
    diagnose_file, format_diagnosis_human, format_recovery_human, recover_file, ProgressCallback,
    RecoveryOptions, RecoveryProgress,
};
use media_shack_lib::tools::find_tool_pair;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return;
    }

    let sub = args[0].as_str();
    let rest = &args[1..];
    match sub {
        "diagnose" => {
            if let Err(e) = cmd_diagnose(rest) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        "recover" => {
            if let Err(e) = cmd_recover(rest) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        "batch" => {
            if let Err(e) = cmd_batch(rest) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("Unknown subcommand `{other}`");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!(
        "video-recover — Intelligent MP4/MOV Diagnosis & Bitstream Recovery CLI (Media Shack)

USAGE:
  video-recover diagnose <damaged.mp4> [--json] [--report <path>]
  video-recover recover  <damaged.mp4> [--reference <healthy.mp4>]... [--reference-dir <dir>]
                         [--output <recovered.mp4>] [--video-only] [--deep]
                         [--timeout-secs <N>] [--no-contact-sheet] [--json] [--report <path>]
  video-recover batch    <damaged_dir> --output-dir <dir> [--reference <healthy.mp4>]...
                         [--reference-dir <dir>] [--video-only] [--deep] [--timeout-secs <N>] [--json]
"
    );
}

fn cmd_diagnose(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut json_out = false;
    let mut report_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_out = true,
            "--report" => {
                i += 1;
                report_path = Some(PathBuf::from(
                    args.get(i).ok_or("--report requires a path")?,
                ));
            }
            s if s.starts_with('-') => return Err(format!("Unknown flag `{s}`")),
            s => {
                if input.is_none() {
                    input = Some(PathBuf::from(s));
                } else {
                    return Err(format!("Unexpected argument `{s}`"));
                }
            }
        }
        i += 1;
    }

    let input_path = input.ok_or("Missing input file path")?;
    let report = diagnose_file(&input_path, Some(&cli_progress_cb()))?;
    eprintln!();

    let rendered = if json_out {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        format_diagnosis_human(&report)
    };
    if let Some(rp) = report_path {
        std::fs::write(&rp, &rendered).map_err(|e| format!("Failed writing report {}: {e}", rp.display()))?;
    }
    println!("{rendered}");
    Ok(())
}

fn cmd_recover(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut refs: Vec<PathBuf> = Vec::new();
    let mut ref_dirs: Vec<PathBuf> = Vec::new();
    let mut output: Option<PathBuf> = None;
    let mut video_only = false;
    let mut deep_scan = false;
    let mut timeout_secs = 600u64;
    let mut contact_sheet = true;
    let mut json_out = false;
    let mut report_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reference" | "-r" => {
                i += 1;
                refs.push(PathBuf::from(
                    args.get(i).ok_or("--reference requires a path")?,
                ));
            }
            "--reference-dir" => {
                i += 1;
                ref_dirs.push(PathBuf::from(
                    args.get(i).ok_or("--reference-dir requires a directory")?,
                ));
            }
            "--output" | "-o" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i).ok_or("--output requires a path")?,
                ));
            }
            "--video-only" => video_only = true,
            "--deep" => deep_scan = true,
            "--no-contact-sheet" => contact_sheet = false,
            "--timeout-secs" => {
                i += 1;
                timeout_secs = args
                    .get(i)
                    .ok_or("--timeout-secs requires integer")?
                    .parse()
                    .map_err(|_| "Invalid integer for --timeout-secs")?;
            }
            "--json" => json_out = true,
            "--report" => {
                i += 1;
                report_path = Some(PathBuf::from(
                    args.get(i).ok_or("--report requires a path")?,
                ));
            }
            s if s.starts_with('-') => return Err(format!("Unknown flag `{s}`")),
            s => {
                if input.is_none() {
                    input = Some(PathBuf::from(s));
                } else {
                    return Err(format!("Unexpected argument `{s}`"));
                }
            }
        }
        i += 1;
    }

    let input_path = input.ok_or("Missing damaged input file path")?;
    collect_refs_from_dirs(&ref_dirs, &mut refs, &input_path);

    let output_path = output.unwrap_or_else(|| default_output_for(&input_path, None));
    let (ffmpeg, ffprobe) = find_tool_pair(None, None, None)?;

    let opts = RecoveryOptions {
        input_path,
        reference_paths: refs,
        output_path,
        video_only,
        deep_scan,
        timeout_secs,
        generate_contact_sheet: contact_sheet,
    };

    let report = recover_file(&opts, &ffmpeg, &ffprobe, Some(&cli_progress_cb()))?;
    eprintln!();

    let rendered = if json_out {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        format_recovery_human(&report)
    };
    if let Some(rp) = report_path {
        std::fs::write(&rp, &rendered).map_err(|e| format!("Failed writing report {}: {e}", rp.display()))?;
    }
    println!("{rendered}");

    if report.overall_status == "unrecoverable" {
        std::process::exit(2);
    }
    Ok(())
}

fn cmd_batch(args: &[String]) -> Result<(), String> {
    let mut input_dir: Option<PathBuf> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut refs: Vec<PathBuf> = Vec::new();
    let mut ref_dirs: Vec<PathBuf> = Vec::new();
    let mut video_only = false;
    let mut deep_scan = false;
    let mut timeout_secs = 600u64;
    let mut json_out = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output-dir" => {
                i += 1;
                output_dir = Some(PathBuf::from(
                    args.get(i).ok_or("--output-dir requires a path")?,
                ));
            }
            "--reference" | "-r" => {
                i += 1;
                refs.push(PathBuf::from(
                    args.get(i).ok_or("--reference requires a path")?,
                ));
            }
            "--reference-dir" => {
                i += 1;
                ref_dirs.push(PathBuf::from(
                    args.get(i).ok_or("--reference-dir requires a dir")?,
                ));
            }
            "--video-only" => video_only = true,
            "--deep" => deep_scan = true,
            "--timeout-secs" => {
                i += 1;
                timeout_secs = args
                    .get(i)
                    .ok_or("--timeout-secs requires integer")?
                    .parse()
                    .map_err(|_| "Invalid integer for --timeout-secs")?;
            }
            "--json" => json_out = true,
            s if s.starts_with('-') => return Err(format!("Unknown flag `{s}`")),
            s => {
                if input_dir.is_none() {
                    input_dir = Some(PathBuf::from(s));
                } else {
                    return Err(format!("Unexpected argument `{s}`"));
                }
            }
        }
        i += 1;
    }

    let in_dir = input_dir.ok_or("Missing input directory")?;
    let out_dir = output_dir.ok_or("Missing --output-dir")?;
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("Failed creating output dir: {e}"))?;

    let (ffmpeg, ffprobe) = find_tool_pair(None, None, None)?;
    let mut reports = Vec::new();

    for entry in std::fs::read_dir(&in_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
            continue;
        }
        let mut item_refs = refs.clone();
        collect_refs_from_dirs(&ref_dirs, &mut item_refs, &path);
        let out_file = default_output_for(&path, Some(&out_dir));
        let opts = RecoveryOptions {
            input_path: path,
            reference_paths: item_refs,
            output_path: out_file,
            video_only,
            deep_scan,
            timeout_secs,
            generate_contact_sheet: true,
        };
        let rep = recover_file(&opts, &ffmpeg, &ffprobe, Some(&cli_progress_cb()))?;
        eprintln!();
        reports.push(rep);
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?
        );
    } else {
        for r in &reports {
            println!("==========================================");
            println!("{}", format_recovery_human(r));
        }
    }
    Ok(())
}

fn collect_refs_from_dirs(dirs: &[PathBuf], refs: &mut Vec<PathBuf>, exclude: &Path) {
    for d in dirs {
        if let Ok(entries) = std::fs::read_dir(d) {
            for ent in entries.flatten() {
                let p = ent.path();
                if p.is_file() && p != exclude {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    if matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "mkv") && !refs.contains(&p) {
                        refs.push(p);
                    }
                }
            }
        }
    }
}

fn default_output_for(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recovered");
    let parent = out_dir.unwrap_or_else(|| input.parent().unwrap_or_else(|| Path::new(".")));
    parent.join(format!("{stem}_recovered.mp4"))
}

fn cli_progress_cb() -> ProgressCallback {
    Arc::new(|p: RecoveryProgress| {
        let pct = if p.total_bytes > 0 {
            (p.bytes_scanned as f64 / p.total_bytes as f64) * 100.0
        } else {
            0.0
        };
        eprint!(
            "\r[{:.1}% | {}] scanned {}/{} bytes | packets={} frames={} ({:.1}s)   ",
            pct,
            p.current_strategy,
            p.bytes_scanned,
            p.total_bytes,
            p.recognized_packets,
            p.recovered_frames,
            p.elapsed_secs
        );
    })
}
