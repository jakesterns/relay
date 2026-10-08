//! The non-blocking hand-off between a real-time thread (render, audio
//! playback, capture) and an NDI® worker.
//!
//! The rule is the vcam tee's, made strict: the producer never waits. A
//! [`Tap`] that is off costs one relaxed atomic load. When it is on, the
//! producer takes a recycled buffer, fills it and offers it on a bounded
//! channel; if the worker is behind, the queue is full or no buffer is free,
//! the frame is dropped and counted, and the producer moves on. Buffers are
//! allocated at most `capacity + 2` times per tap, then reused, so a slow
//! consumer can cost frames but never memory.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Counters for one tap, read by the stats line.
#[derive(Debug, Default)]
pub struct TeeStats {
    /// Offered and accepted by the queue.
    pub queued: AtomicU64,
    /// Handed to the consumer.
    pub sent: AtomicU64,
    /// Dropped because the worker was behind (queue full or no free buffer).
    pub dropped: AtomicU64,
}

impl TeeStats {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "sent": self.sent.load(Ordering::Relaxed),
            "dropped": self.dropped.load(Ordering::Relaxed),
        })
    }
}

/// Producer half: owned by whoever installed it in a [`Tap`].
pub struct TeeTx<B> {
    tx: SyncSender<B>,
    pool: Arc<Mutex<Vec<B>>>,
    allocated: usize,
    max_buffers: usize,
    pub stats: Arc<TeeStats>,
}

impl<B: Default> TeeTx<B> {
    /// A free buffer, or `None` when every buffer is in flight (counted as a
    /// drop). Never blocks: the pool lock is only tried.
    pub fn buffer(&mut self) -> Option<B> {
        if let Ok(mut pool) = self.pool.try_lock() {
            if let Some(b) = pool.pop() {
                return Some(b);
            }
        }
        if self.allocated < self.max_buffers {
            self.allocated += 1;
            return Some(B::default());
        }
        self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Offer a filled buffer. `false` when it was dropped (the worker is
    /// behind, or gone); the buffer goes back to the pool either way.
    pub fn send(&mut self, b: B) -> bool {
        match self.tx.try_send(b) {
            Ok(()) => {
                self.stats.queued.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Full(b)) | Err(TrySendError::Disconnected(b)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.give_back(b);
                false
            }
        }
    }

    /// Return a buffer that was taken but not sent.
    pub fn give_back(&mut self, b: B) {
        match self.pool.try_lock() {
            Ok(mut pool) => pool.push(b),
            // Contended: let it go and allow one more allocation instead.
            Err(_) => self.allocated = self.allocated.saturating_sub(1),
        }
    }
}

/// Consumer half: run by [`spawn_worker`].
pub struct TeeRx<B> {
    rx: Receiver<B>,
    pool: Arc<Mutex<Vec<B>>>,
    stats: Arc<TeeStats>,
}

/// A bounded tee: `capacity` buffers may wait for the worker.
pub fn channel<B: Default>(capacity: usize) -> (TeeTx<B>, TeeRx<B>) {
    let capacity = capacity.max(1);
    let (tx, rx) = mpsc::sync_channel(capacity);
    let pool = Arc::new(Mutex::new(Vec::with_capacity(capacity + 2)));
    let stats = Arc::new(TeeStats::default());
    (
        TeeTx {
            tx,
            pool: pool.clone(),
            allocated: 0,
            // One being filled, `capacity` queued, one being consumed.
            max_buffers: capacity + 2,
            stats: stats.clone(),
        },
        TeeRx { rx, pool, stats },
    )
}

/// Run `consume` on every buffer until the producer side is dropped, then
/// return. Buffers go back to the pool after each call.
pub fn spawn_worker<B: Send + 'static>(
    name: &str,
    rx: TeeRx<B>,
    mut consume: impl FnMut(&B) + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(move || {
        while let Ok(b) = rx.rx.recv() {
            consume(&b);
            rx.stats.sent.fetch_add(1, Ordering::Relaxed);
            rx.pool.lock().unwrap_or_else(|e| e.into_inner()).push(b);
        }
    })
}

/// Where a real-time thread finds the producer half, if NDI output is on.
/// Installed and cleared from the control thread; used from the real-time
/// one with `try_lock` only.
pub struct Tap<B> {
    active: AtomicBool,
    inner: Mutex<Option<TeeTx<B>>>,
}

impl<B> Default for Tap<B> {
    fn default() -> Self {
        Self { active: AtomicBool::new(false), inner: Mutex::new(None) }
    }
}

impl<B> Tap<B> {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The whole cost of NDI output while it is off.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn install(&self, tx: TeeTx<B>) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        self.active.store(true, Ordering::Release);
    }

