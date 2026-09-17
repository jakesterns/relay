//! Self-measurement for the "Memory 9 MB · CPU 0.0 %" readouts and for keeping
//! ourselves honest about the ≤10 MB / ~0 % budget.

use std::time::Instant;

use crate::types::Footprint;

pub struct FootprintMeter {
    last_cpu_100ns: u64,
    last_wall: Instant,
}

impl Default for FootprintMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl FootprintMeter {
    pub fn new() -> Self {
        Self { last_cpu_100ns: cpu_time_100ns(), last_wall: Instant::now() }
    }

    /// CPU % is averaged over the interval since the previous `sample()`.
    pub fn sample(&mut self) -> Footprint {
        let now_cpu = cpu_time_100ns();
        let now_wall = Instant::now();
        let cpu_delta_s = now_cpu.saturating_sub(self.last_cpu_100ns) as f64 / 10_000_000.0;
        let wall_delta_s = now_wall.duration_since(self.last_wall).as_secs_f64().max(1e-3);
        self.last_cpu_100ns = now_cpu;
        self.last_wall = now_wall;
        Footprint {
            rss_bytes: rss_bytes(),
            cpu_percent: ((cpu_delta_s / wall_delta_s) * 100.0) as f32,
        }
    }
}

/// Give back working-set pages the OS mapped for one-off work (COM, WASAPI
/// and display DLLs touched by the hardware probe). Pages fault back in on
/// demand; what remains resident afterwards is what the core actually uses.
/// Called after startup init and after each (rare) hardware re-probe so the
/// ≤10 MB idle budget reflects steady state, not probe residue.
pub use imp::trim_working_set;
use imp::{cpu_time_100ns, rss_bytes};

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{
        K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    pub fn trim_working_set() {
        // SAFETY: trimming our own process; purely a paging hint.
        unsafe {
            let _ = K32EmptyWorkingSet(GetCurrentProcess());
        }
    }

    pub fn rss_bytes() -> u64 {
        let mut pmc = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: querying our own process with a correctly sized out-struct.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, size) };
        if ok.as_bool() {
            pmc.WorkingSetSize as u64
        } else {
            0
        }
    }

    pub fn cpu_time_100ns() -> u64 {
        let mut c = FILETIME::default();
        let mut e = FILETIME::default();
        let mut k = FILETIME::default();
        let mut u = FILETIME::default();
        // SAFETY: querying our own process.
        let r = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if r.is_err() {
            return 0;
        }
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        ft(k) + ft(u)
    }
}

/// Stub: the readouts show zero rather than a guess. The footprint gate is a
/// Windows measurement; a port brings `task_info` (macOS) or `/proc` (Linux).
#[cfg(not(windows))]
mod imp {
    pub fn trim_working_set() {}

    pub fn rss_bytes() -> u64 {
        0
    }

    pub fn cpu_time_100ns() -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_sane() {
        let mut m = FootprintMeter::new();
        let f = m.sample();
        assert!(f.cpu_percent >= 0.0);
        #[cfg(windows)]
        assert!(f.rss_bytes > 0);
    }
}
