//! Keeps sidecar interpreters from outliving Verba.
//!
//! The sidecars have `Drop` impls that kill their child, but `app.exit(0)`
//! terminates the process without unwinding the detached engine thread, so
//! those never run — every launch-and-quit cycle left a `python.exe` holding a
//! loaded model, invisible because the children are spawned with
//! CREATE_NO_WINDOW.
//!
//! A job object fixes it at the level the problem actually lives at: the OS
//! reaps anything assigned to it when the last handle closes, which covers a
//! clean quit, a panic, and a hard kill alike. Doing it in Drop only would
//! still leak on the last two.

use std::sync::OnceLock;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Wrapper so the raw handle can live in a `OnceLock`. Sound because the job
/// handle is created once and only ever passed to Win32 calls that take it by
/// value.
struct Job(HANDLE);
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

static JOB: OnceLock<Option<Job>> = OnceLock::new();

fn job() -> Option<HANDLE> {
    JOB.get_or_init(|| unsafe {
        let handle = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_ok();
        if !ok {
            return None;
        }
        Some(Job(handle))
    })
    .as_ref()
    .map(|j| j.0)
}

/// Tie a spawned child's lifetime to this process.
///
/// Best-effort: a failure here costs a possible orphan, which is strictly
/// better than refusing to transcribe.
pub fn adopt(pid: u32) {
    let Some(job) = job() else {
        crate::log!("job object unavailable; sidecar may outlive Verba");
        return;
    };
    unsafe {
        let Ok(proc) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) else {
            return;
        };
        if AssignProcessToJobObject(job, proc).is_err() {
            crate::log!("could not adopt sidecar pid {pid}");
        }
        let _ = windows::Win32::Foundation::CloseHandle(proc);
    }
}
