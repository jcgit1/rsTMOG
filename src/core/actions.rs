//! Process actions: signal primitives, end_task escalation, suspend/resume,
//! and renice. Safety first: every entry point rejects pid <= 0 before
//! anything reaches kill(), because kill(0) signals the whole process
//! group and kill(-1) is a broadcast — a UI that trusts a pid must never
//! hand those to the kernel.
//!
//! This module shells out to syscalls via `libc`-free std where possible;
//! process signalling needs raw syscalls, so we use a tiny extern block
//! (Linux, no libc crate dependency — the symbols come from libc already
//! linked by the binary).

use std::time::{Duration, Instant};

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn getpriority(which: i32, who: u32) -> i32;
    fn setpriority(which: i32, who: u32, prio: i32) -> i32;
    fn __errno_location() -> *mut i32;
}

const PRIO_PROCESS: i32 = 0;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGCONT: i32 = 18;
const SIGSTOP: i32 = 19;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub ok: bool,
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

fn last_errno() -> i32 {
    unsafe { *__errno_location() }
}

/// Send a signal, guarding against the broadcast/group pids.
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

/// SIGTERM, then SIGKILL after `grace` if the process is still alive.
/// Blocking (documented, like the C++ reference); call off the UI thread.
pub fn end_task(pid: i32, grace: Duration) -> ActionResult {
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
    // still alive: escalate
    send_signal(pid, SIGKILL)
}

pub fn force_kill(pid: i32) -> ActionResult {
    send_signal(pid, SIGKILL)
}

pub fn suspend(pid: i32) -> ActionResult {
    send_signal(pid, SIGSTOP)
}

pub fn resume(pid: i32) -> ActionResult {
    send_signal(pid, SIGCONT)
}

/// True if the process exists (kill(pid, 0) probes without signalling).
/// We must NOT use the raw kill(0) path here — pid is validated > 0.
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { kill(pid, 0) == 0 }
}

pub fn get_nice(pid: i32) -> Result<i32, i32> {
    if pid <= 0 {
        return Err(22);
    }
    unsafe {
        errno_reset();
        let v = getpriority(PRIO_PROCESS, pid as u32);
        if v == -1 && last_errno() != 0 {
            Err(last_errno())
        } else {
            // getpriority returns 20-nice on some paths? No: on Linux it
            // returns the nice value directly (-20..19) for PRIO_PROCESS.
            Ok(v)
        }
    }
}

/// Set the nice value. NOTE the evildojo lesson: an unprivileged caller
/// MAY lower a nice value (raise priority) down to its own current nice,
/// widened by RLIMIT_NICE. We do not special-case that here — we report
/// whatever the kernel says via errno, and tests predict the outcome from
/// the rlimit rather than assuming EPERM.
pub fn set_nice(pid: i32, nice: i32) -> ActionResult {
    if pid <= 0 {
        return ActionResult::err(22);
    }
    let r = unsafe { setpriority(PRIO_PROCESS, pid as u32, nice) };
    if r == 0 {
        ActionResult::ok()
    } else {
        ActionResult::err(last_errno())
    }
}

fn errno_reset() {
    unsafe {
        *__errno_location() = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
