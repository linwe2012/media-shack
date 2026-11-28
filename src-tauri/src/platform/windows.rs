use crate::models::{DestinationCapabilities, ProcessPriority, SourceFingerprint};
use crate::platform::{PublishOutcome, VolumeStats};
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetVolumeInformationW, GetVolumePathNameW, MoveFileExW,
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
    let name = to_wide_null(std::ffi::OsStr::new("Local\\MediaShackSingleInstanceMutex"));
    let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if h.is_null() {
        return Err(format!(
            "CreateMutexW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(h) };
        return Err("Another Media Shack instance is already running.".to_string());
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

/// Platform guarantee on Windows:
/// Legacy `SHFileOperationW(FOF_ALLOWUNDO)` requests Recycle Bin only on a best-effort basis and can
/// silently permanently delete when Recycle Bin is disabled or the file exceeds bin capacity, and cannot
/// bind directly to a verified open handle against concurrent ancestor renames.
/// Until handle-verified recoverable-only `IFileOperation` (`FOFX_RECYCLEONDELETE`) + Recycle Bin
/// policy verification is enabled on Windows, fail closed and keep original files intact.
pub fn check_trash_support(_path: &Path) -> Result<bool, String> {
    Err("Recoverable-only OS Trash is disabled on Windows until strict Recycle Bin policy and FOFX_RECYCLEONDELETE enforcement are enabled; keeping original files intact.".to_string())
}

pub fn move_to_recoverable_trash_verified(
    source_path: &Path,
    expected: &SourceFingerprint,
) -> Result<(), String> {
    let _ = (source_path, expected);
    Err("Recoverable-only OS Trash is disabled on Windows until strict Recycle Bin policy and FOFX_RECYCLEONDELETE enforcement are enabled; original file preserved.".to_string())
}

pub fn destination_capabilities(path: &Path) -> Result<DestinationCapabilities, String> {
    let existing = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("Path does not exist: {}", path.display()))?;
    let w_path = to_wide_null(existing.as_os_str());
    let mut vol_root = vec![0u16; 260];
    let ok = unsafe { GetVolumePathNameW(w_path.as_ptr(), vol_root.as_mut_ptr(), vol_root.len() as u32) };
    if ok == 0 {
        return Err(format!(
            "GetVolumePathNameW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut flags: u32 = 0;
    let mut fs_buf = vec![0u16; 64];
    let info_ok = unsafe {
        GetVolumeInformationW(
            vol_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if info_ok == 0 {
        return Err(format!(
            "GetVolumeInformationW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;
    let fs_end = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    let fs_type = String::from_utf16_lossy(&fs_buf[..fs_end]).to_ascii_lowercase();
    let mp_end = vol_root.iter().position(|&c| c == 0).unwrap_or(vol_root.len());
    let mount_point = String::from_utf16_lossy(&vol_root[..mp_end]);
    let is_local = !mount_point.starts_with("\\\\");
    let is_read_only = (flags & FILE_READ_ONLY_VOLUME) != 0;
    let is_writable = !is_read_only
        && std::fs::metadata(&existing)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(true);
    let trash_note = check_trash_support(path).err();

    Ok(DestinationCapabilities {
        fs_type,
        mount_point,
        is_local,
        is_read_only,
        is_writable,
        supports_atomic_no_replace: !is_read_only,
        supports_trash: false,
        trash_note,
    })
}
