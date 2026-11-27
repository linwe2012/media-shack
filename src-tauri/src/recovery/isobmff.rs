use crate::recovery::{
    AtomNode, ContainerKind, CorruptionRegion, DiagnosisReport, OrphanSignature, ProgressCallback,
    RecoveryProgress, VrMetadataInfo,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

const CONTAINER_BOXES: &[&str] = &[
    "moov", "trak", "edts", "mdia", "minf", "stbl", "dinf", "mvex", "moof", "traf", "udta",
    "meta", "ilst", "sv3d", "proj",
];

const SIGNATURE_TAGS: &[&str] = &["ftyp", "moov", "moof", "trak", "stbl", "mvex", "mdat"];

#[derive(Debug, Clone)]
pub struct ExtractedCodecConfig {
    pub avcc_raw: Option<Vec<u8>>,
    pub hvcc_raw: Option<Vec<u8>>,
    pub esds_asc: Option<Vec<u8>>,
    pub nal_length_size: Option<u8>,
    pub parameter_sets_annex_b: Vec<u8>,
    pub timescale: Option<u32>,
    pub vr_info: VrMetadataInfo,
    pub sample_sizes_sample: Vec<u32>,
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let f = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, f);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Read error: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn diagnose_mp4_file(
    path: &Path,
    progress_cb: Option<&ProgressCallback>,
) -> Result<DiagnosisReport, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Cannot stat {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("Not a regular file: {}", path.display()));
    }
    let file_size = meta.len();
    let start_ts = Instant::now();

    if let Some(cb) = progress_cb {
        cb(RecoveryProgress {
            stage: "diagnosis".into(),
            current_strategy: "SHA-256 & ISO BMFF atom inspection".into(),
            bytes_scanned: 0,
            total_bytes: file_size,
            recognized_packets: 0,
            recovered_frames: 0,
            elapsed_secs: 0.0,
        });
    }

    let sha256 = sha256_file(path)?;
    let mut file = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;

    let mut atoms = Vec::new();
    let mut anomalies = Vec::new();
    let mut mdat_ranges = Vec::new();
    let mut has_ftyp = false;
    let mut has_moov = false;
    let mut has_moof = false;
    let mut has_mvex = false;
    let mut has_mdat = false;

    let mut offset = 0u64;
    while offset + 8 <= file_size {
        if atoms.len() > 2048 {
            anomalies.push("Top-level atom limit exceeded (2048); stopping atom walk.".into());
            break;
        }
        let mut hdr = [0u8; 8];
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Seek error at {offset}: {e}"))?;
        file.read_exact(&mut hdr)
            .map_err(|e| format!("Header read error at {offset}: {e}"))?;

        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let tag_bytes = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let tag = fourcc_to_string(&tag_bytes);

        // Guard against starting on non-atom data when file is headless (e.g. cut first half)
        if offset == 0 && !is_plausible_top_atom(&tag_bytes) {
            anomalies.push(format!(
                "File does not begin with a valid ISO BMFF box header (bytes at 0: {:02x} {:02x} {:02x} {:02x} {:?})",
                hdr[4], hdr[5], hdr[6], hdr[7], tag
            ));
            break;
        }

        let (declared_size, header_size, size_mode) = if size32 == 1 {
            if offset + 16 > file_size {
                anomalies.push(format!("Truncated 64-bit box header for `{tag}` at offset {offset}"));
                break;
            }
            let mut ext = [0u8; 8];
            file.read_exact(&mut ext)
                .map_err(|e| format!("64-bit size read error at {offset}: {e}"))?;
            (u64::from_be_bytes(ext), 16u64, "extended_64".to_string())
        } else if size32 == 0 {
            (file_size - offset, 8u64, "to_eof_zero".to_string())
        } else {
            (size32 as u64, 8u64, "standard_32".to_string())
        };

        if declared_size < header_size {
            anomalies.push(format!(
                "Malformed `{tag}` box at offset {offset}: declared size {declared_size} < header size {header_size}"
            ));
            atoms.push(AtomNode {
                tag,
                offset,
                declared_size,
                physical_end: (offset + header_size).min(file_size),
                header_size,
                size_mode,
                truncated: true,
                malformed_reason: Some("Declared size smaller than header".into()),
                children: Vec::new(),
            });
            break;
        }

        let logical_end = offset.saturating_add(declared_size);
        let truncated = logical_end > file_size;
        let physical_end = logical_end.min(file_size);

        if truncated {
            anomalies.push(format!(
                "Box `{tag}` at offset {offset} declares {declared_size} bytes (end {logical_end}) extending {} bytes beyond physical EOF ({file_size})",
                logical_end - file_size
            ));
        }

        match tag.as_str() {
            "ftyp" => has_ftyp = true,
            "moov" => has_moov = true,
            "moof" => has_moof = true,
            "mvex" => has_mvex = true,
            "mdat" => {
                has_mdat = true;
                let pstart = offset + header_size;
                if pstart < physical_end {
                    mdat_ranges.push((pstart, physical_end));
                }
            }
            _ => {}
        }

        let mut children = Vec::new();
        if CONTAINER_BOXES.contains(&tag.as_str()) && physical_end > offset + header_size {
            children = parse_child_boxes(
                &mut file,
                offset + header_size,
                physical_end,
                0,
                &mut has_mvex,
                &mut anomalies,
            );
        }

        atoms.push(AtomNode {
            tag,
            offset,
            declared_size,
            physical_end,
            header_size,
            size_mode,
            truncated,
            malformed_reason: if truncated {
                Some("Extends beyond EOF".into())
            } else {
                None
            },
            children,
        });

        if truncated || logical_end <= offset {
            break;
        }
        offset = logical_end;
    }

    if offset < file_size && offset > 0 && atoms.last().map(|a| !a.truncated).unwrap_or(false) {
        anomalies.push(format!(
            "Trailing {} bytes after final top-level atom at offset {offset}",
            file_size - offset
        ));
    }

    // Check MPEG-TS signature vs MP4
    let is_mpegts = detect_mpegts(&mut file, file_size);

    // Scan file chunks for zero-filled runs, repetitive blocks, orphan atom signatures, and trailing unindexed payload
    let (corruption_regions, orphan_signatures) =
        scan_anomalies_and_signatures(&mut file, file_size, &atoms, progress_cb, start_ts)?;

    if atoms.iter().any(|a| a.truncated) {
        if let Some(last) = atoms.last() {
            if last.truncated {
                // Ensure TruncatedTail is recorded
                let mut has_tail_region = false;
                for r in &corruption_regions {
                    if r.kind == "truncated_tail" {
                        has_tail_region = true;
                    }
                }
                if !has_tail_region {
                    anomalies.push(format!(
                        "Truncated `{}` box leaves {} physical payload bytes available",
                        last.tag,
                        last.physical_end.saturating_sub(last.offset + last.header_size)
                    ));
                }
            }
        }
    }

    let container_kind = if is_mpegts && !has_ftyp && !has_mdat {
        ContainerKind::MpegTs
    } else if has_moof || has_mvex {
        ContainerKind::FragmentedMp4
    } else if has_ftyp || has_moov || has_mdat {
        ContainerKind::ConventionalMp4
    } else if scan_has_annex_b_start_codes(&mut file, file_size.min(2 * 1024 * 1024)) {
        ContainerKind::RawBitstream
    } else {
        ContainerKind::Unknown
    };

    // If headless file (cut off beginning!), treat whole file as scannable media range
    if mdat_ranges.is_empty() && file_size > 0 {
        if matches!(container_kind, ContainerKind::ConventionalMp4 | ContainerKind::Unknown | ContainerKind::RawBitstream) {
            mdat_ranges.push((0, file_size));
            anomalies.push("No intact `mdat` box boundary found; full physical file queued for raw binary bitstream scan.".into());
        }
    }

    let mut missing_atoms = Vec::new();
    if !has_ftyp && container_kind != ContainerKind::MpegTs {
        missing_atoms.push("ftyp".into());
    }
    if !has_moov && !has_moof && container_kind != ContainerKind::MpegTs {
        missing_atoms.push("moov".into());
    }
    if !has_mdat && container_kind != ContainerKind::MpegTs {
        missing_atoms.push("mdat".into());
    }

    let mut summary_lines = Vec::new();
    summary_lines.push(format!(
        "File size: {} bytes ({:.2} MiB), SHA-256: {}",
        file_size,
        file_size as f64 / (1024.0 * 1024.0),
        sha256
    ));
    summary_lines.push(format!("Detected container classification: {container_kind:?}"));
    if !missing_atoms.is_empty() {
        summary_lines.push(format!("Missing critical atoms: {}", missing_atoms.join(", ")));
    }
    if !mdat_ranges.is_empty() {
        let total_mdat: u64 = mdat_ranges.iter().map(|(s, e)| e.saturating_sub(*s)).sum();
        summary_lines.push(format!(
            "Physically readable media span(s): {} ({:.2} MiB)",
            mdat_ranges.len(),
            total_mdat as f64 / (1024.0 * 1024.0)
        ));
    }
    if !corruption_regions.is_empty() {
        summary_lines.push(format!(
            "Detected {} suspicious/corrupt regions (zeros or repetitive filler).",
            corruption_regions.len()
        ));
    }
    if !orphan_signatures.is_empty() {
        summary_lines.push(format!(
            "Found {} orphaned box signature(s) during deep boundary scan.",
            orphan_signatures.len()
        ));
    }

    let mut recommended_strategies = Vec::new();
    match container_kind {
        ContainerKind::MpegTs => {
            recommended_strategies.push("Strategy A: FFmpeg container remux (input appears to be MPEG-TS wrapped/named .mp4)".into());
        }
        _ => {
            if has_moov || has_moof {
                recommended_strategies.push("Strategy A: Container repair / FFmpeg lossless remux".into());
            }
            if !orphan_signatures.is_empty() {
                recommended_strategies.push("Strategy A: Salvage surviving moov/moof metadata".into());
            }
            recommended_strategies.push("Strategy B: Reference-assisted Untrunc (if available)".into());
            recommended_strategies.push("Strategy C/D: Custom binary H.264/HEVC NAL bitstream scan & Annex B reconstruction".into());
            recommended_strategies.push("Strategy E: Interleaved AAC audio scan & A/V muxing".into());
            recommended_strategies.push("Strategy F: Discontinuity partial-segment clip extraction (if stream has corruption gaps)".into());
        }
    }

    Ok(DiagnosisReport {
        file_path: path.to_string_lossy().to_string(),
        file_size,
        sha256,
        container_kind,
        atoms,
        has_ftyp,
        has_moov,
        has_moof,
        has_mdat,
        mdat_ranges,
        missing_atoms,
        anomalies,
        orphan_signatures,
        corruption_regions,
        summary_lines,
        recommended_strategies,
    })
}

