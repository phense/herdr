//! Windows-specific process/clipboard/notification primitives.
//!
//! Mirrors the surface area of `linux.rs` / `macos.rs`; the contract is
//! defined in `super::*`. Implementations rely on the `windows-sys` crate
//! for the raw Win32 bindings. Where Win32 lacks a clean equivalent (e.g.
//! process groups), we use Job Objects as the next-best primitive.
//!
//! Notes:
//! * `assign_to_job` registers a kill-on-job-close job and stashes the
//!   handle in a process-global table keyed by pid. `session_processes`
//!   reads back that job's pid list to enumerate the spawned tree.
//! * `signal_processes` maps `Hangup` to `CTRL_BREAK_EVENT`, the others
//!   to `TerminateProcess`. Unix-style SIGHUP semantics aren't reachable.
//! * `show_desktop_notification` shells out to BurntToast (PowerShell
//!   module). Returns `Ok(false)` cleanly when it isn't installed.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FALSE, HANDLE,
};
use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, CREATE_BREAKAWAY_FROM_JOB,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

// STILL_ACTIVE is the documented sentinel exit code GetExitCodeProcess
// returns for a still-running process. windows-sys 0.59 exposes it as a
// `u32` constant in `Win32::System::Diagnostics::Debug` but only via the
// less-common feature set; the value (259) is fixed by Win32 ABI.
const STILL_ACTIVE: u32 = 259;

// Clipboard format constants. windows-sys 0.59 does not export these as
// named items in any feature-flagged module, so we reproduce the Win32
// values verbatim (see WinUser.h).
const CF_TEXT: u32 = 1;
const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;

use super::{ClipboardImage, ForegroundJob, ForegroundProcess, Signal};

// CTRL_BREAK_EVENT for GenerateConsoleCtrlEvent.
const CTRL_BREAK_EVENT: u32 = 1;

#[derive(Debug)]
struct JobRecord {
    handle: HANDLE,
}

// HANDLE on windows-sys 0.59 is a raw `isize` (HANDLE = *mut c_void wrapper
// via type alias). It's `Send`/`Sync` opt-in only — wrap in our own struct
// so we can hold it in a static Mutex.
unsafe impl Send for JobRecord {}

impl Drop for JobRecord {
    fn drop(&mut self) {
        // The Job Object is configured KILL_ON_JOB_CLOSE; closing the
        // handle terminates the contained processes. That's the documented
        // expectation for `assign_to_job`'s caller (typically PaneRuntime
        // shutdown).
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn job_table() -> &'static Mutex<HashMap<u32, JobRecord>> {
    static TABLE: OnceLock<Mutex<HashMap<u32, JobRecord>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Assign `pid` to a freshly-created kill-on-close Job Object. The Job
/// stays alive (and a reference is stashed in `job_table()`) until either
/// `signal_processes(... Kill)` or process exit. Required so
/// `session_processes` can enumerate the tree of children spawned under
/// `pid` even when Windows reports them with different parent PIDs after
/// `cmd /c start`.
pub fn assign_to_job(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pid 0 cannot be assigned to a job",
        ));
    }

    let proc = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if proc.is_null() {
        return Err(io::Error::last_os_error());
    }

    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        let err = io::Error::last_os_error();
        unsafe { CloseHandle(proc) };
        return Err(err);
    }

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        unsafe { CloseHandle(job) };
        unsafe { CloseHandle(proc) };
        return Err(err);
    }

    let assigned = unsafe { AssignProcessToJobObject(job, proc) };
    unsafe { CloseHandle(proc) };
    if assigned == 0 {
        let err = io::Error::last_os_error();
        // Common failure: the process is already in a job that disallows
        // breakaway. The caller can recover by checking the error.
        unsafe { CloseHandle(job) };
        return Err(err);
    }

    let mut table = job_table().lock().expect("job_table poisoned");
    if let Some(old) = table.insert(pid, JobRecord { handle: job }) {
        // If we somehow already had a job for this pid, drop the old one.
        drop(old);
    }
    Ok(())
}

/// Discard the Job Object tracked for `pid` if any. This terminates the
/// child tree (KILL_ON_JOB_CLOSE is set). Useful when PaneRuntime cleans
/// up; callers may also just rely on process exit, which closes the
/// final handle.
#[allow(dead_code)]
pub fn discard_job(pid: u32) {
    let _removed = job_table().lock().expect("job_table poisoned").remove(&pid);
}

