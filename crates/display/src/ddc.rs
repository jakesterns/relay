//! DDC/CI monitor controls over `dxva2`.
//!
//! A physical-monitor handle is opened from the `HMONITOR`, used for the few
//! get/set calls, and destroyed again — handles are not cached because they
//! go stale on display changes. DDC/CI is slow (tens of ms per transaction)
//! and flaky (monitors NAK while their OSD is open or they are settling), so
//! every call retries with a pause, and writes are spaced by the per-model
//! write delay.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::debug;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply,
    SetVCPFeature, PHYSICAL_MONITOR,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

const RETRIES: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// An open physical-monitor handle. Short-lived by design.
pub struct PhysicalMonitor {
    phys: [PHYSICAL_MONITOR; 1],
    /// Pause after each write, from the model quirks.
    write_delay: Duration,
}

impl PhysicalMonitor {
    /// Open from the volatile `HMONITOR` (as `i64`, the probe's wire format).
    pub fn open(hmonitor: i64, write_delay_ms: u64) -> Result<Self> {
        if hmonitor == 0 {
            bail!("no HMONITOR for this monitor");
        }
        let mut phys = [PHYSICAL_MONITOR::default()];
        // SAFETY: array of one, per the API contract; destroyed in Drop.
        unsafe { GetPhysicalMonitorsFromHMONITOR(HMONITOR(hmonitor as *mut _), &mut phys) }
            .context("GetPhysicalMonitorsFromHMONITOR")?;
        Ok(Self { phys, write_delay: Duration::from_millis(write_delay_ms) })
    }

    /// Current and maximum value of one VCP code.
    pub fn get_vcp(&self, code: u8) -> Result<(u32, u32)> {
        let mut last_err = None;
        for attempt in 0..RETRIES {
            if attempt > 0 {
                std::thread::sleep(RETRY_PAUSE);
            }
            let (mut current, mut max) = (0u32, 0u32);
            // SAFETY: out-params are plain u32s on our stack.
            let ok = unsafe {
                GetVCPFeatureAndVCPFeatureReply(
                    self.phys[0].hPhysicalMonitor,
                    code,
                    None,
                    &mut current,
                    Some(&mut max),
                )
            };
            if ok != 0 {
                return Ok((current, max));
            }
            last_err = Some(anyhow::anyhow!("GetVCPFeature 0x{code:02X} failed"));
            debug!(code = format!("0x{code:02X}"), attempt, "DDC/CI read retry");
        }
        Err(last_err.unwrap())
    }

    /// Write one VCP code, then wait the model's settle delay.
    pub fn set_vcp(&self, code: u8, value: u32) -> Result<()> {
        let mut last_err = None;
        for attempt in 0..RETRIES {
            if attempt > 0 {
                std::thread::sleep(RETRY_PAUSE);
            }
            // SAFETY: plain call on our handle.
            let ok = unsafe { SetVCPFeature(self.phys[0].hPhysicalMonitor, code, value) };
            if ok != 0 {
                std::thread::sleep(self.write_delay);
                return Ok(());
            }
            last_err = Some(anyhow::anyhow!("SetVCPFeature 0x{code:02X}={value} failed"));
            debug!(code = format!("0x{code:02X}"), attempt, "DDC/CI write retry");
        }
        Err(last_err.unwrap())
    }
}

impl Drop for PhysicalMonitor {
    fn drop(&mut self) {
        // SAFETY: handles were opened in `open`.
        unsafe {
            let _ = DestroyPhysicalMonitors(&self.phys);
        }
    }
}