fn parse_child_boxes(
    file: &mut File,
    start: u64,
    end: u64,
    depth: usize,
    has_mvex: &mut bool,
    anomalies: &mut Vec<String>,
) -> Vec<AtomNode> {
    if depth > 6 {
        return Vec::new();
    }
    let mut children = Vec::new();
    let mut off = start;
    // `meta` has a 4-byte version/flags header before child atoms
    while off + 8 <= end {
        if children.len() > 256 {
            break;
        }
        let mut hdr = [0u8; 8];
        if file.seek(SeekFrom::Start(off)).is_err() || file.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let tag_bytes = [hdr[4], hdr[5], hdr[6], hdr[7]];
        if !is_printable_fourcc(&tag_bytes) {
            break;
        }
        let tag = fourcc_to_string(&tag_bytes);
        let (declared_size, header_size, size_mode) = if size32 == 1 {
            if off + 16 > end {
                break;
            }
            let mut ext = [0u8; 8];
            if file.read_exact(&mut ext).is_err() {
                break;
            }
            (u64::from_be_bytes(ext), 16u64, "extended_64".to_string())
        } else if size32 == 0 {
            (end - off, 8u64, "to_eof_zero".to_string())
        } else {
            (size32 as u64, 8u64, "standard_32".to_string())
        };

        if declared_size < header_size {
            anomalies.push(format!("Malformed child box `{tag}` at {off} (size {declared_size})"));
            break;
        }
        let logical_end = off.saturating_add(declared_size);
        let truncated = logical_end > end;
        let physical_end = logical_end.min(end);
        if tag == "mvex" {
            *has_mvex = true;
        }

        let sub_start = if tag == "meta" {
            off + header_size + 4
        } else {
            off + header_size
        };
        let sub_children = if CONTAINER_BOXES.contains(&tag.as_str()) && physical_end > sub_start {
            parse_child_boxes(file, sub_start, physical_end, depth + 1, has_mvex, anomalies)
        } else {
            Vec::new()
        };

        children.push(AtomNode {
            tag,
            offset: off,
            declared_size,
            physical_end,
            header_size,
            size_mode,
            truncated,
            malformed_reason: if truncated {
                Some("Child extends beyond parent boundary".into())
            } else {
                None
            },
            children: sub_children,
        });

        if truncated || logical_end <= off {
            break;
        }
        off = logical_end;
    }
    children
}

