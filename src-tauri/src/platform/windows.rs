use crate::models::{ProcessPriority, SourceFingerprint};
use crate::platform::common::{compute_head_tail_hash, verify_no_symlink_ancestors_impl};
use crate::platform::{PublishOutcome, VolumeStats};
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandle, GetVolumeInformationW,
    GetVolumePathNameW, MoveFileExW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW,
    IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION,
};
use windows_sys::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
    SHFILEOPSTRUCTW,
};

fn to_wide_null(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

pub struct PowerAssertion {
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PowerAssertion {
    pub fn acquire(_reason: &str) -> Result<Self, String> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = stop_flag.clone();
        let handle = std::thread::spawn(move || {
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
            }
            while !flag_clone.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(15));
                if !flag_clone.load(Ordering::Relaxed) {
                    unsafe {
                        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
                    }
                }
            }
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        });
        Ok(Self {
            stop_flag,
            handle: Some(handle),
        })
    }
}

impl Drop for PowerAssertion {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct InstanceLock {
    mutex_handle: HANDLE,
}

unsafe impl Send for InstanceLock {}
unsafe impl Sync for InstanceLock {}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if !self.mutex_handle.is_null() {
            unsafe {
                CloseHandle(self.mutex_handle);
            }
        }
    }
}

pub fn acquire_instance_lock(_app_data_dir: &Path) -> Result<InstanceLock, String> {
    let name = to_wide_null(std::ffi::OsStr::new("Local\\FFmpegQueueSingleInstanceMutex"));
    let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if h.is_null() {
        return Err(format!(
            "CreateMutexW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(h) };
        return Err("Another FFmpeg Queue instance is already running.".to_string());
    }
    Ok(InstanceLock { mutex_handle: h })
}

pub fn apply_process_priority(pid: u32, priority: ProcessPriority) -> Result<(), String> {
    let class = match priority {
        ProcessPriority::Background => IDLE_PRIORITY_CLASS,
        ProcessPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        ProcessPriority::Normal => NORMAL_PRIORITY_CLASS,
    };
    let h = unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(format!(
            "OpenProcess failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ok = unsafe { SetPriorityClass(h, class) };
    unsafe { CloseHandle(h) };
    if ok == 0 {
        return Err(format!(
            "SetPriorityClass failed: {}",
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
    job_handle: HANDLE,
}

unsafe impl Send for SupervisedChild {}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if !self.job_handle.is_null() {
            unsafe {
                CloseHandle(self.job_handle);
            }
        }
    }
}

impl SupervisedChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn kill_group(&mut self) {
        let _ = self.child.kill();
    }
}

pub fn spawn_supervised_ffmpeg(
    ffmpeg_path: &Path,
    args: &[String],
    priority: ProcessPriority,
) -> Result<SupervisedChild, String> {
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "CreateJobObjectW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let set_ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if set_ok == 0 {
        unsafe { CloseHandle(job) };
        return Err("SetInformationJobObject failed".to_string());
    }

    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            unsafe { CloseHandle(job) };
            return Err(format!("Failed to spawn ffmpeg: {e}"));
        }
    };

    let proc_handle = child.as_raw_handle() as HANDLE;
    unsafe {
        let _ = AssignProcessToJobObject(job, proc_handle);
    }
    let _ = apply_process_priority(child.id(), priority);

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    Ok(SupervisedChild {
        child,
        stdin,
        stdout,
        stderr,
        job_handle: job,
    })
}

pub fn run_supervisor_entrypoint(_args: &[String]) -> i32 {
    0
}

pub fn disk_space_for_path(path: &Path) -> Result<VolumeStats, String> {
    let existing = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("No existing ancestor for {}", path.display()))?;
    let wide = to_wide_null(existing.as_os_str());
    let mut vol_root = vec![0u16; 260];
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), vol_root.as_mut_ptr(), vol_root.len() as u32) };
    if ok == 0 {
        return Err(format!(
            "GetVolumePathNameW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut serial: u32 = 0;
    let info_ok = unsafe {
        GetVolumeInformationW(
            vol_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if info_ok == 0 {
        return Err(format!(
            "GetVolumeInformationW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut avail: u64 = 0;
    let mut total: u64 = 0;
    let space_ok = unsafe {
        GetDiskFreeSpaceExW(
            vol_root.as_ptr(),
            &mut avail,
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if space_ok == 0 {
        return Err(format!(
            "GetDiskFreeSpaceExW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let end = vol_root.iter().position(|&c| c == 0).unwrap_or(vol_root.len());
    let mount_point = String::from_utf16_lossy(&vol_root[..end]);

    Ok(VolumeStats {
        volume_id: format!("vol:{serial:08x}"),
        mount_point,
        free_bytes: avail,
        total_bytes: total,
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
    if let Ok(f) = File::open(staged_path) {
        let _ = f.sync_all();
    }
    let w_src = to_wide_null(staged_path.as_os_str());
    let w_dst = to_wide_null(final_path.as_os_str());
    // MoveFileExW without MOVEFILE_REPLACE_EXISTING fails if target exists
    let ok = unsafe { MoveFileExW(w_src.as_ptr(), w_dst.as_ptr(), 0) };
    if ok != 0 {
        return PublishOutcome::Published;
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::AlreadyExists || err.raw_os_error() == Some(183) || err.raw_os_error() == Some(80) {
        return PublishOutcome::DestinationExists;
    }
    PublishOutcome::Error(format!("MoveFileExW failed: {err}"))
}

pub fn check_trash_support(path: &Path) -> Result<bool, String> {
    let existing = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("Path does not exist: {}", path.display()))?;
    let wide = to_wide_null(existing.as_os_str());
    let mut vol_root = vec![0u16; 260];
    if unsafe { GetVolumePathNameW(wide.as_ptr(), vol_root.as_mut_ptr(), vol_root.len() as u32) } == 0 {
        return Err("Cannot resolve volume root".to_string());
    }
    let dtype = unsafe { GetDriveTypeW(vol_root.as_ptr()) };
    if dtype != 3 {
        return Err("Recycle Bin is only guaranteed on fixed local drives (not network or removable volumes).".to_string());
    }
    // Keep automatic/app Recycle Bin deletion disabled on Windows until FOFX_RECYCLEONDELETE
    // and disposable-fixture Recycle Bin tests have been verified on a Windows 11 runner.
    Err("Recoverable-only Recycle Bin guarantee is unverified on Windows until tested on a Windows 11 runner; keeping original intact.".to_string())
}

pub fn move_to_recoverable_trash_verified(
    source_path: &Path,
    expected: &SourceFingerprint,
) -> Result<(), String> {
    if !check_trash_support(source_path)? {
        return Err("Recoverable Recycle Bin is unavailable on this volume.".to_string());
    }
    if expected.link_count > 1 {
        return Err("Ambiguous hard-linked file excluded from automatic cleanup.".to_string());
    }
    verify_no_symlink_ancestors_impl(source_path)?;

    let sym = std::fs::symlink_metadata(source_path)
        .map_err(|e| format!("Cannot inspect {}: {e}", source_path.display()))?;
    if sym.file_type().is_symlink() || (sym.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err("Symlink/reparse point excluded.".to_string());
    }

    let mut file = File::open(source_path).map_err(|e| format!("Open failed: {e}"))?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err("GetFileInformationByHandle failed".to_string());
    }
    if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err("Reparse point detected.".to_string());
    }
    if info.nNumberOfLinks > 1 {
        return Err("Hard-linked file excluded.".to_string());
    }
    let size_bytes = ((info.nFileSizeHigh as u64) << 32) | (info.nFileSizeLow as u64);
    let file_index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    let actual_vol = format!("vol:{:08x}", info.dwVolumeSerialNumber);
    let actual_id = format!("vol:{:08x}:idx:{file_index}", info.dwVolumeSerialNumber);
    if actual_vol != expected.volume_id || actual_id != expected.file_id || size_bytes != expected.size_bytes {
        return Err("Source file identity changed before Recycle Bin move.".to_string());
    }
    let hash = compute_head_tail_hash(&mut file, size_bytes)?;
    if hash != expected.head_hash {
        return Err("Source content changed before Recycle Bin move.".to_string());
    }
    drop(file);

    // Double-null-terminated UTF-16 path for SHFileOperationW
    let mut p_from: Vec<u16> = source_path.as_os_str().encode_wide().collect();
    p_from.push(0);
    p_from.push(0);

    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = p_from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16;
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 || op.fAnyOperationsAborted != 0 {
        return Err(format!("SHFileOperationW Recycle Bin move failed (code {rc})"));
    }
    Ok(())
}
