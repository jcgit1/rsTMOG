//! Windows `Collector` impl: the same logical quantities as the Linux
//! collectors, sourced from Win32 APIs. Only compiled for
//! `cfg(target_os = "windows")`; the Linux CI gate type-checks this module
//! with `cargo check --target x86_64-pc-windows-msvc` (rustc can compile,
//! just not link, without the MSVC linker).
//!
//! Mapping (Linux → Windows):
//! - `/proc/stat` cpu times   → `GetSystemTimes` (aggregate; always
//!   available, no service dependency) + PDH wildcard counters
//!   `\Processor(*)\% Processor Time` and `\% Idle Time` for per-core rows.
//! - `/proc/meminfo`          → `GlobalMemoryStatusEx`. There is no
//!   buffers/cached split on Windows; `mem_available` is filled from
//!   `ullAvailPhys`, which is what `Meminfo::used_percent` prefers.
//! - `/proc/[pid]/*`          → `CreateToolhelp32Snapshot` for identity,
//!   `GetProcessTimes` for CPU, `K32GetProcessMemoryInfo` for RSS/pagefile,
//!   remote PEB read for the command line, token → SID → name for owner.
//! - `/proc/diskstats`        → PDH `\PhysicalDisk(*)\Disk Read/Write Bytes/sec`
//!   (rates integrated into monotonic counters — see `Pdh::disks`).
//! - `/proc/net/dev`          → `GetIfTable2` (iphlpapi) byte counters.
//! - `hwmon` temps            → no documented Win32 API → empty (Thermals
//!   page shows a notice; NVML GPU temperature still works because
//!   nvml-wrapper loads nvml.dll).
//! - ZFS ARC, PSI, loadavg    → Linux-only → `None`.
//! - `/proc/cpuinfo`          → `GetLogicalProcessorInformationEx` + CPUID.
//! - `kill(2)` in `actions`   → `TerminateProcess` (see `actions`).
//!
//! PDH handles are opened once at construction; `PdhCollectQueryData` is
//! one cheap call per tick. If the Performance Counters service is
//! unavailable, `Pdh::new` returns `None` and the app degrades to
//! `GetSystemTimes` aggregate CPU with no disk rates — the same "None
//! means dash" contract the NVML handle already uses.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Security::{GetTokenInformation, LookupAccountSidW, TOKEN_QUERY, TokenUser};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::*;
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::SystemInformation::*;
use windows::Win32::System::Threading::*;
use windows::core::{PCWSTR, PWSTR};

use crate::cpu::CpuJiffies;
use crate::cpuinfo::CpuInfo;
use crate::diskstats::DiskCounters;
use crate::gpu::{GpuInfo, Nvml};
use crate::meminfo::Meminfo;
use crate::netdev::NetDevCounters;
use crate::platform::{Collector, RawCounters};
use crate::process::Proc;
use crate::sysinfo::SysInfo;

/// Windows CPU times are 100 ns units; we express them in 10 ms "jiffies"
/// so `CpuJiffies` arithmetic and the per-process CPU% denominator behave
/// exactly like the Linux path (100 jiffies == 1 second).
const JIFFY_100NS: u64 = 100_000;

/// Synthetic jiffies each PDH percentage sample contributes per collect.
/// A collect window is ≥100 ms (the app's fastest poll is 500 ms), so
/// rounding a percentage into this window loses <1% even worst-case:
/// busy_percent over one collect is exactly (100 − idle)%.
const SYNTH_JIFFIES_PER_COLLECT: u64 = 10_000;

pub struct WindowsCollector {
    nvml: Option<Nvml>,
    pdh: Option<Pdh>,
    /// static identity + topology: collected once, cloned per tick
    sysinfo: SysInfo,
    cpu_info: Option<CpuInfo>,
}

impl WindowsCollector {
    pub fn new() -> Self {
        Self {
            nvml: Nvml::new(),
            pdh: Pdh::new(),
            sysinfo: collect_sysinfo(),
            cpu_info: collect_cpu_info(),
        }
    }

