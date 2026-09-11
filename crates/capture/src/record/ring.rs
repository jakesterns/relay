//! Replay buffer: a ring of encoded HEVC access units and Opus packets,
//! budgeted in seconds and bytes. Pure accounting — the writer thread owns
//! the actual muxing of a saved slice.
//!
//! Invariants: the ring always starts at a video keyframe (items arriving
//! before the first keyframe are dropped), and eviction removes whole GOP
//! groups (a keyframe and everything up to the next keyframe) so any suffix
//! of the ring that starts at a keyframe muxes into a decodable clip.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Video { keyframe: bool },
    Audio { dur_100ns: i64 },
}

#[derive(Debug, Clone)]
pub struct RingItem {
    pub kind: ItemKind,
    pub pts_100ns: i64,
    pub data: Vec<u8>,
}

impl RingItem {
    fn is_keyframe(&self) -> bool {
        matches!(self.kind, ItemKind::Video { keyframe: true })
    }
}

pub struct ReplayRing {
    items: VecDeque<RingItem>,
    bytes: usize,
    window_100ns: i64,
    max_bytes: usize,
    /// Items dropped while waiting for the first keyframe.
    pub dropped_awaiting_key: u64,
    /// GOP groups evicted because the byte cap was hit before the window.
    pub evicted_for_bytes: u64,
}

impl ReplayRing {
    /// `window_secs` of retention; `max_bytes` caps memory when the bitrate
    /// outruns the estimate (evicting the oldest GOP first).
    pub fn new(window_secs: u32, max_bytes: usize) -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            window_100ns: window_secs as i64 * 10_000_000,
            max_bytes,
            dropped_awaiting_key: 0,
            evicted_for_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Buffered span in 100 ns (newest pts − oldest pts).
    pub fn span_100ns(&self) -> i64 {
        match (self.items.front(), self.items.back()) {
            (Some(f), Some(b)) => b.pts_100ns - f.pts_100ns,
            _ => 0,
        }
    }

    /// 0.0..=1.0 — how much of the configured window is buffered.
    pub fn fill(&self) -> f64 {
        if self.window_100ns == 0 {
            return 0.0;
        }
        (self.span_100ns() as f64 / self.window_100ns as f64).clamp(0.0, 1.0)
    }

    pub fn push(&mut self, item: RingItem) {
        if self.items.is_empty() && !item.is_keyframe() {
            self.dropped_awaiting_key += 1;
            return;
        }
        self.bytes += item.data.len();
        self.items.push_back(item);
        self.evict();
    }

