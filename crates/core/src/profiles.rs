//! Profile store: a JSON file on disk plus the selection rule.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hardware::ConnectedHardware;
use crate::types::{Profile, ProfileStatus};

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
}

const FILE_VERSION: u32 = 1;

#[derive(Debug)]
pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<Profile>,
}

impl ProfileStore {
    /// Load from disk; a missing file is an empty store, not an error.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let profiles = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: ProfilesFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?;
                file.profiles
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, profiles })
    }

    pub fn in_memory(profiles: Vec<Profile>) -> Self {
        Self { path: PathBuf::new(), profiles }
    }

    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let file = ProfilesFile { version: FILE_VERSION, profiles: self.profiles.clone() };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)
    }

    pub fn all(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn get(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn upsert(&mut self, profile: Profile) {
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, id: Uuid) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        before != self.profiles.len()
    }

    /// Pick the best profile for the foreground process given what is plugged in.
    ///
    /// Rules, in order:
    /// 1. The game must match. Draft profiles never auto-apply.
    /// 2. A profile whose headset is *specified* must match the connected headset;
    ///    "Any" (`None`) always matches but scores lower.
    /// 3. Same for monitor.
    /// 4. Ties resolve to the earlier row in the file so ordering is user-controllable.
    pub fn select(&self, exe_name: &str, title: &str, hw: &ConnectedHardware) -> Option<&Profile> {
        let mut best: Option<(u8, &Profile)> = None;
        for p in &self.profiles {
            if p.status != ProfileStatus::Ready || !p.game.matches(exe_name, title) {
                continue;
            }
            let headset_score = match (&p.headset, &hw.headset) {
                (None, _) => 1,
                (Some(want), Some(have)) if want == have => 2,
                _ => continue,
            };
            let monitor_score = match &p.monitor {
                None => 1,
                Some(want) if hw.has_monitor(want) => 2,
                Some(_) => continue,
            };
            let score = headset_score + monitor_score;
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, p));
            }
        }
        best.map(|(_, p)| p)
    }
}

