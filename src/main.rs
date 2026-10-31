//! rsTMOG — Task Manager OG, rewritten in Rust.
//!
//! `core` is the metrics engine (procfs/sysfs/NVML); `gui` is the egui
//! frontend. Kept as modules of one crate for now; split into a workspace
//! (core lib + gui bin) once the core API settles.

pub mod core;

mod gui;

fn main() -> anyhow::Result<()> {
    gui::run()
}