    /// Drop whole front GOP groups while the ring is over budget. A group is
    /// only dropped if the ring still starts at a keyframe afterwards.
    fn evict(&mut self) {
        loop {
            let Some(second_key) = self
                .items
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, it)| it.is_keyframe())
                .map(|(i, _)| i)
            else {
                return; // one GOP (or less) — nothing evictable
            };
            let newest = self.items.back().map(|b| b.pts_100ns).unwrap_or(0);
            let span_without_front = newest - self.items[second_key].pts_100ns;
            let over_window = span_without_front >= self.window_100ns;
            let over_bytes = self.bytes > self.max_bytes;
            if !over_window && !over_bytes {
                return;
            }
            if over_bytes && !over_window {
                self.evicted_for_bytes += 1;
            }
            for _ in 0..second_key {
                if let Some(it) = self.items.pop_front() {
                    self.bytes -= it.data.len();
                }
            }
        }
    }

    /// The items making up roughly the last `n_secs`: from the latest
    /// keyframe at or before `newest − n` to the end. With a long GOP the
    /// slice can start up to one GOP earlier than requested; if the ring
    /// holds less than `n`, everything buffered is returned.
    pub fn save_slice(&self, n_secs: u32) -> impl Iterator<Item = &RingItem> {
        let want_from =
            self.items.back().map(|b| b.pts_100ns - n_secs as i64 * 10_000_000).unwrap_or(0);
        let start = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.is_keyframe() && it.pts_100ns <= want_from)
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        self.items.iter().skip(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 10_000_000;

    fn video(pts_s: f64, key: bool, len: usize) -> RingItem {
        RingItem {
            kind: ItemKind::Video { keyframe: key },
            pts_100ns: (pts_s * S as f64) as i64,
            data: vec![0; len],
        }
    }

    fn audio(pts_s: f64) -> RingItem {
        RingItem {
            kind: ItemKind::Audio { dur_100ns: 100_000 },
            pts_100ns: (pts_s * S as f64) as i64,
            data: vec![0; 40],
        }
    }

    /// 2 s GOPs: keyframe at every even second, P-frames between.
    fn fill_ring(ring: &mut ReplayRing, secs: u32) {
        for t in 0..secs * 2 {
            let ts = t as f64 * 0.5;
            ring.push(video(ts, t % 4 == 0, 1000));
            ring.push(audio(ts));
        }
    }

    #[test]
    fn waits_for_a_keyframe() {
        let mut r = ReplayRing::new(60, usize::MAX);
        r.push(video(0.0, false, 100));
        r.push(audio(0.0));
        assert!(r.is_empty());
        assert_eq!(r.dropped_awaiting_key, 2);
        r.push(video(0.5, true, 100));
        r.push(audio(0.5));
        assert_eq!(r.len(), 2);
        assert_eq!(r.bytes(), 140);
    }

    #[test]
    fn evicts_whole_gops_beyond_the_window() {
        let mut r = ReplayRing::new(10, usize::MAX);
        fill_ring(&mut r, 30);
        // Ring keeps ≥ window; front is always a keyframe; span stays within
        // window + one GOP (2 s here).
        assert!(r.items.front().unwrap().is_keyframe());
        let span_s = r.span_100ns() as f64 / S as f64;
        assert!((10.0..=12.5).contains(&span_s), "span {span_s}");
    }

    #[test]
    fn byte_cap_evicts_but_never_below_one_gop() {
        // Each GOP: 4 video × 1000 B + 4 audio × 40 B = 4160 B.
        let mut r = ReplayRing::new(3600, 6000);
        fill_ring(&mut r, 20);
        assert!(r.bytes() <= 6000 + 1040, "bytes {}", r.bytes());
        assert!(r.evicted_for_bytes > 0);
        assert!(r.items.front().unwrap().is_keyframe());

        // A single over-sized GOP is kept — the cap cannot empty the ring.
        let mut r = ReplayRing::new(3600, 100);
        r.push(video(0.0, true, 5000));
        r.push(video(0.5, false, 5000));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn fill_reports_window_fraction() {
        let mut r = ReplayRing::new(10, usize::MAX);
        assert_eq!(r.fill(), 0.0);
        fill_ring(&mut r, 5);
        let f = r.fill();
        assert!((0.4..0.6).contains(&f), "fill {f}");
        let mut full = ReplayRing::new(10, usize::MAX);
        fill_ring(&mut full, 30);
        assert_eq!(full.fill(), 1.0);
    }

    #[test]
    fn save_slice_starts_at_a_keyframe_covering_the_request() {
        let mut r = ReplayRing::new(30, usize::MAX);
        fill_ring(&mut r, 30);
        let slice: Vec<_> = r.save_slice(5).collect();
        let first = slice.first().unwrap();
        assert!(first.is_keyframe());
        let newest = r.items.back().unwrap().pts_100ns;
        let covered = (newest - first.pts_100ns) as f64 / S as f64;
        // At least the requested 5 s, at most 5 s + one GOP (2 s).
        assert!((5.0..=7.0).contains(&covered), "covered {covered}");

        // Request beyond what is buffered → everything, from the front.
        let all: Vec<_> = r.save_slice(3600).collect();
        assert_eq!(all.len(), r.len());
    }
}
