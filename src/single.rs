//! Single-instance guard for the GUI: one rsTMOG window per user session.
//!
//! A plain `flock(LOCK_EX | LOCK_NB)` on a per-user lock file is the
//! arbiter — the kernel releases it automatically when the holder exits,
//! so a crashed instance never wedges future launches. The file carries
//! `pid\nstarttime` so a second launch can (a) verify the holder is the
//! same process it claims (starttime = /proc/[pid]/stat field 22, the
//! pid-reuse guard used throughout tmog-core) and (b) signal it.
//!
//! Focus policy lives in the GUI: a second launch sends `SIGUSR1` and
//! exits; the running instance's repaint loop notices and asks the
//! compositor to show/focus its window (the app-internal path — Wayland
//! forbids raising another app's window from outside).

use std::path::{Path, PathBuf};

/// Raw lock guard: holds the flock by owning the fd; dropping closes it
/// and releases the lock. The inner File is never read — ownership is
/// the whole point.
#[allow(dead_code)]
pub struct LockFile(std::fs::File);

/// Outcome of trying to become the single instance.
pub enum Acquisition {
    /// We hold the lock; keep the value alive for the process lifetime.
    Acquired(LockFile),
    /// Another live instance holds it. `holder_pid` is advisory (read
    /// from the lock file; may be stale) and only feeds the message.
    Running { holder_pid: Option<u32> },
}

/// Where the lock lives: `$XDG_RUNTIME_DIR/rstmog.lock`, falling back to
/// `$TMPDIR/rstmog-<uid>.lock` (uid-suffixed so other users never share
/// or pre-create our lock).
pub fn lock_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
        && Path::new(&dir).is_dir()
    {
        return Path::new(&dir).join("rstmog.lock");
    }
    let mut name = std::ffi::OsString::from("rstmog-");
    // getuid via libc FFI (no libc crate; same pattern as netdev.rs)
    name.push(uid().to_string());
    name.push(".lock");
    std::env::temp_dir().join(name)
}

fn uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Try to take the instance lock. flock is the arbiter: a dead holder's
/// lock is gone with its fd, so contention here always means a live
/// holder — its recorded pid is reported for the user message.
pub fn acquire(path: &Path) -> Acquisition {
    match try_acquire(path) {
        Ok(lock) => Acquisition::Acquired(lock),
        Err(()) => Acquisition::Running {
            holder_pid: read_holder(path).map(|(p, _)| p),
        },
    }
}

fn try_acquire(path: &Path) -> Result<LockFile, ()> {
    // create with 0600: the lock file names a pid — not world content
    #[allow(unused_imports)]
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
    {
        Ok(f) => f,
        Err(_) => return Err(()),
    };
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    unsafe extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    // keep the fd alive across the call; File owns it afterwards
    let fd = {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    };
    if unsafe { flock(fd, LOCK_EX | LOCK_NB) } != 0 {
        return Err(());
    }
    // We hold it — stamp our identity. write(2) via std::fs::File's own
    // fd (no buffered writer; std Write for File is unbuffered), so a
    // half-written line can't poison the next launch's holder check.
    let text = format!("{}\n{}\n", std::process::id(), self_starttime());
    {
        use std::io::Write;
        let mut f = &file;
        let _ = f.write_all(text.as_bytes());
    }
    Ok(LockFile(file))
}

/// Parse `pid\nstarttime` from the lock file.
pub fn read_holder(path: &Path) -> Option<(u32, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse::<u32>().ok()?;
    // a missing/garbage starttime means the stamp half-failed: trust the
    // pid (flock already proved the holder is live when we got here)
    let starttime = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0);
    if pid == 0 {
        return None;
    }
    Some((pid, starttime))
}

/// starttime (field 22) of the current process.
fn self_starttime() -> u64 {
    field22_of(std::process::id())
}

