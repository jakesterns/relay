//! Original-state snapshot.
//!
//! Invariant: nothing on the machine is changed until the pre-change state has
//! been fsync'd to disk. If the core starts and finds a snapshot still marked
//! `applied`, it restores from it before doing anything else. That is the
//! crash / power-loss / reboot recovery path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profiles::write_atomic;
use crate::types::{GpuColor, MonitorId, MonitorSettings};

/// Audio-side state we need to put back. The APO is parameterised, so
/// "original" is simply "bypass"; we still record it so the shape can grow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AudioState {
    pub bypass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DisplayStateSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuColor>,
    #[serde(default)]
    pub monitors: Vec<(MonitorId, MonitorSettings)>,
    /// Raw pre-apply state per touched monitor (the real backend's format).
    /// Everything needed to put the monitor back without consulting the
    /// profile: exact VCP values, the exact gamma ramp, raw vendor levels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<MonitorStateSnapshot>,
}

/// Original state of one monitor, captured before any change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorStateSnapshot {
    /// Stable id — the restore key after a crash/reboot, when the volatile
    /// handles below have gone stale and must be re-resolved.
    pub monitor: MonitorId,
    /// GDI device name at capture time (best-effort fallback for restore).
    pub gdi_name: String,
    /// `HMONITOR` at capture time. Valid within the same session only.
    pub hmonitor: i64,
    /// (VCP code, original value), in apply order; restore walks in reverse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcp: Vec<(u8, u32)>,
    /// Exact pre-apply gamma ramp (preserves f.lux / Night Light curves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma: Option<relay_display::gamma::Ramp>,
    /// Raw GPU-colour state, when a vendor colour path was going to be
    /// touched. The serde name stays `nvapi` so a snapshot written by an
    /// older build — the one case where getting this wrong means *not*
    /// restoring someone's monitor after an upgrade — still loads.
    #[serde(default, rename = "nvapi", alias = "gpu", skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuColorSnapshot>,
}

/// Which vendor's colour API produced a snapshot. Restore must go back
/// through the same one: on a two-GPU machine the raw units are not
/// comparable, and a snapshot taken through NvAPI means nothing to ADL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    /// Also the default, so a pre-ADL snapshot (no `vendor` field) restores
    /// through NvAPI exactly as the build that wrote it would have.
    #[default]
    Nvidia,
    Amd,
}

/// Raw per-display vibrance/saturation + hue, in the vendor's own units.
///
/// One shape for both vendors because the *restore* contract is identical:
/// put these exact numbers back. The profile-unit mapping that produced them
/// is per-vendor and lives in `relay_display::{nvapi, amd}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuColorSnapshot {
    #[serde(default)]
    pub vendor: GpuVendor,
    /// NVIDIA: DVC level. AMD: saturation.
    pub dvc: i32,
    pub dvc_min: i32,
    pub dvc_max: i32,
    /// NVIDIA: hue angle 0..359. AMD: signed hue trim.
    pub hue_deg: i32,
    /// AMD only: the driver-reported hue range, needed because ADL rejects
    /// an out-of-range write — including, on restore, the original value if
    /// the range were assumed instead of recorded.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hue_min: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hue_max: i32,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub written_at_unix: u64,
    /// True from the moment we start changing things until restore completes.
    pub applied: bool,
    pub profile_id: Option<Uuid>,
    pub audio: AudioState,
    pub display: DisplayStateSnapshot,
}

const VERSION: u32 = 1;