    /// Aggregate CPU from `GetSystemTimes`, in 10 ms jiffies. Note the
    /// Windows quirk: kernel time INCLUDES idle.
    fn aggregate_jiffies() -> Option<CpuJiffies> {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        let j = |ft: &FILETIME| -> u64 {
            ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) / JIFFY_100NS
        };
        let idle = j(&idle);
        let kernel = j(&kernel);
        let user = j(&user);
        Some(CpuJiffies {
            name: "cpu".into(),
            user,
            nice: 0,
            system: kernel.saturating_sub(idle),
            idle,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        })
    }
}

impl Default for WindowsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for WindowsCollector {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn sample(&mut self, _now_us: u64) -> RawCounters {
        let mut cpu = Vec::new();
        if let Some(agg) = Self::aggregate_jiffies() {
            cpu.push(("cpu".to_string(), agg));
        }
        if let Some(pdh) = &self.pdh {
            if let Some(per_core) = pdh.cpu_per_core() {
                cpu.extend(per_core);
            }
            // disks() reuses this tick's collect (guarded internally).
            let disks = pdh.disks();
            return RawCounters {
                cpu,
                mem: collect_meminfo(),
                arc: None,  // Linux-only
                load: None, // Linux-only
                uptime_s: uptime_seconds(),
                procs: collect_procs(),
                disks: disks.unwrap_or_default(),
                nets: collect_netdevs(),
                temps: Vec::new(),
                psi_memory: None, // Linux-only
                psi_io: None,     // Linux-only
                sysinfo: self.sysinfo.clone(),
                cpu_info: self.cpu_info.clone(),
            };
        }
        RawCounters {
            cpu,
            mem: collect_meminfo(),
            arc: None,
            load: None,
            uptime_s: uptime_seconds(),
            procs: collect_procs(),
            disks: Vec::new(),
            nets: collect_netdevs(),
            temps: Vec::new(),
            psi_memory: None,
            psi_io: None,
            sysinfo: self.sysinfo.clone(),
            cpu_info: self.cpu_info.clone(),
        }
    }

    fn gpu(&self) -> Option<GpuInfo> {
        self.nvml.as_ref().and_then(|n| n.sample(0))
    }

    fn set_root(&mut self, _root: PathBuf) {
        // Win32 APIs have no fixture root; Windows collectors are verified
        // on-target by scripts/win-verify.ps1 and --self-test/--smoke.
    }
}

// SAFETY: the PDH query is only touched from the sampler thread (the one
// place `Collector::sample` runs); the atomics/mutexes below keep the
// bookkeeping sound if a second thread ever peeks.
unsafe impl Send for WindowsCollector {}

// ---------------------------------------------------------------- memory

/// `GlobalMemoryStatusEx` → `Meminfo`, KiB throughout (the Linux parser's
/// unit). Page file is reported with the physical totals subtracted from
/// the combined committed totals, matching Task Manager's "Committed".
fn collect_meminfo() -> Option<Meminfo> {
    let mut st = MEMORYSTATUSEX::default();
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut st) }.ok()?;
    let kib = |b: u64| b / 1024;
    Some(Meminfo {
        mem_total: kib(st.ullTotalPhys),
        mem_free: kib(st.ullAvailPhys),
        mem_available: Some(kib(st.ullAvailPhys)),
        buffers: 0,
        cached: 0,
        s_reclaimable: 0,
        swap_total: kib(st.ullTotalPageFile.saturating_sub(st.ullTotalPhys)),
        swap_free: kib(st.ullAvailPageFile.saturating_sub(st.ullAvailPhys)),
    })
}

// ---------------------------------------------------------------- uptime

/// `GetTickCount64` is monotonic since boot and cannot fail — the honest
/// equivalent of `/proc/uptime`'s first field.
fn uptime_seconds() -> Option<f64> {
    Some(unsafe { GetTickCount64() } as f64 / 1000.0)
}

