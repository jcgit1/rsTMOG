//! Background sampling: a dedicated thread owns the `Sampler` and publishes
//! an `Arc<Snapshot>` after every tick; the UI thread only reads the latest
//! published snapshot.
//!
//! Why a thread and not a Mutex<Snapshot>: `RwLock<Arc<Snapshot>>` swaps
//! are copy-on-publish — readers clone an `Arc` (one refcount bump) and
//! never block behind a ~60 ms process walk. The sampler thread also
//! isolates tick cost from the paint path entirely: even a 156 ms tick
//! (observed worst case over ~700 pids) never hitches the UI.

use crate::sampler::{Sampler, Snapshot};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Handle to the background sampler. Dropping the last clone stops the
/// thread (the stop flag is checked each cycle).
pub struct SamplerHandle {
    shared: Arc<RwLock<Arc<Snapshot>>>,
    stop: Arc<AtomicBool>,
    refresh_ms: Arc<AtomicU32>,
    tx_detail: crossbeam_channel::Sender<Vec<u32>>,
    joiner: Option<std::thread::JoinHandle<()>>,
}

impl SamplerHandle {
    /// Spawn the sampler thread. Seeds one synchronous tick so the first
    /// frame already has data (no blank first paint).
    pub fn start(refresh_ms: u32) -> Self {
        let shared = Arc::new(RwLock::new(Arc::new(Snapshot::default())));
        let stop = Arc::new(AtomicBool::new(false));
        let rate = Arc::new(AtomicU32::new(refresh_ms));

        let mut sampler = Sampler::new();
        sampler.tick(); // seed: aggregate deltas need a previous sample
        *shared.write().unwrap() = Arc::new(sampler.snapshot.clone());

        let shared_t = Arc::clone(&shared);
        let stop_t = Arc::clone(&stop);
        let rate_t = Arc::clone(&rate);
        let (tx_detail, rx_detail) = crossbeam_channel::unbounded::<Vec<u32>>();
        let joiner = std::thread::Builder::new()
            .name("rstmog-sampler".into())
            .spawn(move || {
                // Adaptive loop: sleep(rest-of-period), tick, publish. If a
                // tick overruns the period, next sleep is zero and the
                // effective cadence degrades gracefully.
                while !stop_t.load(Ordering::Relaxed) {
                    let began = Instant::now();
                    // slow-tier detail requests from the UI thread
                    if let Ok(pids) = rx_detail.try_recv() {
                        sampler.request_detail(&pids);
                    }
                    sampler.tick();
                    *shared_t.write().unwrap() = Arc::new(sampler.snapshot.clone());
                    let period = Duration::from_millis(rate_t.load(Ordering::Relaxed) as u64);
                    let elapsed = began.elapsed();
                    let mut waited = Duration::ZERO;
                    while !stop_t.load(Ordering::Relaxed)
                        && waited < period.saturating_sub(elapsed.min(period))
                    {
                        std::thread::sleep(Duration::from_millis(20));
                        waited += Duration::from_millis(20);
                    }
                }
            })
            .expect("spawn sampler thread");

        SamplerHandle {
            shared,
            stop,
            refresh_ms: rate,
            tx_detail,
            joiner: Some(joiner),
        }
    }

    /// Latest published snapshot (cheap: Arc clone under a short read lock).
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.read().unwrap().clone()
    }

    /// Ask the sampler thread to fully enrich these pids next tick
    /// (visible table slice + selection). Non-blocking.
    pub fn request_detail(&self, pids: &[u32]) {
        let _ = self.tx_detail.try_send(pids.to_vec());
    }

    /// Shared refresh-rate handle (for the repaint driver to track).
    pub fn refresh_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.refresh_ms)
    }

    /// Change the poll cadence at runtime. Wired to the Phase 4 settings
    /// dialog; the sampler loop and repaint driver pick the new value up
    /// on their next cycle.
    #[allow(dead_code)] // exercised by tests; GUI wiring lands with the settings dialog
    pub fn set_refresh_ms(&self, ms: u32) {
        self.refresh_ms
            .store(ms.clamp(250, 5000), Ordering::Relaxed);
    }

    /// Ask the thread to stop and wait for it. Idempotent.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.joiner.take() {
            let _ = j.join();
        }
    }
}

impl Drop for SamplerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_publishes_and_shuts_down() {
        let mut h = SamplerHandle::start(60);
        // seed tick already published before start() returned
        let s0 = h.snapshot();
        assert!(s0.now.is_some(), "seed snapshot should be published");
        // wait for at least one more published tick
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let s = h.snapshot();
            if s.tick > 0 {
                break;
            }
            assert!(Instant::now() < deadline, "sampler thread never advanced");
            std::thread::sleep(Duration::from_millis(25));
        }
        h.shutdown();
        h.shutdown(); // idempotent
    }

    #[test]
    fn set_refresh_ms_clamps() {
        let mut h = SamplerHandle::start(1000);
        h.set_refresh_ms(1); // clamped to 250
        assert_eq!(h.refresh_ms.load(Ordering::Relaxed), 250);
        h.set_refresh_ms(99999); // clamped to 5000
        assert_eq!(h.refresh_ms.load(Ordering::Relaxed), 5000);
        h.shutdown();
    }
}
