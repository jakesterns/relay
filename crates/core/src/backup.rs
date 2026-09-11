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
    /// profile: exact VCP values, the exact gamma ramp, raw NvAPI levels.
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
    /// Raw NvAPI state, when the NVIDIA path was going to be touched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvapi: Option<NvApiSnapshot>,
}

/// Raw NvAPI digital-vibrance + hue state (vendor units, not profile units).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvApiSnapshot {
    pub dvc: i32,
    pub dvc_min: i32,
    pub dvc_max: i32,
    pub hue_deg: i32,
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
                    nvapi: Some(NvApiSnapshot { dvc: 0, dvc_min: 0, dvc_max: 63, hue_deg: 0 }),
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