impl Snapshot {
    pub fn new(profile_id: Uuid, audio: AudioState, display: DisplayStateSnapshot) -> Self {
        Self {
            version: VERSION,
            written_at_unix: now_unix(),
            applied: true,
            profile_id: Some(profile_id),
            audio,
            display,
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct BackupFile {
    path: PathBuf,
}

impl BackupFile {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist before any change. `write_atomic` fsyncs before the rename, so
    /// when this returns the snapshot is durable.
    pub fn write(&self, snapshot: &Snapshot) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        write_atomic(&self.path, &bytes).context("writing original-state snapshot")
    }

    /// A snapshot left with `applied = true` from a previous run, if any.
    pub fn pending(&self) -> Result<Option<Snapshot>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let snap: Snapshot = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", self.path.display()))?;
                Ok(if snap.applied { Some(snap) } else { None })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    /// Mark restored. We keep the file (with `applied = false`) as an audit trail
    /// of what the last original state looked like.
    pub fn clear(&self) -> Result<()> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let mut snap: Snapshot = serde_json::from_slice(&bytes)?;
                snap.applied = false;
                write_atomic(&self.path, &serde_json::to_vec_pretty(&snap)?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (PathBuf, BackupFile) {
        let dir = std::env::temp_dir().join(format!("relay-backup-{}", Uuid::new_v4()));
        (dir.clone(), BackupFile::at(dir.join("original-state.json")))
    }

    /// A snapshot written by a pre-ADL build must still restore. This is the
    /// one back-compat case that matters: get it wrong and someone upgrades
    /// mid-apply, the old snapshot fails to load, and their monitor stays
    /// dimmed with no way back.
    #[test]
    fn a_pre_adl_snapshot_still_loads_and_restores_through_nvapi() {
        let old = serde_json::json!({
            "monitor": "mon:GSM5C7C:402NTCZ9E219",
            "gdi_name": r"\\.\DISPLAY1",
            "hmonitor": 65537,
            "vcp": [[16, 100]],
            "nvapi": { "dvc": 13, "dvc_min": 0, "dvc_max": 63, "hue_deg": 0 }
        });
        let ms: MonitorStateSnapshot = serde_json::from_value(old).expect("old snapshot loads");
        let gpu = ms.gpu.expect("colour state survived the rename");
        assert_eq!(gpu.vendor, GpuVendor::Nvidia, "no vendor field means the NVIDIA path");
        assert_eq!(gpu.dvc, 13);
        assert_eq!((gpu.hue_min, gpu.hue_max), (0, 0));
    }

    /// And the round trip both ways for a current one.
    #[test]
    fn a_vendor_tagged_snapshot_round_trips() {
        for vendor in [GpuVendor::Nvidia, GpuVendor::Amd] {
            let ms = MonitorStateSnapshot {
                monitor: MonitorId("mon:X".into()),
                gdi_name: r"\\.\DISPLAY1".into(),
                hmonitor: 1,
                vcp: vec![(0x10, 40)],
                gamma: None,
                gpu: Some(GpuColorSnapshot {
                    vendor,
                    dvc: 120,
                    dvc_min: 0,
                    dvc_max: 200,
                    hue_deg: -5,
                    hue_min: -30,
                    hue_max: 30,
                }),
            };
            let text = serde_json::to_string(&ms).unwrap();
            assert!(text.contains("\"nvapi\""), "wire name is unchanged: {text}");
            let back: MonitorStateSnapshot = serde_json::from_str(&text).unwrap();
            assert_eq!(back, ms, "{vendor:?}");
        }
    }

    #[test]
    fn no_file_means_nothing_pending() {
        let (dir, b) = temp();
        assert!(b.pending().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn write_then_pending_then_clear() {
        let (dir, b) = temp();
        let snap = Snapshot::new(
            Uuid::new_v4(),
            AudioState { bypass: true },
            DisplayStateSnapshot {
                gpu: Some(GpuColor::default()),
                monitors: vec![],
                targets: vec![MonitorStateSnapshot {
                    monitor: MonitorId("mon:GSM5C7C:402NTCZ9E219".into()),
                    gdi_name: r"\\.\DISPLAY1".into(),
                    hmonitor: 0x10001,
                    vcp: vec![(0x10, 40), (0x12, 70)],
                    gamma: Some(relay_display::gamma::Ramp::identity()),
                    gpu: Some(GpuColorSnapshot {
                        vendor: GpuVendor::Nvidia,
                        dvc: 0,
                        dvc_min: 0,
                        dvc_max: 63,
                        hue_deg: 0,
                        hue_min: 0,
                        hue_max: 0,
                    }),
                }],
            },
        );
        b.write(&snap).unwrap();
        let pending = b.pending().unwrap().expect("pending after write");
        assert_eq!(pending.display.gpu, Some(GpuColor::default()));
        assert_eq!(pending.display.targets, snap.display.targets, "raw target state round-trips");
        b.clear().unwrap();
        assert!(b.pending().unwrap().is_none(), "cleared snapshot is not pending");
        assert!(b.path().exists(), "audit trail kept");
        let _ = std::fs::remove_dir_all(dir);
    }
}
