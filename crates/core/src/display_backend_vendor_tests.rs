//! AMD-side coverage of the `DisplayIo` seam.
//!
//! The point of these is parity: whatever the vendor, capture → apply →
//! restore must leave the machine exactly as it was found, and only the
//! target monitor may ever move. They run against the same two-monitor fake
//! the NVIDIA tests use, driven by a vendor-tagged colour state — because on
//! this development machine no AMD GPU drives a display (see
//! `docs/plans/M2-display.md`), so fixtures are the honest limit.

use super::tests::{amd_state, nvidia_state, settings, target_a, FakeIo};
use super::*;
use crate::backup::GpuVendor;
use std::collections::BTreeMap;

/// Every invariant M2 proved on NVIDIA, re-proved on AMD from the same
/// assertions — so the two backends cannot quietly diverge.
#[test]
fn apply_and_restore_round_trip_identically_on_both_vendors() {
    for gpu in [nvidia_state(), amd_state()] {
        let vendor = gpu.vendor;
        let io = FakeIo::two_monitors_with(gpu);
        let adapter = DisplayAdapter::with_io(io.clone());
        let t = target_a(&io);
        let before_a = io.snapshot_of(1);
        let before_b = io.snapshot_of(2);
        let before_gpu = io.gpu_of(r"\\.\DISPLAY1");

        let snap = adapter.capture(Some(&t), &settings()).unwrap();
        assert_eq!(snap.targets.len(), 1, "{vendor:?}");
        assert_eq!(snap.targets[0].gpu.unwrap().vendor, vendor, "vendor recorded");
        assert_eq!(snap.targets[0].vcp, vec![(0x10, 40), (0x12, 70)], "{vendor:?}");

        let via = adapter.apply(Some(&t), &settings()).unwrap();
        assert!(via.ddcci && via.gamma, "{vendor:?}");
        match vendor {
            GpuVendor::Nvidia => assert!(via.nvapi && !via.amd),
            GpuVendor::Amd => assert!(via.amd && !via.nvapi),
        }
        assert_ne!(io.gpu_of(r"\\.\DISPLAY1").dvc, before_gpu.dvc, "colour moved");
        assert_eq!(io.snapshot_of(2), before_b, "monitor B untouched ({vendor:?})");

        adapter.restore(&snap).unwrap();
        assert_eq!(io.snapshot_of(1), before_a, "VCP back ({vendor:?})");
        assert_eq!(io.gpu_of(r"\\.\DISPLAY1"), before_gpu, "colour back ({vendor:?})");
        assert_eq!(io.snapshot_of(2), before_b, "monitor B untouched ({vendor:?})");
    }
}

/// The crash/reboot path: the snapshot is all that is left, and it must
/// restore through the vendor that wrote it — the fake's `gpu_restore`
/// asserts the vendor matches — even after the handles have gone stale.
#[test]
fn amd_restore_re_resolves_stale_handles_by_stable_id() {
    let io = FakeIo::two_monitors_with(amd_state());
    let adapter = DisplayAdapter::with_io(io.clone());
    let t = target_a(&io);
    let snap = adapter.capture(Some(&t), &settings()).unwrap();
    adapter.apply(Some(&t), &settings()).unwrap();

    // Reboot: same panel, new HMONITOR and new GDI name.
    io.rename_display_1(9, r"\\.\DISPLAY3");

    adapter.restore(&snap).unwrap();
    assert_eq!(io.snapshot_of(9), BTreeMap::from([(0x10, 40), (0x12, 70)]));
    assert_eq!(io.gpu_of(r"\\.\DISPLAY3"), amd_state(), "AMD colour back after a reboot");
}

