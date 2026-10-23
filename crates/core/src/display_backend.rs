//! The real display backend: DDC/CI + gamma ramp + NvAPI, glued to the
//! [`DisplayControl`] contract.
//!
//! All decisions (which VCP codes, which paths, what to capture) live in
//! [`DisplayAdapter`], generic over the [`DisplayIo`] primitive operations so
//! every multi-monitor and failure path is unit-testable with a fake. The
//! real I/O ([`RealIo`], Windows only) is a thin veneer over `relay-display`.
//!
//! Invariants enforced here:
//! - Only the target monitor is read or written. Ever.
//! - Capture reads exactly the values apply will change; failing to *read*
//!   aborts the apply (never write what you cannot put back).
//! - Restore re-resolves the monitor by its stable id first — after a crash
//!   or reboot the snapshot's `HMONITOR`/GDI name may be stale. A monitor
//!   that is no longer attached fails the restore so the snapshot stays
//!   pending and is retried on the next start (it may be plugged in again).

use anyhow::{bail, Context, Result};
use relay_display::gamma::{build_ramp, Ramp, RampParams};
use relay_display::mode::{DisplayMode, HdrState};
use relay_display::nvcolor::{self, NvColorRaw};
use relay_display::vcp::{self, MonitorPlanInput, Unsupported, VcpWrite};
use tracing::warn;

use crate::apply::DisplayControl;
use crate::backup::{DisplayStateSnapshot, GpuColorSnapshot, GpuVendor, MonitorStateSnapshot};
use crate::hardware::MonitorProbe;
use crate::types::{DisplaySettings, DisplayVia, GpuColor, MonitorSettings};

/// Primitive operations, separated for testability. Batch calls keep the
/// slow DDC/CI handle churn to one open per transaction group.
pub trait DisplayIo: Send + Sync {
    /// Read current values of the given VCP codes, in order.
    fn read_vcp(&self, hmonitor: i64, delay_ms: u64, codes: &[u8]) -> Result<Vec<(u8, u32)>>;
    /// Write the given (code, value) pairs, in order.
    fn write_vcp(&self, hmonitor: i64, delay_ms: u64, writes: &[(u8, u32)]) -> Result<()>;
    fn get_ramp(&self, gdi_name: &str) -> Result<Ramp>;
    fn set_ramp(&self, gdi_name: &str, ramp: &Ramp) -> Result<()>;
    /// Raw vendor colour state, `None` when no vendor API drives this
    /// display. Which vendor answered is recorded in the snapshot, because
    /// restore has to go back through the same one.
    fn gpu_read(&self, gdi_name: &str) -> Option<GpuColorSnapshot>;
    /// Apply profile-unit vibrance/hue. `Ok(None)` = no vendor path here, so
    /// the caller reports those fields unsupported instead of pretending.
    /// The returned strings are fields the vendor could not honour in full
    /// (AMD's hue range is a narrow trim, so a large angle is clamped).
    fn gpu_apply(
        &self,
        gdi_name: &str,
        vibrance_percent: i32,
        hue_deg: i32,
    ) -> Result<Option<GpuApplied>>;
    /// Put back raw vendor state, through the vendor that captured it.
    fn gpu_restore(&self, gdi_name: &str, snap: &GpuColorSnapshot) -> Result<()>;
    /// Currently attached monitors, for re-resolving stale snapshots.
    fn monitors(&self) -> Vec<MonitorProbe>;
    /// S52: modes the driver lists for this display (empty = unknown).
    fn modes(&self, gdi_name: &str) -> Vec<DisplayMode>;
    fn get_mode(&self, gdi_name: &str) -> Result<DisplayMode>;
    /// Switch mode, dynamically (never saved to the registry).
    fn set_mode(&self, gdi_name: &str, mode: DisplayMode) -> Result<()>;
    /// Windows' HDR state, `None` when unknown.
    fn hdr_state(&self, gdi_name: &str) -> Option<HdrState>;
    fn set_hdr(&self, gdi_name: &str, enabled: bool) -> Result<()>;
    /// Raw NVIDIA output colour, `None` when NvAPI does not drive this
    /// display or the driver lacks the colour control.
    fn nv_color_read(&self, gdi_name: &str) -> Option<NvColorRaw>;
    fn nv_color_write(&self, gdi_name: &str, raw: &NvColorRaw) -> Result<()>;
}

/// What a vendor colour apply actually managed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuApplied {
    pub vendor: GpuVendor,
    /// Fields the hardware honoured only partially, for `DisplayVia`.
    pub partial: Vec<String>,
}

fn plan_input(m: &MonitorSettings) -> MonitorPlanInput {
    MonitorPlanInput {
        brightness: m.brightness,
        contrast: m.contrast,
        black_equalizer: m.black_equalizer,
        response: m.response.clone(),
        sharpness: m.sharpness,
        color_preset: m.color_preset,
    }
}

fn ramp_params(gpu: &GpuColor) -> RampParams {
    RampParams { gamma: gpu.gamma, contrast: gpu.contrast, shadow_lift: gpu.shadow_lift }
}

/// Does this profile ask for anything only a vendor colour API can do?
/// Gamma, contrast and shadow lift ride the vendor-neutral ramp instead, so
/// they are deliberately not here.
fn wants_gpu_color(gpu: &GpuColor) -> bool {
    gpu.vibrance != 50 || gpu.hue_deg != 0
}

/// What one apply is going to do on the target monitor.
struct Plan {
    writes: Vec<VcpWrite>,
    unsupported: Vec<Unsupported>,
    delay_ms: u64,
    ramp: Option<Ramp>,
    gpu: bool,
}

