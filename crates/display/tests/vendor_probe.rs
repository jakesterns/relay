//! Human-in-the-loop verification of vendor VCP opcodes.
//!
//! This is the *only* way a candidate in `vcp::QUIRKS` becomes
//! `Evidence::Osd`. It writes one vendor-reserved opcode at a time, holds each
//! value long enough for a person to read the monitor's own OSD, then puts the
//! original value back. Nothing here changes the table — you watch, you report,
//! and someone edits `QUIRKS` by hand.
//!
//! Set-and-read-back is deliberately *not* treated as proof: a panel will
//! store and return a value for a control whose on-screen meaning is something
//! else entirely. Only the OSD counts.
//!
//! Run (the env var is required — there is no default sweep):
//!
//! ```text
//! $env:RELAY_VCP_PROBE = "F5:1|2|3|4"
//! cargo test -p relay-display --test vendor_probe -- --ignored --nocapture
//! ```
//!
//! Syntax: comma-separated `CODE:v1|v2|…`, hex code, decimal values.
//! Open the monitor's OSD on the page you want to watch *before* starting;
//! many panels NAK DDC/CI writes while the OSD menu is being navigated, so
//! park it on the settings page and keep your hands off the joystick.
//!
//! Guard rails:
//! - only 0xE0–0xFF (the vendor-reserved range) may be probed, so a typo
//!   cannot land on 0x04 "restore factory defaults";
//! - the original value is read first, and a code that cannot be read is
//!   skipped — without a read there is no restore;
//! - the original is written back after every code, including on panic.

#![cfg(windows)]

use std::time::Duration;

use relay_display::ddc::PhysicalMonitor;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};

/// Lowest opcode this harness will touch. Everything below is standard MCCS
/// and includes the factory-reset codes.
const VENDOR_RANGE_START: u8 = 0xE0;

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

/// `F5:1|2|3|4,F6:0|1|2` → [(0xF5, [1,2,3,4]), (0xF6, [0,1,2])].
fn parse_spec(spec: &str) -> Result<Vec<(u8, Vec<u32>)>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (code, values) =
            part.split_once(':').ok_or_else(|| format!("{part:?}: want CODE:v|v"))?;
        let code = u8::from_str_radix(code.trim().trim_start_matches("0x"), 16)
            .map_err(|e| format!("{code:?}: {e}"))?;
        if code < VENDOR_RANGE_START {
            return Err(format!(
                "0x{code:02X} is below the vendor-reserved range; this harness only probes \
                 0x{VENDOR_RANGE_START:02X}-0xFF"
            ));
        }
        let values = values
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|v| v.parse::<u32>().map_err(|e| format!("{v:?}: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        if values.is_empty() {
            return Err(format!("0x{code:02X}: no values to sweep"));
        }
        out.push((code, values));
    }
    if out.is_empty() {
        return Err("nothing to probe".into());
    }
    Ok(out)
}

/// Puts the original value back when the sweep ends, however it ends.
struct Restore<'a> {
    pm: &'a PhysicalMonitor,
    code: u8,
    original: u32,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        match self.pm.set_vcp(self.code, self.original) {
            Ok(()) => println!("   restored 0x{:02X} = {}", self.code, self.original),
            Err(e) => println!("   !! RESTORE FAILED for 0x{:02X}: {e}", self.code),
        }
    }
}

#[test]
#[ignore = "writes vendor VCP codes on the live monitor; run by hand while watching the OSD"]
fn probe_vendor_codes_while_watching_the_osd() {
    let Ok(spec) = std::env::var("RELAY_VCP_PROBE") else {
        println!(
            "RELAY_VCP_PROBE is not set, so nothing was written.\n\
             Set it to a sweep, e.g. RELAY_VCP_PROBE=\"F5:1|2|3|4\", and re-run."
        );
        return;
    };
    let plan = parse_spec(&spec).expect("RELAY_VCP_PROBE");
    let dwell = Duration::from_millis(
        std::env::var("RELAY_VCP_DWELL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000),
    );
    let only = std::env::var("RELAY_VCP_GDI").ok();

    for (hmon, gdi) in monitors() {
        if only.as_deref().is_some_and(|want| want != gdi) {
            continue;
        }
        println!("\n== {gdi} (HMONITOR {hmon:#x})");
        let pm = match PhysicalMonitor::open(hmon, 80) {
            Ok(pm) => pm,
            Err(e) => {
                println!("   DDC/CI unavailable: {e}");
                continue;
            }
        };

        for (code, values) in &plan {
            let (original, max) = match pm.get_vcp(*code) {
                Ok(v) => v,
                Err(e) => {
                    println!("\n-- 0x{code:02X}: unreadable ({e}) — skipped, no restore possible");
                    continue;
                }
            };
            println!("\n-- 0x{code:02X}: current {original}, max {max}");
            println!("   watch the OSD. Which setting moves, and to what?");
            let _restore = Restore { pm: &pm, code: *code, original };
            for value in values {
                match pm.set_vcp(*code, *value) {
                    Ok(()) => println!("   0x{code:02X} = {value}   (holding {:?})", dwell),
                    Err(e) => {
                        println!("   0x{code:02X} = {value}   WRITE FAILED: {e}");
                        continue;
                    }
                }
                std::thread::sleep(dwell);
            }
        }
    }

    println!(
        "\nReport back per code: the OSD label that changed and which value maps to which\n\
         level. That becomes an `Evidence::Osd {{ observer, date, monitor }}` row in\n\
         crates/display/src/vcp.rs. A code whose OSD effect you could not see stays\n\
         `Evidence::Unverified` — it is not a failure, it is the honest answer."
    );
}

#[test]
fn spec_parsing_rejects_standard_and_malformed_codes() {
    assert_eq!(parse_spec("F5:1|2|3|4").unwrap(), vec![(0xF5, vec![1, 2, 3, 4])]);
    assert_eq!(
        parse_spec("0xF6:0|1|2, FA:0|1").unwrap(),
        vec![(0xF6, vec![0, 1, 2]), (0xFA, vec![0, 1])]
    );
    // Standard codes are out of reach: 0x04 is "restore factory defaults".
    assert!(parse_spec("04:1").is_err());
    assert!(parse_spec("10:50").is_err());
    assert!(parse_spec("F5").is_err());
    assert!(parse_spec("F5:").is_err());
    assert!(parse_spec("ZZ:1").is_err());
    assert!(parse_spec("").is_err());
}