// ---------------------------------------------------------------- procs

/// Process snapshot → `Proc` rows. Command line and owner are best-effort:
/// protected/elevated processes we cannot open keep empty values, and
/// `display_name` falls back to the image name, like Linux kernel-thread
/// rows.
fn collect_procs() -> Vec<Proc> {
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    unsafe {
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                out.push(proc_from_entry(&entry));
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

unsafe fn proc_from_entry(entry: &PROCESSENTRY32W) -> Proc {
    let pid = entry.th32ProcessID;
    let exe = wide_array_to_string(&entry.szExeFile);
    let comm = exe.rsplit(['\\', '/']).next().unwrap_or(&exe).to_string();

    let mut p = Proc {
        pid,
        comm,
        // Toolhelp has no task state char; 'S' is the honest neutral —
        // the UI renders unknown states as plain rows.
        state: 'S',
        ppid: entry.th32ParentProcessID,
        utime: 0,
        stime: 0,
        starttime: 0,
        nice: 0, // no Windows analogue (priority classes are not nice values)
        threads: entry.cntThreads,
        rss_bytes: 0,
        minflt: 0,
        majflt: 0,
        vm_swap_kib: 0,
        last_cpu: u32::MAX, // Windows exposes no cheap "last CPU"
        uid: None,
        user: process_owner(pid),
        cmdline: Vec::new(),
    };

    let Some(h) = open_process_limited(pid) else {
        return p;
    };
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kern = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(h, &mut creation, &mut exit, &mut kern, &mut user).is_ok() {
            let to_j = |ft: &FILETIME| -> u64 {
                ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) / JIFFY_100NS
            };
            p.utime = to_j(&user);
            p.stime = to_j(&kern);
            p.starttime = to_j(&creation);
        }
        let mut pmc = PROCESS_MEMORY_COUNTERS_EX::default();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if K32GetProcessMemoryInfo(
            h,
            &mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX as *mut _,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
        .as_bool()
        {
            p.rss_bytes = pmc.WorkingSetSize as u64;
            // PrivateUsage (PROCESS_MEMORY_COUNTERS_EX) is the pagefile-
            // backed private charge — the closest analogue to VmSwap.
            p.vm_swap_kib = pmc.PrivateUsage as u64 / 1024;
            p.minflt = pmc.PageFaultCount as u64;
        }
        p.cmdline = read_cmdline(h);
        let _ = CloseHandle(h);
    }
    p
}

/// Limited-information opens work across most integrity levels without
/// SeDebugPrivilege. TERMINATE is included so the process-actions layer
/// can reuse the same access contract.
fn open_process_limited(pid: u32) -> Option<HANDLE> {
    let flags = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_TERMINATE;
    match unsafe { OpenProcess(flags, false, pid) } {
        Ok(h) if !h.is_invalid() => Some(h),
        _ => None,
    }
}

unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        info: *mut core::ffi::c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

/// Read the remote PEB → RTL_USER_PROCESS_PARAMETERS → CommandLine
/// (best-effort; any failure yields an empty vec). x64 layout:
/// PEB.ProcessParameters @ +0x20; RTL_USER_PROCESS_PARAMETERS.CommandLine
/// (a UNICODE_STRING: Length u16, MaxLen u16, pad u32, Buffer u64) @ +0x70.
#[cfg(target_arch = "x86_64")]
unsafe fn read_cmdline(h: HANDLE) -> Vec<String> {
    unsafe { read_cmdline_opt(h) }.unwrap_or_default()
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_cmdline_opt(h: HANDLE) -> Option<Vec<String>> {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessBasicInformation {
        reserved: [*mut u8; 4],
        peb: *mut u8,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct UnicodeString64 {
        length: u16,
        maximum: u16,
        _pad: u32,
        buffer: u64,
    }

    fn peek<T: Copy + Default>(h: HANDLE, addr: *const u8) -> Option<T> {
        if addr.is_null() {
            return None;
        }
        let mut out = T::default();
        let mut got = 0usize;
        unsafe {
            ReadProcessMemory(
                h,
                addr as *const core::ffi::c_void,
                &mut out as *mut T as *mut core::ffi::c_void,
                std::mem::size_of::<T>(),
                Some(&mut got),
            )
            .ok()?;
        }
        (got == std::mem::size_of::<T>()).then_some(out)
    }

    let mut bi = ProcessBasicInformation::default();
    let mut ret = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            h,
            0, // ProcessBasicInformation
            &mut bi as *mut _ as *mut _,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            &mut ret,
        )
    };
    // NTSTATUS success is 0.
    if status != 0 {
        return None;
    }
    let params = peek::<*mut u8>(h, unsafe { bi.peb.add(0x20) })?;
    let cmd = peek::<UnicodeString64>(h, unsafe { params.add(0x70) })?;
    if cmd.length == 0 || cmd.buffer == 0 || cmd.length > 32_767 {
        return None;
    }
    let n = cmd.length as usize / 2;
    let mut w = vec![0u16; n];
    let mut got = 0usize;
    unsafe {
        ReadProcessMemory(
            h,
            cmd.buffer as *const core::ffi::c_void,
            w.as_mut_ptr() as *mut core::ffi::c_void,
            w.len() * 2,
            Some(&mut got),
        )
        .ok()?;
    }
    let text = String::from_utf16_lossy(&w[..got / 2]);
    Some(crate::process::parse_cmdline_windows(&text))
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn read_cmdline(_h: HANDLE) -> Vec<String> {
    Vec::new() // PEB offsets above are x64; aarch64 rows show image name only
}

/// Owner name for a pid as "DOMAIN\\user" (local accounts: bare name).
/// None when the process or its token cannot be opened — the row renders
/// with a blank user column.
fn process_owner(pid: u32) -> Option<String> {
    unsafe {
        let h = open_process_limited(pid)?;
        let mut token = HANDLE::default();
        let ok = OpenProcessToken(h, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(h);
        if ok.is_err() {
            return None;
        }
        let mut len = 0u32;
        // sizing pass — fails with ERROR_INSUFFICIENT_BUFFER and reports size
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if len == 0 {
            let _ = CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        if ok.is_err() {
            return None;
        }
        // TOKEN_USER { SID_AND_ATTRIBUTES User } — the SID pointer at +0.
        let sid = windows::Win32::Security::PSID(*(buf.as_ptr() as *mut *mut core::ffi::c_void));
        if sid.0.is_null() {
            return None;
        }
        let mut name_len = 0u32;
        let mut dom_len = 0u32;
        let mut use_ = windows::Win32::Security::SID_NAME_USE::default();
        // sizing pass
        let _ = LookupAccountSidW(
            PCWSTR::null(),
            sid,
            None,
            &mut name_len,
            None,
            &mut dom_len,
            &mut use_,
        );
        if name_len == 0 {
            return None;
        }
        let mut name = vec![0u16; name_len as usize];
        let mut dom = vec![0u16; dom_len.max(1) as usize];
        let ok = LookupAccountSidW(
            PCWSTR::null(),
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(dom.as_mut_ptr())),
            &mut dom_len,
            &mut use_,
        );
        if ok.is_err() {
            return None;
        }
        let cut = |v: &[u16]| -> String {
            let e = v.iter().position(|&c| c == 0).unwrap_or(v.len());
            String::from_utf16_lossy(&v[..e])
        };
        let name = cut(&name);
        let dom = cut(&dom);
        if name.is_empty() {
            None
        } else if dom.is_empty() {
            Some(name)
        } else {
            Some(format!("{dom}\\{name}"))
        }
    }
}

fn wide_array_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

// ---------------------------------------------------------------- netdev

/// iphlpapi `GetIfTable2`: per-interface byte counters. Filtered to up,
/// non-loopback interfaces so the Network page reads like the Linux one
/// (which drops `lo` and virtual kinds).
fn collect_netdevs() -> Vec<NetDevCounters> {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return Vec::new();
        }
        // MIB_IF_TABLE2.Table is a [MIB_IF_ROW2; 1]; index rows by size.
        let row_size = std::mem::size_of::<MIB_IF_ROW2>();
        let count = (*table).NumEntries as usize;
        let base = (*table).Table.as_ptr() as *const u8;
        let mut out = Vec::new();
        for i in 0..count {
            let row = &*(base.add(i * row_size) as *const MIB_IF_ROW2);
            let mut name = wide_array_to_string(&row.Alias);
            if name.is_empty() {
                name = wide_array_to_string(&row.Description);
            }
            if name.is_empty() {
                continue;
            }
            const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
            if row.Type == IF_TYPE_SOFTWARE_LOOPBACK || row.OperStatus != IfOperStatusUp {
                continue;
            }
            out.push(NetDevCounters {
                name,
                rx_bytes: row.InOctets,
                rx_packets: row.InUcastPkts + row.InNUcastPkts,
                rx_drop: row.InDiscards,
                rx_errs: row.InErrors,
                tx_bytes: row.OutOctets,
                tx_packets: row.OutUcastPkts + row.OutNUcastPkts,
                tx_drop: row.OutDiscards,
                tx_errs: row.OutErrors,
            });
        }
        FreeMibTable(table as *const core::ffi::c_void);
        out
    }
}

// ---------------------------------------------------------------- identity

/// Hostname + OS caption/build + arch. Boot time is derived
/// (now − GetTickCount64), same convention as the Linux collector.
fn collect_sysinfo() -> SysInfo {
    let hostname = unsafe {
        let mut buf = vec![0u16; 256];
        let mut len = buf.len() as u32;
        if GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
        .is_ok()
        {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            "unknown".into()
        }
    };
    let (release, distro) = windows_version();
    SysInfo {
        hostname,
        sysname: "Windows".into(),
        release,
        machine: target_arch().to_string(),
        distro,
        boot_time: boot_time(),
    }
}

fn target_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

/// (build-number, human caption). `RtlGetVersion` from ntdll is the only
/// version API that is not compatibility-lie'd to Windows 8.
fn windows_version() -> (String, String) {
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOEXW) -> i32;
    }
    let mut v = OSVERSIONINFOEXW::default();
    v.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;
    if unsafe { RtlGetVersion(&mut v) } != 0 {
        return (String::new(), "Windows".into());
    }
    let build = v.dwBuildNumber;
    let product = reg_product_name().unwrap_or_default();
    // 22000 is the first Windows 11 build.
    let family = if build >= 22000 {
        "Windows 11"
    } else {
        "Windows 10"
    };
    let caption = if product.is_empty() {
        format!("{family} (Build {build})")
    } else if product.contains("Windows") {
        format!("{product} (Build {build})")
    } else {
        format!("{family} {product} (Build {build})")
    };
    (build.to_string(), caption)
}

/// HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProductName.
fn reg_product_name() -> Option<String> {
    use windows::Win32::System::Registry::*;
    unsafe {
        let path: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\0"
            .encode_utf16()
            .collect();
        let value: Vec<u16> = "ProductName\0".encode_utf16().collect();
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            None,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
        .is_err()
        {
            return None;
        }
        let mut buf = vec![0u16; 256];
        let mut cb = (buf.len() * 2) as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let res = RegQueryValueExW(
            key,
            PCWSTR(value.as_ptr()),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut cb),
        );
        let _ = RegCloseKey(key);
        if res.is_err() {
            return None;
        }
        let end = buf[..cb as usize / 2]
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(cb as usize / 2);
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.is_empty()).then_some(s)
    }
}

fn boot_time() -> Option<std::time::SystemTime> {
    uptime_seconds().and_then(|up| {
        std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs_f64(up))
    })
}

/// CPU identity: logical count (`GetNativeSystemInfo`), physical cores and
/// packages (`GetLogicalProcessorInformationEx`), brand/vendor (CPUID).
fn collect_cpu_info() -> Option<CpuInfo> {
    let logical = unsafe {
        let mut si = SYSTEM_INFO::default();
        GetNativeSystemInfo(&mut si);
        si.dwNumberOfProcessors.max(1) as usize
    };
    let (cores, sockets) = topology();
    let (model, vendor) = cpuid_brand();
    Some(CpuInfo {
        model,
        vendor,
        logical,
        cores: cores.max(1),
        sockets: sockets.max(1),
    })
}