fn plan(target: &MonitorProbe, settings: &DisplaySettings) -> Plan {
    // Keyed on the EDID manufacturer + product code; the display name rides
    // along for logs only, because one marketing name covers several panels.
    let key = vcp::MonitorKey::from_id(&target.id.0).map(|k| k.with_name(&target.name));
    let quirks = key.as_ref().map(vcp::quirks_for_key).unwrap_or_default();
    let mut input = plan_input(&settings.monitor);
    let mut preset_unsupported = None;
    // A colour preset the panel does not list is never written: the value
    // set is the monitor's own answer, not MCCS's full table.
    if let (Some(v), Some(allowed)) = (input.color_preset, target.color_presets.as_deref()) {
        if !allowed.contains(&v) {
            input.color_preset = None;
            preset_unsupported = Some(Unsupported {
                field: "color_preset",
                reason: vcp::UnsupportedReason::NotAdvertised,
            });
        }
    }
    let (writes, mut unsupported) = vcp::plan_writes(&input, target.ddc.as_deref(), &quirks);
    unsupported.extend(preset_unsupported);
    let params = ramp_params(&settings.gpu);
    Plan {
        writes,
        unsupported,
        delay_ms: quirks.write_delay_ms,
        ramp: (!params.is_neutral()).then(|| build_ramp(&params)),
        gpu: wants_gpu_color(&settings.gpu),
    }
}

/// S52: the Windows/NVIDIA output half of one apply. Unlike [`Plan`] it
/// depends on live state (the mode list, HDR support, the current NVIDIA
/// colour), so it is planned through the I/O and only carries changes: a
/// setting already in effect is neither captured nor written.
#[derive(Default)]
struct OutputPlan {
    mode: Option<DisplayMode>,
    /// The mode in effect now (captured for restore).
    mode_was: Option<DisplayMode>,
    hdr: Option<bool>,
    hdr_was: Option<bool>,
    nv: Option<NvColorRaw>,
    nv_was: Option<NvColorRaw>,
    unsupported: Vec<String>,
}

fn plan_output<Io: DisplayIo>(io: &Io, gdi: &str, s: &DisplaySettings) -> Result<OutputPlan> {
    let mut p = OutputPlan::default();
    if let Some(want) = s.mode {
        if io.modes(gdi).contains(&want) {
            let now = io.get_mode(gdi).context("reading the current display mode")?;
            if now != want {
                p.mode = Some(want);
                p.mode_was = Some(now);
            }
        } else {
            p.unsupported.push("mode".into());
        }
    }
    if let Some(want) = s.hdr {
        match io.hdr_state(gdi) {
            Some(h) if h.supported => {
                if h.enabled != want {
                    p.hdr = Some(want);
                    p.hdr_was = Some(h.enabled);
                }
            }
            _ => p.unsupported.push("hdr".into()),
        }
    }
    if !s.nvidia_color.is_inert() {
        match io.nv_color_read(gdi) {
            Some(now) => match nvcolor::plan(&now, &s.nvidia_color) {
                Ok(Some(next)) => {
                    p.nv = Some(next);
                    p.nv_was = Some(now);
                }
                Ok(None) => {}
                Err(field) => p.unsupported.push(field.into()),
            },
            None => p.unsupported.push("nvidia_color".into()),
        }
    }
    Ok(p)
}

pub struct DisplayAdapter<Io> {
    io: Io,
}

impl<Io: DisplayIo> DisplayAdapter<Io> {
    pub fn with_io(io: Io) -> Self {
        Self { io }
    }

    /// Fresh (hmonitor, gdi_name) for a snapshot's monitor: by stable id
    /// against the current topology, falling back to the captured handles
    /// (still valid when restoring in the same session with no display
    /// change). `None` = the monitor is not attached at all.
    fn resolve(
        &self,
        current: &[MonitorProbe],
        ms: &MonitorStateSnapshot,
    ) -> Option<(i64, String)> {
        if let Some(m) = current.iter().find(|m| m.id == ms.monitor) {
            return Some((m.hmonitor, m.gdi_name.clone()));
        }
        // Same-session fallback: the handle may still be alive even if the
        // probe now reports differently (e.g. EDID temporarily unreadable).
        if current.iter().any(|m| m.gdi_name == ms.gdi_name) {
            return Some((ms.hmonitor, ms.gdi_name.clone()));
        }
        None
    }
}

impl<Io: DisplayIo> DisplayControl for DisplayAdapter<Io> {
    fn capture(
        &self,
        target: Option<&MonitorProbe>,
        settings: &DisplaySettings,
    ) -> Result<DisplayStateSnapshot> {
        let mut snap = DisplayStateSnapshot::default();
        let Some(t) = target else { return Ok(snap) };
        let plan = plan(t, settings);

        let mut ms = MonitorStateSnapshot {
            monitor: t.id.clone(),
            gdi_name: t.gdi_name.clone(),
            hmonitor: t.hmonitor,
            vcp: Vec::new(),
            gamma: None,
            gpu: None,
            mode: None,
            hdr: None,
            nv_color: None,
        };
        let out = plan_output(&self.io, &t.gdi_name, settings)?;
        ms.mode = out.mode_was;
        ms.hdr = out.hdr_was;
        ms.nv_color = out.nv_was;
        if !plan.writes.is_empty() {
            let codes: Vec<u8> = plan.writes.iter().map(|w| w.code).collect();
            ms.vcp = self
                .io
                .read_vcp(t.hmonitor, plan.delay_ms, &codes)
                .context("reading original VCP values")?;
        }
        if plan.ramp.is_some() {
            ms.gamma = Some(self.io.get_ramp(&t.gdi_name).context("reading original gamma ramp")?);
        }
        if plan.gpu {
            // No vendor colour API is not an error — apply will skip the
            // path and say so. A machine with neither NVIDIA nor AMD gets
            // the gamma ramp and DDC/CI, never a failure.
            ms.gpu = self.io.gpu_read(&t.gdi_name);
        }
        if !ms.vcp.is_empty()
            || ms.gamma.is_some()
            || ms.gpu.is_some()
            || ms.mode.is_some()
            || ms.hdr.is_some()
            || ms.nv_color.is_some()
        {
            snap.targets.push(ms);
        }
        Ok(snap)
    }