fn scan_anomalies_and_signatures(
    file: &mut File,
    file_size: u64,
    atoms: &[AtomNode],
    progress_cb: Option<&ProgressCallback>,
    start_ts: Instant,
) -> Result<(Vec<CorruptionRegion>, Vec<OrphanSignature>), String> {
    let mut regions: Vec<CorruptionRegion> = Vec::new();
    let mut orphans: Vec<OrphanSignature> = Vec::new();
    let known_offsets: Vec<u64> = atoms.iter().map(|a| a.offset).collect();

    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("Seek error: {e}"))?;

    const CHUNK: usize = 2 * 1024 * 1024;
    let mut buf = vec![0u8; CHUNK + 16];
    let mut carry = 0usize;
    let mut file_pos = 0u64;

    let mut zero_run_start: Option<u64> = None;
    let mut rep_run_start: Option<(u64, u8)> = None;

    let flush_zero = |start_opt: &mut Option<u64>, end: u64, regs: &mut Vec<CorruptionRegion>| {
        if let Some(s) = start_opt.take() {
            if end.saturating_sub(s) >= 4096 {
                regs.push(CorruptionRegion {
                    start: s,
                    end,
                    kind: "zero_filled".into(),
                    detail: format!("{} bytes of continuous 0x00 filler", end - s),
                });
            }
        }
    };

    let flush_rep = |start_opt: &mut Option<(u64, u8)>, end: u64, regs: &mut Vec<CorruptionRegion>| {
        if let Some((s, byte)) = start_opt.take() {
            if byte != 0 && end.saturating_sub(s) >= 4096 {
                regs.push(CorruptionRegion {
                    start: s,
                    end,
                    kind: "repetitive_pattern".into(),
                    detail: format!("{} bytes repeating 0x{byte:02x}", end - s),
                });
            }
        }
    };

    loop {
        let n = file
            .read(&mut buf[carry..carry + CHUNK])
            .map_err(|e| format!("Scan read error: {e}"))?;
        if n == 0 {
            break;
        }
        let valid_len = carry + n;
        let fresh_start = carry;
        let fresh_slice = &buf[fresh_start..valid_len];

        // 1. Zero/Repetitive region scan over newly read bytes
        let mut idx = 0usize;
        while idx < fresh_slice.len() {
            let abs = file_pos + idx as u64;
            let b = fresh_slice[idx];
            if b == 0 {
                flush_rep(&mut rep_run_start, abs, &mut regions);
                if zero_run_start.is_none() {
                    zero_run_start = Some(abs);
                }
            } else {
                flush_zero(&mut zero_run_start, abs, &mut regions);
                match rep_run_start {
                    Some((_, prev_b)) if prev_b == b => {}
                    _ => {
                        flush_rep(&mut rep_run_start, abs, &mut regions);
                        rep_run_start = Some((abs, b));
                    }
                }
            }
            idx += 1;
        }

        // 2. Orphan atom signature scan (step byte-by-byte only near plausible 4CC tags)
        if orphans.len() < 64 && valid_len >= 8 {
            let base_file_off = file_pos.saturating_sub(carry as u64);
            for i in 0..=valid_len - 8 {
                let tag = &buf[i + 4..i + 8];
                if let Some(tag_str) = SIGNATURE_TAGS.iter().find(|t| t.as_bytes() == tag) {
                    let sz = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as u64;
                    let abs_off = base_file_off + i as u64;
                    if !known_offsets.contains(&abs_off) && sz >= 8 && sz <= file_size.saturating_sub(abs_off) + 1024 {
                        // Additional plausibility guard for `moov`/`moof`/`trak`/`stbl`: expect sub-box right after
                        let plausible = if i + 16 <= valid_len && matches!(*tag_str, "moov" | "moof" | "trak" | "stbl") {
                            is_printable_fourcc(&buf[i + 12..i + 16])
                        } else if *tag_str == "ftyp" && i + 12 <= valid_len {
                            is_printable_fourcc(&buf[i + 8..i + 12]) && sz < 512
                        } else {
                            *tag_str == "mdat" && sz > 64
                        };
                        if plausible && !orphans.iter().any(|o| o.offset == abs_off) {
                            orphans.push(OrphanSignature {
                                offset: abs_off,
                                tag: (*tag_str).to_string(),
                                confidence: "structural_header_match".into(),
                            });
                        }
                    }
                }
            }
        }

        file_pos += n as u64;
        if let Some(cb) = progress_cb {
            if (file_pos / (CHUNK as u64)) % 8 == 0 || file_pos >= file_size {
                cb(RecoveryProgress {
                    stage: "diagnosis".into(),
                    current_strategy: "Deep atom & corruption scan".into(),
                    bytes_scanned: file_pos.min(file_size),
                    total_bytes: file_size,
                    recognized_packets: orphans.len() as u64,
                    recovered_frames: 0,
                    elapsed_secs: start_ts.elapsed().as_secs_f64(),
                });
            }
        }

        let keep = valid_len.min(16);
        buf.copy_within(valid_len - keep..valid_len, 0);
        carry = keep;
    }

    flush_zero(&mut zero_run_start, file_size, &mut regions);
    flush_rep(&mut rep_run_start, file_size, &mut regions);

    Ok((regions, orphans))
}

