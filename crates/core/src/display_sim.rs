//! A simulated display rig, for the crash-restore harness.
//!
//! Selected with `RELAY_DISPLAY_SIM=<path>` (and `RELAY_DISPLAY_SIM_VENDOR=
//! nvidia|amd`); never used in production. The sibling of
//! [`FileRecorder`](crate::apply::FileRecorder), and here for the same
//! reason: the crash-restore proof needs a core that really dies, so the
//! hardware it drives has to live in a file that outlives the process.
//!
//! What this is and is not: it plugs in at [`DisplayIo`], *below* every
//! decision the product makes. Planning, capture-before-apply, the ordering
//! of the restore, vendor dispatch and the snapshot format are all the real
//! ones — only the four primitive reads and writes at the bottom are
//! simulated. So it proves the AMD **path** end to end through a hard kill.
//! It does not prove the AMD **driver**: that needs an AMD GPU with a
//! monitor attached, which this development machine does not have (see
//! `docs/plans/M2-display.md`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use relay_display::gamma::Ramp;
use serde::{Deserialize, Serialize};

use crate::backup::{GpuColorSnapshot, GpuVendor};
use crate::display_backend::{DisplayIo, GpuApplied};
use crate::hardware::{HardwareProbe, MonitorProbe, ProbeReport};
use crate::types::MonitorId;

/// Environment variable naming the file the simulated rig lives in.
pub const SIM_ENV: &str = "RELAY_DISPLAY_SIM";
/// Which vendor drives the simulated monitor. Defaults to AMD, because the
/// NVIDIA path already has a live crash-restore proof on real hardware.
pub const SIM_VENDOR_ENV: &str = "RELAY_DISPLAY_SIM_VENDOR";

const SIM_MONITOR_ID: &str = "mon:SIM0000:relay-sim";
const SIM_GDI_NAME: &str = r"\\.\SIMDISPLAY1";
const SIM_HMONITOR: i64 = 0x5151;

/// The simulated hardware's registers. Whatever is in this file is "what the
/// monitor is currently set to"; the test reads it before, during and after
/// the kill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimState {
    pub vcp: BTreeMap<u8, u32>,
    pub ramp: Ramp,
    pub gpu: GpuColorSnapshot,
}

impl SimState {
    /// The rig as found at boot: brightness 100, contrast 70 (the real LG's
    /// values from the M2 live pass), an identity ramp, and neutral colour in
    /// the chosen vendor's own units.
    pub fn initial(vendor: GpuVendor) -> Self {
        let gpu = match vendor {
            GpuVendor::Nvidia => GpuColorSnapshot {
                vendor,
                dvc: 0,
                dvc_min: 0,
                dvc_max: 63,
                hue_deg: 0,
                hue_min: 0,
                hue_max: 0,
            },
            GpuVendor::Amd => GpuColorSnapshot {
                vendor,
                dvc: 100,
                dvc_min: 0,
                dvc_max: 200,
                hue_deg: 0,
                hue_min: -30,
                hue_max: 30,
            },
        };
        Self {
            vcp: BTreeMap::from([(0x10u8, 100u32), (0x12u8, 70u32)]),
            ramp: Ramp::identity(),
            gpu,
        }
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// The three numbers the harness compares. Small enough to print, which
    /// is what makes a failure readable.
    pub fn summary(&self) -> (u32, u16, i32) {
        (self.vcp.get(&0x10).copied().unwrap_or(0), self.ramp.r[128], self.gpu.dvc)
    }
}

fn vendor_from_env() -> GpuVendor {
    match std::env::var(SIM_VENDOR_ENV).unwrap_or_default().to_ascii_lowercase().as_str() {
        "nvidia" | "nv" => GpuVendor::Nvidia,
        _ => GpuVendor::Amd,
    }
}

/// The simulated monitor, as the probe would report it.
pub fn sim_monitor() -> MonitorProbe {
    MonitorProbe {
        id: MonitorId(SIM_MONITOR_ID.into()),
        name: "RELAY SIM".into(),
        native: Some((2560, 1440)),
        refresh_hz: Some(144.0),
        primary: true,
        hmonitor: SIM_HMONITOR,
        gdi_name: SIM_GDI_NAME.into(),
        ddc: Some(vec![0x10, 0x12]),
        color: None,
    }
}

/// Hardware probe that reports exactly the simulated monitor, so the focus
/// path has something to target.
#[derive(Debug)]
pub struct SimHardwareProbe;

impl HardwareProbe for SimHardwareProbe {
    fn probe(&self, _with_ddc: bool) -> ProbeReport {
        ProbeReport { endpoints: Vec::new(), monitors: vec![sim_monitor()] }
    }
}

/// File-backed [`DisplayIo`].
#[derive(Debug)]
pub struct SimIo {
    path: PathBuf,
    vendor: GpuVendor,
}

impl SimIo {
    pub fn from_env(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), vendor: vendor_from_env() }
    }

    fn load(&self) -> SimState {
        SimState::read(&self.path).unwrap_or_else(|_| SimState::initial(self.vendor))
    }

    fn store(&self, state: &SimState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        crate::profiles::write_atomic(&self.path, &serde_json::to_vec_pretty(state)?)
    }

    fn update(&self, f: impl FnOnce(&mut SimState)) -> Result<()> {
        let mut state = self.load();
        f(&mut state);
        self.store(&state)
    }

    fn is_target(&self, gdi_name: &str) -> bool {
        gdi_name.eq_ignore_ascii_case(SIM_GDI_NAME)
    }
}