    fn apply(
        &self,
        target: Option<&MonitorProbe>,
        settings: &DisplaySettings,
    ) -> Result<DisplayVia> {
        let mut via = DisplayVia::default();
        let Some(t) = target else { return Ok(via) };
        let plan = plan(t, settings);
        via.unsupported = plan.unsupported.iter().map(|u| u.field.to_string()).collect();

        // Mode and HDR first: both can reset the ramp and the GPU's output
        // colour, so everything after re-asserts on top of the new mode.
        let out = plan_output(&self.io, &t.gdi_name, settings)?;
        via.unsupported.extend(out.unsupported.iter().cloned());
        if let Some(m) = out.mode {
            self.io.set_mode(&t.gdi_name, m).context("changing the display mode")?;
            via.windows = true;
        }
        if let Some(on) = out.hdr {
            self.io.set_hdr(&t.gdi_name, on).context("switching HDR")?;
            via.windows = true;
        }
        if let Some(raw) = &out.nv {
            self.io.nv_color_write(&t.gdi_name, raw).context("setting NVIDIA output colour")?;
            via.nvapi = true;
        }

        if !plan.writes.is_empty() {
            let writes: Vec<(u8, u32)> = plan.writes.iter().map(|w| (w.code, w.value)).collect();
            self.io.write_vcp(t.hmonitor, plan.delay_ms, &writes).context("writing VCP values")?;
            via.ddcci = true;
        }
        if let Some(ramp) = &plan.ramp {
            self.io.set_ramp(&t.gdi_name, ramp).context("setting gamma ramp")?;
            via.gamma = true;
        }
        if plan.gpu {
            match self
                .io
                .gpu_apply(&t.gdi_name, settings.gpu.vibrance, settings.gpu.hue_deg)
                .context("applying GPU vibrance/hue")?
            {
                Some(applied) => {
                    match applied.vendor {
                        GpuVendor::Nvidia => via.nvapi = true,
                        GpuVendor::Amd => via.amd = true,
                    }
                    via.unsupported.extend(applied.partial);
                }
                None => {
                    if settings.gpu.vibrance != 50 {
                        via.unsupported.push("vibrance".into());
                    }
                    if settings.gpu.hue_deg != 0 {
                        via.unsupported.push("hue".into());
                    }
                }
            }
        }
        Ok(via)
    }

