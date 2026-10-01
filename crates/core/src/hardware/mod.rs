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
pub use listening::{EndpointListening, ListeningDevice};

pub mod autoeq;
pub mod catalog;
pub mod ddc;
pub mod edid;
pub mod edid_color;
pub mod listening;
pub mod other_processing;
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
    /// The MMDevices endpoint GUID (`{...}`, the last brace group of the
    /// endpoint id): where its FX property store lives. Read-only use (S41).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fx_guid: String,
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
    /// GDI device name (`\\.\DISPLAY1`) for gamma-ramp DCs and NvAPI display
    /// matching. Volatile, like `hmonitor`.
    #[serde(default)]
    pub gdi_name: String,
    /// Parsed DDC/CI VCP code list, filled by the full probe only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddc: Option<Vec<u8>>,
    /// What the panel reports about its own colour: primaries, white point,
    /// gamma, bit depth, advertised colorimetry and HDR formats. Read from
    /// the same EDID blob the id comes from, so it costs nothing extra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<edid_color::ColorInfo>,
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
    /// What each output feeds, as the user described it (S41). Keyed by
    /// [`listening::listening_key`].
    #[serde(default)]
    pub listening: Vec<EndpointListening>,
    /// The default output's listening key, so the UI and the tray know which
    /// list is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_listening: Option<String>,
    /// The default output's active listening device, resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_listening: Option<ListeningDevice>,
    /// Other processing seen on each output: third-party APOs in its FX
    /// store and known vendor audio software running (read-only scan).
    #[serde(default)]
    pub other_processing: Vec<other_processing::EndpointProcessing>,
}