/// The documented divergence, asserted at the seam rather than only in the
/// mapping unit test: the same profile on the same rig, one GPU each.
#[test]
fn a_desaturating_profile_moves_amd_and_is_reported_on_nvidia() {
    let mut d = DisplaySettings::default();
    d.gpu.vibrance = 20;

    let amd_io = FakeIo::two_monitors_with(amd_state());
    let amd_adapter = DisplayAdapter::with_io(amd_io.clone());
    let t = target_a(&amd_io);
    let via = amd_adapter.apply(Some(&t), &d).unwrap();
    assert!(via.amd);
    assert!(via.unsupported.is_empty(), "AMD honours it: {:?}", via.unsupported);
    assert_eq!(amd_io.gpu_of(r"\\.\DISPLAY1").dvc, 40, "saturation pulled below the default");

    let nv_io = FakeIo::two_monitors_with(nvidia_state());
    let nv_adapter = DisplayAdapter::with_io(nv_io.clone());
    let t = target_a(&nv_io);
    let via = nv_adapter.apply(Some(&t), &d).unwrap();
    assert!(via.nvapi);
    assert_eq!(nv_io.gpu_of(r"\\.\DISPLAY1").dvc, 0, "NVIDIA stays neutral");
    assert!(
        via.unsupported.iter().any(|u| u.starts_with("vibrance")),
        "and the UI is told why: {:?}",
        via.unsupported
    );
}

/// AMD hue is a narrow signed trim, so a large angle is clamped — and said
/// so, rather than silently under-delivering.
#[test]
fn a_large_hue_angle_is_clamped_on_amd_and_reported() {
    let mut d = DisplaySettings::default();
    d.gpu.hue_deg = 90;
    let io = FakeIo::two_monitors_with(amd_state());
    let adapter = DisplayAdapter::with_io(io.clone());
    let t = target_a(&io);
    let via = adapter.apply(Some(&t), &d).unwrap();
    assert!(via.amd);
    assert_eq!(io.gpu_of(r"\\.\DISPLAY1").hue_deg, 30);
    assert!(
        via.unsupported.iter().any(|u| u.starts_with("hue (AMD trims")),
        "got {:?}",
        via.unsupported
    );
}

/// Neither vendor: an Intel iGPU, a VM, a remote session. The gamma ramp and
/// DDC/CI still carry what they can, the vendor-only fields report
/// unsupported, and nothing panics — this is the path most machines that are
/// not this desk will take.
#[test]
fn neither_vendor_degrades_to_unsupported_and_never_panics() {
    let io = FakeIo::two_monitors();
    io.clear_gpu_vendors();
    let adapter = DisplayAdapter::with_io(io.clone());
    let t = target_a(&io);

    let mut d = settings();
    d.gpu.hue_deg = 15;
    let snap = adapter.capture(Some(&t), &d).unwrap();
    assert!(snap.targets[0].gpu.is_none(), "nothing captured that cannot be restored");

    let via = adapter.apply(Some(&t), &d).unwrap();
    assert!(via.gamma && via.ddcci, "the vendor-neutral paths still carry it");
    assert!(!via.nvapi && !via.amd);
    assert_eq!(via.unsupported, vec!["vibrance".to_string(), "hue".to_string()]);

    // And restoring a snapshot with no vendor state is a clean no-op.
    adapter.restore(&snap).unwrap();
}

/// A GPU-colour-only profile must not open a DDC/CI transaction: that is the
/// ~100 ms of the 144 ms restore measured in M2, and AMD machines should not
/// pay it either.
#[test]
fn gpu_only_profiles_never_touch_the_monitor_over_ddc() {
    let io = FakeIo::two_monitors_with(amd_state());
    let adapter = DisplayAdapter::with_io(io.clone());
    let t = target_a(&io);
    let mut d = DisplaySettings::default();
    d.gpu.vibrance = 80;
    let snap = adapter.capture(Some(&t), &d).unwrap();
    adapter.apply(Some(&t), &d).unwrap();
    adapter.restore(&snap).unwrap();
    assert!(io.log().iter().all(|l| !l.starts_with("vcp")), "no DDC traffic: {:?}", io.log());
}
