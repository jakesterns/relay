//! Hardware library and probe: what is plugged in, and what the user calls it.
//!
//! Two layers, deliberately separate:
//!
//! - **Identity** — stable keys derived from the hardware itself, never from
//!   bus position. A [`MonitorId`] comes only from EDID bytes (manufacturer,
//!   product, serial), so the same panel yields the same id on any port, any
//!   output, any reboot. An *endpoint key* for audio prefers the device
//!   container GUID (stable across USB ports for devices that expose a serial)
//!   and falls back to the endpoint id string.
//! - **Library** — user-named entries in `hardware.json`. A headset is a user
//!   object (`hd560s`, "HD 560S") *bound to* one or more endpoint keys,
//!   because Windows only sees the DAC/interface, not the headphones plugged
//!   into it. Profiles reference library ids, so re-binding a headset to a new
//!   endpoint never touches any profile.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::{HeadsetId, MonitorId};

pub mod autoeq;
pub mod ddc;
pub mod edid;
#[cfg(windows)]
pub mod probe_win;
#[cfg(windows)]
pub mod watch_win;

/// What is plugged in right now, reduced to what profile selection needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConnectedHardware {
    /// The default render endpoint's known headset, if it is in the library.
    pub headset: Option<HeadsetId>,
    /// All attached monitors, primary first.
    pub monitors: Vec<MonitorId>,
}

impl ConnectedHardware {
    pub fn has_monitor(&self, id: &MonitorId) -> bool {
        self.monitors.iter().any(|m| m == id)
    }
}

/// One active WASAPI render endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointInfo {
    /// Stable key: `ep:c:<container-guid>` or `ep:d:<endpoint-id>`.
    /// See [`endpoint_key`].
    pub key: String,
    /// Friendly name, e.g. "Speakers (USB Audio 2.0)".
    pub name: String,
    /// This is the current default render endpoint.
    pub default: bool,
}

/// One attached monitor as seen by the probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorProbe {
    /// EDID-derived stable id; see [`edid::monitor_id`].
    pub id: MonitorId,
    /// Display name from the EDID descriptor, e.g. "LG ULTRAGEAR+".
    pub name: String,
    /// Native resolution from the preferred timing descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_hz: Option<f32>,
    pub primary: bool,
    /// The `HMONITOR` this panel currently maps to (consumed by M2 for
    /// per-monitor apply). Volatile: valid only until the next display change.
    pub hmonitor: i64,
    /// Parsed DDC/CI VCP code list, filled by the full probe only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddc: Option<Vec<u8>>,
}

/// Everything one probe pass saw. `ConnectedHardware` is derived from this
/// plus the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProbeReport {
    /// Active render endpoints; the default one is flagged.
    pub endpoints: Vec<EndpointInfo>,
    /// Attached monitors, primary first.
    pub monitors: Vec<MonitorProbe>,
}

impl ProbeReport {
    pub fn default_endpoint(&self) -> Option<&EndpointInfo> {
        self.endpoints.iter().find(|e| e.default)
    }
}

/// The connected-hardware block inside [`crate::types::CoreState`]: the last
/// probe plus which library headset the default endpoint resolved to. This is
/// what drives the UI's Plugged / Main / Second pills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HardwareView {
    pub endpoints: Vec<EndpointInfo>,
    /// Primary first, mirroring [`ProbeReport::monitors`].
    pub monitors: Vec<MonitorProbe>,
    /// Library headset bound to the default endpoint, if any.
    pub headset: Option<HeadsetId>,
}

impl HardwareView {
    pub fn from_report(report: ProbeReport, library: &HardwareStore) -> Self {
        let headset = library.connected(&report).headset;
        Self { endpoints: report.endpoints, monitors: report.monitors, headset }
    }
}

/// Source of truth for connected hardware. The Windows implementation reads
/// WASAPI endpoints and EDID; the no-op version reports nothing connected so
/// only "Any" profiles match.
pub trait HardwareProbe: Send + Sync {
    /// Enumerate endpoints and monitors. `with_ddc` additionally queries each
    /// monitor's DDC/CI capabilities string (slow — an explicit user action,
    /// never the device-change fast path).
    fn probe(&self, with_ddc: bool) -> ProbeReport;
}

#[derive(Debug, Default)]
pub struct NoopHardwareProbe;

