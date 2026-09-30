//! Other processing on an output, detected read-only (S41).
//!
//! Relay's headphone correction assumes it is the only thing shaping the
//! sound. It often is not: a vendor APO sits in the endpoint's FX chain
//! (Nahimic, Dolby, Realtek, Sonic Studio), or vendor software runs its own
//! mixer (SteelSeries Sonar, G HUB, Synapse, Voicemeeter). Correction on top
//! of someone else's EQ is not accurate, and the user deserves to be told.
//!
//! Two sources, both read-only, never modified or disabled:
//! - The endpoint's FX property store (`relay_apo::livereg::read_fx_store`):
//!   every effect CLSID that is neither Microsoft's nor Relay's.
//! - Running process image names against a small table of known vendor
//!   audio software. A process cannot be tied to one output, so these are
//!   reported as "running, may be processing" on every output.

use serde::{Deserialize, Serialize};

/// Where the finding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessorKind {
    /// An audio effect installed on this output's FX chain. Certain.
    Apo,
    /// Vendor audio software is running. It may or may not touch this output.
    Software,
}

/// One other processor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherProcessor {
    /// Plain name, e.g. "SteelSeries Sonar".
    pub name: String,
    pub kind: ProcessorKind,
    /// What to do for accurate correction, e.g. "set Sonar's EQ flat".
    pub advice: String,
    /// The effect's CLSID, for APO findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clsid: Option<String>,
}

/// Everything else processing one output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointProcessing {
    /// [`super::listening::listening_key`] of the output.
    pub endpoint: String,
    pub processors: Vec<OtherProcessor>,
}

// ---------------------------------------------------------------------------
// APO classification

/// How an effect CLSID in an FX store is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApoClass {
    /// Relay's own APO.
    Relay,
    /// Windows' inbox effects (enhancements, loudness, spatial proxy).
    Microsoft,
    /// Anything else: somebody else's processing.
    Other,
}

/// Windows' own effect CLSIDs as they appear in FX stores. Small on purpose:
/// an unlisted Microsoft effect shows as "another audio effect", which is
/// an over-report, never a hidden vendor.
const MICROSOFT_APOS: &[&str] = &[
    "{62dc1a93-ae24-464c-a43e-452f824c4250}", // WMALFXGFXDSP (legacy LFX/GFX)
    "{637c490d-eee3-4c0a-973f-371958802da2}", // MsApoFxProxy (SFX)
    "{5860e1c5-f95c-4a7a-8ec8-8aef24f379a1}", // MsApoFxProxy (MFX)
    "{c9453e73-8c5c-4463-9984-af8bab2f5447}", // MsApoFxProxy (EFX)
    "{13ab3ebd-137e-4903-9d89-60be8277fd17}", // Windows enhancements (EFX)
];

pub fn classify_clsid(clsid: &str) -> ApoClass {
    let c = clsid.trim().to_ascii_lowercase();
    if c == relay_apo::ids::APO_CLSID.to_ascii_lowercase() {
        ApoClass::Relay
    } else if MICROSOFT_APOS.contains(&c.as_str()) {
        ApoClass::Microsoft
    } else {
        ApoClass::Other
    }
}

/// The FX property set: `PKEY_FX_*` and `PKEY_CompositeFX_*` all live under
/// this format id; the value data are effect CLSIDs (REG_SZ or REG_MULTI_SZ).
/// Processing-mode lists live under a different format id and are skipped.
const FX_FMTID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},";