    fn restore(&self, original: &DisplayStateSnapshot) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        let current = if original.targets.is_empty() { Vec::new() } else { self.io.monitors() };
        // Reverse of capture order. Within one monitor: the mode and HDR go
        // back first (each can reset the ramp and output colour), then the
        // NVIDIA output colour, GPU colour, ramp, and VCP codes
        // last-written-first.
        for ms in original.targets.iter().rev() {
            let Some((hmonitor, gdi_name)) = self.resolve(&current, ms) else {
                warn!(monitor = %ms.monitor.0, "monitor not attached; restore stays pending");
                first_err.get_or_insert_with(|| {
                    anyhow::anyhow!("monitor {} not attached; will retry", ms.monitor.0)
                });
                continue;
            };
            if let Some(m) = ms.mode {
                if let Err(e) = self.io.set_mode(&gdi_name, m) {
                    warn!(error = %e, "display mode restore failed");
                    first_err.get_or_insert(e);
                }
            }
            if let Some(on) = ms.hdr {
                if let Err(e) = self.io.set_hdr(&gdi_name, on) {
                    warn!(error = %e, "HDR restore failed");
                    first_err.get_or_insert(e);
                }
            }
            if let Some(raw) = &ms.nv_color {
                if let Err(e) = self.io.nv_color_write(&gdi_name, raw) {
                    warn!(error = %e, "NVIDIA output colour restore failed");
                    first_err.get_or_insert(e);
                }
            }
            if let Some(gpu) = &ms.gpu {
                if let Err(e) = self.io.gpu_restore(&gdi_name, gpu) {
                    warn!(error = %e, "GPU colour restore failed");
                    first_err.get_or_insert(e);
                }
            }
            if let Some(ramp) = &ms.gamma {
                if let Err(e) = self.io.set_ramp(&gdi_name, ramp) {
                    warn!(error = %e, "gamma ramp restore failed");
                    first_err.get_or_insert(e);
                }
            }
            if !ms.vcp.is_empty() {
                let delay = vcp::quirks_for(&ms.monitor.0).write_delay_ms;
                let reversed: Vec<(u8, u32)> = ms.vcp.iter().rev().copied().collect();
                if let Err(e) = self.io.write_vcp(hmonitor, delay, &reversed) {
                    warn!(error = %e, "VCP restore failed");
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Real I/O (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub use real::{RealIo, WinDisplay};

#[cfg(windows)]
mod real {
    use super::*;
    use relay_display::{amd, ddc, gamma, mode, nvapi};

    /// The production backend type wired in `service::Backends`.
    pub type WinDisplay = DisplayAdapter<RealIo>;

    impl WinDisplay {
        pub fn new() -> Self {
            DisplayAdapter::with_io(RealIo)
        }
    }

    impl Default for WinDisplay {
        fn default() -> Self {
            Self::new()
        }
    }

    pub struct RealIo;

    impl DisplayIo for RealIo {
        fn read_vcp(&self, hmonitor: i64, delay_ms: u64, codes: &[u8]) -> Result<Vec<(u8, u32)>> {
            let pm = ddc::PhysicalMonitor::open(hmonitor, delay_ms)?;
            codes.iter().map(|&code| pm.get_vcp(code).map(|(cur, _max)| (code, cur))).collect()
        }

        fn write_vcp(&self, hmonitor: i64, delay_ms: u64, writes: &[(u8, u32)]) -> Result<()> {
            let pm = ddc::PhysicalMonitor::open(hmonitor, delay_ms)?;
            for &(code, value) in writes {
                pm.set_vcp(code, value)?;
            }
            Ok(())
        }

        fn get_ramp(&self, gdi_name: &str) -> Result<Ramp> {
            gamma::io::get_ramp(gdi_name)
        }

        fn set_ramp(&self, gdi_name: &str, ramp: &Ramp) -> Result<()> {
            gamma::io::set_ramp(gdi_name, ramp)
        }

        fn gpu_read(&self, gdi_name: &str) -> Option<GpuColorSnapshot> {
            nv_read(gdi_name).or_else(|| amd_read(gdi_name))
        }

        fn gpu_apply(
            &self,
            gdi_name: &str,
            vibrance_percent: i32,
            hue_deg: i32,
        ) -> Result<Option<GpuApplied>> {
            match nv_apply(gdi_name, vibrance_percent, hue_deg)? {
                Some(a) => Ok(Some(a)),
                None => amd_apply(gdi_name, vibrance_percent, hue_deg),
            }
        }

        fn gpu_restore(&self, gdi_name: &str, snap: &GpuColorSnapshot) -> Result<()> {
            // Through the vendor that captured it, never "whichever answers
            // now": on a two-GPU machine the raw units are not comparable.
            match snap.vendor {
                GpuVendor::Nvidia => nv_restore(gdi_name, snap),
                GpuVendor::Amd => amd_restore(gdi_name, snap),
            }
        }

        fn monitors(&self) -> Vec<MonitorProbe> {
            crate::hardware::probe_win::probe_monitors(false)
        }

        fn modes(&self, gdi_name: &str) -> Vec<DisplayMode> {
            mode::io::list_modes(gdi_name)
        }

        fn get_mode(&self, gdi_name: &str) -> Result<DisplayMode> {
            mode::io::current_mode(gdi_name)
        }

        fn set_mode(&self, gdi_name: &str, m: DisplayMode) -> Result<()> {
            mode::io::set_mode(gdi_name, m)
        }

        fn hdr_state(&self, gdi_name: &str) -> Option<HdrState> {
            mode::io::hdr_state(gdi_name)
        }

        fn set_hdr(&self, gdi_name: &str, enabled: bool) -> Result<()> {
            mode::io::set_hdr(gdi_name, enabled)
        }

        fn nv_color_read(&self, gdi_name: &str) -> Option<NvColorRaw> {
            let api = nvapi::NvApi::load()?;
            let d = api.display_by_gdi_name(gdi_name)?;
            api.get_color(&d).ok()
        }

        fn nv_color_write(&self, gdi_name: &str, raw: &NvColorRaw) -> Result<()> {
            let Some(api) = nvapi::NvApi::load() else { bail!("NvAPI unavailable for {gdi_name}") };
            let Some(d) = api.display_by_gdi_name(gdi_name) else {
                bail!("NvAPI does not drive {gdi_name}")
            };
            api.set_color(&d, raw)
        }
    }

    // -----------------------------------------------------------------
    // NVIDIA
    // -----------------------------------------------------------------

    fn nv_read(gdi_name: &str) -> Option<GpuColorSnapshot> {
        let api = nvapi::NvApi::load()?;
        let d = api.display_by_gdi_name(gdi_name)?;
        let dvc = api.get_vibrance(&d).ok()?;
        let (hue, _default) = api.get_hue(&d).ok()?;
        Some(GpuColorSnapshot {
            vendor: GpuVendor::Nvidia,
            dvc: dvc.current,
            dvc_min: dvc.min,
            dvc_max: dvc.max,
            hue_deg: hue,
            hue_min: 0,
            hue_max: 0,
        })
    }

    fn nv_apply(gdi_name: &str, vibrance_percent: i32, hue_deg: i32) -> Result<Option<GpuApplied>> {
        let Some(api) = nvapi::NvApi::load() else { return Ok(None) };
        let Some(d) = api.display_by_gdi_name(gdi_name) else { return Ok(None) };
        let dvc = api.get_vibrance(&d)?;
        let level = nvapi::vibrance_percent_to_dvc(vibrance_percent, dvc.min, dvc.max);
        api.set_vibrance(&d, level)?;
        api.set_hue(&d, hue_deg.rem_euclid(360))?;
        let mut partial = Vec::new();
        // NvAPI's DVC cannot go below neutral; say so rather than leaving the
        // user wondering why the bottom half of the slider does nothing.
        if vibrance_percent < 50 {
            partial.push("vibrance (NVIDIA cannot desaturate below neutral)".into());
        }
        Ok(Some(GpuApplied { vendor: GpuVendor::Nvidia, partial }))
    }

    fn nv_restore(gdi_name: &str, snap: &GpuColorSnapshot) -> Result<()> {
        let Some(api) = nvapi::NvApi::load() else {
            bail!("NvAPI unavailable while restoring {gdi_name}")
        };
        let Some(d) = api.display_by_gdi_name(gdi_name) else {
            bail!("NvAPI no longer drives {gdi_name}")
        };
        api.set_vibrance(&d, snap.dvc)?;
        api.set_hue(&d, snap.hue_deg)?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // AMD
    // -----------------------------------------------------------------

    fn amd_read(gdi_name: &str) -> Option<GpuColorSnapshot> {
        let adl = amd::Adl::load()?;
        let d = adl.display_by_gdi_name(gdi_name)?;
        let sat = adl.get_saturation(&d).ok()?;
        let hue = adl.get_hue(&d).ok()?;
        Some(GpuColorSnapshot {
            vendor: GpuVendor::Amd,
            dvc: sat.current,
            dvc_min: sat.min,
            dvc_max: sat.max,
            hue_deg: hue.current,
            hue_min: hue.min,
            hue_max: hue.max,
        })
    }

    fn amd_apply(
        gdi_name: &str,
        vibrance_percent: i32,
        hue_deg: i32,
    ) -> Result<Option<GpuApplied>> {
        let Some(adl) = amd::Adl::load() else { return Ok(None) };
        let Some(d) = adl.display_by_gdi_name(gdi_name) else { return Ok(None) };
        let sat = adl.get_saturation(&d)?;
        adl.set_saturation(&d, amd::vibrance_percent_to_saturation(vibrance_percent, &sat))?;
        let hue_range = adl.get_hue(&d)?;
        let (value, clamped) = amd::hue_deg_to_adl(hue_deg, &hue_range);
        adl.set_hue(&d, value)?;
        let mut partial = Vec::new();
        if clamped {
            partial.push(format!(
                "hue (AMD trims to {}..{}°, asked for {hue_deg}°)",
                hue_range.min, hue_range.max
            ));
        }
        Ok(Some(GpuApplied { vendor: GpuVendor::Amd, partial }))
    }

    fn amd_restore(gdi_name: &str, snap: &GpuColorSnapshot) -> Result<()> {
        let Some(adl) = amd::Adl::load() else {
            bail!("ADL unavailable while restoring {gdi_name}")
        };
        let Some(d) = adl.display_by_gdi_name(gdi_name) else {
            bail!("AMD no longer drives {gdi_name}")
        };
        // The captured raw values, not a re-derived mapping.
        adl.set_saturation(&d, snap.dvc)?;
        adl.set_hue(&d, snap.hue_deg)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "display_backend_vendor_tests.rs"]
mod vendor_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MonitorId;
    use parking_lot::Mutex;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const RAW_DISPLAY1: &str = r"\\.\DISPLAY1";

    /// In-memory display topology: per-monitor VCP registers, ramps, vendor
    /// state, plus failure injection and a write log.
    #[derive(Default)]
    pub(super) struct FakeIo {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        /// hmonitor → (code → value)
        vcp: BTreeMap<i64, BTreeMap<u8, u32>>,
        /// gdi name → ramp
        ramps: BTreeMap<String, Ramp>,
        /// gdi name → raw vendor colour state (whichever vendor drives it)
        nv: BTreeMap<String, GpuColorSnapshot>,
        monitors: Vec<MonitorProbe>,
        log: Vec<String>,
        /// Fail every VCP write after this many have succeeded.
        fail_writes_after: Option<usize>,
        writes_done: usize,
        /// S52: gdi name → (listed modes, current mode, HDR, NVIDIA colour).
        modes: BTreeMap<String, Vec<DisplayMode>>,
        mode: BTreeMap<String, DisplayMode>,
        hdr: BTreeMap<String, HdrState>,
        nvc: BTreeMap<String, NvColorRaw>,
    }

    fn monitor(
        id: &str,
        hmonitor: i64,
        gdi: &str,
        primary: bool,
        ddc: Option<Vec<u8>>,
    ) -> MonitorProbe {
        MonitorProbe {
            id: MonitorId(id.into()),
            name: id.to_uppercase(),
            native: None,
            refresh_hz: None,
            primary,
            hmonitor,
            gdi_name: gdi.into(),
            ddc,
            color: None,
            color_presets: None,
        }
    }

    /// Raw NVIDIA state as a real RTX reports it: DVC 0..63, neutral at the
    /// minimum.
    pub(super) fn nvidia_state() -> GpuColorSnapshot {
        GpuColorSnapshot {
            vendor: GpuVendor::Nvidia,
            dvc: 0,
            dvc_min: 0,
            dvc_max: 63,
            hue_deg: 0,
            hue_min: 0,
            hue_max: 0,
        }
    }

    /// Raw AMD state as Radeon Software reports it: saturation 0..200 with
    /// the default at 100, and hue a narrow signed trim.
    pub(super) fn amd_state() -> GpuColorSnapshot {
        GpuColorSnapshot {
            vendor: GpuVendor::Amd,
            dvc: 100,
            dvc_min: 0,
            dvc_max: 200,
            hue_deg: 0,
            hue_min: -30,
            hue_max: 30,
        }
    }

    impl FakeIo {
        pub(super) fn two_monitors() -> Arc<Self> {
            Self::two_monitors_with(nvidia_state())
        }

        /// The same two-monitor rig driven by the given vendor, so every
        /// invariant can be asserted against both without a second harness.
        pub(super) fn two_monitors_with(gpu: GpuColorSnapshot) -> Arc<Self> {
            let io = Arc::new(FakeIo::default());
            {
                let mut s = io.state.lock();
                s.monitors = vec![
                    monitor("mon:A", 1, r"\\.\DISPLAY1", true, Some(vec![0x10, 0x12])),
                    monitor("mon:B", 2, r"\\.\DISPLAY2", false, Some(vec![0x10, 0x12])),
                ];
                s.vcp.insert(1, BTreeMap::from([(0x10u8, 40u32), (0x12u8, 70u32)]));
                s.vcp.insert(2, BTreeMap::from([(0x10u8, 55u32), (0x12u8, 60u32)]));
                s.ramps.insert(r"\\.\DISPLAY1".into(), Ramp::identity());
                s.ramps.insert(r"\\.\DISPLAY2".into(), Ramp::identity());
                s.nv.insert(r"\\.\DISPLAY1".into(), gpu);
                s.nv.insert(r"\\.\DISPLAY2".into(), gpu);
                for gdi in [r"\\.\DISPLAY1", r"\\.\DISPLAY2"] {
                    s.modes.insert(gdi.into(), vec![mode_4k(240), mode_4k(60), mode_1440(240)]);
                    s.mode.insert(gdi.into(), mode_1440(240));
                    s.hdr.insert(gdi.into(), HdrState { supported: true, enabled: true });
                    if gpu.vendor == GpuVendor::Nvidia {
                        s.nvc.insert(gdi.into(), nv_now());
                    }
                }
            }
            io
        }

        pub(super) fn log(&self) -> Vec<String> {
            self.state.lock().log.clone()
        }

        pub(super) fn snapshot_of(&self, hmonitor: i64) -> BTreeMap<u8, u32> {
            self.state.lock().vcp.get(&hmonitor).cloned().unwrap_or_default()
        }

        pub(super) fn gpu_of(&self, gdi: &str) -> GpuColorSnapshot {
            self.state.lock().nv[gdi]
        }

        /// No NvAPI and no ADL: the machine has neither vendor.
        pub(super) fn clear_gpu_vendors(&self) {
            self.state.lock().nv.clear();
        }

        /// Simulate a reboot: monitor A comes back on a new handle and a new
        /// GDI name, with its registers intact.
        pub(super) fn rename_display_1(&self, hmonitor: i64, gdi: &str) {
            let mut s = self.state.lock();
            let regs = s.vcp.remove(&1).unwrap();
            s.vcp.insert(hmonitor, regs);
            let ramp = s.ramps.remove(RAW_DISPLAY1).unwrap();
            s.ramps.insert(gdi.into(), ramp);
            let gpu = s.nv.remove(RAW_DISPLAY1).unwrap();
            s.nv.insert(gdi.into(), gpu);
            s.monitors[0].hmonitor = hmonitor;
            s.monitors[0].gdi_name = gdi.into();
        }
    }

    impl DisplayIo for Arc<FakeIo> {
        fn read_vcp(&self, hmonitor: i64, _delay: u64, codes: &[u8]) -> Result<Vec<(u8, u32)>> {
            let s = self.state.lock();
            let regs = s.vcp.get(&hmonitor).context("no such monitor")?;
            codes
                .iter()
                .map(|c| regs.get(c).map(|v| (*c, *v)).context("code unsupported"))
                .collect()
        }

        fn write_vcp(&self, hmonitor: i64, _delay: u64, writes: &[(u8, u32)]) -> Result<()> {
            let mut s = self.state.lock();
            for &(code, value) in writes {
                if let Some(limit) = s.fail_writes_after {
                    if s.writes_done >= limit {
                        anyhow::bail!("monitor vanished mid-write");
                    }
                }
                s.log.push(format!("vcp {hmonitor} {code:02X}={value}"));
                s.vcp.get_mut(&hmonitor).context("no such monitor")?.insert(code, value);
                s.writes_done += 1;
            }
            Ok(())
        }

        fn get_ramp(&self, gdi: &str) -> Result<Ramp> {
            self.state.lock().ramps.get(gdi).cloned().context("no ramp")
        }

        fn set_ramp(&self, gdi: &str, ramp: &Ramp) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("ramp {gdi}"));
            *s.ramps.get_mut(gdi).context("no such display")? = ramp.clone();
            Ok(())
        }

        fn gpu_read(&self, gdi: &str) -> Option<GpuColorSnapshot> {
            self.state.lock().nv.get(gdi).copied()
        }

        /// Mirrors `RealIo`'s dispatch, running the *production* mapping
        /// functions so the fixtures exercise the real curves.
        fn gpu_apply(&self, gdi: &str, vibrance: i32, hue: i32) -> Result<Option<GpuApplied>> {
            let mut s = self.state.lock();
            let Some(cur) = s.nv.get(gdi).copied() else { return Ok(None) };
            let mut partial = Vec::new();
            let (raw, hue_raw) = match cur.vendor {
                GpuVendor::Nvidia => {
                    if vibrance < 50 {
                        partial.push("vibrance (NVIDIA cannot desaturate below neutral)".into());
                    }
                    (
                        relay_display::nvapi::vibrance_percent_to_dvc(
                            vibrance,
                            cur.dvc_min,
                            cur.dvc_max,
                        ),
                        hue.rem_euclid(360),
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
                    let (hue_raw, clamped) = relay_display::amd::hue_deg_to_adl(hue, &hue_range);
                    if clamped {
                        partial.push(format!(
                            "hue (AMD trims to {}..{}°, asked for {hue}°)",
                            hue_range.min, hue_range.max
                        ));
                    }
                    (relay_display::amd::vibrance_percent_to_saturation(vibrance, &sat), hue_raw)
                }
            };
            s.log.push(format!("gpu {gdi} raw={raw} hue={hue_raw}"));
            let entry = s.nv.get_mut(gdi).unwrap();
            entry.dvc = raw;
            entry.hue_deg = hue_raw;
            Ok(Some(GpuApplied { vendor: cur.vendor, partial }))
        }

        fn gpu_restore(&self, gdi: &str, snap: &GpuColorSnapshot) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("gpu-restore {gdi}"));
            let entry = s.nv.get_mut(gdi).context("display gone")?;
            assert_eq!(entry.vendor, snap.vendor, "restore went to the wrong vendor API");
            *entry = *snap;
            Ok(())
        }

        fn monitors(&self) -> Vec<MonitorProbe> {
            self.state.lock().monitors.clone()
        }

        fn modes(&self, gdi: &str) -> Vec<DisplayMode> {
            self.state.lock().modes.get(gdi).cloned().unwrap_or_default()
        }

        fn get_mode(&self, gdi: &str) -> Result<DisplayMode> {
            self.state.lock().mode.get(gdi).copied().context("no such display")
        }

        fn set_mode(&self, gdi: &str, m: DisplayMode) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("mode {gdi} {m}"));
            *s.mode.get_mut(gdi).context("no such display")? = m;
            Ok(())
        }

        fn hdr_state(&self, gdi: &str) -> Option<HdrState> {
            self.state.lock().hdr.get(gdi).copied()
        }

        fn set_hdr(&self, gdi: &str, enabled: bool) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("hdr {gdi} {enabled}"));
            s.hdr.get_mut(gdi).context("no such display")?.enabled = enabled;
            Ok(())
        }