impl HardwareProbe for NoopHardwareProbe {
    fn probe(&self, _with_ddc: bool) -> ProbeReport {
        ProbeReport::default()
    }
}

/// Build the stable endpoint key. The container GUID groups every endpoint of
/// one physical device and — for USB hardware that exposes a serial number —
/// stays the same on any port. Serial-less devices get a fresh container per
/// port; nothing in the OS is stable for those, so we fall back to the
/// endpoint id string and the user re-binds if the id ever changes.
/// `container` must be lowercase hyphenated GUID text without braces.
pub fn endpoint_key(container: Option<&str>, endpoint_id: &str) -> String {
    /// Windows puts bus-internal devices in this well-known "local system
    /// device" container; it does not identify any one device.
    const NULL_CONTAINER: &str = "00000000-0000-0000-ffff-ffffffffffff";
    match container {
        Some(c)
            if !c.is_empty()
                && c != NULL_CONTAINER
                && c != "00000000-0000-0000-0000-000000000000" =>
        {
            format!("ep:c:{c}")
        }
        _ => format!("ep:d:{endpoint_id}"),
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HeadsetKind {
    #[default]
    Headphone,
    Iem,
    Speakers,
}

/// A user-named headset/IEM. `endpoints` are the endpoint keys this headset is
/// reachable through (a headset may be bound to both a USB DAC and onboard
/// 3.5 mm). It is "connected" when the default endpoint's key is bound here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headset {
    pub id: HeadsetId,
    pub name: String,
    #[serde(default)]
    pub kind: HeadsetKind,
    /// Measured response, (Hz, dB) ascending. From the AutoEQ importer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<(f32, f32)>>,
    /// Where the curve came from, e.g. "oratory1990" or "manual".
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: MonitorId,
    pub name: String,
    /// Panel type, e.g. "Nano IPS", free text.
    #[serde(default)]
    pub panel: String,
    /// VCP codes the monitor advertised over DDC/CI, once probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddcci: Option<Vec<u8>>,
}

/// An audio interface (Scarlett, RØDECaster…) the user wants tracked; used by
/// the DAW share preset later. Identity reuses endpoint keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInterface {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HardwareFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    headsets: Vec<Headset>,
    #[serde(default)]
    monitors: Vec<Monitor>,
    #[serde(default)]
    interfaces: Vec<AudioInterface>,
}

const FILE_VERSION: u32 = 1;

/// `hardware.json` on disk. Same shape as [`crate::profiles::ProfileStore`].
#[derive(Debug, Default)]
pub struct HardwareStore {
    path: PathBuf,
    pub headsets: Vec<Headset>,
    pub monitors: Vec<Monitor>,
    pub interfaces: Vec<AudioInterface>,
}