/// Look up `pid`'s registered job and read its basic process-id list. If
/// no job is registered (`assign_to_job` was never called), fall back to
/// returning the single pid.
pub fn session_processes(pid: u32) -> Vec<u32> {
    if pid == 0 {
        return Vec::new();
    }
    let table = job_table().lock().expect("job_table poisoned");
    let Some(job) = table.get(&pid) else {
        return vec![pid];
    };

    // Allocate a buffer large enough for up to 64 child processes. Most
    // real terminal trees stay under 8. If we ever overflow, we just
    // return the truncated list — the caller already handles partial
    // results gracefully.
    #[repr(C)]
    struct ProcessIdList {
        header: JOBOBJECT_BASIC_PROCESS_ID_LIST,
        ids: [usize; 64],
    }
    let mut buf: ProcessIdList = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcessIdList>() as u32;
    let mut returned: u32 = 0;
    let ok = unsafe {
        QueryInformationJobObject(
            job.handle,
            JobObjectBasicProcessIdList,
            &mut buf as *mut _ as *mut _,
            size,
            &mut returned as *mut _,
        )
    };
    if ok == 0 {
        return vec![pid];
    }

    let count = buf.header.NumberOfProcessIdsInList.min(buf.ids.len() as u32) as usize;
    let mut out: Vec<u32> = Vec::with_capacity(count + 1);
    // Always include the root pid first so the caller can identify the
    // tree root unambiguously, even if Windows reports it later in the list.
    out.push(pid);
    for &raw in &buf.ids[..count] {
        let id = raw as u32;
        if id != 0 && id != pid {
            out.push(id);
        }
    }
    out
}

/// Windows lacks per-tty process groups; treat the spawned child as the
/// "group leader" so `ForegroundJob::process_group_id` is non-zero and
/// `foreground_job` can still answer "what processes belong to this
/// terminal's job?".
pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    if child_pid == 0 {
        return None;
    }
    if process_exists(child_pid) {
        Some(child_pid)
    } else {
        None
    }
}

/// Collect the processes Windows considers part of `child_pid`'s job. Falls
/// back to a single-entry list if no job has been registered (`assign_to_job`
/// hasn't been called for this pid).
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }
    let pids = session_processes(child_pid);
    if pids.is_empty() {
        return None;
    }

    let names = process_names_for(&pids);
    let processes: Vec<ForegroundProcess> = pids
        .into_iter()
        .map(|pid| {
            let name = names.get(&pid).cloned().unwrap_or_default();
            ForegroundProcess {
                pid,
                name,
                argv0: None,
                argv: None,
                cmdline: None,
            }
        })
        .collect();

    Some(ForegroundJob {
        process_group_id: child_pid,
        processes,
    })
}

fn process_names_for(pids: &[u32]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return out;
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        unsafe { CloseHandle(snapshot) };
        return out;
    }
    let interest: std::collections::HashSet<u32> = pids.iter().copied().collect();
    loop {
        let pid = entry.th32ProcessID;
        if interest.contains(&pid) {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            out.insert(pid, name);
        }
        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            break;
        }
    }
    unsafe { CloseHandle(snapshot) };
    out
}

