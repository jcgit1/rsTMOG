//! Metrics core: collectors over /proc, /sys and NVML.
//!
//! Every collector is a pure parser over text or a fixture directory tree,
//! unit-tested without root or a live procfs (mirrors the evildojo-tmog
//! design, in idiomatic Rust).

pub mod cpu;
pub mod fs;
pub mod loadavg;
pub mod meminfo;
pub mod ring;
