//! Read-only live inspection of this PC's displays. Ignored by default; run
//! by hand to record the pre-M2 state in the plan file:
//!
//! `cargo test -p relay-display --test live_read -- --ignored --nocapture`
//!
//! Changes nothing: only Get* calls.

#![cfg(windows)]

use relay_display::{ddc, gamma, nvapi, vcp};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};

fn monitors() -> Vec<(i64, String)> {
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        let out = unsafe { &mut *(lparam.0 as *mut Vec<(i64, String)>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
            let end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            out.push((hmon.0 as i64, String::from_utf16_lossy(&info.szDevice[..end])));
        }
        true.into()
    }
    let mut out = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

#[test]
#[ignore = "reads the live monitor/GPU state; run by hand"]
fn live_read_current_state() {
    for (hmon, gdi) in monitors() {
        println!("== {gdi} (HMONITOR {hmon:#x})");

        match ddc::PhysicalMonitor::open(hmon, 60) {
            Ok(pm) => {
                for (label, code) in [
                    ("brightness 0x10", vcp::BRIGHTNESS),
                    ("contrast   0x12", vcp::CONTRAST),
                    ("sharpness  0x87", vcp::SHARPNESS),
                ] {
                    match pm.get_vcp(code) {
                        Ok((cur, max)) => println!("  {label}: {cur} / {max}"),
                        Err(e) => println!("  {label}: {e}"),
                    }
                }
            }
            Err(e) => println!("  DDC/CI: {e}"),
        }

        match gamma::io::get_ramp(&gdi) {
            Ok(ramp) => {
                let id = ramp == gamma::Ramp::identity();
                println!(
                    "  gamma ramp: identity={id} r[0]={} r[128]={} r[255]={}",
                    ramp.r[0], ramp.r[128], ramp.r[255]
                );
            }
            Err(e) => println!("  gamma ramp: {e}"),
        }
    }

    match nvapi::NvApi::load() {
        Some(api) => {
            for d in api.displays() {
                let dvc = api.get_vibrance(&d);
                let hue = api.get_hue(&d);
                println!("== NvAPI {}: dvc={dvc:?} hue={hue:?}", d.gdi_name);
            }
        }
        None => println!("== NvAPI unavailable"),
    }
}

/// Read-only ADL probe. Separate from the NVIDIA one because on a machine
/// with both GPUs (this one) the interesting answer is *which* displays each
/// vendor claims — an AMD adapter with nothing plugged into it must report no
/// displays, not an empty-but-usable one.
///
/// `cargo test -p relay-display --test live_read -- --ignored --nocapture`
#[test]
#[ignore = "reads the live AMD driver state; run by hand"]
fn live_read_amd_state() {
    use relay_display::amd;

    let Some(adl) = amd::Adl::load() else {
        println!("== ADL unavailable (no AMD driver, or it refused to start)");
        return;
    };
    for line in adl.adapter_summary() {
        println!("  {line}");
    }
    let displays = adl.displays();
    println!("== ADL loaded; {} AMD-driven display(s)", displays.len());
    for d in &displays {
        println!("  {} on adapter {} ({})", d.gdi_name, d.adapter_index, d.adapter_name);
        println!("    saturation: {:?}", adl.get_saturation(d));
        println!("    hue:        {:?}", adl.get_hue(d));
    }
}
