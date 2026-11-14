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
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Handle to the background sampler. Dropping the last clone stops the
/// thread (the stop flag is checked each cycle).
pub struct SamplerHandle {
    shared: Arc<RwLock<Arc<Snapshot>>>,
    stop: Arc<AtomicBool>,
    refresh_ms: Arc<AtomicU32>,
    /// Latest detail request from the UI. The UI asks every frame and the
    /// sampler wakes once per tick, so only the newest request matters: a
    /// new one overwrites whatever the sampler has not picked up yet.
    detail_req: Arc<Mutex<Option<Vec<u32>>>>,
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
        let detail_req: Arc<Mutex<Option<Vec<u32>>>> = Arc::new(Mutex::new(None));
        let detail_req_t = Arc::clone(&detail_req);
        let joiner = std::thread::Builder::new()
            .name("rstmog-sampler".into())
            .spawn(move || {
                // Adaptive loop: sleep(rest-of-period), tick, publish. If a
                // tick overruns the period, next sleep is zero and the
                // effective cadence degrades gracefully.
                while !stop_t.load(Ordering::Relaxed) {
                    let began = Instant::now();
                    // slow-tier detail requests from the UI thread
                    let req = detail_req_t.lock().map(|mut g| g.take()).unwrap_or(None);
                    if let Some(pids) = req {
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
            detail_req,
            joiner: Some(joiner),
        }
    }

    /// Latest published snapshot (cheap: Arc clone under a short read lock).
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.read().unwrap().clone()
    }

    /// Ask the sampler thread to fully enrich these pids next tick
    /// (visible table slice + selection). Replaces any request the sampler
    /// has not picked up yet. Non-blocking apart from a short lock.
    pub fn request_detail(&self, pids: &[u32]) {
        if let Ok(mut g) = self.detail_req.lock() {
            *g = Some(pids.to_vec());
        }
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
    fn detail_requests_keep_only_the_newest() {
        let mut h = SamplerHandle::start(2000);
        for i in 0..1000u32 {
            h.request_detail(&[i]);
        }
        // The sampler takes the slot at most once per cycle, and only the
        // newest request is ever in it: so it is either still the newest,
        // or the sampler already took exactly that one.
        let slot = h.detail_req.lock().unwrap().clone();
        assert!(slot.is_none() || slot == Some(vec![999]), "got {slot:?}");
        h.shutdown();
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