        fn nv_color_read(&self, gdi: &str) -> Option<NvColorRaw> {
            self.state.lock().nvc.get(gdi).copied()
        }

        fn nv_color_write(&self, gdi: &str, raw: &NvColorRaw) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("nvcolor {gdi} range={} bpc={}", raw.range, raw.bpc));
            *s.nvc.get_mut(gdi).context("no NvAPI on this display")? = *raw;
            Ok(())
        }
    }

    fn mode_4k(hz: u32) -> DisplayMode {
        DisplayMode { width: 3840, height: 2160, refresh_hz: hz }
    }

    fn mode_1440(hz: u32) -> DisplayMode {
        DisplayMode { width: 2560, height: 1440, refresh_hz: hz }
    }

    /// What the dev PC's RTX reported live (2026-10-22, read-only GET):
    /// RGB, BT.2100 colorimetry (HDR on), full range, 8-bit, user policy.
    fn nv_now() -> NvColorRaw {
        NvColorRaw { format: 0, colorimetry: 8, range: 0, bpc: 2, policy: 0, depth: 0 }
    }

    pub(super) fn settings() -> DisplaySettings {
        let mut d = DisplaySettings::default();
        d.monitor.brightness = Some(80);
        d.monitor.contrast = Some(50);
        d.gpu.vibrance = 75;
        d.gpu.gamma = 1.2;
        d
    }

    pub(super) fn target_a(io: &Arc<FakeIo>) -> MonitorProbe {
        io.state.lock().monitors[0].clone()
    }

    #[test]
    fn apply_touches_only_the_target_monitor_and_restore_reverts_it() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let before_b = io.snapshot_of(2);
        let t = target_a(&io);

        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        assert_eq!(snap.targets.len(), 1);
        let ms = &snap.targets[0];
        assert_eq!(ms.vcp, vec![(0x10, 40), (0x12, 70)], "originals recorded");
        assert!(ms.gamma.is_some());
        assert!(ms.gpu.is_some());

        let via = adapter.apply(Some(&t), &settings()).unwrap();
        assert!(via.ddcci && via.gamma && via.nvapi);
        assert!(via.unsupported.is_empty());
        assert_eq!(io.snapshot_of(1), BTreeMap::from([(0x10, 80), (0x12, 50)]));
        assert_eq!(io.snapshot_of(2), before_b, "monitor B untouched by apply");
        assert!(io.log().iter().all(|l| !l.contains("DISPLAY2") && !l.contains("vcp 2 ")));

        adapter.restore(&snap).unwrap();
        assert_eq!(io.snapshot_of(1), BTreeMap::from([(0x10, 40), (0x12, 70)]));
        assert_eq!(io.snapshot_of(2), before_b, "monitor B untouched by restore");
        let s = io.state.lock();
        assert_eq!(s.ramps[r"\\.\DISPLAY1"], Ramp::identity());
        assert_eq!(s.nv[r"\\.\DISPLAY1"].dvc, 0);
    }

    #[test]
    fn capture_reads_only_what_apply_will_write() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        // Only brightness set; GPU neutral → no ramp, no NvAPI in snapshot.
        let mut d = DisplaySettings::default();
        d.monitor.brightness = Some(90);
        let snap = adapter.capture(Some(&t), &d).unwrap();
        let ms = &snap.targets[0];
        assert_eq!(ms.vcp, vec![(0x10, 40)]);
        assert!(ms.gamma.is_none());
        assert!(ms.gpu.is_none());
    }

    #[test]
    fn display_inert_settings_produce_an_empty_snapshot() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &DisplaySettings::default()).unwrap();
        assert!(snap.targets.is_empty());
        let via = adapter.apply(Some(&t), &DisplaySettings::default()).unwrap();
        assert_eq!(via, DisplayVia::default());
        assert!(io.log().is_empty(), "nothing touched");
    }

    #[test]
    fn no_target_means_no_op() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        assert!(adapter.capture(None, &settings()).unwrap().targets.is_empty());
        assert_eq!(adapter.apply(None, &settings()).unwrap(), DisplayVia::default());
        assert!(io.log().is_empty());
    }

    #[test]
    fn mixed_ddc_support_skips_unadvertised_codes_and_reports_them() {
        let io = FakeIo::two_monitors();
        // Monitor advertises brightness only.
        io.state.lock().monitors[0].ddc = Some(vec![0x10]);
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        assert_eq!(snap.targets[0].vcp, vec![(0x10, 40)], "contrast not captured");
        let via = adapter.apply(Some(&t), &settings()).unwrap();
        assert!(via.ddcci);
        assert_eq!(via.unsupported, vec!["contrast".to_string()]);
        assert_eq!(io.snapshot_of(1)[&0x12], 70, "unadvertised code untouched");
    }

    #[test]
    fn nvapi_unavailable_falls_back_and_reports_vibrance_unsupported() {
        let io = FakeIo::two_monitors();
        io.state.lock().nv.clear();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        assert!(snap.targets[0].gpu.is_none());
        let via = adapter.apply(Some(&t), &settings()).unwrap();
        assert!(via.gamma && via.ddcci && !via.nvapi);
        assert_eq!(via.unsupported, vec!["vibrance".to_string()]);
    }

    #[test]
    fn monitor_unplugged_mid_apply_fails_cleanly_without_touching_others() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let _snap = adapter.capture(Some(&t), &settings()).unwrap();
        io.state.lock().fail_writes_after = Some(1);
        let err = adapter.apply(Some(&t), &settings()).unwrap_err();
        assert!(err.to_string().contains("VCP"), "{err}");
        assert_eq!(io.snapshot_of(2)[&0x10], 55, "monitor B untouched by the failure");
    }

    #[test]
    fn restore_re_resolves_stale_handles_by_stable_id() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        adapter.apply(Some(&t), &settings()).unwrap();

        // Reboot: same panel, new HMONITOR (9) and new GDI name (DISPLAY3).
        {
            let mut s = io.state.lock();
            let regs = s.vcp.remove(&1).unwrap();
            s.vcp.insert(9, regs);
            let ramp = s.ramps.remove(r"\\.\DISPLAY1").unwrap();
            s.ramps.insert(r"\\.\DISPLAY3".into(), ramp);
            let nv = s.nv.remove(r"\\.\DISPLAY1").unwrap();
            s.nv.insert(r"\\.\DISPLAY3".into(), nv);
            s.monitors[0].hmonitor = 9;
            s.monitors[0].gdi_name = r"\\.\DISPLAY3".into();
        }
        adapter.restore(&snap).unwrap();
        assert_eq!(io.snapshot_of(9), BTreeMap::from([(0x10, 40), (0x12, 70)]));
        assert_eq!(io.state.lock().nv[r"\\.\DISPLAY3"].dvc, 0);
    }

    #[test]
    fn restore_with_monitor_gone_fails_so_the_snapshot_stays_pending() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        adapter.apply(Some(&t), &settings()).unwrap();
        io.state.lock().monitors.remove(0);
        let before_b = io.snapshot_of(2);
        let err = adapter.restore(&snap).unwrap_err();
        assert!(err.to_string().contains("not attached"), "{err}");
        assert_eq!(io.snapshot_of(2), before_b, "monitor B untouched");
    }

    /// S52: mode, HDR, NVIDIA output colour and the colour preset are
    /// captured, applied on the target only, and put back — mode and HDR
    /// first on restore.
    #[test]
    fn s52_output_controls_capture_apply_and_restore_on_the_target_only() {
        let io = FakeIo::two_monitors();
        io.state.lock().monitors[0].color_presets = Some(vec![0x05, 0x08, 0x0B]);
        io.state.lock().vcp.get_mut(&1).unwrap().insert(0x14, 0x0B);
        io.state.lock().monitors[0].ddc = Some(vec![0x10, 0x12, 0x14]);
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let mut d =
            DisplaySettings { mode: Some(mode_4k(240)), hdr: Some(false), ..Default::default() };
        d.nvidia_color.bpc = Some(10);
        d.monitor.color_preset = Some(0x05);

        let snap = adapter.capture(Some(&t), &d).unwrap();
        let ms = &snap.targets[0];
        assert_eq!(ms.mode, Some(mode_1440(240)));
        assert_eq!(ms.hdr, Some(true));
        assert_eq!(ms.nv_color, Some(nv_now()));
        assert_eq!(ms.vcp, vec![(0x14, 0x0B)]);

        let via = adapter.apply(Some(&t), &d).unwrap();
        assert!(via.windows && via.nvapi && via.ddcci, "{via:?}");
        assert!(via.unsupported.is_empty(), "{via:?}");
        {
            let s = io.state.lock();
            assert_eq!(s.mode[RAW_DISPLAY1], mode_4k(240));
            assert!(!s.hdr[RAW_DISPLAY1].enabled);
            assert_eq!(s.nvc[RAW_DISPLAY1].bpc, 3, "10-bit");
            assert_eq!(s.nvc[RAW_DISPLAY1].policy, 0);
            assert_eq!(s.mode[r"\\.\DISPLAY2"], mode_1440(240), "monitor B untouched");
            assert!(s.hdr[r"\\.\DISPLAY2"].enabled);
        }
        assert_eq!(io.snapshot_of(1)[&0x14], 0x05);
        let log = io.log();
        let first_mode = log.iter().position(|l| l.starts_with("mode")).unwrap();
        let first_vcp = log.iter().position(|l| l.starts_with("vcp")).unwrap();
        assert!(first_mode < first_vcp, "mode before colour on apply: {log:?}");

        io.state.lock().log.clear();
        adapter.restore(&snap).unwrap();
        let s = io.state.lock();
        assert_eq!(s.mode[RAW_DISPLAY1], mode_1440(240));
        assert!(s.hdr[RAW_DISPLAY1].enabled);
        assert_eq!(s.nvc[RAW_DISPLAY1], nv_now());
        assert_eq!(s.vcp[&1][&0x14], 0x0B);
        assert!(s.log[0].starts_with("mode") && s.log[1].starts_with("hdr"), "{:?}", s.log);
    }

    #[test]
    fn s52_settings_already_in_effect_are_neither_captured_nor_written() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let mut d =
            DisplaySettings { mode: Some(mode_1440(240)), hdr: Some(true), ..Default::default() };
        d.nvidia_color.bpc = Some(8);
        let snap = adapter.capture(Some(&t), &d).unwrap();
        assert!(snap.targets.is_empty(), "{snap:?}");
        assert_eq!(adapter.apply(Some(&t), &d).unwrap(), DisplayVia::default());
        assert!(io.log().is_empty());
    }

    #[test]
    fn s52_unlisted_modes_unsupported_hdr_and_no_nvapi_are_reported_not_forced() {
        let io = FakeIo::two_monitors_with(amd_state());
        io.state
            .lock()
            .hdr
            .insert(RAW_DISPLAY1.into(), HdrState { supported: false, enabled: false });
        io.state.lock().monitors[0].color_presets = Some(vec![0x05]);
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let mut d = DisplaySettings {
            mode: Some(DisplayMode { width: 1280, height: 720, refresh_hz: 60 }),
            hdr: Some(true),
            ..Default::default()
        };
        d.nvidia_color.range = Some(relay_display::nvcolor::DynamicRange::Full);
        d.monitor.color_preset = Some(0x0B);
        let snap = adapter.capture(Some(&t), &d).unwrap();
        assert!(snap.targets.is_empty());
        let via = adapter.apply(Some(&t), &d).unwrap();
        let mut un = via.unsupported.clone();
        un.sort();
        assert_eq!(un, vec!["color_preset", "hdr", "mode", "nvidia_color"]);
        assert!(io.log().is_empty(), "nothing written: {:?}", io.log());
    }

    #[test]
    fn restore_writes_vcp_in_reverse_capture_order() {
        let io = FakeIo::two_monitors();
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        adapter.apply(Some(&t), &settings()).unwrap();
        io.state.lock().log.clear();
        adapter.restore(&snap).unwrap();
        let vcp_lines: Vec<String> =
            io.log().into_iter().filter(|l| l.starts_with("vcp")).collect();
        assert_eq!(vcp_lines, vec!["vcp 1 12=70".to_string(), "vcp 1 10=40".to_string()]);
    }
}