fn detect_mpegts(file: &mut File, file_size: u64) -> bool {
    if file_size < 188 * 4 {
        return false;
    }
    let mut buf = [0u8; 188 * 4];
    if file.seek(SeekFrom::Start(0)).is_err() || file.read_exact(&mut buf).is_err() {
        return false;
    }
    buf[0] == 0x47 && buf[188] == 0x47 && buf[376] == 0x47 && buf[564] == 0x47
}

fn scan_has_annex_b_start_codes(file: &mut File, limit: u64) -> bool {
    let mut buf = vec![0u8; limit as usize];
    if file.seek(SeekFrom::Start(0)).is_err() || file.read_exact(&mut buf).is_err() {
        return false;
    }
    let mut hits = 0usize;
    for w in buf.windows(4) {
        if w == [0x00, 0x00, 0x00, 0x01] {
            hits += 1;
            if hits >= 3 {
                return true;
            }
        }
    }
    false
}

/// Extract decoder config (`avcC` / `hvcC` -> Annex B SPS/PPS/VPS, `esds` AudioSpecificConfig,
/// timescale, VR `st3d`/`sv3d` presence) directly from an ISO BMFF file's `moov` box.
pub fn extract_mp4_codec_configs(path: &Path) -> Result<ExtractedCodecConfig, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Stat failed: {e}"))?;
    let file_size = meta.len();
    let mut file = File::open(path).map_err(|e| format!("Open failed: {e}"))?;

    let mut cfg = ExtractedCodecConfig {
        avcc_raw: None,
        hvcc_raw: None,
        esds_asc: None,
        nal_length_size: None,
        parameter_sets_annex_b: Vec::new(),
        timescale: None,
        vr_info: VrMetadataInfo::default(),
        sample_sizes_sample: Vec::new(),
    };

    // Walk top-level atoms to locate `moov`
    let mut off = 0u64;
    while off + 8 <= file_size {
        let mut hdr = [0u8; 8];
        file.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        if file.read_exact(&mut hdr).is_err() {
            break;
        }
        let s32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let tag = fourcc_to_string(&[hdr[4], hdr[5], hdr[6], hdr[7]]);
        let (size, hlen) = if s32 == 1 {
            let mut ext = [0u8; 8];
            if file.read_exact(&mut ext).is_err() {
                break;
            }
            (u64::from_be_bytes(ext), 16u64)
        } else if s32 == 0 {
            (file_size - off, 8u64)
        } else {
            (s32 as u64, 8u64)
        };
        if size < hlen || off + size > file_size {
            break;
        }
        if tag == "moov" && size <= 64 * 1024 * 1024 {
            let mut moov_bytes = vec![0u8; (size - hlen) as usize];
            file.seek(SeekFrom::Start(off + hlen)).map_err(|e| e.to_string())?;
            file.read_exact(&mut moov_bytes).map_err(|e| e.to_string())?;
            parse_moov_payload(&moov_bytes, &mut cfg);
            break;
        }
        off += size;
    }

    Ok(cfg)
}