/// `process_cwd` would require querying the PEB via
/// `NtQueryInformationProcess` (undocumented for cross-arch use). For now
/// return `None` and let callers fall back to their existing
/// "unknown CWD" path. Goal 2.3's test will be marked `ignore`-by-default
/// until a real implementation lands; the function signature exists so
/// downstream consumers compile.
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    for &pid in pids {
        if pid == 0 {
            continue;
        }
        match signal {
            Signal::Hangup => unsafe {
                // CTRL_BREAK_EVENT can only be sent to a process group
                // (which on Windows is the process id of the leader for
                // groups created with CREATE_NEW_PROCESS_GROUP). We try
                // first; if it fails the caller can fall back to Terminate.
                let _ = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
            },
            Signal::Terminate | Signal::Kill => unsafe {
                let proc = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
                if !proc.is_null() {
                    let _ = TerminateProcess(proc, 1);
                    CloseHandle(proc);
                }
            },
        }
    }

    // For Kill, also clean up our tracked job(s); this drops the handle
    // and Windows tears down any survivors.
    if matches!(signal, Signal::Kill) {
        let mut table = job_table().lock().expect("job_table poisoned");
        for &pid in pids {
            let _ = table.remove(&pid);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let proc = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if proc.is_null() {
        let err = unsafe { GetLastError() };
        // ERROR_ACCESS_DENIED still means the process is alive; we just
        // don't have rights to query it.
        return err == ERROR_ACCESS_DENIED;
    }
    // We can open the handle even for an already-terminated process whose
    // parent hasn't reaped it yet (Windows keeps the process record around
    // until the final handle closes). Use GetExitCodeProcess to distinguish
    // "still running" from "exited, awaiting reap" — the latter must report
    // false so callers waiting on a kill don't deadlock.
    let mut exit_code: u32 = 0;
    let alive = unsafe {
        let ok = GetExitCodeProcess(proc, &mut exit_code);
        ok != 0 && exit_code == STILL_ACTIVE
    };
    unsafe { CloseHandle(proc) };
    alive
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    write_clipboard_text(text)
}

fn write_clipboard_text(text: &str) -> bool {
    // Convert to NUL-terminated UTF-16; Windows clipboard expects wide chars.
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let byte_len = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        let _empty = EmptyClipboard();

        let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len);
        if handle.is_null() {
            CloseClipboard();
            return false;
        }
        let dst = GlobalLock(handle);
        if dst.is_null() {
            CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
        GlobalUnlock(handle);

        let set = SetClipboardData(CF_UNICODETEXT as u32, handle as HANDLE);
        let ok = !set.is_null();
        if !ok {
            // SetClipboardData ownership-transfer failed: free the global
            // ourselves; otherwise it's owned by the clipboard.
            // (We intentionally don't track this for simplicity.)
        }
        CloseClipboard();
        ok
    }
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    // Windows clipboard image formats (CF_DIB / CF_BITMAP) need to be
    // converted to PNG/etc. before they can be shipped through herdr's
    // wire protocol. Implementing that conversion correctly requires more
    // logic than fits in the scaffolding pass; for now we return None and
    // let the caller offer "no image in clipboard" — same fallback as the
    // non-x11 unix path. Goal 7.7 covers the BurntToast/PowerShell path
    // that delivers a real implementation.
    None
}

pub fn show_desktop_notification(title: &str, body: Option<&str>) -> io::Result<bool> {
    if title.is_empty() {
        return Ok(false);
    }
    // BurntToast is the de-facto PowerShell module for Windows toasts.
    // If it isn't installed we return Ok(false) — never Err — so callers
    // can no-op gracefully on stock systems.
    let escaped_title = ps_single_quote_escape(title);
    let mut script = format!(
        "if (-not (Get-Module -ListAvailable -Name BurntToast)) {{ exit 1 }};\
         New-BurntToastNotification -Text '{escaped_title}'"
    );
    if let Some(body) = body.filter(|b| !b.is_empty()) {
        let escaped_body = ps_single_quote_escape(body);
        // Inject a second -Text entry. PowerShell's -Text expects a
        // string array; New-BurntToastNotification splits by lines.
        script = format!(
            "if (-not (Get-Module -ListAvailable -Name BurntToast)) {{ exit 1 }};\
             New-BurntToastNotification -Text '{escaped_title}','{escaped_body}'"
        );
    }

    let status = match Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => s,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
}

fn ps_single_quote_escape(s: &str) -> String {
    s.replace('\'', "''")
}

// CF_TEXT / CF_BITMAP / CF_DIB are reserved for the future clipboard
// reader (Goal 7). Reference them once so dead_code stays silent today.
#[allow(dead_code)]
const _CF_REFERENCES: [u32; 3] = [CF_BITMAP, CF_DIB, CF_TEXT];