fn field22_of(pid: u32) -> u64 {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // comm (field 2) can contain spaces and parens: skip past the last ')'
    let rest = match text.rfind(')') {
        Some(i) => &text[i + 1..],
        None => return 0,
    };
    // fields after comm start at 3; field 22 is index 19 in the remainder
    rest.split_whitespace()
        .nth(19)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// True if `pid` still exists AND still has `starttime` — the holder is
/// provably the same process that wrote the lock. Not used on the fast
/// path (flock already proves liveness); kept for diagnostics/tests.
#[allow(dead_code)]
pub fn holder_alive(pid: u32, starttime: u64) -> bool {
    if std::fs::metadata(format!("/proc/{pid}")).is_err() {
        return false;
    }
    // starttime 0 in the lock means the stamp failed back then; trust
    // /proc existence alone rather than refusing to report a holder
    starttime == 0 || field22_of(pid) == starttime
}

/// Ask the running instance to surface its window. SIGUSR1 is our
/// private ping (the GUI installs the handler); EPERM/ESRCH are
/// non-fatal — the caller then just reports that an instance is running.
pub fn request_surface(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGUSR1: i32 = 10;
    pid <= i32::MAX as u32 && unsafe { kill(pid as i32, SIGUSR1) } == 0
}

/// Install the SIGUSR1 handler. SA_RESTART so signals arriving during the
/// sampler's /proc reads don't turn into EINTR churn (the repaint loop
/// polls `surface_requested` anyway). Async-signal-safe handler: one
/// relaxed atomic store.
pub fn install_surface_handler() {
    extern "C" fn on_usr1(_sig: i32) {
        FLAG.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // must stay referenced for the process lifetime — the kernel keeps
    // the fn pointer, and we hand its address across FFI
    #[repr(C)]
    struct Sigaction {
        handler: usize,
        flags: u32,
        restorer: usize,
        mask: [u64; 16],
    }
    unsafe extern "C" {
        fn sigaction(signum: i32, act: *const Sigaction, old: *mut Sigaction) -> i32;
    }
    const SIGUSR1: i32 = 10;
    const SA_RESTART: u32 = 0x1000_0000;
    let act = Sigaction {
        handler: (on_usr1 as extern "C" fn(i32)) as usize,
        flags: SA_RESTART,
        restorer: 0,
        mask: [0; 16],
    };
    // failure just means a second launch cannot focus us (it still exits
    // quietly), so don't abort startup
    let _ = unsafe { sigaction(SIGUSR1, &act, std::ptr::null_mut()) };
}

static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True (once) after a SIGUSR1 arrived, clearing the flag.
pub fn surface_requested() -> bool {
    FLAG.swap(false, std::sync::atomic::Ordering::SeqCst)
}

// ---- test seams (no FFI needed) -------------------------------------

/// Test-only: flip the flag exactly as the handler would.
#[cfg(test)]
fn fake_signal() {
    FLAG.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_roundtrip_and_garbage() {
        let dir = std::env::temp_dir().join(format!("rstmog-single-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rstmog.lock");
        assert_eq!(read_holder(&p), None); // absent file
        std::fs::write(&p, "4242\n99\n").unwrap();
        assert_eq!(read_holder(&p), Some((4242, 99)));
        // pid 0 is never a holder
        std::fs::write(&p, "0\n99\n").unwrap();
        assert_eq!(read_holder(&p), None);
        // non-numeric junk
        std::fs::write(&p, "nope\n").unwrap();
        assert_eq!(read_holder(&p), None);
        // missing starttime → pid trusted, starttime 0
        std::fs::write(&p, "7\n").unwrap();
        assert_eq!(read_holder(&p), Some((7, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_stamp_is_self_consistent() {
        // the stamped holder must be the current process
        let dir = std::env::temp_dir().join(format!("rstmog-single-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rstmog.lock");
        let _lock = try_acquire(&p).expect("acquire fresh lock");
        assert_eq!(
            read_holder(&p),
            Some((std::process::id(), self_starttime()))
        );
        // our own starttime passes the liveness probe
        let (pid, st) = read_holder(&p).unwrap();
        assert!(holder_alive(pid, st));
        // a recycled-pid holder does not (starttime mismatch)
        assert!(!holder_alive(pid, st.wrapping_add(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_starttime_matches_proc_directly() {
        let raw = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        let rest = &raw[raw.rfind(')').unwrap() + 1..];
        let direct: u64 = rest.split_whitespace().nth(19).unwrap().parse().unwrap();
        assert_eq!(self_starttime(), direct);
    }

    #[test]
    fn surface_flag_is_edge_triggered() {
        // drain any earlier flag, then signal once
        let _ = surface_requested();
        assert!(!surface_requested());
        fake_signal();
        assert!(surface_requested());
        assert!(!surface_requested()); // consumed, not level
    }
}
