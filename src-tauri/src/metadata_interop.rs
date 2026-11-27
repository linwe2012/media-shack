use crate::catalog::{refresh_fts_for_asset, to_long_fs_path};
use crate::db::Database;
use crate::organize::assign_tags_bulk;
use crate::platform::capture_file_identity;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Manual,
    PreferDb,
    PreferExternal,
    Merge,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableSidecar {
    pub schema_version: u32,
    pub asset_id: String,
    pub file_key: String,
    pub filename: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub recorded_at: Option<String>,
    pub tags: Vec<String>,
    pub collections: Vec<String>,
    pub blake3_hash: Option<String>,
    pub sha256_hash: Option<String>,
    pub head_hash: String,
    pub exported_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataSyncReport {
    pub file_key: String,
    pub target: String,
    pub status: String, // "synced" | "unsupported" | "conflict" | "error"
    pub message: String,
    pub db_tags: Vec<String>,
    pub external_tags: Vec<String>,
}

pub fn discover_exiftool(explicit_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit_path {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\ExifTool\ExifTool.exe"),
            PathBuf::from(r"C:\Program Files\ExifTool\exiftool.exe"),
            PathBuf::from(r"C:\exiftool\exiftool.exe"),
        ];
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local_app).join("Programs").join("ExifTool");
            candidates.push(base.join("ExifTool.exe"));
            candidates.push(base.join("exiftool.exe"));
        }
        for c in candidates {
            if c.is_file() {
                return Some(c);
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                for name in ["ExifTool.exe", "exiftool.exe"] {
                    let p = dir.join(name);
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            let p = Path::new(dir).join("exiftool");
            if p.is_file() {
                return Some(p);
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let p = dir.join("exiftool");
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

// ============================================================================
// 7.1 Windows Property System (`System.Keywords` / `PKEY_Keywords` / `IPropertyStore`)
// ============================================================================

#[cfg(target_os = "windows")]
pub fn windows_read_keywords(path: &Path) -> Result<Vec<String>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringVectorAlloc;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName, GPS_DEFAULT,
        PROPERTYKEY,
    };

    unsafe {
        let hr_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let did_init = hr_init.is_ok();
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let wide_key: Vec<u16> = "System.Keywords".encode_utf16().chain(std::iter::once(0)).collect();

        let res = (|| -> Result<Vec<String>, String> {
            let mut pkey = PROPERTYKEY::default();
            PSGetPropertyKeyFromName(PCWSTR(wide_key.as_ptr()), &mut pkey)
                .map_err(|e| format!("PSGetPropertyKeyFromName failed: {e}"))?;

            let store: IPropertyStore = SHGetPropertyStoreFromParsingName(
                PCWSTR(wide_path.as_ptr()),
                None,
                GPS_DEFAULT,
            )
            .map_err(|e| format!("Property handler unavailable for reading: {e}"))?;

            let pv = store
                .GetValue(&pkey)
                .map_err(|e| format!("IPropertyStore::GetValue failed: {e}"))?;

            let mut count = 0u32;
            let mut ptrs: *mut windows::core::PWSTR = std::ptr::null_mut();
            let mut out = Vec::new();
            if PropVariantToStringVectorAlloc(&pv, &mut ptrs, &mut count).is_ok() && !ptrs.is_null() {
                let slice = std::slice::from_raw_parts(ptrs, count as usize);
                for pw in slice {
                    if let Ok(s) = pw.to_string() {
                        let t = s.trim().to_string();
                        if !t.is_empty() {
                            out.push(t);
                        }
                    }
                    CoTaskMemFree(Some(pw.0 as *const _));
                }
                CoTaskMemFree(Some(ptrs as *const _));
            }
            Ok(out)
        })();

        if did_init {
            CoUninitialize();
        }
        res
    }
}

#[cfg(target_os = "windows")]
pub fn windows_write_keywords(path: &Path, keywords: &[String]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::StructuredStorage::InitPropVariantFromStringVector;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName, GPS_READWRITE,
        PROPERTYKEY,
    };

    unsafe {
        let hr_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let did_init = hr_init.is_ok();
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let wide_key: Vec<u16> = "System.Keywords".encode_utf16().chain(std::iter::once(0)).collect();

        let res = (|| -> Result<(), String> {
            let mut pkey = PROPERTYKEY::default();
            PSGetPropertyKeyFromName(PCWSTR(wide_key.as_ptr()), &mut pkey)
                .map_err(|e| format!("PSGetPropertyKeyFromName failed: {e}"))?;

            let store: IPropertyStore = SHGetPropertyStoreFromParsingName(
                PCWSTR(wide_path.as_ptr()),
                None,
                GPS_READWRITE,
            )
            .map_err(|e| format!("Windows property handler does not support writing tags for this format: {e}"))?;

            let wide_vecs: Vec<Vec<u16>> = keywords
                .iter()
                .map(|k| k.encode_utf16().chain(std::iter::once(0)).collect())
                .collect();
            let ptrs: Vec<PCWSTR> = wide_vecs.iter().map(|v| PCWSTR(v.as_ptr())).collect();
            let pv = InitPropVariantFromStringVector(if ptrs.is_empty() { None } else { Some(&ptrs) })
                .map_err(|e| format!("InitPropVariantFromStringVector failed: {e}"))?;

            store
                .SetValue(&pkey, &pv)
                .map_err(|e| format!("Windows property handler rejected System.Keywords write: {e}"))?;
            store
                .Commit()
                .map_err(|e| format!("IPropertyStore::Commit failed: {e}"))?;
            Ok(())
        })();

        if did_init {
            CoUninitialize();
        }
        res
    }
}

#[cfg(not(target_os = "windows"))]
pub fn windows_read_keywords(_path: &Path) -> Result<Vec<String>, String> {
    Err("Windows Property System is only available on Windows.".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn windows_write_keywords(_path: &Path, _keywords: &[String]) -> Result<(), String> {
    Err("Windows Property System is only available on Windows.".to_string())
}

// ============================================================================
// 7.2 macOS Finder Tags (`com.apple.metadata:_kMDItemUserTags`)
// ============================================================================

#[cfg(target_os = "macos")]
pub fn macos_read_finder_tags(path: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("xattr")
        .args(["-px", "com.apple.metadata:_kMDItemUserTags"])
        .arg(path)
        .output()
        .map_err(|e| format!("Failed to invoke xattr: {e}"))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let hex_str: String = String::from_utf8_lossy(&out.stdout)
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    let mut bytes = Vec::new();
    for i in (0..hex_str.len()).step_by(2) {
        if let Some(chunk) = hex_str.get(i..i + 2) {
            if let Ok(b) = u8::from_str_radix(chunk, 16) {
                bytes.push(b);
            }
        }
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    // Parse with plutil -> json for full binary plist compatibility
    let tmp = std::env::temp_dir().join(format!("ms_tags_{}.plist", Uuid::new_v4()));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    let json_out = Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(&tmp)
        .output();
    let _ = std::fs::remove_file(&tmp);
    let Ok(jo) = json_out else {
        return Ok(Vec::new());
    };
    if !jo.status.success() {
        return Ok(Vec::new());
    }
    let arr: Vec<String> = serde_json::from_slice(&jo.stdout).unwrap_or_default();
    Ok(arr
        .into_iter()
        .map(|s| s.split('\n').next().unwrap_or(&s).to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

#[cfg(target_os = "macos")]
pub fn macos_write_finder_tags(path: &Path, tags: &[String]) -> Result<(), String> {
    let json_bytes = serde_json::to_vec(tags).map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join(format!("ms_tags_w_{}.json", Uuid::new_v4()));
    std::fs::write(&tmp, &json_bytes).map_err(|e| e.to_string())?;
    let bin_out = Command::new("plutil")
        .args(["-convert", "binary1", "-o", "-"])
        .arg(&tmp)
        .output();
    let _ = std::fs::remove_file(&tmp);
    let bin = bin_out.map_err(|e| format!("plutil failed: {e}"))?;
    if !bin.status.success() {
        return Err("Failed converting Finder tags to binary plist.".to_string());
    }
    let hex: String = bin.stdout.iter().map(|b| format!("{b:02x}")).collect();
    let set_out = Command::new("xattr")
        .args(["-wx", "com.apple.metadata:_kMDItemUserTags", &hex])
        .arg(path)
        .output()
        .map_err(|e| format!("xattr write failed: {e}"))?;
    if !set_out.status.success() {
        let err = String::from_utf8_lossy(&set_out.stderr);
        return Err(format!("Filesystem does not support Finder tag xattr: {err}"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn macos_read_finder_tags(_path: &Path) -> Result<Vec<String>, String> {
    Err("macOS Finder tags are only available on macOS.".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn macos_write_finder_tags(_path: &Path, _tags: &[String]) -> Result<(), String> {
    Err("macOS Finder tags are only available on macOS.".to_string())
}

// ============================================================================
// 7.3 Embedded Metadata (ExifTool + lossless FFmpeg fallback)
// ============================================================================

pub fn read_embedded_keywords(exiftool: Option<&Path>, path: &Path) -> Result<Vec<String>, String> {
    let Some(et) = exiftool else {
        return Err("ExifTool not installed or configured.".to_string());
    };
    let mut cmd = Command::new(et);
    cmd.args(["-json", "-Keywords", "-Subject", "-Category", "-XPKeywords", "-Artist", "-Title"])
        .arg(to_long_fs_path(path));
    crate::analysis::apply_no_window(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Failed running exiftool: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut tags = Vec::new();
    if let Some(obj) = arr.first().and_then(|v| v.as_object()) {
        for key in ["Keywords", "Subject", "Category"] {
            if let Some(val) = obj.get(key) {
                match val {
                    serde_json::Value::Array(items) => {
                        for it in items {
                            if let Some(s) = it.as_str() {
                                if !s.trim().is_empty() && !tags.contains(&s.trim().to_string()) {
                                    tags.push(s.trim().to_string());
                                }
                            }
                        }
                    }
                    serde_json::Value::String(s) => {
                        for part in s.split(&[',', ';'][..]) {
                            let t = part.trim();
                            if !t.is_empty() && !tags.contains(&t.to_string()) {
                                tags.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(tags)
}

pub fn write_embedded_metadata_lossless(
    db: &Database,
    exiftool: Option<&Path>,
    ffmpeg_path: Option<&Path>,
    file_key: &str,
    tags: &[String],
    title: Option<&str>,
    creator: Option<&str>,
) -> Result<String, String> {
    let (file_path, ext): (String, String) = {
        let conn = db.lock_conn()?;
        conn.query_row(
            "SELECT path, extension FROM file_locations WHERE file_key = ?1 AND is_current = 1 LIMIT 1",
            params![file_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("Catalog file not found: {e}"))?
    };
    let target = PathBuf::from(&file_path);
    let long_target = to_long_fs_path(&target);
    if !long_target.exists() {
        return Err("File is offline or missing.".to_string());
    }

    let method_used = if let Some(et) = exiftool {
        // ExifTool updates container metadata in-place without re-encoding video/audio streams
        let mut cmd = Command::new(et);
        cmd.arg("-overwrite_original");
        if let Some(t) = title {
            cmd.arg(format!("-Title={t}"));
        }
        if let Some(c) = creator {
            cmd.arg(format!("-Artist={c}"));
        }
        cmd.arg("-Keywords=");
        cmd.arg("-Subject=");
        for tag in tags {
            cmd.arg(format!("-Keywords+={tag}"));
            cmd.arg(format!("-Subject+={tag}"));
        }
        cmd.arg(&long_target);
        crate::analysis::apply_no_window(&mut cmd);
        let out = cmd.output().map_err(|e| format!("ExifTool launch failed: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("ExifTool refused write for .{ext}: {stderr}"));
        }
        "exiftool"
    } else if let Some(ff) = ffmpeg_path {
        // Lossless remux fallback preserving all streams, color, chapters, and VR side data
        let tmp = target.with_file_name(format!(".mediashack-meta-{}.{ext}", Uuid::new_v4()));
        let mut cmd = Command::new(ff);
        cmd.args([
            "-hide_banner",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(&long_target)
        .args(["-map", "0", "-map_metadata", "0", "-map_chapters", "0", "-c", "copy"]);
        if let Some(t) = title {
            cmd.args(["-metadata", &format!("title={t}")]);
        }
        if let Some(c) = creator {
            cmd.args(["-metadata", &format!("artist={c}")]);
        }
        if !tags.is_empty() {
            cmd.args(["-metadata", &format!("keywords={}", tags.join(";"))]);
        }
        cmd.arg(to_long_fs_path(&tmp));
        crate::analysis::apply_no_window(&mut cmd);
        let out = cmd.output().map_err(|e| format!("FFmpeg metadata remux failed: {e}"))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(to_long_fs_path(&tmp));
            return Err(format!(
                "FFmpeg lossless metadata remux failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        std::fs::rename(to_long_fs_path(&tmp), &long_target)
            .map_err(|e| format!("Failed replacing file after metadata remux: {e}"))?;
        "ffmpeg_stream_copy"
    } else {
        return Err("Neither ExifTool nor FFmpeg is available to write embedded metadata.".to_string());
    };

    // Because metadata edits alter container bytes, update identity & invalidate old content hashes
    if let Ok(fp) = capture_file_identity(&target) {
        let now = Utc::now().timestamp();
        let conn = db.lock_conn()?;
        conn.execute(
            "UPDATE media_files SET
                size_bytes = ?1, mtime_ns = ?2, head_hash = ?3,
                blake3_hash = NULL, sha256_hash = NULL, integrity_status = 'unverified', updated_at = ?4
             WHERE file_key = ?5",
            params![fp.size_bytes as i64, fp.mtime_ns, fp.head_hash, now, file_key],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(format!("Updated embedded metadata via {method_used} (streams preserved losslessly)."))
}

// ============================================================================
// 7.4 Portable Sidecars (JSON & XMP) + Conflict Resolution
// ============================================================================

pub fn sidecar_json_path(media_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.mediashack.json", media_path.display()))
}

pub fn sidecar_xmp_path(media_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.xmp", media_path.display()))
}

pub fn export_sidecars_for_file(
    db: &Database,
    file_key: &str,
    write_json: bool,
    write_xmp: bool,
) -> Result<Vec<String>, String> {
    let mut items = crate::catalog::load_entries_by_file_keys(db, &[file_key.to_string()])?;
    let entry = items
        .pop()
        .ok_or_else(|| "Catalog file not found.".to_string())?;

    let tags: Vec<String> = entry
        .tags
        .iter()
        .filter(|t| t.status == "active")
        .map(|t| t.name.clone())
        .collect();
    let now = Utc::now().timestamp();
    let media_p = Path::new(&entry.path);
    let mut written = Vec::new();

    if write_json {
        let sidecar = PortableSidecar {
            schema_version: 1,
            asset_id: entry.asset_id.clone(),
            file_key: entry.file_key.clone(),
            filename: entry.filename.clone(),
            title: entry.title.clone(),
            description: entry.description.clone(),
            creator: entry.creator.clone(),
            recorded_at: entry.recorded_at.clone(),
            tags: tags.clone(),
            collections: Vec::new(),
            blake3_hash: entry.blake3_hash.clone(),
            sha256_hash: entry.sha256_hash.clone(),
            head_hash: entry.head_hash.clone(),
            exported_at: now,
        };
        let jp = sidecar_json_path(media_p);
        let bytes = serde_json::to_string_pretty(&sidecar).map_err(|e| e.to_string())?;
        std::fs::write(to_long_fs_path(&jp), bytes)
            .map_err(|e| format!("Failed writing JSON sidecar: {e}"))?;
        written.push(jp.to_string_lossy().to_string());
    }

    if write_xmp {
        let xp = sidecar_xmp_path(media_p);
        let xmp = render_xmp_sidecar(&entry, &tags);
        std::fs::write(to_long_fs_path(&xp), xmp)
            .map_err(|e| format!("Failed writing XMP sidecar: {e}"))?;
        written.push(xp.to_string_lossy().to_string());
    }

    let conn = db.lock_conn()?;
    conn.execute(
        "INSERT INTO metadata_sync_state (id, file_key, target, sync_status, last_synced_hash, updated_at)
         VALUES (?1, ?2, 'sidecar', 'synced', ?3, ?4)
         ON CONFLICT(file_key, target) DO UPDATE SET sync_status = 'synced', updated_at = excluded.updated_at",
        params![Uuid::new_v4().to_string(), file_key, entry.blake3_hash, now],
    )
    .map_err(|e| e.to_string())?;

    Ok(written)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_xmp_sidecar(entry: &crate::catalog::CatalogEntry, tags: &[String]) -> String {
    let subjects: String = tags
        .iter()
        .map(|t| format!("     <rdf:li>{}</rdf:li>", xml_escape(t)))
        .collect::<Vec<_>>()
        .join("\n");
    let title = entry.title.as_deref().unwrap_or("");
    let creator = entry.creator.as_deref().unwrap_or("");
    let desc = entry.description.as_deref().unwrap_or("");
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:mediashack="https://mediashack.local/ns/1.0/">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:title>
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:description>
   <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>
   <dc:subject>
    <rdf:Bag>
{}
    </rdf:Bag>
   </dc:subject>
   <mediashack:AssetId>{}</mediashack:AssetId>
   <mediashack:Blake3>{}</mediashack:Blake3>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        xml_escape(title),
        xml_escape(desc),
        xml_escape(creator),
        subjects,
        xml_escape(&entry.asset_id),
        xml_escape(entry.blake3_hash.as_deref().unwrap_or("")),
    )
}

pub fn import_sidecar_for_file(
    db: &Database,
    file_key: &str,
    policy: ConflictPolicy,
) -> Result<MetadataSyncReport, String> {
    let mut items = crate::catalog::load_entries_by_file_keys(db, &[file_key.to_string()])?;
    let entry = items
        .pop()
        .ok_or_else(|| "Catalog file not found.".to_string())?;

    let jp = sidecar_json_path(Path::new(&entry.path));
    let xp = sidecar_xmp_path(Path::new(&entry.path));
    let (sc, target_name) = if to_long_fs_path(&jp).exists() {
        let raw = std::fs::read_to_string(to_long_fs_path(&jp)).map_err(|e| e.to_string())?;
        let parsed: PortableSidecar =
            serde_json::from_str(&raw).map_err(|e| format!("Invalid sidecar JSON: {e}"))?;
        (parsed, "json_sidecar")
    } else if to_long_fs_path(&xp).exists() {
        let raw = std::fs::read_to_string(to_long_fs_path(&xp)).map_err(|e| e.to_string())?;
        (parse_xmp_sidecar(&raw, &entry), "xmp_sidecar")
    } else {
        return Ok(MetadataSyncReport {
            file_key: file_key.to_string(),
            target: "sidecar".to_string(),
            status: "unsupported".to_string(),
            message: "No .mediashack.json or .xmp sidecar found next to media file.".to_string(),
            db_tags: entry.tags.iter().map(|t| t.name.clone()).collect(),
            external_tags: Vec::new(),
        });
    };
    let db_set: HashSet<String> = entry
        .tags
        .iter()
        .filter(|t| t.status == "active")
        .map(|t| t.name.clone())
        .collect();
    let ext_set: HashSet<String> = sc.tags.iter().cloned().collect();

    if db_set != ext_set && !db_set.is_empty() && !ext_set.is_empty() && policy == ConflictPolicy::Manual {
        let conflict = serde_json::json!({
            "db_tags": db_set.iter().collect::<Vec<_>>(),
            "external_tags": sc.tags,
        })
        .to_string();
        let conn = db.lock_conn()?;
        conn.execute(
            "INSERT INTO metadata_sync_state (id, file_key, target, sync_status, conflict_json, updated_at)
             VALUES (?1, ?2, ?3, 'conflict', ?4, ?5)
             ON CONFLICT(file_key, target) DO UPDATE SET sync_status = 'conflict', conflict_json = excluded.conflict_json, updated_at = excluded.updated_at",
            params![Uuid::new_v4().to_string(), file_key, target_name, conflict, Utc::now().timestamp()],
        )
        .map_err(|e| e.to_string())?;
        return Ok(MetadataSyncReport {
            file_key: file_key.to_string(),
            target: target_name.to_string(),
            status: "conflict".to_string(),
            message: "Conflict detected between SQLite catalog tags and sidecar tags; resolve explicitly.".to_string(),
            db_tags: db_set.into_iter().collect(),
            external_tags: sc.tags,
        });
    }

    match policy {
        ConflictPolicy::PreferDb => {
            export_sidecars_for_file(db, file_key, true, true)?;
        }
        ConflictPolicy::PreferExternal => {
            let all_db_tag_ids: Vec<String> = entry.tags.iter().map(|t| t.id.clone()).collect();
            crate::organize::remove_tags_bulk(db, &[entry.asset_id.clone()], &all_db_tag_ids, false)?;
            assign_tags_bulk(db, &[entry.asset_id.clone()], &sc.tags, "imported", 1.0)?;
            crate::catalog::update_asset_metadata_fields(
                db,
                &entry.asset_id,
                sc.title.as_deref(),
                sc.description.as_deref(),
                sc.creator.as_deref(),
                sc.recorded_at.as_deref(),
            )?;
        }
        ConflictPolicy::Merge | ConflictPolicy::Manual => {
            assign_tags_bulk(db, &[entry.asset_id.clone()], &sc.tags, "imported", 1.0)?;
            crate::catalog::update_asset_metadata_fields(
                db,
                &entry.asset_id,
                sc.title.as_deref(),
                sc.description.as_deref(),
                sc.creator.as_deref(),
                sc.recorded_at.as_deref(),
            )?;
        }
    }
    let _ = refresh_fts_for_asset(db, &entry.asset_id);
    Ok(MetadataSyncReport {
        file_key: file_key.to_string(),
        target: target_name.to_string(),
        status: "synced".to_string(),
        message: "Sidecar metadata synchronized.".to_string(),
        db_tags: db_set.into_iter().collect(),
        external_tags: sc.tags,
    })
}

fn parse_xmp_sidecar(raw: &str, entry: &crate::catalog::CatalogEntry) -> PortableSidecar {
    let extract_first = |tag: &str| -> Option<String> {
        let re = regex::Regex::new(&format!(r"(?s)<{tag}[^>]*>(.*?)</{tag}>")).ok()?;
        let cap = re.captures(raw)?;
        let inner = cap.get(1)?.as_str();
        if let Ok(li_re) = regex::Regex::new(r"(?s)<rdf:li[^>]*>(.*?)</rdf:li>") {
            if let Some(li_cap) = li_re.captures(inner) {
                let v = xml_unescape(li_cap.get(1).map(|m| m.as_str()).unwrap_or("")).trim().to_string();
                return if v.is_empty() { None } else { Some(v) };
            }
        }
        let v = xml_unescape(inner).trim().to_string();
        if v.is_empty() { None } else { Some(v) }
    };
    let mut tags = Vec::new();
    if let Ok(subj_re) = regex::Regex::new(r"(?s)<dc:subject>(.*?)</dc:subject>") {
        if let Some(cap) = subj_re.captures(raw) {
            if let Ok(li_re) = regex::Regex::new(r"(?s)<rdf:li[^>]*>(.*?)</rdf:li>") {
                for m in li_re.captures_iter(cap.get(1).map(|x| x.as_str()).unwrap_or("")) {
                    let t = xml_unescape(m.get(1).map(|x| x.as_str()).unwrap_or("")).trim().to_string();
                    if !t.is_empty() && !tags.contains(&t) {
                        tags.push(t);
                    }
                }
            }
        }
    }
    PortableSidecar {
        schema_version: 1,
        asset_id: entry.asset_id.clone(),
        file_key: entry.file_key.clone(),
        filename: entry.filename.clone(),
        title: extract_first("dc:title"),
        description: extract_first("dc:description"),
        creator: extract_first("dc:creator"),
        recorded_at: None,
        tags,
        collections: Vec::new(),
        blake3_hash: entry.blake3_hash.clone(),
        sha256_hash: entry.sha256_hash.clone(),
        head_hash: entry.head_hash.clone(),
        exported_at: Utc::now().timestamp(),
    }
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
}

pub fn sync_os_tags_for_file(
    db: &Database,
    file_key: &str,
    direction: &str, // "push" | "pull"
    policy: ConflictPolicy,
) -> Result<MetadataSyncReport, String> {
    let mut items = crate::catalog::load_entries_by_file_keys(db, &[file_key.to_string()])?;
    let entry = items
        .pop()
        .ok_or_else(|| "Catalog file not found.".to_string())?;
    let path = Path::new(&entry.path);
    let db_tags: Vec<String> = entry
        .tags
        .iter()
        .filter(|t| t.status == "active")
        .map(|t| t.name.clone())
        .collect();

    #[cfg(target_os = "windows")]
    let target = "windows_prop";
    #[cfg(target_os = "macos")]
    let target = "macos_finder";
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let target = "os_tags";

    let ext_tags = if cfg!(target_os = "windows") {
        windows_read_keywords(path).unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        macos_read_finder_tags(path).unwrap_or_default()
    } else {
        Vec::new()
    };

    if direction == "pull" {
        let db_set: HashSet<&String> = db_tags.iter().collect();
        let ext_set: HashSet<&String> = ext_tags.iter().collect();
        if db_set != ext_set && !db_tags.is_empty() && !ext_tags.is_empty() && policy == ConflictPolicy::Manual {
            return Ok(MetadataSyncReport {
                file_key: file_key.to_string(),
                target: target.to_string(),
                status: "conflict".to_string(),
                message: "Conflict between DB tags and OS tags; choose merge, prefer DB, or prefer OS.".to_string(),
                db_tags,
                external_tags: ext_tags,
            });
        }
        assign_tags_bulk(db, &[entry.asset_id.clone()], &ext_tags, "imported", 1.0)?;
        return Ok(MetadataSyncReport {
            file_key: file_key.to_string(),
            target: target.to_string(),
            status: "synced".to_string(),
            message: "Pulled OS tags into SQLite catalog.".to_string(),
            db_tags,
            external_tags: ext_tags,
        });
    }

    // Push DB tags to OS
    let write_res = if cfg!(target_os = "windows") {
        windows_write_keywords(path, &db_tags)
    } else if cfg!(target_os = "macos") {
        macos_write_finder_tags(path, &db_tags)
    } else {
        Err("OS native tags unsupported on this OS".to_string())
    };

    let now = Utc::now().timestamp();
    match write_res {
        Ok(()) => {
            // Writing Windows/Finder properties can modify container metadata bytes; refresh identity and invalidate hashes
            if let Ok(fp) = capture_file_identity(path) {
                let conn = db.lock_conn()?;
                let _ = conn.execute(
                    "UPDATE media_files SET size_bytes = ?1, mtime_ns = ?2, head_hash = ?3,
                            blake3_hash = NULL, sha256_hash = NULL, integrity_status = 'unverified', updated_at = ?4
                     WHERE file_key = ?5",
                    params![fp.size_bytes as i64, fp.mtime_ns, fp.head_hash, now, file_key],
                );
            }
            let conn = db.lock_conn()?;
            conn.execute(
                "INSERT INTO metadata_sync_state (id, file_key, target, sync_status, updated_at)
                 VALUES (?1, ?2, ?3, 'synced', ?4)
                 ON CONFLICT(file_key, target) DO UPDATE SET sync_status = 'synced', updated_at = excluded.updated_at",
                params![Uuid::new_v4().to_string(), file_key, target, now],
            )
            .map_err(|e| e.to_string())?;
            Ok(MetadataSyncReport {
                file_key: file_key.to_string(),
                target: target.to_string(),
                status: "synced".to_string(),
                message: "Synchronized tags to OS metadata handler.".to_string(),
                db_tags,
                external_tags: ext_tags,
            })
        }
        Err(reason) => {
            let conn = db.lock_conn()?;
            conn.execute(
                "INSERT INTO metadata_sync_state (id, file_key, target, sync_status, conflict_json, updated_at)
                 VALUES (?1, ?2, ?3, 'unsupported', ?4, ?5)
                 ON CONFLICT(file_key, target) DO UPDATE SET sync_status = 'unsupported', conflict_json = excluded.conflict_json, updated_at = excluded.updated_at",
                params![Uuid::new_v4().to_string(), file_key, target, reason, now],
            )
            .map_err(|e| e.to_string())?;
            Ok(MetadataSyncReport {
                file_key: file_key.to_string(),
                target: target.to_string(),
                status: "unsupported".to_string(),
                message: reason,
                db_tags,
                external_tags: ext_tags,
            })
        }
    }
}