impl DisplayIo for SimIo {
    fn read_vcp(&self, hmonitor: i64, _delay_ms: u64, codes: &[u8]) -> Result<Vec<(u8, u32)>> {
        anyhow::ensure!(hmonitor == SIM_HMONITOR, "simulated rig has no monitor {hmonitor}");
        let state = self.load();
        codes
            .iter()
            .map(|c| state.vcp.get(c).map(|v| (*c, *v)).with_context(|| format!("code {c:#04x}")))
            .collect()
    }

    fn write_vcp(&self, hmonitor: i64, _delay_ms: u64, writes: &[(u8, u32)]) -> Result<()> {
        anyhow::ensure!(hmonitor == SIM_HMONITOR, "simulated rig has no monitor {hmonitor}");
        self.update(|s| {
            for &(code, value) in writes {
                s.vcp.insert(code, value);
            }
        })
    }

    fn get_ramp(&self, gdi_name: &str) -> Result<Ramp> {
        anyhow::ensure!(self.is_target(gdi_name), "no such display {gdi_name}");
        Ok(self.load().ramp)
    }

    fn set_ramp(&self, gdi_name: &str, ramp: &Ramp) -> Result<()> {
        anyhow::ensure!(self.is_target(gdi_name), "no such display {gdi_name}");
        self.update(|s| s.ramp = ramp.clone())
    }

    fn gpu_read(&self, gdi_name: &str) -> Option<GpuColorSnapshot> {
        self.is_target(gdi_name).then(|| self.load().gpu)
    }

