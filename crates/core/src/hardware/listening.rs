//! What the user is actually listening on, per output (S41).
//!
//! Windows sees the output device (the endpoint: a RODECaster, a USB DAC,
//! the motherboard jack), never what is plugged into it. One interface can
//! feed IEMs, a headset and a home theatre on different days, so "one
//! headset per endpoint" is wrong. Each output instead carries an ordered
//! list of the listening devices the user said are connected to it, and
//! which one is active. Headphone correction and profile selection follow
//! the active one.
//!
//! Rules:
//! - One entry: it is active, whatever `active` says.
//! - Several entries: `active` if it is still in the list, otherwise none.
//!   Relay does not guess between two headsets; a wrong curve is worse than
//!   no curve.
//! - `Speakers` is the honest "no correction" entry: nothing to correct
//!   against, and headset-bound profiles do not match.

use serde::{Deserialize, Serialize};

use super::{EndpointInfo, ProbeReport};
use crate::types::HeadsetId;

/// One thing a person can listen on through an output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListeningDevice {
    /// A headset/IEM from the hardware library (added from the AutoEQ
    /// catalogue or by hand), by its library id.
    Headset { id: HeadsetId },
    /// Speakers, a home theatre, anything with no measured curve.
    Speakers,
}

impl ListeningDevice {
    pub fn headset(&self) -> Option<&HeadsetId> {
        match self {
            ListeningDevice::Headset { id } => Some(id),
            ListeningDevice::Speakers => None,
        }
    }
}

/// The listening devices one output feeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointListening {
    /// [`listening_key`] of the output.
    pub endpoint: String,
    /// In the order the user added them; the tray lists them in this order.
    #[serde(default)]
    pub devices: Vec<ListeningDevice>,
    /// The user's pick. Ignored when `devices` has one entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ListeningDevice>,
}

impl EndpointListening {
    /// The device correction and selection should use; see the module rules.
    pub fn active(&self) -> Option<&ListeningDevice> {
        match self.devices.as_slice() {
            [] => None,
            [only] => Some(only),
            all => self.active.as_ref().filter(|a| all.contains(a)),
        }
    }

    /// Replace the list, deduplicated, keeping the pick if it survives.
    pub fn set_devices(&mut self, devices: Vec<ListeningDevice>) {
        let mut out: Vec<ListeningDevice> = Vec::with_capacity(devices.len());
        for d in devices {
            if !out.contains(&d) {
                out.push(d);
            }
        }
        self.devices = out;
        if self.active.as_ref().is_some_and(|a| !self.devices.contains(a)) {
            self.active = None;
        }
    }

    /// Make `device` active. False (and no change) when it is not in the list.
    pub fn set_active(&mut self, device: &ListeningDevice) -> bool {
        if !self.devices.contains(device) {
            return false;
        }
        self.active = Some(device.clone());
        true
    }

    /// Advance to the next device in list order (the cycle hotkey). With no
    /// current pick, the first. Returns the new active device.
    pub fn cycle(&mut self) -> Option<&ListeningDevice> {
        if self.devices.is_empty() {
            return None;
        }
        let next = match self.active().and_then(|a| self.devices.iter().position(|d| d == a)) {
            Some(i) => (i + 1) % self.devices.len(),
            None => 0,
        };
        self.active = Some(self.devices[next].clone());
        self.active.as_ref()
    }
}

/// The key listening devices are stored under. Usually the endpoint key; but
/// every endpoint of one physical device shares a container key, and a
/// device can expose several outputs (a RODECaster's "System" and "Chat", a
/// headset's game and chat channels) that feed different things. When the
/// report holds more than one endpoint with the same key, the friendly name
/// tells them apart.
pub fn listening_key(report_endpoints: &[EndpointInfo], ep: &EndpointInfo) -> String {
    let shared = report_endpoints.iter().filter(|e| e.key == ep.key).count() > 1;
    if shared {
        format!("{}#{}", ep.key, ep.name)
    } else {
        ep.key.clone()
    }
}

