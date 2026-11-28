use crate::models::{ProcessPriority, SourceFingerprint};
use crate::platform::common::{compute_head_tail_hash, verify_no_symlink_ancestors_impl};
use crate::platform::{PublishOutcome, VolumeStats};
use std::ffi::{c_void, CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

const RENAME_EXCL: u32 = 0x0000_0004;

extern "C" {
    fn renameatx_np(
        fromfd: i32,
        from: *const libc::c_char,
        tofd: i32,
        to: *const libc::c_char,
        flags: u32,
    ) -> libc::c_int;
}

// IOKit & CoreFoundation FFI for power assertion (allows display sleep, prevents idle system sleep)
#[link(name = "IOKit", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
#[link(name = "Foundation", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;

    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const libc::c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);

    fn objc_getClass(name: *const libc::c_char) -> *const c_void;
    fn sel_registerName(name: *const libc::c_char) -> *const c_void;
    fn objc_msgSend();
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

pub struct PowerAssertion {
    assertion_id: Option<u32>,
}

impl PowerAssertion {
    pub fn acquire(reason: &str) -> Result<Self, String> {
        let type_c = CString::new("PreventUserIdleSystemSleep").unwrap();
        let name_c = CString::new(reason).unwrap_or_else(|_| CString::new("Media Shack active").unwrap());
        unsafe {
            let cf_type = CFStringCreateWithCString(std::ptr::null(), type_c.as_ptr(), K_CF_STRING_ENCODING_UTF8);
            let cf_name = CFStringCreateWithCString(std::ptr::null(), name_c.as_ptr(), K_CF_STRING_ENCODING_UTF8);
            if cf_type.is_null() || cf_name.is_null() {
                if !cf_type.is_null() {
                    CFRelease(cf_type);
                }
                if !cf_name.is_null() {
                    CFRelease(cf_name);
                }
                return Err("Failed to create CFString for IOKit assertion".to_string());
            }
            let mut id: u32 = 0;
            let rc = IOPMAssertionCreateWithName(cf_type, K_IOPM_ASSERTION_LEVEL_ON, cf_name, &mut id);
            CFRelease(cf_type);
            CFRelease(cf_name);
            if rc != 0 {
                return Err(format!("IOPMAssertionCreateWithName failed with IOReturn {rc}"));
            }
            Ok(Self {
                assertion_id: Some(id),
            })
        }
    }
}

impl Drop for PowerAssertion {
    fn drop(&mut self) {
        if let Some(id) = self.assertion_id.take() {
            unsafe {
                let _ = IOPMAssertionRelease(id);
            }
        }
    }
}

pub struct InstanceLock {
    _file: File,
}

pub fn acquire_instance_lock(app_data_dir: &Path) -> Result<InstanceLock, String> {
    std::fs::create_dir_all(app_data_dir).map_err(|e| e.to_string())?;
    let lock_path = app_data_dir.join("media_shack.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open instance lock: {e}"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err("Another Media Shack instance is already running.".to_string());
    }
    Ok(InstanceLock { _file: file })
}

pub fn apply_process_priority(pid: u32, priority: ProcessPriority) -> Result<(), String> {
    let nice_val: libc::c_int = match priority {
        ProcessPriority::Background => 10,
        ProcessPriority::BelowNormal => 5,
        ProcessPriority::Normal => 0,
    };
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice_val) };
    if rc != 0 {
        return Err(format!(
            "setpriority(pid={pid}, nice={nice_val}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub struct SupervisedChild {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    // Closing/dropping this pipe write end signals the supervisor that the parent exited
    _control_pipe_write: Option<File>,
    pgid: i32,
}

impl SupervisedChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn kill_group(&mut self) {
        if self.pgid > 0 {
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
    }
}

pub fn spawn_supervised_ffmpeg(
    ffmpeg_path: &Path,
    args: &[String],
    priority: ProcessPriority,
) -> Result<SupervisedChild, String> {
    // Create a pipe whose read end is inherited by the supervisor process.
    // On parent crash/SIGKILL, the OS closes the write end and supervisor receives EOF, reaping its process group.
    let mut fds: [RawFd; 2] = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!("pipe() failed: {}", std::io::Error::last_os_error()));
    }
    let read_fd = fds[0];
    let write_fd = fds[1];

    // Keep write_fd closed in child (`FD_CLOEXEC`), keep read_fd open in child across exec
    unsafe {
        libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(read_fd, libc::F_SETFD, 0);
    }

    let mut exe = std::env::current_exe().map_err(|e| format!("current_exe error: {e}"))?;
    // When running inside `cargo test`, `current_exe()` is `target/<profile>/deps/<test_bin>`.
    // Resolve the sibling `media-shack` binary built by Cargo in `target/<profile>/media-shack`.
    if exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("deps")
    {
        if let Some(candidate) = exe.parent().and_then(|p| p.parent()).map(|p| p.join("media-shack")) {
            if candidate.is_file() {
                exe = candidate;
            }
        }
    }
    let mut cmd = Command::new(&exe);
    cmd.arg("--supervise-ffmpeg")
        .arg(read_fd.to_string())
        .arg(match priority {
            ProcessPriority::Background => "10",
            ProcessPriority::BelowNormal => "5",
            ProcessPriority::Normal => "0",
        })
        .arg(ffmpeg_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(format!("Failed to spawn FFmpeg supervisor: {e}"));
        }
    };

    unsafe {
        libc::close(read_fd);
    }
    let control_write = unsafe { File::from_raw_fd(write_fd) };
    let pgid = child.id() as i32;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    Ok(SupervisedChild {
        child,
        stdin,
        stdout,
        stderr,
        _control_pipe_write: Some(control_write),
        pgid,
    })
}