fn parse_moov_payload(data: &[u8], cfg: &mut ExtractedCodecConfig) {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let s32 = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let tag = &data[off + 4..off + 8];
        let (size, hlen) = if s32 == 1 && off + 16 <= data.len() {
            let s64 = u64::from_be_bytes(data[off + 8..off + 16].try_into().unwrap()) as usize;
            (s64, 16usize)
        } else if s32 == 0 {
            (data.len() - off, 8usize)
        } else {
            (s32, 8usize)
        };
        if size < hlen || off + size > data.len() {
            break;
        }
        let body = &data[off + hlen..off + size];
        match tag {
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"edts" => {
                parse_moov_payload(body, cfg);
            }
            b"mdhd" => {
                if cfg.timescale.is_none() && body.len() >= 16 {
                    let version = body[0];
                    if version == 0 && body.len() >= 16 {
                        let ts = u32::from_be_bytes([body[12], body[13], body[14], body[15]]);
                        if ts > 0 {
                            cfg.timescale = Some(ts);
                        }
                    } else if version == 1 && body.len() >= 24 {
                        let ts = u32::from_be_bytes([body[20], body[21], body[22], body[23]]);
                        if ts > 0 {
                            cfg.timescale = Some(ts);
                        }
                    }
                }
            }
            b"stsd" => {
                // FullBox: 4 bytes version/flags + 4 bytes entry_count
                if body.len() > 8 {
                    parse_stsd_entries(&body[8..], cfg);
                }
            }
            b"stsz" => {
                if cfg.sample_sizes_sample.is_empty() && body.len() >= 12 {
                    let sample_size = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                    let count = u32::from_be_bytes([body[8], body[9], body[10], body[11]]) as usize;
                    if sample_size == 0 {
                        let take = count.min(64).min((body.len() - 12) / 4);
                        for i in 0..take {
                            let base = 12 + i * 4;
                            cfg.sample_sizes_sample.push(u32::from_be_bytes([
                                body[base],
                                body[base + 1],
                                body[base + 2],
                                body[base + 3],
                            ]));
                        }
                    }
                }
            }
            b"st3d" => {
                cfg.vr_info.raw_st3d_detected = true;
                cfg.vr_info.is_stereoscopic = true;
                if body.len() >= 5 {
                    cfg.vr_info.stereo_mode = match body[4] {
                        1 => Some("top_bottom".into()),
                        2 => Some("side_by_side".into()),
                        _ => cfg.vr_info.stereo_mode.clone(),
                    };
                }
            }
            b"sv3d" => {
                cfg.vr_info.raw_sv3d_detected = true;
                if cfg.vr_info.projection.is_none() {
                    cfg.vr_info.projection = Some("equirectangular".into());
                }
            }
            _ => {}
        }
        off += size;
    }
}

