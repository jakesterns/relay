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
use relay_display::vcp::{self, MonitorPlanInput, Unsupported, VcpWrite};
use tracing::warn;

use crate::apply::DisplayControl;
use crate::backup::{DisplayStateSnapshot, MonitorStateSnapshot, NvApiSnapshot};
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
    /// Raw NvAPI state, `None` when NvAPI or this display is unavailable.
    fn nv_read(&self, gdi_name: &str) -> Option<NvApiSnapshot>;
    /// Apply profile-unit vibrance/hue. `Ok(false)` = path unavailable.
    fn nv_apply(&self, gdi_name: &str, vibrance_percent: i32, hue_deg: i32) -> Result<bool>;
    /// Put back raw NvAPI state.
    fn nv_restore(&self, gdi_name: &str, snap: &NvApiSnapshot) -> Result<()>;
    /// Currently attached monitors, for re-resolving stale snapshots.
    fn monitors(&self) -> Vec<MonitorProbe>;
}

fn plan_input(m: &MonitorSettings) -> MonitorPlanInput {
    MonitorPlanInput {
        brightness: m.brightness,
        contrast: m.contrast,
        black_equalizer: m.black_equalizer,
        response: m.response.clone(),
        sharpness: m.sharpness,
    }
}

fn ramp_params(gpu: &GpuColor) -> RampParams {
    RampParams { gamma: gpu.gamma, contrast: gpu.contrast, shadow_lift: gpu.shadow_lift }
}

fn wants_nv(gpu: &GpuColor) -> bool {
    gpu.vibrance != 50 || gpu.hue_deg != 0
}

/// What one apply is going to do on the target monitor.
struct Plan {
    writes: Vec<VcpWrite>,
    unsupported: Vec<Unsupported>,
    delay_ms: u64,
    ramp: Option<Ramp>,
    nv: bool,
}

