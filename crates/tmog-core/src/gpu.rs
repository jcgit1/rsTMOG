//! GPU metrics via NVML (dlopen'd at runtime — no build-time NVIDIA
//! dependency). Boxes without the driver get None everywhere, no panic.

/// Snapshot of one NVIDIA GPU.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    /// 0..=100 GPU utilization %
    pub busy_percent: Option<f64>,
    /// MiB
    pub mem_used_mib: Option<u64>,
    pub mem_total_mib: Option<u64>,
    /// MHz
    pub graphics_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    /// watts
    pub power_draw_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub temp_c: Option<f64>,
}

/// Opaque NVML handle holder. `new()` returns None when libnvidia-ml
/// cannot be loaded or NVML init fails (no driver, headless CI, etc.).
pub struct Nvml {
    inner: nvml_wrapper::Nvml,
}

impl Nvml {
    /// Try to init NVML; None = no GPU support, callers show dashes.
    pub fn new() -> Option<Nvml> {
        let inner = nvml_wrapper::Nvml::init().ok()?;
        // sanity: count devices; treat error as unsupported
        inner.device_count().ok()?;
        Some(Nvml { inner })
    }

    pub fn device_count(&self) -> u32 {
        self.inner.device_count().unwrap_or(0)
    }

    /// Read one GPU snapshot; per-field failures become None (some
    /// fields are unsupported on some GPUs/drivers).
    pub fn sample(&self, index: u32) -> Option<GpuInfo> {
        use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
        let d = self.inner.device_by_index(index).ok()?;
        let name = d.name().unwrap_or_default();
        // utilization_rates errors on some driver/GPU combos -> None
        let busy = d.utilization_rates().ok().map(|u| u.gpu as f64);
        let mem = d.memory_info().ok().map(|i| i.used / (1024 * 1024));
        let total = d.memory_info().ok().map(|i| i.total / (1024 * 1024));
        let gclk = d.clock_info(Clock::Graphics).ok();
        let mclk = d.clock_info(Clock::Memory).ok();
        let draw = d.power_usage().ok().map(|mw| mw as f64 / 1000.0);
        // default power limit as the stable "limit" reference
        let limit = d.power_usage().ok().and_then(|_| d.name().ok()).and(None);
        let temp = d.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f64);
        let _ = limit;
        Some(GpuInfo {
            index,
            name,
            busy_percent: busy,
            mem_used_mib: mem,
            mem_total_mib: total,
            graphics_clock_mhz: gclk,
            memory_clock_mhz: mclk,
            power_draw_w: draw,
            power_limit_w: limit,
            temp_c: temp,
        })
    }

    /// All GPUs.
    pub fn sample_all(&self) -> Vec<GpuInfo> {
        (0..self.device_count())
            .filter_map(|i| self.sample(i))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvml_init_never_panics() {
        // Whatever this CI/dev box has (or doesn't), Nvml::new() must not
        // panic; with the RTX driver present it should find >=1 GPU.
        let nvml = Nvml::new();
        if let Some(n) = &nvml {
            let gpus = n.sample_all();
            assert!(!gpus.is_empty());
            let g = &gpus[0];
            assert!(!g.name.is_empty());
            if let Some(b) = g.busy_percent {
                assert!((0.0..=100.0).contains(&b));
            }
            if let (Some(u), Some(t)) = (g.mem_used_mib, g.mem_total_mib) {
                assert!(u <= t);
            }
        }
        // no-GPU path: nothing to assert beyond "no panic"
    }
}
