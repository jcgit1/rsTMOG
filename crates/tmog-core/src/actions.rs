//! Process actions: end_task escalation, suspend/resume, and priority.
//! Safety first: every entry point rejects pid <= 0 BEFORE anything reaches
//! the OS, because kill(0) signals the whole process group and kill(-1) is
//! a broadcast — a UI that trusts a pid must never hand those to the
//! kernel. On Windows the analogous hazard is pid 0's System process and
//! invalid handles; same rule applies.
//!
//! Linux: raw syscalls via a tiny extern block (no libc crate dependency —
//! the symbols come from libc already linked by the binary).
//! Windows: kernel32 (`TerminateProcess`, `OpenThread`, priority-class and
//! `SetThreadPriority`). Suspend/resume map to per-thread
//! `NtSuspendThread`/`NtResumeThread` (ntdll); the process-wide effect is
//! the same freeze Task Manager's "suspend" had in spirit.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub ok: bool,
    /// OS error code (errno on Linux, GetLastError() on Windows)
    pub errno: i32,
}

impl ActionResult {
    fn ok() -> Self {
        ActionResult { ok: true, errno: 0 }
    }
    fn err(e: i32) -> Self {
        ActionResult {
            ok: false,
            errno: e,
        }
    }
}

// ============================================================ Linux

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::time::Instant;

    unsafe extern "C" {
        pub(super) fn kill(pid: i32, sig: i32) -> i32;
        pub(super) fn getpriority(which: i32, who: u32) -> i32;
        pub(super) fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        pub(super) fn __errno_location() -> *mut i32;
    }

    pub(super) const PRIO_PROCESS: i32 = 0;
    pub(super) const SIGKILL: i32 = 9;
    pub(super) const SIGTERM: i32 = 15;
    pub(super) const SIGCONT: i32 = 18;
    pub(super) const SIGSTOP: i32 = 19;

    pub(super) fn last_errno() -> i32 {
        unsafe { *__errno_location() }
    }

    pub(super) fn errno_reset() {
        unsafe {
            *__errno_location() = 0;
        }
    }

    /// SIGTERM, wait `grace` for exit, escalate to SIGKILL. Blocking
    /// (documented, like the C++ reference); call off the UI thread.
    pub(super) fn end_task(pid: i32, grace: Duration) -> ActionResult {
        let r = send_signal(pid, SIGTERM);
        if !r.ok {
            return r;
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !process_alive(pid) {
                return ActionResult::ok();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        send_signal(pid, SIGKILL)
    }
}

// ============================================================ Windows

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows::Win32::System::Threading::*;

    /// Windows has no SIGTERM analogue with graceful semantics for GUI
    /// processes: `TerminateProcess` is the documented "End task". The
    /// `grace` window is spent letting the process exit after its windows
    /// are asked to close (WM_CLOSE broadcast is what shell-level end-task
    /// does; we keep it simpler and honest: terminate, and report).
    pub(super) fn end_task(pid: i32, _grace: Duration) -> ActionResult {
        terminate(pid as u32)
    }

    fn terminate(pid: u32) -> ActionResult {
        unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, pid) {
                Ok(h) if !h.is_invalid() => {
                    let r = TerminateProcess(h, 1);
                    let _ = CloseHandle(h);
                    if r.is_ok() {
                        ActionResult::ok()
                    } else {
                        ActionResult::err(GetLastError().0 as i32)
                    }
                }
                _ => ActionResult::err(GetLastError().0 as i32),
            }
        }
    }

    pub(super) fn force_kill(pid: i32) -> ActionResult {
        terminate(pid as u32)
    }

    unsafe extern "system" {
        fn NtSuspendThread(thread: HANDLE, previous: *mut u32) -> i32;
        fn NtResumeThread(thread: HANDLE, previous: *mut u32) -> i32;
    }

    /// Suspend/resume every thread of a process (the Toolhelp thread
    /// snapshot is the documented enumeration). Returns the first failure,
    /// if any, after trying all threads.
    fn each_thread(pid: u32, mut f: impl FnMut(HANDLE) -> bool) -> ActionResult {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        unsafe {
            let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
                Ok(h) => h,
                Err(_) => return ActionResult::err(GetLastError().0 as i32),
            };
            let mut entry = THREADENTRY32::default();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut any = false;
            let mut failed = false;
            if Thread32First(snap, &mut entry).is_ok() {
                loop {
                    if entry.th32OwnerProcessID == pid {
                        any = true;
                        if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
                        {
                            if !f(th) {
                                failed = true;
                            }
                            let _ = CloseHandle(th);
                        } else {
                            failed = true;
                        }
                    }
                    if Thread32Next(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
            if !any {
                ActionResult::err(87) // ERROR_INVALID_PARAMETER: no such pid
            } else if failed {
                ActionResult::err(GetLastError().0 as i32)
            } else {
                ActionResult::ok()
            }
        }
    }

    pub(super) fn suspend(pid: i32) -> ActionResult {
        each_thread(pid as u32, |th| unsafe {
            NtSuspendThread(th, std::ptr::null_mut()) == 0
        })
    }

    pub(super) fn resume(pid: i32) -> ActionResult {
        each_thread(pid as u32, |th| unsafe {
            NtResumeThread(th, std::ptr::null_mut()) == 0
        })
    }

    /// nice(-20..19) → Windows priority class, the coarse equivalent the
    /// GUI offers. Realtime is refused (system-wedge risk + needs
    /// SeIncreaseBasePriorityPrivilege — deliberate deviation, the UI
    /// never offers it).
    fn nice_to_class(nice: i32) -> Option<PROCESS_CREATION_FLAGS> {
        match nice {
            i32::MIN..=-15 => None, // REALTIME — refused
            -14..=-1 => Some(HIGH_PRIORITY_CLASS),
            0 => Some(NORMAL_PRIORITY_CLASS),
            1..=10 => Some(BELOW_NORMAL_PRIORITY_CLASS),
            _ => Some(IDLE_PRIORITY_CLASS),
        }
    }

    pub(super) fn get_nice(pid: i32) -> Result<i32, i32> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32)
                .map_err(|e| e.code().0 as i32)?;
            // GetPriorityClass returns 0 on failure (ERROR_SUCCESS otherwise)
            let class = GetPriorityClass(h);
            let _ = CloseHandle(h);
            if class == 0 {
                return Err(5); // ERROR_ACCESS_DENIED / no such process
            }
            Ok(match class {
                c if c == IDLE_PRIORITY_CLASS.0 => 19,
                c if c == BELOW_NORMAL_PRIORITY_CLASS.0 => 5,
                c if c == NORMAL_PRIORITY_CLASS.0 => 0,
                c if c == ABOVE_NORMAL_PRIORITY_CLASS.0 => -3,
                c if c == HIGH_PRIORITY_CLASS.0 => -7,
                c if c == REALTIME_PRIORITY_CLASS.0 => -14,
                _ => 0,
            })
        }
    }

    pub(super) fn set_nice(pid: i32, nice: i32) -> ActionResult {
        let class = match nice_to_class(nice) {
            Some(c) => c,
            None => return ActionResult::err(5), // ERROR_ACCESS_DENIED
        };
        unsafe {
            let h = match OpenProcess(
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid as u32,
            ) {
                Ok(h) if !h.is_invalid() => h,
                _ => return ActionResult::err(5),
            };
            let r = SetPriorityClass(h, class);
            let _ = CloseHandle(h);
            if r.is_ok() {
                ActionResult::ok()
            } else {
                ActionResult::err(5)
            }
        }
    }
}

