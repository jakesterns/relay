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

/// The key listening devices are stored under: the endpoint's own key.
/// Since S41b [`unique_endpoint_keys`] makes every endpoint key in a report
/// unique, so this is `ep.key`; kept as one function so every caller
/// (listening, other processing, profile selection) agrees.
pub fn listening_key(_report_endpoints: &[EndpointInfo], ep: &EndpointInfo) -> String {
    ep.key.clone()
}

/// The device part of a key: `ep:c:<container>` for `ep:c:<container>#<endpoint>`.
pub fn base_key(key: &str) -> &str {
    key.split_once('#').map_or(key, |(b, _)| b)
}

/// Make every endpoint key in `endpoints` unique. Every endpoint of one
/// physical device shares its container key, and a device can expose several
/// outputs (a RODECaster's "Main" and "Chat", a headset's game and chat
/// channels) that feed different things. Those get `<container key>#<endpoint
/// guid>` (the MMDevices GUID, stable across renames); a device with one
/// output keeps the plain container key. Idempotent.
pub fn unique_endpoint_keys(endpoints: &mut [EndpointInfo]) {
    let bases: Vec<String> = endpoints.iter().map(|e| base_key(&e.key).to_owned()).collect();
    for (i, ep) in endpoints.iter_mut().enumerate() {
        let shared = bases.iter().filter(|b| **b == bases[i]).count() > 1;
        if !shared {
            ep.key = bases[i].clone();
            continue;
        }
        let tag = ep.fx_guid.trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
        let tag = if tag.is_empty() { ep.name.clone() } else { tag };
        ep.key = format!("{}#{}", bases[i], tag);
    }
}

/// Keys a saved listening list may still carry for `ep` (S41 forms):
/// `<container>#<friendly name>` for shared containers, and the bare
/// container key. The bare key goes to one endpoint of the group only: the
/// default one if it is in the group, otherwise the first.
pub fn legacy_keys(all: &[EndpointInfo], ep: &EndpointInfo) -> Vec<String> {
    let base = base_key(&ep.key);
    let mut out = vec![format!("{base}#{}", ep.name)];
    let group: Vec<&EndpointInfo> = all.iter().filter(|e| base_key(&e.key) == base).collect();
    let owner = group.iter().find(|e| e.default).or(group.first()).copied();
    if owner.is_some_and(|o| o.key == ep.key) && ep.key != base {
        out.push(base.to_owned());
    }
    out
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
        let mut all = vec![
            ep("ep:c:rode", "Main (RODECaster Duo)"),
            ep("ep:c:rode", "Chat (RODECaster Duo)"),
            ep("ep:c:dac", "Speakers (USB DAC)"),
        ];
        all[0].fx_guid = "{AAAA}".into();
        all[1].fx_guid = "{bbbb}".into();
        all[1].default = true;
        unique_endpoint_keys(&mut all);
        assert_eq!(all[0].key, "ep:c:rode#aaaa");
        assert_eq!(all[1].key, "ep:c:rode#bbbb");
        assert_eq!(all[2].key, "ep:c:dac");
        let before = all.clone();
        unique_endpoint_keys(&mut all);
        assert_eq!(all, before, "idempotent");
        assert_eq!(listening_key(&all, &all[0]), all[0].key);
        assert_eq!(base_key(&all[0].key), "ep:c:rode");

        // The S41 forms still map to exactly one endpoint each.
        assert_eq!(legacy_keys(&all, &all[0]), vec!["ep:c:rode#Main (RODECaster Duo)".to_owned()]);
        assert_eq!(
            legacy_keys(&all, &all[1]),
            vec!["ep:c:rode#Chat (RODECaster Duo)".to_owned(), "ep:c:rode".to_owned()]
        );

        // One output left: back to the plain key.
        let mut one = vec![before[0].clone()];
        unique_endpoint_keys(&mut one);
        assert_eq!(one[0].key, "ep:c:rode");
    }
}