impl HardwareView {
    pub fn from_report(mut report: ProbeReport, library: &HardwareStore) -> Self {
        listening::unique_endpoint_keys(&mut report.endpoints);
        let headset = library.connected(&report).headset;
        let default_listening = listening::default_listening_key(&report);
        let active_listening = default_listening
            .as_deref()
            .and_then(|k| library.listening_for(k))
            .and_then(|l| l.active().cloned());
        Self {
            endpoints: report.endpoints,
            monitors: report.monitors,
            headset,
            listening: library.listening.clone(),
            default_listening,
            active_listening,
            other_processing: Vec::new(),
        }
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

/// The MMDevices GUID of an endpoint id (`{0.0.0.00000000}.{guid}` gives `{guid}`),
/// the name of its key under the MMDevices render root. Empty when the id
/// has no brace group.
pub fn fx_guid_of(endpoint_id: &str) -> String {
    match endpoint_id.rfind('{') {
        Some(i) if endpoint_id.ends_with('}') => endpoint_id[i..].to_owned(),
        _ => String::new(),
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
    /// Colour characteristics from the panel's EDID, cached at probe time so
    /// the library screen can show them without a re-probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<edid_color::ColorInfo>,
}

/// Which vendor-private DDC/CI controls one monitor actually has, decided by
/// the core from `relay_display::vcp::QUIRKS` and the advertised opcode list.
///
/// The client is told, never asked: the quirks table and its evidence live in
/// Rust, so a UI that got this wrong could enable a slider over an unverified
/// opcode. `response` is empty when the control is unavailable, otherwise it
/// lists exactly the level names the verified value map covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorVendorControls {
    pub monitor: MonitorId,
    pub black_equalizer: bool,
    #[serde(default)]
    pub response: Vec<String>,
}

impl MonitorVendorControls {
    pub fn resolve(id: &MonitorId, advertised: Option<&[u8]>) -> Self {
        let quirks = relay_display::vcp::quirks_for(&id.0);
        let controls = relay_display::vcp::vendor_controls(&quirks, advertised);
        Self {
            monitor: id.clone(),
            black_equalizer: controls.black_equalizer,
            response: controls.response.into_iter().map(str::to_owned).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.black_equalizer && self.response.is_empty()
    }
}

/// Vendor controls for every monitor the client knows about — the library
/// entries plus anything currently attached that is not in the library yet.
/// Monitors with nothing to offer are omitted, so an empty list means "no
/// verified vendor opcodes anywhere", which is the shipping state today.
pub fn vendor_controls(
    library: &[Monitor],
    connected: &[MonitorProbe],
) -> Vec<MonitorVendorControls> {
    let mut out: Vec<MonitorVendorControls> = Vec::new();
    let from_library = library.iter().map(|m| (&m.id, m.ddcci.as_deref()));
    let from_connected = connected.iter().map(|m| (&m.id, m.ddc.as_deref()));
    for (id, advertised) in from_library.chain(from_connected) {
        if out.iter().any(|c| &c.monitor == id) {
            continue;
        }
        let controls = MonitorVendorControls::resolve(id, advertised);
        if !controls.is_empty() {
            out.push(controls);
        }
    }
    out
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
    #[serde(default)]
    listening: Vec<EndpointListening>,
}

const FILE_VERSION: u32 = 1;

/// `hardware.json` on disk. Same shape as [`crate::profiles::ProfileStore`].
#[derive(Debug, Default)]
pub struct HardwareStore {
    path: PathBuf,
    pub headsets: Vec<Headset>,
    pub monitors: Vec<Monitor>,
    pub interfaces: Vec<AudioInterface>,
    /// Per output: what the user listens on through it (S41).
    pub listening: Vec<EndpointListening>,
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
            listening: file.listening,
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
            listening: self.listening.clone(),
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
        // A removed headset cannot stay listed as connected to anything.
        for l in &mut self.listening {
            let keep: Vec<ListeningDevice> = l
                .devices
                .iter()
                .filter(|d| d.headset().is_none_or(|h| h.0 != id))
                .cloned()
                .collect();
            l.set_devices(keep);
        }
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
        // M1 bindings carry the device key; an endpoint key may add `#<endpoint>`.
        let base = listening::base_key(key);
        self.headsets
            .iter()
            .find(|h| h.endpoints.iter().any(|e| e == key))
            .or_else(|| self.headsets.iter().find(|h| h.endpoints.iter().any(|e| e == base)))
    }

    /// Rename listening lists saved under an S41 key form to the endpoint's
    /// current key (see [`listening::legacy_keys`]). Never overwrites a list
    /// already stored under the current key. True if anything changed.
    pub fn migrate_listening_keys(&mut self, report: &ProbeReport) -> bool {
        let mut eps = report.endpoints.clone();
        listening::unique_endpoint_keys(&mut eps);
        let mut changed = false;
        for ep in &eps {
            if self.listening_for(&ep.key).is_some() {
                continue;
            }
            for old in listening::legacy_keys(&eps, ep) {
                if old == ep.key {
                    continue;
                }
                // An old key that is some other endpoint's current key stays.
                if eps.iter().any(|e| e.key == old) {
                    continue;
                }
                if let Some(l) = self.listening.iter_mut().find(|l| l.endpoint == old) {
                    l.endpoint = ep.key.clone();
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// The listening list stored for an output, if the user made one.
    pub fn listening_for(&self, key: &str) -> Option<&EndpointListening> {
        self.listening.iter().find(|l| l.endpoint == key)
    }

    /// The listening list for an output, created empty on first use.
    pub fn listening_entry(&mut self, key: &str) -> &mut EndpointListening {
        if let Some(i) = self.listening.iter().position(|l| l.endpoint == key) {
            return &mut self.listening[i];
        }
        self.listening.push(EndpointListening {
            endpoint: key.to_owned(),
            devices: Vec::new(),
            active: None,
        });
        self.listening.last_mut().expect("just pushed")
    }

    /// Reduce a probe report to what profile selection consumes.
    ///
    /// The headset is the default output's ACTIVE listening device (S41)
    /// when the user has listed what that output feeds: a headset entry
    /// resolves to it, `Speakers` or an unresolved pick to none. Outputs
    /// with no list fall back to the M1 binding (a headset bound to the
    /// endpoint key), so existing libraries keep working.
    pub fn connected(&self, report: &ProbeReport) -> ConnectedHardware {
        let mut eps = report.endpoints.clone();
        listening::unique_endpoint_keys(&mut eps);
        let headset = eps.iter().find(|e| e.default).and_then(|ep| {
            let key = listening::listening_key(&eps, ep);
            match self.listening_for(&key).filter(|l| !l.devices.is_empty()) {
                Some(l) => l.active().and_then(|d| d.headset()).cloned(),
                None => self.headset_for_endpoint(&ep.key).map(|h| h.id.clone()),
            }
        });
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

    fn monitor(id: &str, ddcci: Option<Vec<u8>>) -> Monitor {
        Monitor {
            id: MonitorId(id.into()),
            name: id.into(),
            panel: String::new(),
            ddcci,
            color: None,
        }
    }

    /// Today no model in the quirks table has verified vendor evidence, so
    /// the reply carries nothing and every vendor slider stays disabled —
    /// even for the LG that advertises both candidate opcodes.
    #[test]
    fn no_vendor_controls_are_offered_while_every_entry_is_unverified() {
        let library = vec![
            monitor("mon:GSM5C7C:402NTCZ9E219", Some(vec![0x10, 0x12, 0xF5, 0xF6])),
            monitor("mon:DEL4099:XYZ", None),
        ];
        assert!(vendor_controls(&library, &[]).is_empty());
        let one = MonitorVendorControls::resolve(&library[0].id, library[0].ddcci.as_deref());
        assert!(one.is_empty());
        assert!(!one.black_equalizer && one.response.is_empty());
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
                EndpointInfo {
                    key: "ep:c:cccc".into(),
                    name: "HDMI".into(),
                    default: false,
                    fx_guid: String::new(),
                },
                EndpointInfo {
                    key: "ep:c:bbbb".into(),
                    name: "Dongle".into(),
                    default: true,
                    fx_guid: String::new(),
                },
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
                fx_guid: String::new(),
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
            color: None,
        });
        store.save().unwrap();

        let mut again = HardwareStore::load(&path).unwrap();
        assert_eq!(again.headsets.len(), 1);
        assert_eq!(again.headsets[0].curve.as_ref().unwrap().len(), 2);
        assert_eq!(again.monitors[0].ddcci, Some(vec![0x10, 0x12, 0x60]));
        assert!(again.remove("hd560s"));
        assert!(!again.remove("hd560s"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn listening_lists_persist_and_lose_removed_headsets() {
        let dir = std::env::temp_dir().join(format!("relay-hwtest-{}", uuid::Uuid::new_v4()));
        let path = dir.join("hardware.json");
        let mut store = HardwareStore::load(&path).unwrap();
        store.upsert_headset(headset("hd560s", &[]));
        let hd = ListeningDevice::Headset { id: HeadsetId("hd560s".into()) };
        let l = store.listening_entry("ep:c:rode");
        l.set_devices(vec![hd.clone(), ListeningDevice::Speakers]);
        l.set_active(&hd);
        store.save().unwrap();

        let mut again = HardwareStore::load(&path).unwrap();
        let l = again.listening_for("ep:c:rode").unwrap();
        assert_eq!(l.devices.len(), 2);
        assert_eq!(l.active(), Some(&hd));

        assert!(again.remove("hd560s"));
        let l = again.listening_for("ep:c:rode").unwrap();
        assert_eq!(l.devices, vec![ListeningDevice::Speakers]);
        assert_eq!(l.active(), Some(&ListeningDevice::Speakers));
        assert_eq!(fx_guid_of("{0.0.0.00000000}.{9d47ae05-2c1c}"), "{9d47ae05-2c1c}");
        assert_eq!(fx_guid_of("nothing"), "");
        assert_eq!(again.headsets.len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod key_migration_tests {
    use super::*;

    fn ep(key: &str, name: &str, fx: &str, default: bool) -> EndpointInfo {
        EndpointInfo { key: key.into(), name: name.into(), default, fx_guid: fx.into() }
    }

    fn hs(id: &str) -> ListeningDevice {
        ListeningDevice::Headset { id: HeadsetId(id.into()) }
    }

    /// The RODECaster Duo on the second PC: Main and Chat share a container.
    fn rode_report() -> ProbeReport {
        ProbeReport {
            endpoints: vec![
                ep("ep:c:rode", "Main (RODECaster Duo)", "{aaaa}", true),
                ep("ep:c:rode", "Chat (RODECaster Duo)", "{bbbb}", false),
            ],
            monitors: vec![],
        }
    }

    #[test]
    fn view_keys_are_unique_and_match_listening_and_selection() {
        let mut lib = HardwareStore::in_memory();
        lib.listening_entry("ep:c:rode#aaaa").set_devices(vec![hs("hd560s")]);
        lib.listening_entry("ep:c:rode#bbbb").set_devices(vec![hs("blessing3")]);
        let view = HardwareView::from_report(rode_report(), &lib);
        assert_eq!(view.endpoints[0].key, "ep:c:rode#aaaa");
        assert_eq!(view.endpoints[1].key, "ep:c:rode#bbbb");
        assert_eq!(view.default_listening.as_deref(), Some("ep:c:rode#aaaa"));
        assert_eq!(lib.connected(&rode_report()).headset, Some(HeadsetId("hd560s".into())));
    }

    #[test]
    fn s41_named_and_bare_keys_migrate_to_the_new_form() {
        let mut lib = HardwareStore::in_memory();
        lib.listening_entry("ep:c:rode#Chat (RODECaster Duo)").set_devices(vec![hs("blessing3")]);
        lib.listening_entry("ep:c:rode").set_devices(vec![hs("hd560s")]);
        assert!(lib.migrate_listening_keys(&rode_report()));
        assert!(lib.listening_for("ep:c:rode#bbbb").is_some());
        // The bare key went to the default endpoint of the group.
        assert_eq!(lib.listening_for("ep:c:rode#aaaa").unwrap().devices, vec![hs("hd560s")]);
        assert!(!lib.migrate_listening_keys(&rode_report()), "second run is a no-op");
    }

    #[test]
    fn migration_never_overwrites_a_current_list() {
        let mut lib = HardwareStore::in_memory();
        lib.listening_entry("ep:c:rode#aaaa").set_devices(vec![hs("new")]);
        lib.listening_entry("ep:c:rode").set_devices(vec![hs("old")]);
        lib.listening_entry("ep:c:rode#Chat (RODECaster Duo)").set_devices(vec![hs("chat")]);
        lib.migrate_listening_keys(&rode_report());
        assert_eq!(lib.listening_for("ep:c:rode#aaaa").unwrap().devices, vec![hs("new")]);
        assert_eq!(lib.listening_for("ep:c:rode#bbbb").unwrap().devices, vec![hs("chat")]);
    }

    #[test]
    fn m1_headset_bindings_on_the_device_key_still_resolve() {
        let mut lib = HardwareStore::in_memory();
        lib.upsert_headset(Headset {
            id: HeadsetId("hd560s".into()),
            name: "HD 560S".into(),
            kind: HeadsetKind::Headphone,
            curve: None,
            source: String::new(),
            endpoints: vec!["ep:c:rode".into()],
        });
        assert_eq!(lib.connected(&rode_report()).headset, Some(HeadsetId("hd560s".into())));
    }
}