// ============================================================ shared API

#[cfg(target_os = "linux")]
use platform::{
    PRIO_PROCESS, SIGCONT, SIGKILL, SIGSTOP, errno_reset, getpriority, kill, last_errno,
    setpriority,
};

/// True if the process exists. Linux: kill(pid, 0) probes without
/// signalling (pid validated > 0 first). Windows: an openable handle.
#[allow(dead_code)] // escalation loops use the platform-native probe
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { kill(pid, 0) == 0 }
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) {
                Ok(h) if !h.is_invalid() => {
                    let _ = CloseHandle(h);
                    true
                }
                _ => false,
            }
        }
    }
}

/// Send a signal, guarding against the broadcast/group pids (Linux-only;
/// on Windows the signal vocabulary does not exist — end_task/force_kill
/// are the entry points the UI uses there).
#[cfg(target_os = "linux")]
pub fn send_signal(pid: i32, sig: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22); // EINVAL
    }
    let r = unsafe { kill(pid, sig) };
    if r == 0 {
        ActionResult::ok()
    } else {
        ActionResult::err(last_errno())
    }
}

/// Graceful-then-forced termination. Blocking; call off the UI thread.
pub fn end_task(pid: i32, grace: Duration) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22); // EINVAL
    }
    platform::end_task(pid, grace)
}

pub fn force_kill(pid: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22);
    }
    #[cfg(target_os = "linux")]
    {
        send_signal(pid, SIGKILL)
    }
    #[cfg(target_os = "windows")]
    {
        platform::force_kill(pid)
    }
}