fn parse_stsd_entries(data: &[u8], cfg: &mut ExtractedCodecConfig) {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let size = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let tag = &data[off + 4..off + 8];
        if size < 8 || off + size > data.len() {
            break;
        }
        let body = &data[off + 8..off + size];
        match tag {
            b"avc1" | b"avc3" | b"hvc1" | b"hev1" => {
                // VisualSampleEntry fixed header is 78 bytes after size+type (86 total - 8)
                if body.len() > 78 {
                    parse_sample_entry_children(&body[78..], cfg);
                }
            }
            b"mp4a" => {
                // AudioSampleEntry fixed header is 28 bytes after size+type
                if body.len() > 28 {
                    parse_sample_entry_children(&body[28..], cfg);
                }
            }
            _ => {}
        }
        off += size;
    }
}

fn parse_sample_entry_children(data: &[u8], cfg: &mut ExtractedCodecConfig) {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let size = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let tag = &data[off + 4..off + 8];
        if size < 8 || off + size > data.len() {
            break;
        }
        let body = &data[off + 8..off + size];
        match tag {
            b"avcC" => {
                cfg.avcc_raw = Some(body.to_vec());
                if let Some((nal_len, annex_b)) = parse_avcc_to_annex_b(body) {
                    cfg.nal_length_size = Some(nal_len);
                    cfg.parameter_sets_annex_b = annex_b;
                }
            }
            b"hvcC" => {
                cfg.hvcc_raw = Some(body.to_vec());
                if let Some((nal_len, annex_b)) = parse_hvcc_to_annex_b(body) {
                    cfg.nal_length_size = Some(nal_len);
                    cfg.parameter_sets_annex_b = annex_b;
                }
            }
            b"esds" => {
                if let Some(asc) = parse_esds_asc(body) {
                    cfg.esds_asc = Some(asc);
                }
            }
            b"st3d" => {
                cfg.vr_info.raw_st3d_detected = true;
                cfg.vr_info.is_stereoscopic = true;
                if body.len() >= 5 {
                    cfg.vr_info.stereo_mode = match body[4] {
                        1 => Some("top_bottom".into()),
                        2 => Some("side_by_side".into()),
                        _ => cfg.vr_info.stereo_mode.clone(),
                    };
                }
            }
            b"sv3d" => {
                cfg.vr_info.raw_sv3d_detected = true;
                if cfg.vr_info.projection.is_none() {
                    cfg.vr_info.projection = Some("equirectangular".into());
                }
            }
            _ => {}
        }
        off += size;
    }
}

pub fn parse_avcc_to_annex_b(avcc: &[u8]) -> Option<(u8, Vec<u8>)> {
    if avcc.len() < 7 {
        return None;
    }
    let nal_len_size = (avcc[4] & 0x03) + 1;
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut pos = 6usize;
    let mut out = Vec::new();
    for _ in 0..num_sps {
        if pos + 2 > avcc.len() {
            return None;
        }
        let len = u16::from_be_bytes([avcc[pos], avcc[pos + 1]]) as usize;
        pos += 2;
        if pos + len > avcc.len() {
            return None;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&avcc[pos..pos + len]);
        pos += len;
    }
    if pos >= avcc.len() {
        return Some((nal_len_size, out));
    }
    let num_pps = avcc[pos] as usize;
    pos += 1;
    for _ in 0..num_pps {
        if pos + 2 > avcc.len() {
            break;
        }
        let len = u16::from_be_bytes([avcc[pos], avcc[pos + 1]]) as usize;
        pos += 2;
        if pos + len > avcc.len() {
            break;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&avcc[pos..pos + len]);
        pos += len;
    }
    if out.is_empty() {
        None
    } else {
        Some((nal_len_size, out))
    }
}

