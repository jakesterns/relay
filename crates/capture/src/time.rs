//! QPC time in 100 ns units — the same clock and unit WGC stamps frames with
//! (`SystemRelativeTime`), so capture latency is a plain subtraction.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Current QPC reading converted to 100 ns ticks.
pub fn qpc_now_100ns() -> i64 {
    let mut freq = 0i64;
    let mut now = 0i64;
    // SAFETY: plain out-pointer queries; cannot fail on XP+.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
        let _ = QueryPerformanceCounter(&mut now);
    }
    // Split to avoid overflow: seconds part + remainder scaled to 100 ns.
    let secs = now / freq;
    let rem = now % freq;
    secs * 10_000_000 + rem * 10_000_000 / freq
}

/// Convert a raw QPC counter value (e.g. `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`)
/// to 100 ns ticks on the same scale as [`qpc_now_100ns`].
pub fn qpc_raw_to_100ns(counter: i64) -> i64 {
    let mut freq = 0i64;
    // SAFETY: plain out-pointer query.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    let secs = counter / freq;
    let rem = counter % freq;
    secs * 10_000_000 + rem * 10_000_000 / freq
}

pub fn ticks_to_ms(ticks_100ns: i64) -> f64 {
    ticks_100ns as f64 / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_is_monotonic_and_ticks_at_100ns() {
        let a = qpc_now_100ns();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = qpc_now_100ns();
        let elapsed = b - a;
        // 20 ms sleep must land between 15 ms and 2 s in 100 ns units.
        assert!(elapsed >= 150_000, "elapsed {elapsed}");
        assert!(elapsed < 20_000_000, "elapsed {elapsed}");
    }

    #[test]
    fn raw_counter_converts_to_the_same_scale() {
        use windows::Win32::System::Performance::QueryPerformanceCounter;
        let mut raw = 0i64;
        // SAFETY: plain out-pointer query.
        unsafe {
            let _ = QueryPerformanceCounter(&mut raw);
        }
        let converted = qpc_raw_to_100ns(raw);
        let now = qpc_now_100ns();
        // Same clock read twice: within 100 ms of each other on the 100 ns scale.
        assert!((now - converted).abs() < 1_000_000, "converted {converted}, now {now}");
    }

    #[test]
    fn ticks_to_ms_scales() {
        assert_eq!(ticks_to_ms(10_000), 1.0);
        assert_eq!(ticks_to_ms(0), 0.0);
        assert_eq!(ticks_to_ms(-10_000), -1.0);
        assert_eq!(ticks_to_ms(166_667), 16.6667);
    }
}