/// Write via a temp file + fsync + rename so a crash mid-write never leaves a
/// torn file and the bytes are durable before the rename makes them visible.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes).with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().with_context(|| format!("fsync {}", tmp.display()))?;
    }
    // On Windows `rename` fails if the target exists; remove first. The window
    // between remove and rename is acceptable because the .tmp is complete.
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GameMatch, HeadsetId, MonitorId};

    fn ready(name: &str, exe: &str, headset: Option<&str>, monitor: Option<&str>) -> Profile {
        let mut p = Profile::new(name, GameMatch::exe(exe));
        p.headset = headset.map(|h| HeadsetId(h.into()));
        p.monitor = monitor.map(|m| MonitorId(m.into()));
        p.status = ProfileStatus::Ready;
        p
    }

    fn hw(headset: Option<&str>, monitors: &[&str]) -> ConnectedHardware {
        ConnectedHardware {
            headset: headset.map(|h| HeadsetId(h.into())),
            monitors: monitors.iter().map(|m| MonitorId((*m).into())).collect(),
        }
    }

    #[test]
    fn prefers_exact_hardware_over_any() {
        let store = ProfileStore::in_memory(vec![
            ready("CoD any", "cod.exe", None, None),
            ready("CoD HD560S", "cod.exe", Some("hd560s"), Some("lg27gp850")),
            ready("CoD IEM", "cod.exe", Some("blessing3"), Some("lg27gp850")),
        ]);
        let pick = store.select("cod.exe", "", &hw(Some("blessing3"), &["lg27gp850"])).unwrap();
        assert_eq!(pick.name, "CoD IEM");
    }

    #[test]
    fn falls_back_to_any_when_hardware_unknown() {
        let store = ProfileStore::in_memory(vec![
            ready("CoD HD560S", "cod.exe", Some("hd560s"), None),
            ready("CoD any", "cod.exe", None, None),
        ]);
        let pick = store.select("cod.exe", "", &hw(None, &[])).unwrap();
        assert_eq!(pick.name, "CoD any");
    }

    #[test]
    fn specified_hardware_that_is_not_connected_never_matches() {
        let store =
            ProfileStore::in_memory(vec![ready("Val", "valorant.exe", None, Some("xl2566k"))]);
        assert!(store.select("valorant.exe", "", &hw(None, &["lg27gp850"])).is_none());
    }

    #[test]
    fn drafts_never_auto_apply() {
        let mut p = ready("Elden", "eldenring.exe", None, None);
        p.status = ProfileStatus::Draft;
        let store = ProfileStore::in_memory(vec![p]);
        assert!(store.select("eldenring.exe", "", &hw(None, &[])).is_none());
    }

    /// The M1 acceptance case: two Call of Duty rows keyed to different
    /// headsets, and swapping the default endpoint (USB DAC ↔ dongle) flips
    /// which row wins — driven end-to-end through the hardware library
    /// (endpoint key → headset binding → ConnectedHardware → select).
    #[test]
    fn two_cod_rows_follow_the_default_endpoint_swap() {
        use crate::hardware::{
            endpoint_key, EndpointInfo, HardwareStore, Headset, HeadsetKind, MonitorProbe,
            ProbeReport,
        };

        // Library: HD 560S behind the ASUS USB DAC, Blessing 3 on a dongle.
        // Container GUIDs shaped like the real probe reports them.
        let dac_key =
            endpoint_key(Some("31f634a2-5a67-4f9c-8ab0-6d1b2a3c4d5e"), "{0.0.0.00000000}.{a1}");
        let dongle_key =
            endpoint_key(Some("77c0f2b1-9e2d-4d3f-b1aa-0f9e8d7c6b5a"), "{0.0.0.00000000}.{b2}");
        let mut lib = HardwareStore::in_memory();
        lib.upsert_headset(Headset {
            id: HeadsetId("hd560s".into()),
            name: "HD 560S".into(),
            kind: HeadsetKind::Headphone,
            curve: None,
            source: String::new(),
            endpoints: vec![dac_key.clone()],
        });
        lib.upsert_headset(Headset {
            id: HeadsetId("blessing3".into()),
            name: "Moondrop Blessing 3".into(),
            kind: HeadsetKind::Iem,
            curve: None,
            source: String::new(),
            endpoints: vec![dongle_key.clone()],
        });

        let store = ProfileStore::in_memory(vec![
            ready("CoD HD560S", "cod.exe", Some("hd560s"), Some("mon:GSM5C7C:402NTCZ9E219")),
            ready("CoD IEM", "cod.exe", Some("blessing3"), Some("mon:GSM5C7C:402NTCZ9E219")),
            ready("CoD any", "cod.exe", None, None),
        ]);

        let lg = MonitorProbe {
            id: MonitorId("mon:GSM5C7C:402NTCZ9E219".into()),
            name: "LG ULTRAGEAR+".into(),
            native: Some((3840, 2160)),
            refresh_hz: Some(144.0),
            primary: true,
            hmonitor: 0x10001,
            gdi_name: r"\\.\DISPLAY1".into(),
            ddc: None,
        };
        let report_with_default = |default_key: &str| ProbeReport {
            endpoints: vec![
                EndpointInfo {
                    key: dac_key.clone(),
                    name: "USB Audio 2.0".into(),
                    default: default_key == dac_key,
                },
                EndpointInfo {
                    key: dongle_key.clone(),
                    name: "USB-C dongle".into(),
                    default: default_key == dongle_key,
                },
            ],
            monitors: vec![lg.clone()],
        };

        let on_dac = lib.connected(&report_with_default(&dac_key));
        assert_eq!(store.select("cod.exe", "", &on_dac).unwrap().name, "CoD HD560S");

        // Swap the default endpoint — no focus change, only hardware state.
        let on_dongle = lib.connected(&report_with_default(&dongle_key));
        assert_eq!(store.select("cod.exe", "", &on_dongle).unwrap().name, "CoD IEM");

        // Unplug both (default falls to an unbound endpoint): the Any row.
        let unbound = ProbeReport {
            endpoints: vec![EndpointInfo {
                key: "ep:c:hdmi".into(),
                name: "HDMI".into(),
                default: true,
            }],
            monitors: vec![lg.clone()],
        };
        let on_hdmi = lib.connected(&unbound);
        assert_eq!(store.select("cod.exe", "", &on_hdmi).unwrap().name, "CoD any");
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("relay-test-{}", Uuid::new_v4()));
        let path = dir.join("profiles.json");
        let mut store = ProfileStore::load(&path).unwrap();
        assert!(store.all().is_empty());
        store.upsert(ready("CoD", "cod.exe", None, None));
        store.save().unwrap();
        let again = ProfileStore::load(&path).unwrap();
        assert_eq!(again.all().len(), 1);
        assert_eq!(again.all()[0].name, "CoD");
        let _ = std::fs::remove_dir_all(dir);
    }
}