/// The default output's listening key, if there is a default output.
pub fn default_listening_key(report: &ProbeReport) -> Option<String> {
    report.default_endpoint().map(|ep| listening_key(&report.endpoints, ep))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(id: &str) -> ListeningDevice {
        ListeningDevice::Headset { id: HeadsetId(id.into()) }
    }

    fn entry(devices: Vec<ListeningDevice>, active: Option<ListeningDevice>) -> EndpointListening {
        EndpointListening { endpoint: "ep:c:rode".into(), devices, active }
    }

    #[test]
    fn one_entry_is_active_automatically() {
        let e = entry(vec![hs("blessing3")], None);
        assert_eq!(e.active(), Some(&hs("blessing3")));
        // Even a stale pick does not override the only entry.
        let e = entry(vec![hs("blessing3")], Some(hs("hd560s")));
        assert_eq!(e.active(), Some(&hs("blessing3")));
    }

    #[test]
    fn several_entries_need_a_pick_and_never_guess() {
        let e = entry(vec![hs("blessing3"), ListeningDevice::Speakers], None);
        assert_eq!(e.active(), None);
        let e = entry(vec![hs("blessing3"), ListeningDevice::Speakers], Some(hs("hd560s")));
        assert_eq!(e.active(), None, "a pick that is no longer listed is ignored");
        let e = entry(
            vec![hs("blessing3"), ListeningDevice::Speakers],
            Some(ListeningDevice::Speakers),
        );
        assert_eq!(e.active(), Some(&ListeningDevice::Speakers));
        assert_eq!(e.active().unwrap().headset(), None);
    }

    #[test]
    fn set_devices_dedupes_and_drops_a_removed_pick() {
        let mut e = entry(vec![], None);
        e.set_devices(vec![hs("a"), hs("b"), hs("a")]);
        assert_eq!(e.devices, vec![hs("a"), hs("b")]);
        assert!(e.set_active(&hs("b")));
        assert!(!e.set_active(&hs("zzz")));
        assert_eq!(e.active(), Some(&hs("b")));
        e.set_devices(vec![hs("a"), ListeningDevice::Speakers]);
        assert_eq!(e.active, None);
    }

    #[test]
    fn cycle_walks_the_list_in_order() {
        let mut e = entry(vec![hs("a"), hs("b"), ListeningDevice::Speakers], None);
        assert_eq!(e.cycle(), Some(&hs("a")));
        assert_eq!(e.cycle(), Some(&hs("b")));
        assert_eq!(e.cycle(), Some(&ListeningDevice::Speakers));
        assert_eq!(e.cycle(), Some(&hs("a")));
        assert_eq!(entry(vec![], None).cycle(), None);
    }

    #[test]
    fn serialises_with_a_kind_tag() {
        let json = serde_json::to_string(&hs("hd560s")).unwrap();
        assert_eq!(json, r#"{"kind":"headset","id":"hd560s"}"#);
        let sp: ListeningDevice = serde_json::from_str(r#"{"kind":"speakers"}"#).unwrap();
        assert_eq!(sp, ListeningDevice::Speakers);
    }

    #[test]
    fn endpoints_sharing_a_container_key_are_told_apart_by_name() {
        let ep = |key: &str, name: &str| EndpointInfo {
            key: key.into(),
            name: name.into(),
            default: false,
            fx_guid: String::new(),
        };
        let all = vec![
            ep("ep:c:rode", "System (RODECaster Pro II)"),
            ep("ep:c:rode", "Chat (RODECaster Pro II)"),
            ep("ep:c:dac", "Speakers (USB DAC)"),
        ];
        assert_eq!(listening_key(&all, &all[0]), "ep:c:rode#System (RODECaster Pro II)");
        assert_eq!(listening_key(&all, &all[1]), "ep:c:rode#Chat (RODECaster Pro II)");
        assert_eq!(listening_key(&all, &all[2]), "ep:c:dac");
    }
}
