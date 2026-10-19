//! Keep ffmpeg / sidecar children tied to the vigil process lifetime.
//!
//! - Windows: Job Object with KILL_ON_JOB_CLOSE (OS kills children when vigil exits).
//! - Unix: PR_SET_PDEATHSIG so children get SIGTERM when parent dies.
//! - All platforms: PID registry + explicit kill on graceful shutdown.

use std::collections::HashSet;
use std::sync::Mutex;
use tracing::{debug, warn};

static TRACKED_PIDS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

/// Call once at daemon start (before spawning children).
pub fn init() {
    let mut g = TRACKED_PIDS.lock().unwrap();
    *g = Some(HashSet::new());
    #[cfg(windows)]
    win_job::ensure_job();
    debug!("process guard initialized");
}

/// After `Command::spawn`, bind the child so it dies with vigil.
pub fn track_child(child: &tokio::process::Child) {
    let Some(pid) = child.id() else {
        return;
    };
    if let Ok(mut g) = TRACKED_PIDS.lock() {
        if let Some(set) = g.as_mut() {
            set.insert(pid);
        }
    }
    #[cfg(windows)]
    {
        if let Err(e) = win_job::assign_pid(pid) {
            warn!(pid, error = %e, "failed to assign child to job object");
        }
    }
    debug!(pid, "tracked child process");
}

pub fn untrack_pid(pid: u32) {
    if let Ok(mut g) = TRACKED_PIDS.lock() {
        if let Some(set) = g.as_mut() {
            set.remove(&pid);
        }
    }
}

/// Best-effort kill of every tracked child (Ctrl+C / graceful shutdown).
pub fn kill_all_tracked() {
    let pids: Vec<u32> = TRACKED_PIDS
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|s| s.drain().collect()))
        .unwrap_or_default();
    for pid in pids {
        force_kill_pid(pid);
    }
}

fn force_kill_pid(pid: u32) {
    #[cfg(windows)]
    win_job::terminate_pid(pid);
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    debug!(pid, "force-killed child");
}

/// Configure `Command` so the child exits when vigil dies (Unix PDEATHSIG).
pub fn apply_spawn_hooks(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // Kernel sends SIGTERM to this process when the parent dies.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Race: parent already gone between fork and prctl.
                if libc::getppid() == 1 {
                    libc::raise(libc::SIGTERM);
                }
                Ok(())
            });
        }
    }
    let _ = cmd;
}

#[cfg(windows)]
mod win_job {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr;
    use std::sync::OnceLock;

    type HANDLE = *mut c_void;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_SET_QUOTA: u32 = 0x0100;
    const PROCESS_ASSIGN: u32 = PROCESS_TERMINATE | PROCESS_SET_QUOTA;

    #[repr(C)]
    struct IoCounters {
        read_op: u64,
        write_op: u64,
        other_op: u64,
        read_trans: u64,
        write_trans: u64,
        other_trans: u64,
    }

    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    struct JobObjectExtendedLimitInformation {
        basic: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> HANDLE;
        fn SetInformationJobObject(
            job: HANDLE,
            info_class: u32,
            info: *mut c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: HANDLE, process: HANDLE) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
        fn TerminateProcess(process: HANDLE, exit_code: u32) -> i32;
        fn CloseHandle(handle: HANDLE) -> i32;
        fn GetLastError() -> u32;
    }

    static JOB: OnceLock<isize> = OnceLock::new();

    pub fn ensure_job() {
        JOB.get_or_init(|| {
            unsafe {
                let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
                if job.is_null() {
                    tracing::error!("CreateJobObjectW failed: {}", GetLastError());
                    return 0;
                }
                let mut info: JobObjectExtendedLimitInformation = zeroed();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    &mut info as *mut _ as *mut c_void,
                    size_of::<JobObjectExtendedLimitInformation>() as u32,
                );
                if ok == 0 {
                    tracing::error!("SetInformationJobObject failed: {}", GetLastError());
                    CloseHandle(job);
                    return 0;
                }
                // Intentionally leak the HANDLE for process lifetime: when vigil
                // exits, the handle closes and Windows kills all assigned children.
                job as isize
            }
        });
    }

    pub fn assign_pid(pid: u32) -> Result<(), String> {
        ensure_job();
        let job = *JOB.get().unwrap_or(&0);
        if job == 0 {
            return Err("job object unavailable".into());
        }
        unsafe {
            let proc = OpenProcess(PROCESS_ASSIGN, 0, pid);
            if proc.is_null() {
                return Err(format!("OpenProcess({pid}) err={}", GetLastError()));
            }
            let ok = AssignProcessToJobObject(job as HANDLE, proc);
            let err = GetLastError();
            CloseHandle(proc);
            if ok == 0 {
                return Err(format!("AssignProcessToJobObject err={err}"));
            }
        }
        Ok(())
    }

    pub fn terminate_pid(pid: u32) {
        unsafe {
            let proc = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !proc.is_null() {
                TerminateProcess(proc, 1);
                CloseHandle(proc);
            }
        }
    }
}