pub fn parse_hvcc_to_annex_b(hvcc: &[u8]) -> Option<(u8, Vec<u8>)> {
    if hvcc.len() < 23 {
        return None;
    }
    let nal_len_size = (hvcc[21] & 0x03) + 1;
    let num_arrays = hvcc[22] as usize;
    let mut pos = 23usize;
    let mut out = Vec::new();
    for _ in 0..num_arrays {
        if pos + 3 > hvcc.len() {
            break;
        }
        let _nal_type = hvcc[pos] & 0x3F;
        let count = u16::from_be_bytes([hvcc[pos + 1], hvcc[pos + 2]]) as usize;
        pos += 3;
        for _ in 0..count {
            if pos + 2 > hvcc.len() {
                break;
            }
            let len = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
            pos += 2;
            if pos + len > hvcc.len() {
                break;
            }
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            out.extend_from_slice(&hvcc[pos..pos + len]);
            pos += len;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some((nal_len_size, out))
    }
}

fn parse_esds_asc(body: &[u8]) -> Option<Vec<u8>> {
    // FullBox: 4 bytes version/flags, then ES_Descriptor hierarchy (tag 0x03 -> 0x04 -> 0x05)
    if body.len() < 8 {
        return None;
    }
    let mut pos = 4usize;
    while pos + 2 < body.len() {
        let tag = body[pos];
        pos += 1;
        // Variable length descriptor size (up to 4 bytes with high bit continuation)
        let mut dlen = 0usize;
        for _ in 0..4 {
            if pos >= body.len() {
                return None;
            }
            let b = body[pos];
            pos += 1;
            dlen = (dlen << 7) | ((b & 0x7F) as usize);
            if (b & 0x80) == 0 {
                break;
            }
        }
        match tag {
            0x03 => {
                // ES_Descriptor: skip 2B ES_ID + 1B flags (+ optional fields)
                if pos + 3 > body.len() {
                    return None;
                }
                let flags = body[pos + 2];
                pos += 3;
                if (flags & 0x80) != 0 {
                    pos += 2;
                }
                if (flags & 0x40) != 0 && pos < body.len() {
                    let urllen = body[pos] as usize;
                    pos += 1 + urllen;
                }
                if (flags & 0x20) != 0 {
                    pos += 2;
                }
            }
            0x04 => {
                // DecoderConfigDescriptor: 13 bytes fixed header before sub-descriptors
                pos += 13;
            }
            0x05 => {
                // DecoderSpecificInfo = AudioSpecificConfig
                if pos + dlen <= body.len() && dlen >= 2 {
                    return Some(body[pos..pos + dlen].to_vec());
                }
                return None;
            }
            _ => {
                pos += dlen;
            }
        }
    }
    None
}

/// Create a separate working copy with patched `mdat` size matching physical EOF
/// (never touches the original damaged file!).
pub fn write_patched_mdat_copy(src: &Path, dst: &Path, diag: &DiagnosisReport) -> Result<bool, String> {
    let truncated_mdat = diag.atoms.iter().find(|a| a.tag == "mdat" && a.truncated && a.header_size == 8);
    let Some(mdat) = truncated_mdat else {
        return Ok(false);
    };
    let fixed_size = diag.file_size.saturating_sub(mdat.offset);
    if fixed_size > u32::MAX as u64 || fixed_size < 8 {
        return Ok(false);
    }

    let mut in_f = File::open(src).map_err(|e| format!("Open src failed: {e}"))?;
    let mut out_f = File::create(dst).map_err(|e| format!("Create dst failed: {e}"))?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total_written = 0u64;
    loop {
        let n = in_f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        // Patch the 4-byte mdat size at mdat.offset
        let chunk_end = total_written + n as u64;
        if mdat.offset >= total_written && mdat.offset + 4 <= chunk_end {
            let rel = (mdat.offset - total_written) as usize;
            buf[rel..rel + 4].copy_from_slice(&(fixed_size as u32).to_be_bytes());
        }
        out_f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        total_written += n as u64;
    }
    // Patch edge case where 4-byte size straddles buffer boundary (unlikely at offset 32, but safe)
    out_f.seek(SeekFrom::Start(mdat.offset)).map_err(|e| e.to_string())?;
    out_f
        .write_all(&(fixed_size as u32).to_be_bytes())
        .map_err(|e| e.to_string())?;
    out_f.flush().map_err(|e| e.to_string())?;
    Ok(true)
}

fn is_printable_fourcc(b: &[u8]) -> bool {
    b.len() == 4 && b.iter().all(|c| (0x20..=0x7E).contains(c))
}

/// Inject an ISO BMFF `st3d` Stereo 3D box (`1` = top-bottom, `2` = side-by-side) into an
/// existing MP4 file's video visual sample entry (`avc1`/`hvc1`), adjusting parent sizes and
/// `stco`/`co64` chunk offsets if `moov` precedes `mdat`. Never fails destructively; returns Ok(true) if added.
pub fn inject_st3d_box_into_mp4(mp4_path: &Path, stereo_mode: &str) -> Result<bool, String> {
    let mode_byte: u8 = match stereo_mode {
        "top_bottom" => 1,
        "side_by_side" | "left_right" => 2,
        _ => return Ok(false),
    };
    let mut data = std::fs::read(mp4_path).map_err(|e| format!("Read failed: {e}"))?;
    let mut off = 0usize;
    let mut moov_off = None;
    let mut moov_end = None;
    let mut mdat_off = None;
    while off + 8 <= data.len() {
        let s32 = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        if s32 < 8 || off + s32 > data.len() {
            break;
        }
        let tag = &data[off + 4..off + 8];
        if tag == b"moov" {
            moov_off = Some(off);
            moov_end = Some(off + s32);
        } else if tag == b"mdat" {
            mdat_off = Some(off);
        }
        off += s32;
    }
    let (Some(ms), Some(me)) = (moov_off, moov_end) else {
        return Ok(false);
    };
    // Locate first VisualSampleEntry (`avc1`/`avc3`/`hvc1`/`hev1`) inside moov (past 86B header)
    let mut target_insert: Option<usize> = None;
    let mut i = ms;
    while i + 8 <= me {
        let tag = &data[i + 4..i + 8];
        if matches!(tag, b"avc1" | b"avc3" | b"hvc1" | b"hev1") && i >= 4 {
            let entry_len = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            if entry_len > 86 && i + entry_len <= me {
                // Check st3d not already inside
                if !data[i..i + entry_len].windows(4).any(|w| w == b"st3d") {
                    target_insert = Some(i + entry_len);
                }
                break;
            }
        }
        i += 1;
    }
    let Some(insert_at) = target_insert else {
        return Ok(false);
    };
    let st3d_box: [u8; 13] = [
        0, 0, 0, 13, b's', b't', b'3', b'd', 0, 0, 0, 0, mode_byte,
    ];
    // Bump ancestor boxes enclosing `insert_at` within [ms..me)
    bump_enclosing_boxes(&mut data, ms, me, insert_at, 13);
    // If `moov` appears before `mdat`, bump all `stco`/`co64` chunk offsets by +13
    if let Some(md) = mdat_off {
        if ms < md {
            shift_chunk_offsets(&mut data, ms, me, md, 13);
        }
    }
    data.splice(insert_at..insert_at, st3d_box);
    std::fs::write(mp4_path, &data).map_err(|e| format!("Write failed: {e}"))?;
    Ok(true)
}

fn bump_enclosing_boxes(data: &mut [u8], start: usize, end: usize, target: usize, delta: u32) {
    let mut off = start;
    while off + 8 <= end {
        let s = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        if s < 8 || off + s > end {
            break;
        }
        let tag = &data[off + 4..off + 8].to_vec();
        if target >= off && target <= off + s {
            let new_s = (s as u32) + delta;
            data[off..off + 4].copy_from_slice(&new_s.to_be_bytes());
            if tag.as_slice() == b"stsd" {
                bump_enclosing_boxes(data, off + 16, off + s, target, delta);
            } else if matches!(tag.as_slice(), b"avc1" | b"avc3" | b"hvc1" | b"hev1") {
                return;
            } else {
                bump_enclosing_boxes(data, off + 8, off + s, target, delta);
            }
            return;
        }
        off += s;
    }
}

fn shift_chunk_offsets(data: &mut [u8], start: usize, end: usize, mdat_off: usize, delta: u64) {
    let mut off = start;
    while off + 8 <= end {
        let s = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        if s < 8 || off + s > end {
            break;
        }
        let tag = &data[off + 4..off + 8].to_vec();
        match tag.as_slice() {
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"moov" => {
                shift_chunk_offsets(data, off + 8, off + s, mdat_off, delta);
            }
            b"stco" if off + 16 <= off + s => {
                let cnt = u32::from_be_bytes(data[off + 12..off + 16].try_into().unwrap()) as usize;
                for k in 0..cnt {
                    let p = off + 16 + k * 4;
                    if p + 4 > off + s {
                        break;
                    }
                    let val = u32::from_be_bytes(data[p..p + 4].try_into().unwrap());
                    if (val as usize) >= mdat_off {
                        data[p..p + 4].copy_from_slice(&(val + delta as u32).to_be_bytes());
                    }
                }
            }
            b"co64" if off + 16 <= off + s => {
                let cnt = u32::from_be_bytes(data[off + 12..off + 16].try_into().unwrap()) as usize;
                for k in 0..cnt {
                    let p = off + 16 + k * 8;
                    if p + 8 > off + s {
                        break;
                    }
                    let val = u64::from_be_bytes(data[p..p + 8].try_into().unwrap());
                    if (val as usize) >= mdat_off {
                        data[p..p + 8].copy_from_slice(&(val + delta).to_be_bytes());
                    }
                }
            }
            _ => {}
        }
        off += s;
    }
}

fn is_plausible_top_atom(b: &[u8]) -> bool {
    const KNOWN_TOP: &[&[u8; 4]] = &[b"ftyp", b"free", b"skip", b"wide", b"mdat", b"moov", b"moof", b"uuid"];
    KNOWN_TOP.iter().any(|k| *k == b)
}

fn fourcc_to_string(b: &[u8; 4]) -> String {
    b.iter()
        .map(|c| {
            if c.is_ascii_graphic() || *c == b' ' {
                *c as char
            } else {
                '.'
            }
        })
        .collect()
}