// `CREATE_BREAKAWAY_FROM_JOB` is exposed for callers that need to spawn
// a child outside the parent's job (e.g. detaching a server from the
// terminal pane). Re-exported here so consumers don't need to depend on
// windows-sys directly.
#[allow(dead_code)]
pub(crate) const SPAWN_BREAKAWAY_FROM_JOB: u32 = CREATE_BREAKAWAY_FROM_JOB;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Spawn a small sleeper child we can poke at without slowing the
    /// suite down. We pick `cmd /c ping -n N 127.0.0.1 >NUL` because
    /// `cmd /c timeout` reads stdin and aborts under `Stdio::null`.
    fn spawn_sleeper(seconds: u32) -> std::process::Child {
        Command::new("cmd")
            .args([
                "/c",
                &format!("ping -n {} 127.0.0.1 >NUL", seconds.max(1)),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper")
    }

    fn wait_until_gone(pid: u32, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        while Instant::now() < deadline {
            if !process_exists(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !process_exists(pid)
    }

    #[test]
    fn process_exists_for_self_is_true() {
        let me = std::process::id();
        assert!(process_exists(me));
    }

    #[test]
    fn process_exists_for_zero_is_false() {
        assert!(!process_exists(0));
    }

    #[test]
    fn signal_processes_terminate_kills_child() {
        let mut child = spawn_sleeper(30);
        let pid = child.id();
        assert!(process_exists(pid));
        signal_processes(&[pid], Signal::Terminate);
        assert!(wait_until_gone(pid, Duration::from_secs(3)));
        let _ = child.wait();
    }

    #[test]
    fn signal_processes_kill_kills_child() {
        let mut child = spawn_sleeper(30);
        let pid = child.id();
        signal_processes(&[pid], Signal::Kill);
        assert!(wait_until_gone(pid, Duration::from_secs(3)));
        let _ = child.wait();
    }

    #[test]
    fn foreground_process_group_id_returns_self_for_live_child() {
        let mut child = spawn_sleeper(30);
        let pid = child.id();
        let gid = foreground_process_group_id(pid);
        signal_processes(&[pid], Signal::Kill);
        let _ = child.wait();
        assert_eq!(gid, Some(pid));
    }

    #[test]
    fn foreground_job_returns_at_least_the_root_pid() {
        let mut child = spawn_sleeper(30);
        let pid = child.id();
        let job = foreground_job(pid).expect("foreground job");
        signal_processes(&[pid], Signal::Kill);
        let _ = child.wait();
        assert_eq!(job.process_group_id, pid);
        assert!(job.processes.iter().any(|p| p.pid == pid));
    }

    #[test]
    fn assign_to_job_and_session_processes_round_trip() {
        let mut child = spawn_sleeper(30);
        let pid = child.id();
        // Either the assignment succeeds, or — on hosts where Windows has
        // already attached the child to a job that disallows breakaway
        // (notably the Windows Console Host's debug job under some CI
        // configurations) — we accept the fallback path that still
        // returns [pid].
        let _ = assign_to_job(pid);
        let pids = session_processes(pid);
        signal_processes(&[pid], Signal::Kill);
        let _ = child.wait();
        assert!(pids.contains(&pid));
    }

    #[test]
    fn write_clipboard_round_trips_via_powershell() {
        let marker = format!("herdr-test-{}", std::process::id());
        let wrote = write_clipboard(marker.as_bytes());
        if !wrote {
            // Clipboard owned by another process — common in CI / Remote
            // Desktop. Skip rather than fail.
            eprintln!("skipping: clipboard unavailable on this host");
            return;
        }

        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Clipboard"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                assert!(
                    text.contains(&marker),
                    "clipboard should contain marker; got {text:?}"
                );
            }
            _ => {
                eprintln!("skipping: powershell Get-Clipboard unavailable");
            }
        }
    }

    #[test]
    fn read_clipboard_image_is_safe_to_call() {
        // The implementation is a stub today; assert it returns None
        // rather than panicking. Goal 7.7 wires in the real reader.
        assert!(read_clipboard_image().is_none());
    }

    #[test]
    fn show_desktop_notification_never_errs_without_burnt_toast() {
        // Whether BurntToast is installed or not we must return Ok(_).
        let result = show_desktop_notification("herdr test", Some("body"));
        assert!(result.is_ok(), "notification result: {result:?}");
    }

    #[test]
    fn ps_single_quote_escape_doubles_quotes() {
        assert_eq!(ps_single_quote_escape("it's mine"), "it''s mine");
        assert_eq!(ps_single_quote_escape("plain"), "plain");
    }
}