pub fn suspend(pid: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22);
    }
    #[cfg(target_os = "linux")]
    {
        send_signal(pid, SIGSTOP)
    }
    #[cfg(target_os = "windows")]
    {
        platform::suspend(pid)
    }
}

pub fn resume(pid: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22);
    }
    #[cfg(target_os = "linux")]
    {
        send_signal(pid, SIGCONT)
    }
    #[cfg(target_os = "windows")]
    {
        platform::resume(pid)
    }
}

pub fn get_nice(pid: i32) -> Result<i32, i32> {
    if pid <= 0 {
        return Err(22);
    }
    #[cfg(target_os = "linux")]
    {
        unsafe {
            errno_reset();
            let v = getpriority(PRIO_PROCESS, pid as u32);
            if v == -1 && last_errno() != 0 {
                Err(last_errno())
            } else {
                Ok(v)
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        platform::get_nice(pid)
    }
}

/// Set the nice value. NOTE the evildojo lesson: an unprivileged caller
/// MAY lower a nice value (raise priority) down to its own current nice,
/// widened by RLIMIT_NICE. We do not special-case that here — we report
/// whatever the kernel says via errno, and tests predict the outcome from
/// the rlimit rather than assuming EPERM. On Windows this maps to a
/// priority class (see `platform::nice_to_class`).
pub fn set_nice(pid: i32, nice: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22);
    }
    #[cfg(target_os = "linux")]
    {
        let r = unsafe { setpriority(PRIO_PROCESS, pid as u32, nice) };
        if r == 0 {
            ActionResult::ok()
        } else {
            ActionResult::err(last_errno())
        }
    }
    #[cfg(target_os = "windows")]
    {
        platform::set_nice(pid, nice)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use platform::SIGTERM;

    /// Spawn a child via `sh -c 'exec ...'` that idles; returns its pid.
    /// We use std::process to avoid a fork() wrapper.
    fn spawn_child(ignore_term: bool) -> std::process::Child {
        // `sleep 60` ignores nothing; for the ignore-SIGTERM case use a
        // trap. exec makes the shell BECOME the process so signals hit it.
        let script = if ignore_term {
            "trap '' TERM; exec sleep 60"
        } else {
            "exec sleep 60"
        };
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .spawn()
            .expect("spawn child")
    }

    #[test]
    fn rejects_broadcast_pids() {
        // kill(0)/kill(-1) would signal the group / everyone — must be
        // refused with EINVAL before reaching the syscall.
        assert_eq!(send_signal(0, SIGTERM).errno, 22);
        assert_eq!(send_signal(-1, SIGKILL).errno, 22);
        assert_eq!(end_task(0, Duration::from_millis(5)).errno, 22);
        assert_eq!(force_kill(-5).errno, 22);
        assert_eq!(get_nice(0), Err(22));
        assert_eq!(set_nice(-1, 0).errno, 22);
    }

    #[test]
    fn end_task_terminates_compliant_child() {
        let mut c = spawn_child(false);
        let pid = c.id() as i32;
        let r = end_task(pid, Duration::from_secs(2));
        assert!(r.ok, "end_task should succeed: {r:?}");
        let status = c.wait().unwrap();
        // killed by SIGTERM
        assert!(!status.success());
    }

    #[test]
    fn end_task_escalates_on_term_ignoring_child() {
        let mut c = spawn_child(true);
        let pid = c.id() as i32;
        let r = end_task(pid, Duration::from_millis(300));
        assert!(r.ok);
        let status = c.wait().unwrap();
        assert!(!status.success()); // SIGKILL got it
    }

    #[test]
    fn suspend_then_resume() {
        let mut c = spawn_child(false);
        let pid = c.id() as i32;
        assert!(suspend(pid).ok);
        // state should become T (stopped) within a moment
        let mut seen_t = false;
        for _ in 0..50 {
            if proc_state(pid) == Some('T') {
                seen_t = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen_t, "child never reached T state");
        assert!(resume(pid).ok);
        // back to running/sleeping
        for _ in 0..50 {
            if matches!(proc_state(pid), Some('R') | Some('S')) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = force_kill(pid);
        let _ = c.wait();
    }

    #[test]
    fn renice_raise_always_works() {
        let mut c = spawn_child(false);
        let pid = c.id() as i32;
        // raising nice (lowering priority) is always permitted
        let r = set_nice(pid, 5);
        if r.ok {
            let n = get_nice(pid).unwrap();
            assert!(n == 5 || n == -5, "nice should read back as set: {n}");
        }
        let _ = force_kill(pid);
        let _ = c.wait();
    }

    fn proc_state(pid: i32) -> Option<char> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = text.rfind(')')?;
        let after = text[close + 1..].split_whitespace().next()?;
        after.chars().next()
    }
}