impl HardwareStore {
    /// Load from disk; a missing file is an empty library, not an error.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<HardwareFile>(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HardwareFile::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            headsets: file.headsets,
            monitors: file.monitors,
            interfaces: file.interfaces,
        })
    }

    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let file = HardwareFile {
            version: FILE_VERSION,
            headsets: self.headsets.clone(),
            monitors: self.monitors.clone(),
            interfaces: self.interfaces.clone(),
        };
        crate::profiles::write_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn upsert_headset(&mut self, headset: Headset) {
        match self.headsets.iter_mut().find(|h| h.id == headset.id) {
            Some(slot) => *slot = headset,
            None => self.headsets.push(headset),
        }
    }

    pub fn upsert_monitor(&mut self, monitor: Monitor) {
        match self.monitors.iter_mut().find(|m| m.id == monitor.id) {
            Some(slot) => *slot = monitor,
            None => self.monitors.push(monitor),
        }
    }

    /// Remove whatever carries this id, headset or monitor. True if removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.headsets.len() + self.monitors.len() + self.interfaces.len();
        self.headsets.retain(|h| h.id.0 != id);
        self.monitors.retain(|m| m.id.0 != id);
        self.interfaces.retain(|i| i.id != id);
        before != self.headsets.len() + self.monitors.len() + self.interfaces.len()
    }

    pub fn headset(&self, id: &HeadsetId) -> Option<&Headset> {
        self.headsets.iter().find(|h| &h.id == id)
    }

    pub fn headset_mut(&mut self, id: &HeadsetId) -> Option<&mut Headset> {
        self.headsets.iter_mut().find(|h| &h.id == id)
    }

    /// The library headset bound to this endpoint key, if any.
    pub fn headset_for_endpoint(&self, key: &str) -> Option<&Headset> {
        self.headsets.iter().find(|h| h.endpoints.iter().any(|e| e == key))
    }

    /// Reduce a probe report to what profile selection consumes.
    pub fn connected(&self, report: &ProbeReport) -> ConnectedHardware {
        let headset = report
            .default_endpoint()
            .and_then(|ep| self.headset_for_endpoint(&ep.key))
            .map(|h| h.id.clone());
        ConnectedHardware {
            headset,
            monitors: report.monitors.iter().map(|m| m.id.clone()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headset(id: &str, endpoints: &[&str]) -> Headset {
        Headset {
            id: HeadsetId(id.into()),
            name: id.to_uppercase(),
            kind: HeadsetKind::Headphone,
            curve: None,
            source: String::new(),
            endpoints: endpoints.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn endpoint_key_prefers_container_and_ignores_null_containers() {
        let ep_id = "{0.0.0.00000000}.{9d47ae05-2c1c-4b0b-a672-3a25a2c1bc2e}";
        // Same container reported for the endpoint on two different USB ports
        // (different endpoint ids) → same key.
        let k1 = endpoint_key(Some("8b1a7a46-04b0-11ee-9cbd-806e6f6e6963"), ep_id);
        let k2 =
            endpoint_key(Some("8b1a7a46-04b0-11ee-9cbd-806e6f6e6963"), "{0.0.0.00000000}.{other}");
        assert_eq!(k1, k2);
        assert_eq!(k1, "ep:c:8b1a7a46-04b0-11ee-9cbd-806e6f6e6963");
        // The well-known local-system container identifies nothing: fall back.
        let k3 = endpoint_key(Some("00000000-0000-0000-ffff-ffffffffffff"), ep_id);
        assert_eq!(k3, format!("ep:d:{ep_id}"));
        assert_eq!(endpoint_key(None, ep_id), format!("ep:d:{ep_id}"));
    }

    #[test]
    fn headset_binding_resolves_connected_headset() {
        let mut store = HardwareStore::in_memory();
        store.upsert_headset(headset("hd560s", &["ep:c:aaaa"]));
        store.upsert_headset(headset("blessing3", &["ep:c:bbbb", "ep:d:{x}.{y}"]));

        let report = ProbeReport {
            endpoints: vec![
                EndpointInfo { key: "ep:c:cccc".into(), name: "HDMI".into(), default: false },
                EndpointInfo { key: "ep:c:bbbb".into(), name: "Dongle".into(), default: true },
            ],
            monitors: vec![],
        };
        let hw = store.connected(&report);
        assert_eq!(hw.headset, Some(HeadsetId("blessing3".into())));

        // Default moves to an unbound endpoint → no headset connected.
        let report2 = ProbeReport {
            endpoints: vec![EndpointInfo {
                key: "ep:c:cccc".into(),
                name: "HDMI".into(),
                default: true,
            }],
            monitors: vec![],
        };
        assert_eq!(store.connected(&report2).headset, None);
    }

    #[test]
    fn store_round_trips_and_deletes_by_bare_id() {
        let dir = std::env::temp_dir().join(format!("relay-hwtest-{}", uuid::Uuid::new_v4()));
        let path = dir.join("hardware.json");
        let mut store = HardwareStore::load(&path).unwrap();
        let mut h = headset("hd560s", &["ep:c:aaaa"]);
        h.curve = Some(vec![(20.0, -4.11), (20000.0, -6.0)]);
        h.source = "oratory1990".into();
        store.upsert_headset(h);
        store.upsert_monitor(Monitor {
            id: MonitorId("mon:GSM5C7C:402NTCZ9E219".into()),
            name: "LG ULTRAGEAR+".into(),
            panel: "Nano IPS".into(),
            ddcci: Some(vec![0x10, 0x12, 0x60]),
        });
        store.save().unwrap();

        let mut again = HardwareStore::load(&path).unwrap();
        assert_eq!(again.headsets.len(), 1);
        assert_eq!(again.headsets[0].curve.as_ref().unwrap().len(), 2);
        assert_eq!(again.monitors[0].ddcci, Some(vec![0x10, 0x12, 0x60]));
        assert!(again.remove("hd560s"));
        assert!(!again.remove("hd560s"));
        assert_eq!(again.headsets.len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
