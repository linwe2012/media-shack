use crate::models::SourceFingerprint;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub fn verify_no_symlink_ancestors_impl(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Path must be absolute for identity verification.".to_string());
    }
    let mut cur = PathBuf::new();
    for comp in path.components() {
        cur.push(comp);
        if cur.as_os_str() == "/" {
            continue;
        }
        // On macOS, /var and /tmp are symlinks to /private/var and /private/tmp at root level;
        // check the file itself and any user-controlled directories. Better: canonicalize parent vs lstat.
        let meta = std::fs::symlink_metadata(&cur)
            .map_err(|e| format!("Cannot inspect ancestor {}: {e}", cur.display()))?;
        if meta.file_type().is_symlink() {
            // Allow system macOS root symlinks /var -> private/var, /tmp -> private/tmp only if at depth 1
            #[cfg(target_os = "macos")]
            {
                let s = cur.to_string_lossy();
                if s == "/var" || s == "/tmp" || s == "/etc" {
                    continue;
                }
            }
            return Err(format!("Symlink detected in path component: {}", cur.display()));
        }
    }
    Ok(())
}

pub fn compute_head_tail_hash(file: &mut File, size: u64) -> Result<String, String> {
    const CHUNK: usize = 65536;
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut fnv_update = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CHUNK];
    let n1 = file.read(&mut buf).map_err(|e| e.to_string())?;
    fnv_update(&buf[..n1]);

    if size > CHUNK as u64 {
        let tail_start = size.saturating_sub(CHUNK as u64);
        file.seek(SeekFrom::Start(tail_start))
            .map_err(|e| e.to_string())?;
        let n2 = file.read(&mut buf).map_err(|e| e.to_string())?;
        fnv_update(&buf[..n2]);
    }

    Ok(format!("{size:016x}:{hash:016x}"))
}

#[cfg(unix)]
pub fn capture_file_identity_impl(path: &Path) -> Result<SourceFingerprint, String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;

    let sym = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot stat {}: {e}", path.display()))?;
    if sym.file_type().is_symlink() {
        return Err("Symlinks are excluded from cleanup and identity binding.".to_string());
    }
    if !sym.file_type().is_file() {
        return Err("Target is not a regular file.".to_string());
    }

    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW;
    #[cfg(target_os = "macos")]
    {
        flags |= libc::O_CLOEXEC;
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "Path contains interior NUL byte".to_string())?;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        return Err(format!(
            "Failed to open file safely with O_NOFOLLOW: {}",
            std::io::Error::last_os_error()
        ));
    }

    use std::os::unix::io::FromRawFd;
    let mut file = unsafe { File::from_raw_fd(fd) };

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
        return Err(format!(
            "fstat failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err("Opened descriptor is not a regular file.".to_string());
    }

    let meta = file.metadata().map_err(|e| e.to_string())?;
    let size_bytes = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let head_hash = compute_head_tail_hash(&mut file, size_bytes)?;

    Ok(SourceFingerprint {
        volume_id: format!("dev:{}", st.st_dev),
        file_id: format!("dev:{}:ino:{}", st.st_dev, st.st_ino),
        size_bytes,
        mtime_ns,
        head_hash,
        link_count: meta.nlink(),
    })
}

#[cfg(target_os = "windows")]
pub fn capture_file_identity_impl(path: &Path) -> Result<SourceFingerprint, String> {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let sym = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot stat {}: {e}", path.display()))?;
    if sym.file_type().is_symlink() || (sym.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err("Symlinks and reparse points are excluded.".to_string());
    }
    if !sym.file_type().is_file() {
        return Err("Target is not a regular file.".to_string());
    }

    let mut file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return Err(format!(
            "GetFileInformationByHandle failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err("File became a reparse point.".to_string());
    }

    let size_bytes = ((info.nFileSizeHigh as u64) << 32) | (info.nFileSizeLow as u64);
    let file_index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let head_hash = compute_head_tail_hash(&mut file, size_bytes)?;

    Ok(SourceFingerprint {
        volume_id: format!("vol:{:08x}", info.dwVolumeSerialNumber),
        file_id: format!("vol:{:08x}:idx:{file_index}", info.dwVolumeSerialNumber),
        size_bytes,
        mtime_ns,
        head_hash,
        link_count: info.nNumberOfLinks as u64,
    })
}