/// Every effect CLSID named in an FX store's root values, lowercase, in
/// store order, deduplicated.
pub fn effect_clsids(store: &relay_apo::fxstore::FxStore) -> Vec<String> {
    use relay_apo::regfile::RegKind;
    let mut out = Vec::new();
    let Some(root) = store.keys.iter().find(|(k, _)| k.is_empty()).map(|(_, v)| v) else {
        return out;
    };
    for (name, value) in root.iter() {
        if !name.to_ascii_lowercase().starts_with(FX_FMTID) {
            continue;
        }
        if !matches!(value.kind, RegKind::Sz | RegKind::MultiSz | RegKind::ExpandSz) {
            continue;
        }
        let wide: Vec<u16> =
            value.data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16_lossy(&wide);
        for g in guids_in(&text) {
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out
}

/// `{8-4-4-4-12}` groups in `text`, lowercase.
fn guids_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i + 38 <= b.len() {
        if b[i] == b'{' && b[i + 37] == b'}' {
            let inner = &text[i + 1..i + 37];
            let ok = inner.char_indices().all(|(j, ch)| match j {
                8 | 13 | 18 | 23 => ch == '-',
                _ => ch.is_ascii_hexdigit(),
            });
            if ok {
                out.push(text[i..i + 38].to_ascii_lowercase());
                i += 38;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The third-party APOs in one FX store, named with `name_of` (the COM
/// class's registered name) when it has one.
pub fn third_party_apos(
    store: &relay_apo::fxstore::FxStore,
    name_of: impl Fn(&str) -> Option<String>,
) -> Vec<OtherProcessor> {
    effect_clsids(store)
        .into_iter()
        // The all-zero GUID is an empty slot some drivers write, not an
        // effect (the Realtek SPDIF output on the dev PC, 2026-09-29).
        .filter(|c| c != "{00000000-0000-0000-0000-000000000000}")
        .filter(|c| classify_clsid(c) == ApoClass::Other)
        .map(|c| {
            let name = name_of(&c)
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "Another audio effect".to_owned());
            OtherProcessor {
                advice: String::from(
                    "turn its effects off or set them flat in its own app for accurate correction",
                ),
                name,
                kind: ProcessorKind::Apo,
                clsid: Some(c),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Vendor software

/// A known vendor audio app: image name (case-insensitive), what to call it,
/// and what the user should set in it.
pub struct VendorApp {
    pub exe: &'static str,
    pub name: &'static str,
    pub advice: &'static str,
}

/// Kept small and checked by hand. Several exes may map to one product.
pub const VENDOR_APPS: &[VendorApp] = &[
    VendorApp {
        exe: "SteelSeriesSonar.exe",
        name: "SteelSeries Sonar",
        advice: "set Sonar's EQ flat",
    },
    VendorApp { exe: "SteelSeriesGG.exe", name: "SteelSeries GG", advice: "set Sonar's EQ flat" },
    VendorApp {
        exe: "RazerAppEngine.exe",
        name: "Razer Synapse",
        advice: "turn THX Spatial Audio and Synapse EQ off",
    },
    VendorApp {
        exe: "Razer Synapse 3.exe",
        name: "Razer Synapse",
        advice: "turn THX Spatial Audio and Synapse EQ off",
    },
    VendorApp {
        exe: "THXAudioService.exe",
        name: "THX Spatial Audio",
        advice: "turn THX Spatial Audio off",
    },
    VendorApp { exe: "NahimicSvc64.exe", name: "Nahimic", advice: "turn Nahimic's effects off" },
    VendorApp { exe: "NahimicSvc32.exe", name: "Nahimic", advice: "turn Nahimic's effects off" },
    VendorApp {
        exe: "DolbyAccess.exe",
        name: "Dolby Access",
        advice: "turn Dolby Atmos for Headphones off",
    },
    VendorApp { exe: "DolbyDAX3API.exe", name: "Dolby Audio", advice: "turn Dolby's EQ off" },
    VendorApp {
        exe: "RtkUWP.exe",
        name: "Realtek Audio Console",
        advice: "turn Realtek's effects off",
    },
    VendorApp { exe: "lghub.exe", name: "Logitech G HUB", advice: "set G HUB's EQ flat" },
    VendorApp { exe: "lghub_agent.exe", name: "Logitech G HUB", advice: "set G HUB's EQ flat" },
    VendorApp { exe: "iCUE.exe", name: "Corsair iCUE", advice: "set iCUE's EQ flat" },
    VendorApp {
        exe: "RODE Central.exe",
        name: "RODE Central",
        advice: "set the interface's EQ flat on this output",
    },
    VendorApp {
        exe: "RODECaster App.exe",
        name: "RODECaster app",
        advice: "set the RODECaster's EQ flat on this output",
    },
    VendorApp {
        exe: "voicemeeter8x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
    },
    VendorApp {
        exe: "voicemeeterpro_x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
    },
    VendorApp {
        exe: "voicemeeter_x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
    },
    VendorApp { exe: "FxSound.exe", name: "FxSound", advice: "turn FxSound off" },
];

/// Known vendor apps among `exes`, one entry per product, table order.
pub fn vendors_running<'a>(exes: impl IntoIterator<Item = &'a str>) -> Vec<OtherProcessor> {
    let running: Vec<String> = exes.into_iter().map(|e| e.to_ascii_lowercase()).collect();
    let mut out: Vec<OtherProcessor> = Vec::new();
    for app in VENDOR_APPS {
        if running.iter().any(|e| e == &app.exe.to_ascii_lowercase())
            && !out.iter().any(|o| o.name == app.name)
        {
            out.push(OtherProcessor {
                name: app.name.to_owned(),
                kind: ProcessorKind::Software,
                advice: app.advice.to_owned(),
                clsid: None,
            });
        }
    }
    out
}

/// Put the findings together for every output in `report`.
pub fn assemble(
    report: &super::ProbeReport,
    apos_for: impl Fn(&super::EndpointInfo) -> Vec<OtherProcessor>,
    software: &[OtherProcessor],
) -> Vec<EndpointProcessing> {
    report
        .endpoints
        .iter()
        .map(|ep| {
            let mut processors = apos_for(ep);
            processors.extend(software.iter().cloned());
            EndpointProcessing {
                endpoint: super::listening::listening_key(&report.endpoints, ep),
                processors,
            }
        })
        .filter(|e| !e.processors.is_empty())
        .collect()
}

/// The live scan. Registry and process list are only read.
#[cfg(windows)]
pub fn scan(report: &super::ProbeReport) -> Vec<EndpointProcessing> {
    let exes = crate::processes::list_all();
    let software = vendors_running(exes.iter().map(String::as_str));
    assemble(
        report,
        |ep| {
            if ep.fx_guid.is_empty() {
                return Vec::new();
            }
            match relay_apo::livereg::LiveRegistry::read_fx_store(&ep.fx_guid) {
                Ok(store) => third_party_apos(&store, clsid_name),
                Err(_) => Vec::new(),
            }
        },
        &software,
    )
}

#[cfg(not(windows))]
pub fn scan(_report: &super::ProbeReport) -> Vec<EndpointProcessing> {
    Vec::new()
}

/// The registered name of a COM class (`HKLM\SOFTWARE\Classes\CLSID\{x}`
/// default value). Read-only.
#[cfg(windows)]
fn clsid_name(clsid: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let key: Vec<u16> =
        relay_apo::ids::clsid_key(clsid).encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: valid out-buffer and length; RegGetValueW NUL-terminates and
    // never writes past `len`. A null value name reads the default value.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    Some(s.trim_end_matches('\0').trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_apo::fxstore::FxStore;
    use relay_apo::regfile::{sz_bytes, RegKind, RegValue};

    fn multi_sz(items: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for s in items {
            for u in s.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn store(values: Vec<(&str, RegValue)>) -> FxStore {
        let mut s = FxStore::empty();
        let root = s.keys.get_mut("").unwrap();
        for (k, v) in values {
            root.insert(k.to_owned(), v);
        }
        s
    }

    const NAHIMIC: &str = "{D2B6A7F4-3C51-4B9A-9E0D-1A2B3C4D5E6F}";

    #[test]
    fn clsids_classify_relay_microsoft_and_other() {
        assert_eq!(classify_clsid(relay_apo::ids::APO_CLSID), ApoClass::Relay);
        assert_eq!(
            classify_clsid(&relay_apo::ids::APO_CLSID.to_ascii_lowercase()),
            ApoClass::Relay
        );
        assert_eq!(classify_clsid("{62DC1A93-AE24-464C-A43E-452F824C4250}"), ApoClass::Microsoft);
        assert_eq!(classify_clsid(NAHIMIC), ApoClass::Other);
    }

    #[test]
    fn only_third_party_effects_are_reported_with_their_registered_name() {
        let s = store(vec![
            (
                relay_apo::ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID,
                RegValue {
                    kind: RegKind::MultiSz,
                    data: multi_sz(&[relay_apo::ids::APO_CLSID, NAHIMIC]),
                },
            ),
            (
                "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5",
                RegValue {
                    kind: RegKind::Sz,
                    data: sz_bytes("{637c490d-eee3-4c0a-973f-371958802da2}"),
                },
            ),
            // Processing modes carry GUIDs too; they are not effects.
            (
                relay_apo::ids::PKEY_EFX_MODES,
                RegValue {
                    kind: RegKind::MultiSz,
                    data: multi_sz(&[relay_apo::ids::MODE_DEFAULT]),
                },
            ),
        ]);
        assert_eq!(effect_clsids(&s).len(), 3);
        let found = third_party_apos(&s, |c| {
            (c == NAHIMIC.to_ascii_lowercase()).then(|| "Nahimic APO".to_owned())
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Nahimic APO");
        assert_eq!(found[0].kind, ProcessorKind::Apo);
        assert_eq!(found[0].clsid.as_deref(), Some(NAHIMIC.to_ascii_lowercase().as_str()));

        // No registered name: still reported, generically.
        let unnamed = third_party_apos(&s, |_| None);
        assert_eq!(unnamed[0].name, "Another audio effect");
    }

    #[test]
    fn an_empty_or_relay_only_store_reports_nothing() {
        assert!(third_party_apos(&FxStore::empty(), |_| None).is_empty());
        let s = store(vec![(
            relay_apo::ids::PKEY_FX_ENDPOINT_EFFECT_CLSID,
            RegValue { kind: RegKind::Sz, data: sz_bytes(relay_apo::ids::APO_CLSID) },
        )]);
        assert!(third_party_apos(&s, |_| Some("x".into())).is_empty());
    }

    #[test]
    fn vendor_table_matches_case_insensitively_once_per_product() {
        let found = vendors_running([
            "explorer.exe",
            "steelseriessonar.EXE",
            "SteelSeriesGG.exe",
            "lghub.exe",
            "lghub_agent.exe",
        ]);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["SteelSeries Sonar", "SteelSeries GG", "Logitech G HUB"]);
        assert!(found.iter().all(|f| f.kind == ProcessorKind::Software));
        assert!(found[0].advice.contains("flat"));
        assert!(vendors_running(["chrome.exe", "relay-core.exe"]).is_empty());
    }

    #[test]
    fn vendor_table_has_no_duplicate_exes() {
        let mut seen = std::collections::HashSet::new();
        for a in VENDOR_APPS {
            assert!(seen.insert(a.exe.to_ascii_lowercase()), "duplicate {}", a.exe);
            assert!(!a.advice.is_empty());
        }
    }

    #[test]
    fn assemble_keys_by_listening_key_and_skips_clean_outputs() {
        use super::super::{EndpointInfo, ProbeReport};
        let ep = |key: &str, name: &str, fx: &str| EndpointInfo {
            key: key.into(),
            name: name.into(),
            default: false,
            fx_guid: fx.into(),
        };
        let report = ProbeReport {
            endpoints: vec![ep("ep:c:a", "Headphones", "{a}"), ep("ep:c:b", "HDMI", "{b}")],
            monitors: vec![],
        };
        let apo = OtherProcessor {
            name: "Nahimic APO".into(),
            kind: ProcessorKind::Apo,
            advice: String::new(),
            clsid: None,
        };
        let out =
            assemble(&report, |e| if e.fx_guid == "{a}" { vec![apo.clone()] } else { vec![] }, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].endpoint, "ep:c:a");

        let sonar = vendors_running(["SteelSeriesSonar.exe"]);
        let out = assemble(&report, |_| vec![], &sonar);
        assert_eq!(out.len(), 2, "software is reported on every output");
    }

    #[test]
    fn guid_scan_ignores_malformed_groups() {
        assert!(guids_in("{not-a-guid}").is_empty());
        assert_eq!(guids_in("x{62DC1A93-AE24-464C-A43E-452F824C4250}y").len(), 1);
    }

    #[test]
    fn the_nil_guid_is_not_an_effect() {
        let s = store(vec![(
            "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5",
            RegValue {
                kind: RegKind::Sz,
                data: sz_bytes("{00000000-0000-0000-0000-000000000000}"),
            },
        )]);
        assert!(third_party_apos(&s, |_| None).is_empty());
    }
}