fn plan(target: &MonitorProbe, settings: &DisplaySettings) -> Plan {
    let quirks = vcp::quirks_for(&target.id.0);
    let (writes, unsupported) =
        vcp::plan_writes(&plan_input(&settings.monitor), target.ddc.as_deref(), &quirks);
    let params = ramp_params(&settings.gpu);
    Plan {
        writes,
        unsupported,
        delay_ms: quirks.write_delay_ms,
        ramp: (!params.is_neutral()).then(|| build_ramp(&params)),
        nv: wants_nv(&settings.gpu),
    }
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
            nvapi: None,
        };
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
        if plan.nv {
            // Unavailable NvAPI is not an error — apply will skip the path.
            ms.nvapi = self.io.nv_read(&t.gdi_name);
        }
        if !ms.vcp.is_empty() || ms.gamma.is_some() || ms.nvapi.is_some() {
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

        if !plan.writes.is_empty() {
            let writes: Vec<(u8, u32)> = plan.writes.iter().map(|w| (w.code, w.value)).collect();
            self.io.write_vcp(t.hmonitor, plan.delay_ms, &writes).context("writing VCP values")?;
            via.ddcci = true;
        }
        if let Some(ramp) = &plan.ramp {
            self.io.set_ramp(&t.gdi_name, ramp).context("setting gamma ramp")?;
            via.gamma = true;
        }
        if plan.nv {
            if self
                .io
                .nv_apply(&t.gdi_name, settings.gpu.vibrance, settings.gpu.hue_deg)
                .context("applying NvAPI vibrance/hue")?
            {
                via.nvapi = true;
            } else {
                if settings.gpu.vibrance != 50 {
                    via.unsupported.push("vibrance".into());
                }
                if settings.gpu.hue_deg != 0 {
                    via.unsupported.push("hue".into());
                }
            }
        }
        Ok(via)
    }

    fn restore(&self, original: &DisplayStateSnapshot) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        let current = if original.targets.is_empty() { Vec::new() } else { self.io.monitors() };
        // Reverse of capture order, and within one monitor reverse of apply
        // order: NvAPI, then ramp, then VCP codes last-written-first.
        for ms in original.targets.iter().rev() {
            let Some((hmonitor, gdi_name)) = self.resolve(&current, ms) else {
                warn!(monitor = %ms.monitor.0, "monitor not attached; restore stays pending");
                first_err.get_or_insert_with(|| {
                    anyhow::anyhow!("monitor {} not attached; will retry", ms.monitor.0)
                });
                continue;
            };
            if let Some(nv) = &ms.nvapi {
                if let Err(e) = self.io.nv_restore(&gdi_name, nv) {
                    warn!(error = %e, "NvAPI restore failed");
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
    use relay_display::{ddc, gamma, nvapi};

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

        fn nv_read(&self, gdi_name: &str) -> Option<NvApiSnapshot> {
            let api = nvapi::NvApi::load()?;
            let d = api.display_by_gdi_name(gdi_name)?;
            let dvc = api.get_vibrance(&d).ok()?;
            let (hue, _default) = api.get_hue(&d).ok()?;
            Some(NvApiSnapshot {
                dvc: dvc.current,
                dvc_min: dvc.min,
                dvc_max: dvc.max,
                hue_deg: hue,
            })
        }

        fn nv_apply(&self, gdi_name: &str, vibrance_percent: i32, hue_deg: i32) -> Result<bool> {
            let Some(api) = nvapi::NvApi::load() else { return Ok(false) };
            let Some(d) = api.display_by_gdi_name(gdi_name) else { return Ok(false) };
            let dvc = api.get_vibrance(&d)?;
            let level = nvapi::vibrance_percent_to_dvc(vibrance_percent, dvc.min, dvc.max);
            api.set_vibrance(&d, level)?;
            api.set_hue(&d, hue_deg.rem_euclid(360))?;
            Ok(true)
        }

        fn nv_restore(&self, gdi_name: &str, snap: &NvApiSnapshot) -> Result<()> {
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

        fn monitors(&self) -> Vec<MonitorProbe> {
            crate::hardware::probe_win::probe_monitors(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MonitorId;
    use parking_lot::Mutex;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// In-memory display topology: per-monitor VCP registers, ramps, NvAPI
    /// state, plus failure injection and a write log.
    #[derive(Default)]
    struct FakeIo {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        /// hmonitor → (code → value)
        vcp: BTreeMap<i64, BTreeMap<u8, u32>>,
        /// gdi name → ramp
        ramps: BTreeMap<String, Ramp>,
        /// gdi name → nv state
        nv: BTreeMap<String, NvApiSnapshot>,
        monitors: Vec<MonitorProbe>,
        log: Vec<String>,
        /// Fail every VCP write after this many have succeeded.
        fail_writes_after: Option<usize>,
        writes_done: usize,
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
        }
    }

    impl FakeIo {
        fn two_monitors() -> Arc<Self> {
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
                s.nv.insert(
                    r"\\.\DISPLAY1".into(),
                    NvApiSnapshot { dvc: 0, dvc_min: 0, dvc_max: 63, hue_deg: 0 },
                );
                s.nv.insert(
                    r"\\.\DISPLAY2".into(),
                    NvApiSnapshot { dvc: 0, dvc_min: 0, dvc_max: 63, hue_deg: 0 },
                );
            }
            io
        }

        fn log(&self) -> Vec<String> {
            self.state.lock().log.clone()
        }

        fn snapshot_of(&self, hmonitor: i64) -> BTreeMap<u8, u32> {
            self.state.lock().vcp.get(&hmonitor).cloned().unwrap_or_default()
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

        fn nv_read(&self, gdi: &str) -> Option<NvApiSnapshot> {
            self.state.lock().nv.get(gdi).copied()
        }

        fn nv_apply(&self, gdi: &str, vibrance: i32, hue: i32) -> Result<bool> {
            let mut s = self.state.lock();
            let Some(nv) = s.nv.get(gdi).copied() else { return Ok(false) };
            let dvc =
                relay_display::nvapi::vibrance_percent_to_dvc(vibrance, nv.dvc_min, nv.dvc_max);
            s.log.push(format!("nv {gdi} dvc={dvc} hue={hue}"));
            let entry = s.nv.get_mut(gdi).unwrap();
            entry.dvc = dvc;
            entry.hue_deg = hue;
            Ok(true)
        }

        fn nv_restore(&self, gdi: &str, snap: &NvApiSnapshot) -> Result<()> {
            let mut s = self.state.lock();
            s.log.push(format!("nv-restore {gdi}"));
            *s.nv.get_mut(gdi).context("display gone")? = *snap;
            Ok(())
        }

        fn monitors(&self) -> Vec<MonitorProbe> {
            self.state.lock().monitors.clone()
        }
    }

    fn settings() -> DisplaySettings {
        let mut d = DisplaySettings::default();
        d.monitor.brightness = Some(80);
        d.monitor.contrast = Some(50);
        d.gpu.vibrance = 75;
        d.gpu.gamma = 1.2;
        d
    }

    fn target_a(io: &Arc<FakeIo>) -> MonitorProbe {
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
        assert!(ms.nvapi.is_some());

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
        assert!(ms.nvapi.is_none());
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
        assert!(snap.targets[0].nvapi.is_none());
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
