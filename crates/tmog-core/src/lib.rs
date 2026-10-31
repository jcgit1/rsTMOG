//! tmog-core: metrics library over /proc, /sys and NVML.
//!
//! Every collector is a pure parser over text or a fixture directory tree,
//! unit-tested without root or a live procfs (mirrors the evildojo-tmog
//! design, in idiomatic Rust). Zero GUI dependencies by contract — the
//! rsTMOG bin (and any future TUI) consumes this crate.

pub mod actions;
pub mod cpu;
pub mod cpuinfo;
pub mod diskstats;
pub mod fs;
pub mod gpu;
pub mod hwmon;
pub mod loadavg;
pub mod meminfo;
pub mod netdev;
pub mod pressure;
pub mod process;
pub mod process_table;
pub mod rapl;
pub mod ring;
pub mod settings;
pub mod sysinfo;
pub mod zfs;