/// Entrypoint when binary is launched with `--supervise-ffmpeg <control_read_fd> <nice> <ffmpeg_path> <args...>`
pub fn run_supervisor_entrypoint(args: &[String]) -> i32 {
    if args.len() < 4 {
        return 2;
    }
    let control_fd: RawFd = match args[0].parse() {
        Ok(fd) => fd,
        Err(_) => return 2,
    };
    let nice_val: i32 = args[1].parse().unwrap_or(10);
    let ffmpeg_path = &args[2];
    let ffmpeg_args = &args[3..];

    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(ffmpeg_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    unsafe {
        cmd.pre_exec(move || {
            libc::setpgid(0, 0);
            libc::setpriority(libc::PRIO_PROCESS, 0, nice_val);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Supervisor failed to spawn {ffmpeg_path}: {e}");
            return 127;
        }
    };

    let child_pid = child.id() as i32;

    // Monitor parent pipe EOF in a dedicated thread; child runs in its own PGID (-child_pid)
    // so terminating -child_pid never kills this supervisor before it can escalate to SIGKILL.
    std::thread::spawn(move || {
        let mut pipe_file = unsafe { File::from_raw_fd(control_fd) };
        let mut buf = [0u8; 16];
        loop {
            match pipe_file.read(&mut buf) {
                Ok(0) | Err(_) => {
                    unsafe {
                        libc::kill(child_pid, libc::SIGTERM);
                        if child_pid > 1 {
                            libc::kill(-child_pid, libc::SIGTERM);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    unsafe {
                        libc::kill(child_pid, libc::SIGKILL);
                        if child_pid > 1 {
                            libc::kill(-child_pid, libc::SIGKILL);
                        }
                    }
                    std::process::exit(143);
                }
                Ok(_) => {}
            }
        }
    });

    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}

pub fn disk_space_for_path(path: &Path) -> Result<VolumeStats, String> {
    let existing = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("No existing ancestor for {}", path.display()))?;
    let c_path = CString::new(existing.as_os_str().as_encoded_bytes())
        .map_err(|_| "Path contains NUL".to_string())?;

    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut sfs) } != 0 {
        return Err(format!(
            "statfs failed on {}: {}",
            existing.display(),
            std::io::Error::last_os_error()
        ));
    }

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c_path.as_ptr(), &mut st) } != 0 {
        return Err(format!(
            "stat failed on {}: {}",
            existing.display(),
            std::io::Error::last_os_error()
        ));
    }

    let mount_point = unsafe { CStr::from_ptr(sfs.f_mntonname.as_ptr()) }
        .to_string_lossy()
        .to_string();
    let bsize = sfs.f_bsize as u64;
    let free_bytes = (sfs.f_bavail as u64).saturating_mul(bsize);
    let total_bytes = (sfs.f_blocks as u64).saturating_mul(bsize);
    let volume_id = format!("dev:{}", st.st_dev);

    Ok(VolumeStats {
        volume_id,
        mount_point,
        free_bytes,
        total_bytes,
    })
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cur = path.to_path_buf();
    loop {
        if cur.exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

pub fn atomic_publish_no_replace(staged_path: &Path, final_path: &Path) -> PublishOutcome {
    if let Some(parent) = final_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return PublishOutcome::Error(format!("Failed to create destination directory: {e}"));
        }
    }
    // Flush staged file contents before atomic rename
    if let Ok(f) = File::open(staged_path) {
        let _ = f.sync_all();
    }

    let Ok(c_src) = CString::new(staged_path.as_os_str().as_encoded_bytes()) else {
        return PublishOutcome::Error("Staged path contains NUL".to_string());
    };
    let Ok(c_dst) = CString::new(final_path.as_os_str().as_encoded_bytes()) else {
        return PublishOutcome::Error("Final path contains NUL".to_string());
    };

    let rc = unsafe {
        renameatx_np(
            libc::AT_FDCWD,
            c_src.as_ptr(),
            libc::AT_FDCWD,
            c_dst.as_ptr(),
            RENAME_EXCL,
        )
    };
    if rc == 0 {
        if let Some(parent) = final_path.parent() {
            if let Ok(dir_f) = File::open(parent) {
                let _ = dir_f.sync_all();
            }
        }
        return PublishOutcome::Published;
    }

    let err = std::io::Error::last_os_error();
    let raw = err.raw_os_error().unwrap_or(0);
    if raw == libc::EEXIST {
        return PublishOutcome::DestinationExists;
    }
    // Some non-APFS filesystems (e.g. ExFAT/SMB) return ENOTSUP for renameatx_np(RENAME_EXCL);
    // do not fall back to overwriting rename — report uncertain/error unless hard-link no-replace succeeds.
    if raw == libc::ENOTSUP || raw == libc::EINVAL {
        if std::fs::hard_link(staged_path, final_path).is_ok() {
            let _ = std::fs::remove_file(staged_path);
            return PublishOutcome::Published;
        }
        let link_err = std::io::Error::last_os_error();
        if link_err.raw_os_error() == Some(libc::EEXIST) {
            return PublishOutcome::DestinationExists;
        }
        return PublishOutcome::Error(format!(
            "Atomic no-replace publish unsupported on destination filesystem: {link_err}"
        ));
    }
    PublishOutcome::Error(format!("Atomic publish failed: {err}"))
}