fn topology() -> (usize, usize) {
    unsafe {
        let mut len = 0u32;
        // sizing pass — expected to fail with ERROR_INSUFFICIENT_BUFFER
        let _ = GetLogicalProcessorInformationEx(RelationAll, None, &mut len);
        if len == 0 {
            return (0, 0);
        }
        let mut buf = vec![0u8; len as usize];
        if GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr() as *mut _), &mut len)
            .is_err()
        {
            return (0, 0);
        }
        let (mut cores, mut packages) = (0usize, 0usize);
        let mut ptr = buf.as_ptr();
        let end = buf.as_ptr().add(len as usize);
        while ptr < end {
            let rel = &*(ptr as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX);
            // free constants (not associated consts) in the windows crate
            if rel.Relationship == RelationProcessorCore {
                cores += 1;
            } else if rel.Relationship == RelationProcessorPackage {
                packages += 1;
            }
            ptr = ptr.add(rel.Size as usize);
        }
        (cores, packages)
    }
}

/// CPUID brand string (leaves 0x80000002..04) + vendor string.
#[cfg(target_arch = "x86_64")]
fn cpuid_brand() -> (String, String) {
    use core::arch::x86_64::__cpuid;
    let mut raw = [0u32; 12];
    for i in 0..3usize {
        let leaf = __cpuid(0x80000002 + i as u32);
        raw[i * 4] = leaf.eax;
        raw[i * 4 + 1] = leaf.ebx;
        raw[i * 4 + 2] = leaf.ecx;
        raw[i * 4 + 3] = leaf.edx;
    }
    let bytes: Vec<u8> = raw.iter().flat_map(|w| w.to_le_bytes()).collect();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let model = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    let vend = __cpuid(0);
    let vb: Vec<u8> = [vend.ebx, vend.edx, vend.ecx]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    let vendor = String::from_utf8_lossy(&vb).trim().to_string();
    (model, vendor)
}