    /// Take the producer out; dropping it ends the worker.
    pub fn clear(&self) -> Option<TeeTx<B>> {
        self.active.store(false, Ordering::Release);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Run `f` with the producer when it is installed and not being swapped
    /// right now. Never blocks.
    pub fn try_with<R>(&self, f: impl FnOnce(&mut TeeTx<B>) -> R) -> Option<R> {
        if !self.is_active() {
            return None;
        }
        let mut g = self.inner.try_lock().ok()?;
        g.as_mut().map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn an_idle_tap_does_nothing() {
        let tap: Tap<Vec<u8>> = Tap::default();
        assert!(!tap.is_active());
        assert!(tap.try_with(|_| ()).is_none());
    }

    #[test]
    fn a_slow_consumer_costs_frames_not_time_or_memory() {
        let (tx, rx) = channel::<Vec<u8>>(2);
        let stats = tx.stats.clone();
        let tap = Tap::shared();
        tap.install(tx);
        // A consumer far slower than the producer: 30 ms per frame.
        let worker =
            spawn_worker("slow", rx, |_b: &Vec<u8>| std::thread::sleep(Duration::from_millis(30)))
                .unwrap();

        let mut worst = Duration::ZERO;
        let mut accepted = 0u64;
        for i in 0..200u32 {
            let t = Instant::now();
            let ok = tap
                .try_with(|tx| match tx.buffer() {
                    Some(mut b) => {
                        b.clear();
                        b.extend_from_slice(&i.to_le_bytes());
                        tx.send(b)
                    }
                    None => false,
                })
                .unwrap_or(false);
            worst = worst.max(t.elapsed());
            accepted += ok as u64;
            std::thread::sleep(Duration::from_millis(1));
        }
        // The producer never waited on the consumer.
        assert!(worst < Duration::from_millis(10), "worst push {worst:?}");
        let dropped = stats.dropped.load(Ordering::Relaxed);
        assert!(dropped > 100, "most frames dropped, got {dropped}");
        assert_eq!(accepted + dropped, 200);
        // Allocation is bounded by the queue, not by the frame count.
        let allocated = tap.try_with(|tx| tx.allocated).unwrap();
        assert!(allocated <= 4, "allocated {allocated}");

        drop(tap.clear());
        worker.join().unwrap();
        assert_eq!(stats.sent.load(Ordering::Relaxed), accepted);
    }

    #[test]
    fn buffers_are_recycled_by_a_fast_consumer() {
        let (mut tx, rx) = channel::<Vec<u8>>(4);
        let stats = tx.stats.clone();
        let worker = spawn_worker("fast", rx, |_b: &Vec<u8>| {}).unwrap();
        for _ in 0..1000 {
            if let Some(b) = tx.buffer() {
                tx.send(b);
            }
            std::thread::yield_now();
        }
        assert!(tx.allocated <= 6);
        drop(tx);
        worker.join().unwrap();
        assert!(stats.sent.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn clearing_the_tap_ends_the_worker() {
        let (tx, rx) = channel::<Vec<u8>>(1);
        let tap = Tap::shared();
        tap.install(tx);
        let worker = spawn_worker("end", rx, |_b: &Vec<u8>| {}).unwrap();
        assert!(tap.clear().is_some());
        assert!(!tap.is_active());
        worker.join().unwrap();
    }

    #[test]
    fn a_dead_worker_is_a_drop_not_a_hang() {
        let (mut tx, rx) = channel::<Vec<u8>>(1);
        drop(rx);
        let b = tx.buffer().unwrap();
        assert!(!tx.send(b));
        assert_eq!(tx.stats.dropped.load(Ordering::Relaxed), 1);
    }
}