pub fn check_trash_support(path: &Path) -> Result<bool, String> {
    let existing = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("Path does not exist: {}", path.display()))?;
    let c_path = CString::new(existing.as_os_str().as_encoded_bytes())
        .map_err(|_| "Path contains NUL".to_string())?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut sfs) } != 0 {
        return Err(format!("statfs failed: {}", std::io::Error::last_os_error()));
    }
    let fstype = unsafe { CStr::from_ptr(sfs.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .to_ascii_lowercase();
    let is_local = (sfs.f_flags & libc::MNT_LOCAL as u32) != 0;
    let is_ro = (sfs.f_flags & libc::MNT_RDONLY as u32) != 0;
    if is_ro {
        return Err("Volume is read-only; Trash is unavailable.".to_string());
    }
    if !is_local {
        return Err(format!("Network filesystem ({fstype}) does not guarantee recoverable OS Trash."));
    }
    if matches!(fstype.as_str(), "apfs" | "hfs") {
        Ok(true)
    } else {
        Err(format!("Filesystem '{fstype}' lacks verified recoverable Trash guarantee."))
    }
}

/// Identity-checked, recoverable-only OS Trash move on macOS.
/// Platform guarantee documented:
/// - Rejects read-only volumes, non-local/network shares (`MNT_LOCAL == 0`), and non-APFS/HFS+ filesystems.
/// - Pins both parent directory (`O_DIRECTORY | O_NOFOLLOW`) and target (`openat(..., O_NOFOLLOW)`)
///   before verifying canonical `F_GETPATH`, `fstat` (`S_IFREG`, `nlink == 1`, `dev:ino`, `size`, `mtime`),
///   `head_hash`, and optional `full_blake3` while holding `file` open across `NSFileManager trashItemAtURL:`,
///   then verifies `resultingItemURL` inode matches the pinned open descriptor before closing.
/// - Darwin has no kernel `trashat(fd)` syscall (`NSFileManager trashItemAtURL:` takes `NSURL` to preserve
///   Finder Put-Back records in `.Trash`), so keeping `dir_fd` + `file` open and re-resolving `F_GETPATH`
///   plus verifying `resultingItemURL` is the strictest recoverable-Trash guarantee macOS supports without
///   reimplementing Finder's private Put-Back protocol or permanently deleting.
pub fn move_to_recoverable_trash_verified(
    source_path: &Path,
    expected: &SourceFingerprint,
) -> Result<(), String> {
    if !check_trash_support(source_path)? {
        return Err("Recoverable OS Trash is not supported on this volume.".to_string());
    }
    if expected.link_count > 1 {
        return Err("Ambiguous hard-linked file (link count > 1) excluded from automatic Trash.".to_string());
    }
    verify_no_symlink_ancestors_impl(source_path)?;

    let parent = source_path
        .parent()
        .ok_or_else(|| "Source path has no parent directory.".to_string())?;
    let basename = source_path
        .file_name()
        .ok_or_else(|| "Source path has no filename.".to_string())?;
    let c_parent = CString::new(parent.as_os_str().as_encoded_bytes())
        .map_err(|_| "Parent path contains NUL".to_string())?;
    let c_base = CString::new(basename.as_encoded_bytes())
        .map_err(|_| "Filename contains NUL".to_string())?;

    let dir_fd = unsafe {
        libc::open(
            c_parent.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if dir_fd < 0 {
        return Err(format!(
            "Failed safe parent open (O_NOFOLLOW) on {}: {}",
            parent.display(),
            std::io::Error::last_os_error()
        ));
    }
    let _dir_guard = unsafe { File::from_raw_fd(dir_fd) };

    let fd = unsafe {
        libc::openat(
            dir_fd,
            c_base.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Failed safe openat (O_NOFOLLOW) on {}: {}",
            source_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };

    // Verify F_GETPATH resolves to same canonical path (prevents ancestor swap)
    let mut path_buf = [0u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path_buf.as_mut_ptr()) } != 0 {
        return Err("F_GETPATH verification failed".to_string());
    }
    let resolved_path = unsafe { CStr::from_ptr(path_buf.as_ptr() as *const libc::c_char) }
        .to_string_lossy()
        .to_string();
    let canonical_expected = source_path
        .canonicalize()
        .map_err(|e| format!("Canonicalize failed: {e}"))?;
    if Path::new(&resolved_path) != canonical_expected {
        return Err("Source path substitution detected before Trash operation.".to_string());
    }

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
        return Err(format!("fstat failed: {}", std::io::Error::last_os_error()));
    }
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err("Target is not a regular file.".to_string());
    }
    let actual_file_id = format!("dev:{}:ino:{}", st.st_dev, st.st_ino);
    let actual_vol_id = format!("dev:{}", st.st_dev);
    if actual_vol_id != expected.volume_id || actual_file_id != expected.file_id {
        return Err(format!(
            "Source file identity changed (expected {}, got {actual_file_id}).",
            expected.file_id
        ));
    }

    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.nlink() > 1 {
        return Err("Source gained extra hard links; keeping original.".to_string());
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    if meta.len() != expected.size_bytes || mtime_ns != expected.mtime_ns {
        return Err("Source file size or mtime changed since verification.".to_string());
    }
    let hash = compute_head_tail_hash(&mut file, meta.len())?;
    if hash != expected.head_hash {
        return Err("Source file content fingerprint changed.".to_string());
    }
    if let Some(ref exp_b3) = expected.full_blake3 {
        let actual_b3 = crate::platform::common::compute_full_blake3(&mut file)?;
        if &actual_b3 != exp_b3 {
            return Err("Source full BLAKE3 content hash changed before Trash.".to_string());
        }
    }

    // Re-resolve canonical path from still-open descriptor immediately before invoking NSFileManager
    let mut final_buf = [0u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, final_buf.as_mut_ptr()) } != 0 {
        return Err("Post-hash F_GETPATH verification failed".to_string());
    }
    let final_resolved = unsafe { CStr::from_ptr(final_buf.as_ptr() as *const libc::c_char) }
        .to_string_lossy()
        .to_string();
    if final_resolved != resolved_path {
        return Err("Concurrent move/rename detected before Trash; keeping original.".to_string());
    }

    // Hold `file` and `_dir_guard` open across trashItemAtURL so the verified inode cannot be recycled
    // and we can compare the trashed item's inode directly against our pinned file descriptor.
    ns_file_manager_trash(Path::new(&final_resolved), &actual_file_id)
}

fn ns_file_manager_trash(path: &Path, expected_file_id: &str) -> Result<(), String> {
    let path_c = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "Path contains NUL".to_string())?;

    unsafe {
        let cls_nsstring = objc_getClass(b"NSString\0".as_ptr() as *const _);
        let cls_nsurl = objc_getClass(b"NSURL\0".as_ptr() as *const _);
        let cls_nsfm = objc_getClass(b"NSFileManager\0".as_ptr() as *const _);
        let cls_pool = objc_getClass(b"NSAutoreleasePool\0".as_ptr() as *const _);
        if cls_nsstring.is_null() || cls_nsurl.is_null() || cls_nsfm.is_null() {
            return Err("Foundation Objective-C classes unavailable".to_string());
        }

        type MsgId = unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void;
        type MsgStr = unsafe extern "C" fn(*const c_void, *const c_void, *const libc::c_char) -> *const c_void;
        type MsgTrash = unsafe extern "C" fn(
            *const c_void,
            *const c_void,
            *const c_void,
            *mut *const c_void,
            *mut *const c_void,
        ) -> bool;

        let f_id: MsgId = std::mem::transmute(objc_msgSend as *const c_void);
        let f_str: MsgStr = std::mem::transmute(objc_msgSend as *const c_void);
        let f_trash: MsgTrash = std::mem::transmute(objc_msgSend as *const c_void);

        let sel_alloc = sel_registerName(b"alloc\0".as_ptr() as *const _);
        let sel_init = sel_registerName(b"init\0".as_ptr() as *const _);
        let sel_drain = sel_registerName(b"drain\0".as_ptr() as *const _);
        let pool = f_id(f_id(cls_pool, sel_alloc), sel_init);

        let sel_str_utf8 = sel_registerName(b"stringWithUTF8String:\0".as_ptr() as *const _);
        let ns_str = f_str(cls_nsstring, sel_str_utf8, path_c.as_ptr());
        let sel_file_url = sel_registerName(b"fileURLWithPath:\0".as_ptr() as *const _);

        type MsgUrl = unsafe extern "C" fn(*const c_void, *const c_void, *const c_void) -> *const c_void;
        let f_url: MsgUrl = std::mem::transmute(objc_msgSend as *const c_void);
        let file_url = f_url(cls_nsurl, sel_file_url, ns_str);

        let sel_default_mgr = sel_registerName(b"defaultManager\0".as_ptr() as *const _);
        let fm = f_id(cls_nsfm, sel_default_mgr);

        let sel_trash = sel_registerName(b"trashItemAtURL:resultingItemURL:error:\0".as_ptr() as *const _);
        let mut resulting_url: *const c_void = std::ptr::null();
        let mut ns_err: *const c_void = std::ptr::null();
        let ok = f_trash(fm, sel_trash, file_url, &mut resulting_url, &mut ns_err);

        if !ok {
            let msg = if !ns_err.is_null() {
                let sel_desc = sel_registerName(b"localizedDescription\0".as_ptr() as *const _);
                let sel_utf8 = sel_registerName(b"UTF8String\0".as_ptr() as *const _);
                let desc_ns = f_id(ns_err, sel_desc);
                type MsgCstr = unsafe extern "C" fn(*const c_void, *const c_void) -> *const libc::c_char;
                let f_cstr: MsgCstr = std::mem::transmute(objc_msgSend as *const c_void);
                let cptr = f_cstr(desc_ns, sel_utf8);
                if !cptr.is_null() {
                    CStr::from_ptr(cptr).to_string_lossy().to_string()
                } else {
                    "NSFileManager trashItemAtURL failed".to_string()
                }
            } else {
                "NSFileManager trashItemAtURL failed".to_string()
            };
            f_id(pool, sel_drain);
            return Err(msg);
        }

        // Verify resulting URL in Trash exists and shares the exact inode
        if !resulting_url.is_null() {
            let sel_path = sel_registerName(b"path\0".as_ptr() as *const _);
            let sel_utf8 = sel_registerName(b"UTF8String\0".as_ptr() as *const _);
            let res_ns_path = f_id(resulting_url, sel_path);
            type MsgCstr = unsafe extern "C" fn(*const c_void, *const c_void) -> *const libc::c_char;
            let f_cstr: MsgCstr = std::mem::transmute(objc_msgSend as *const c_void);
            let cptr = f_cstr(res_ns_path, sel_utf8);
            if !cptr.is_null() {
                let trashed_path = CStr::from_ptr(cptr).to_string_lossy().to_string();
                if let Ok(meta) = std::fs::metadata(&trashed_path) {
                    let trashed_id = format!("dev:{}:ino:{}", meta.dev(), meta.ino());
                    if trashed_id != expected_file_id {
                        f_id(pool, sel_drain);
                        return Err("Trashed item inode differed from expected source.".to_string());
                    }
                }
            }
        }

        f_id(pool, sel_drain);
        Ok(())
    }
}