#[cfg(not(target_arch = "x86_64"))]
fn cpuid_brand() -> (String, String) {
    (String::new(), String::new())
}

// ---------------------------------------------------------------- PDH

/// Pre-opened PDH query with wildcard counters, collected once per tick.
///
/// CPU per-core: `\Processor(*)\% Processor Time` + `\% Idle Time`.
/// PDH percentage counters are instantaneous, not cumulative, so we
/// synthesize a `SYNTH_JIFFIES_PER_COLLECT`-unit window: `Sampler::tick`
/// divides busy/total between two ticks and recovers the measured
/// utilization exactly.
///
/// Disks: PDH already reports Bytes/sec, and `Sampler` derives B/s by
/// differencing cumulative counters over its own measured interval. To
/// keep the shared rate math untouched, we integrate each rate over the
/// elapsed time since the previous collect and hand Sampler monotonically
/// growing byte totals — rate() then reproduces the PDH rate within one
/// interval.
struct Pdh {
    query: PDH_HQUERY,
    cpu_total: Option<PDH_HCOUNTER>,
    cpu_idle: Option<PDH_HCOUNTER>,
    disk_read: Option<PDH_HCOUNTER>,
    disk_write: Option<PDH_HCOUNTER>,
    /// integrated byte counters per disk instance (parallel to names)
    disk_names: Mutex<Vec<String>>,
    disk_read_bytes: Mutex<Vec<u64>>,
    disk_write_bytes: Mutex<Vec<u64>>,
    /// monotonic µs of the last PdhCollectQueryData + interval to next
    last_collect_us: AtomicU64,
    collect_dt_us: AtomicU64,
    primed: AtomicBool,
    /// guards one collect per tick (cpu_per_core and disks share the query)
    collected_at: AtomicU64,
}