    fn gpu_apply(
        &self,
        gdi_name: &str,
        vibrance_percent: i32,
        hue_deg: i32,
    ) -> Result<Option<GpuApplied>> {
        if !self.is_target(gdi_name) {
            return Ok(None);
        }
        let cur = self.load().gpu;
        // The production mapping functions, not a stand-in: the point of the
        // harness is that the numbers written are the ones a real driver
        // would be handed.
        let mut partial = Vec::new();
        let (raw, hue_raw) = match cur.vendor {
            GpuVendor::Nvidia => {
                if vibrance_percent < 50 {
                    partial.push("vibrance (NVIDIA cannot desaturate below neutral)".into());
                }
                (
                    relay_display::nvapi::vibrance_percent_to_dvc(
                        vibrance_percent,
                        cur.dvc_min,
                        cur.dvc_max,
                    ),
                    hue_deg.rem_euclid(360),
                )
            }
            GpuVendor::Amd => {
                let sat = relay_display::amd::AdlRange {
                    current: cur.dvc,
                    default: 100,
                    min: cur.dvc_min,
                    max: cur.dvc_max,
                    step: 1,
                };
                let hue_range = relay_display::amd::AdlRange {
                    current: cur.hue_deg,
                    default: 0,
                    min: cur.hue_min,
                    max: cur.hue_max,
                    step: 1,
                };
                let (hue_raw, clamped) = relay_display::amd::hue_deg_to_adl(hue_deg, &hue_range);
                if clamped {
                    partial.push(format!(
                        "hue (AMD trims to {}..{}°, asked for {hue_deg}°)",
                        hue_range.min, hue_range.max
                    ));
                }
                (
                    relay_display::amd::vibrance_percent_to_saturation(vibrance_percent, &sat),
                    hue_raw,
                )
            }
        };
        self.update(|s| {
            s.gpu.dvc = raw;
            s.gpu.hue_deg = hue_raw;
        })?;
        Ok(Some(GpuApplied { vendor: cur.vendor, partial }))
    }

    fn gpu_restore(&self, gdi_name: &str, snap: &GpuColorSnapshot) -> Result<()> {
        anyhow::ensure!(self.is_target(gdi_name), "no such display {gdi_name}");
        let cur = self.load().gpu;
        anyhow::ensure!(
            cur.vendor == snap.vendor,
            "restore addressed the {:?} API on a {:?} display",
            snap.vendor,
            cur.vendor
        );
        self.update(|s| s.gpu = *snap)
    }

    fn monitors(&self) -> Vec<MonitorProbe> {
        vec![sim_monitor()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulated_rig_survives_being_reopened() {
        let dir = std::env::temp_dir().join(format!("relay-sim-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sim.json");
        let io = SimIo { path: path.clone(), vendor: GpuVendor::Amd };

        let before = io.load();
        io.write_vcp(SIM_HMONITOR, 0, &[(0x10, 95)]).unwrap();
        io.gpu_apply(SIM_GDI_NAME, 80, 0).unwrap();

        // A second SimIo is a fresh process as far as the state is concerned.
        let reopened = SimIo { path, vendor: GpuVendor::Amd };
        let after = reopened.load();
        assert_eq!(after.vcp[&0x10], 95, "the write outlived the handle");
        assert_eq!(after.gpu.dvc, 160, "AMD saturation, 80 % of the way to max");
        assert_ne!(after.summary(), before.summary());

        reopened.gpu_restore(SIM_GDI_NAME, &before.gpu).unwrap();
        assert_eq!(reopened.load().gpu, before.gpu);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_restore_addressed_to_the_wrong_vendor_is_refused() {
        let dir = std::env::temp_dir().join(format!("relay-sim-{}", uuid::Uuid::new_v4()));
        let io = SimIo { path: dir.join("sim.json"), vendor: GpuVendor::Amd };
        io.write_vcp(SIM_HMONITOR, 0, &[(0x10, 90)]).unwrap(); // materialise the file
        let nvidia = SimState::initial(GpuVendor::Nvidia).gpu;
        let err = io.gpu_restore(SIM_GDI_NAME, &nvidia).unwrap_err();
        assert!(err.to_string().contains("Nvidia"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_but_the_simulated_monitor_can_be_addressed() {
        let io = SimIo {
            path: std::env::temp_dir().join("relay-sim-none.json"),
            vendor: GpuVendor::Amd,
        };
        assert!(io.gpu_read(r"\\.\DISPLAY1").is_none());
        assert!(io.read_vcp(1, 0, &[0x10]).is_err());
        assert!(io.set_ramp(r"\\.\DISPLAY1", &Ramp::identity()).is_err());
    }
}