impl Pdh {
    fn new() -> Option<Pdh> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != PDH_STATUS_OK {
                return None;
            }
            let add = |q: PDH_HQUERY, path: &str| -> Option<PDH_HCOUNTER> {
                let mut c = PDH_HCOUNTER::default();
                let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                // English paths: locale-independent, no translation needed.
                if PdhAddEnglishCounterW(q, PCWSTR(w.as_ptr()), 0, &mut c) == PDH_STATUS_OK {
                    Some(c)
                } else {
                    None
                }
            };
            Some(Pdh {
                query,
                cpu_total: add(query, "\\Processor(*)\\% Processor Time"),
                cpu_idle: add(query, "\\Processor(*)\\% Idle Time"),
                disk_read: add(query, "\\PhysicalDisk(*)\\Disk Read Bytes/sec"),
                disk_write: add(query, "\\PhysicalDisk(*)\\Disk Write Bytes/sec"),
                disk_names: Mutex::new(Vec::new()),
                disk_read_bytes: Mutex::new(Vec::new()),
                disk_write_bytes: Mutex::new(Vec::new()),
                last_collect_us: AtomicU64::new(0),
                collect_dt_us: AtomicU64::new(1),
                primed: AtomicBool::new(false),
                collected_at: AtomicU64::new(0),
            })
        }
    }

    /// One collect per tick; PDH needs two collects before values are
    /// valid, so the first tick legitimately yields nothing.
    fn collect(&self, now_us: u64) -> bool {
        if self.collected_at.swap(now_us, Ordering::Relaxed) == now_us {
            return true; // already collected this tick (by cpu_per_core)
        }
        let prev = self.last_collect_us.swap(now_us, Ordering::Relaxed);
        if prev != 0 {
            self.collect_dt_us
                .store(now_us.saturating_sub(prev).max(1), Ordering::Relaxed);
        }
        unsafe { PdhCollectQueryData(self.query) == PDH_STATUS_OK }
    }

    /// Read a wildcard double counter into (instance, value) pairs.
    unsafe fn read_array(&self, h: PDH_HCOUNTER) -> Vec<(String, f64)> {
        // PDH wants buffer size in and items in/out; first call sizes it.
        let mut buf_size = 0u32;
        let mut count = 0u32;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(h, PDH_FMT_DOUBLE, &mut buf_size, &mut count, None)
        };
        if status != PDH_MORE_DATA && (buf_size == 0 || count == 0) {
            return Vec::new();
        }
        if buf_size == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; buf_size as usize];
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                h,
                PDH_FMT_DOUBLE,
                &mut buf_size,
                &mut count,
                Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
            )
        };
        if status != PDH_STATUS_OK || count == 0 {
            return Vec::new();
        }
        let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let base = buf.as_ptr();
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let item = unsafe { &*(base.add(i * item_size) as *const PDH_FMT_COUNTERVALUE_ITEM_W) };
            // CStatus 0 == valid for this collect
            if item.FmtValue.CStatus != 0 {
                continue;
            }
            let name = if item.szName.is_null() {
                String::new()
            } else {
                let mut s: Vec<u16> = Vec::new();
                let mut p = item.szName.0;
                while !p.is_null() && unsafe { *p } != 0 {
                    s.push(unsafe { *p });
                    p = unsafe { p.add(1) };
                }
                String::from_utf16_lossy(&s)
            };
            let v = unsafe { item.FmtValue.Anonymous.doubleValue };
            if name.is_empty() || !v.is_finite() {
                continue;
            }
            out.push((name, v));
        }
        out
    }

    /// Per-core (plus aggregate labelled "cpu") synthesized jiffies rows.
    /// Owns the per-tick collect; `disks()` reuses it.
    fn cpu_per_core(&self) -> Option<Vec<(String, CpuJiffies)>> {
        let total_h = self.cpu_total?;
        let idle_h = self.cpu_idle?;
        let now_us = Self::now_us();
        if !self.collect(now_us) {
            return None;
        }
        if !self.primed.swap(true, Ordering::Relaxed) {
            return None; // first collect establishes the % baselines
        }
        let busy = unsafe { self.read_array(total_h) };
        let idle = unsafe { self.read_array(idle_h) };
        if busy.is_empty() {
            return None;
        }
        let total = SYNTH_JIFFIES_PER_COLLECT as f64;
        let mut out = Vec::with_capacity(busy.len());
        for (name, b) in &busy {
            let id = idle
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| *v)
                .unwrap_or(0.0);
            let b = b.clamp(0.0, 100.0);
            let id = id.clamp(0.0, 100.0);
            let label = if name == "_Total" { "cpu" } else { name };
            out.push((
                label.to_string(),
                CpuJiffies {
                    name: label.into(),
                    // % Processor Time is the (user+system) busy share;
                    // park it in `user` so busy() = total − idle − iowait
                    // reproduces it exactly.
                    user: (b / 100.0 * total) as u64,
                    nice: 0,
                    system: 0,
                    idle: (id / 100.0 * total) as u64,
                    iowait: 0,
                    irq: 0,
                    softirq: 0,
                    steal: 0,
                },
            ));
        }
        Some(out)
    }

    /// PhysicalDisk rows: rates integrated into monotonic byte counters,
    /// with a stable instance→index mapping (instances can vanish when a
    /// volume is removed).
    fn disks(&self) -> Option<Vec<DiskCounters>> {
        let read_h = self.disk_read?;
        let write_h = self.disk_write?;
        let now_us = Self::now_us();
        if !self.collect(now_us) {
            return None;
        }
        let dt_s = self.collect_dt_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let r = unsafe { self.read_array(read_h) };
        let w = unsafe { self.read_array(write_h) };
        if r.is_empty() {
            return None;
        }
        let mut names = self.disk_names.lock().unwrap();
        let mut rb = self.disk_read_bytes.lock().unwrap();
        let mut wb = self.disk_write_bytes.lock().unwrap();
        let mut out = Vec::with_capacity(r.len());
        for (name, rps) in &r {
            if name == "_Total" {
                continue;
            }
            let wps = w
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| *v)
                .unwrap_or(0.0);
            let idx = match names.iter().position(|n| n == name) {
                Some(i) => i,
                None => {
                    names.push(name.clone());
                    rb.push(0);
                    wb.push(0);
                    names.len() - 1
                }
            };
            rb[idx] = rb[idx].saturating_add((rps.max(0.0) * dt_s) as u64);
            wb[idx] = wb[idx].saturating_add((wps.max(0.0) * dt_s) as u64);
            out.push(DiskCounters {
                name: name.clone(),
                reads: 0,
                read_bytes: rb[idx],
                writes: 0,
                write_bytes: wb[idx],
                busy_ms: 0,
            });
        }
        Some(out)
    }

    /// Monotonic microseconds (QPC) for PDH interval bookkeeping.
    fn now_us() -> u64 {
        unsafe {
            let mut freq = 0i64;
            let mut ctr = 0i64;
            if QueryPerformanceFrequency(&mut freq).is_ok()
                && QueryPerformanceCounter(&mut ctr).is_ok()
                && freq > 0
            {
                (ctr as u128 * 1_000_000 / freq as u128) as u64
            } else {
                GetTickCount64() * 1000
            }
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

/// PDH_STATUS_SUCCEEDED is 0.
const PDH_STATUS_OK: u32 = 0;
/// PDH_MORE_DATA (0x800007D6) — buffer too small for the item array.
const PDH_MORE_DATA: u32 = 0x8000_07D6;
