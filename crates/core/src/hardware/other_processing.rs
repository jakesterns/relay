//! Other processing on an output, detected read-only (S41, refined S41b).
//!
//! Relay's headphone correction assumes it is the only thing shaping the
//! sound. It often is not: a vendor APO sits in the endpoint's FX chain
//! (Nahimic, Dolby, Realtek, Sonic Studio), Windows spatial sound is on, or
//! vendor software runs its own EQ (SteelSeries Sonar, G HUB, Synapse,
//! Voicemeeter). Correction on top of someone else's EQ is not accurate, and
//! the user deserves to be told, once per processor, without noise.
//!
//! Three sources, all read-only, never modified or disabled:
//! - The endpoint's FX property store: effect CLSIDs in the effect slots
//!   only (not the property-page / UI slots), that are registered as audio
//!   processing objects (`HKLM\SOFTWARE\Classes\AudioEngine\
//!   AudioProcessingObjects\{clsid}`), and are neither Microsoft's nor
//!   Relay's. Deduplicated by CLSID, then grouped to one line per vendor.
//! - The endpoint's spatial sound format (MMDevices `Properties` value
//!   [`PKEY_SPATIAL_FORMAT`]).
//! - Running process image names against a small table of vendor audio
//!   apps. An app is only attached to outputs of its own hardware (by
//!   endpoint name or USB VID), or, for system-wide processors that create
//!   their own virtual outputs (Sonar, Nahimic, Voicemeeter), to those.

use serde::{Deserialize, Serialize};

/// Where the finding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessorKind {
    /// Audio effects installed on this output's FX chain. Certain.
    Apo,
    /// Windows spatial sound (Windows Sonic, Dolby Atmos, DTS) is on. Certain.
    Spatial,
    /// Vendor audio software for this output's hardware is running.
    Software,
}

/// One other processor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherProcessor {
    /// Plain name, e.g. "Realtek audio effects (Realtek Audio Console)".
    pub name: String,
    pub kind: ProcessorKind,
    /// What to do for accurate correction, e.g. "set Sonar's EQ flat".
    pub advice: String,
    /// The effect CLSIDs behind an APO line, deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clsids: Vec<String>,
}

/// Everything else processing one output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointProcessing {
    /// The output's endpoint key ([`super::listening::listening_key`]).
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

/// Windows' own effect CLSIDs as they appear in FX stores.
const MICROSOFT_APOS: &[&str] = &[
    "{62dc1a93-ae24-464c-a43e-452f824c4250}", // WMALFXGFXDSP (legacy LFX/GFX)
    "{637c490d-eee3-4c0a-973f-371958802da2}", // MsApoFxProxy (SFX)
    "{5860e1c5-f95c-4a7a-8ec8-8aef24f379a1}", // MsApoFxProxy (MFX)
    "{c9453e73-8c5c-4463-9984-af8bab2f5447}", // MsApoFxProxy (EFX)
    "{13ab3ebd-137e-4903-9d89-60be8277fd17}", // Windows enhancements (EFX)
];

const NIL_GUID: &str = "{00000000-0000-0000-0000-000000000000}";

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
/// this format id. Processing-mode lists live under a different id.
const FX_FMTID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},";

/// The pids under [`FX_FMTID`] that hold effect CLSIDs: pre-mix (1),
/// post-mix (2), stream (5), mode (6), endpoint (7), offload stream (11) and
/// mode (12), composite SFX/MFX/EFX (13/14/15). Everything else is not an
/// effect: association (0), the property-page / UI CLSID (3; Realtek's
/// "RtkAdvPropPage Class" lives there), friendly name (4), keyword detector
/// slots, and anything a vendor adds.
const EFFECT_PIDS: &[u32] = &[1, 2, 5, 6, 7, 11, 12, 13, 14, 15];

/// Every effect CLSID in an FX store's effect slots, lowercase, in store
/// order, deduplicated (legacy and composite slots often name the same one).
pub fn effect_clsids(store: &relay_apo::fxstore::FxStore) -> Vec<String> {
    use relay_apo::regfile::RegKind;
    let mut out = Vec::new();
    let Some(root) = store.keys.iter().find(|(k, _)| k.is_empty()).map(|(_, v)| v) else {
        return out;
    };
    for (name, value) in root.iter() {
        let lower = name.to_ascii_lowercase();
        let Some(pid) = lower.strip_prefix(FX_FMTID).and_then(|p| p.trim().parse::<u32>().ok())
        else {
            continue;
        };
        if !EFFECT_PIDS.contains(&pid) {
            continue;
        }
        if !matches!(value.kind, RegKind::Sz | RegKind::MultiSz | RegKind::ExpandSz) {
            continue;
        }
        for g in guids_in(&utf16_text(&value.data)) {
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out
}

fn utf16_text(data: &[u8]) -> String {
    let wide: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&wide)
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

/// What the registry says about one CLSID. `None` from the lookup means it
/// is not registered as an audio processing object, so it is not reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApoRegistration {
    /// `FriendlyName` under `AudioProcessingObjects\{clsid}`.
    pub friendly: Option<String>,
    /// The COM class's default value under `Classes\CLSID\{clsid}`.
    pub com_name: Option<String>,
}

impl ApoRegistration {
    fn names(&self) -> impl Iterator<Item = &str> {
        self.friendly.iter().chain(self.com_name.iter()).map(String::as_str)
    }
    fn display(&self) -> Option<&str> {
        self.names().map(str::trim).find(|n| !n.is_empty())
    }
}

/// An APO vendor: patterns found in the APO's registered names, the line to
/// show and the app that controls it.
pub struct ApoVendor {
    pub patterns: &'static [&'static str],
    pub display: &'static str,
    pub app: &'static str,
}

/// Kept small and checked by hand. Patterns match a word start (`rtk`
/// matches "RtkAPOSFX") or, with a space or `&`, a substring.
pub const APO_VENDORS: &[ApoVendor] = &[
    ApoVendor {
        patterns: &["realtek", "rtk"],
        display: "Realtek audio effects",
        app: "Realtek Audio Console",
    },
    ApoVendor { patterns: &["nahimic", "volute", "avolute"], display: "Nahimic", app: "Nahimic" },
    ApoVendor { patterns: &["dolby"], display: "Dolby audio effects", app: "Dolby Access" },
    ApoVendor { patterns: &["dts"], display: "DTS audio effects", app: "DTS Sound Unbound" },
    ApoVendor {
        patterns: &["waves", "maxxaudio"],
        display: "Waves MaxxAudio",
        app: "MaxxAudio Pro",
    },
    ApoVendor { patterns: &["sonar"], display: "SteelSeries Sonar", app: "SteelSeries GG" },
    ApoVendor {
        patterns: &["thx", "razer"],
        display: "Razer THX Spatial Audio",
        app: "Razer Synapse",
    },
    ApoVendor {
        patterns: &["creative", "sbx", "sound blaster"],
        display: "Creative audio effects",
        app: "Creative App",
    },
    ApoVendor {
        patterns: &["conexant", "synaptics", "smartaudio"],
        display: "Conexant/Synaptics audio effects",
        app: "SmartAudio",
    },
    ApoVendor {
        patterns: &["bang & olufsen", "b&o", "bangolufsen"],
        display: "Bang & Olufsen audio",
        app: "B&O Audio Control",
    },
];

fn pattern_hits(hay: &str, pat: &str) -> bool {
    let hay = hay.to_ascii_lowercase();
    if pat.contains(' ') {
        return hay.contains(pat);
    }
    hay.split(|c: char| !(c.is_ascii_alphanumeric() || c == '&')).any(|w| w.starts_with(pat))
}

/// The vendor an APO's registered names point to.
pub fn apo_vendor(reg: &ApoRegistration) -> Option<&'static ApoVendor> {
    APO_VENDORS.iter().find(|v| reg.names().any(|n| v.patterns.iter().any(|p| pattern_hits(n, p))))
}

/// The third-party APOs in one FX store, one line per vendor. `reg_of`
/// looks a CLSID up in the registry; `None` means it is not an APO (a
/// property page, a UI object) and it is skipped.
pub fn third_party_apos(
    store: &relay_apo::fxstore::FxStore,
    reg_of: impl Fn(&str) -> Option<ApoRegistration>,
) -> Vec<OtherProcessor> {
    let mut out: Vec<OtherProcessor> = Vec::new();
    for c in effect_clsids(store) {
        if c == NIL_GUID || classify_clsid(&c) != ApoClass::Other {
            continue;
        }
        let Some(reg) = reg_of(&c) else { continue };
        let (name, advice) = match apo_vendor(&reg) {
            Some(v) => (
                format!("{} ({})", v.display, v.app),
                format!("turn them off or set them flat in {}", v.app),
            ),
            None => (
                reg.display().unwrap_or("Another audio effect").to_owned(),
                "turn it off in the app that installed it".to_owned(),
            ),
        };
        match out.iter_mut().find(|o| o.name == name) {
            Some(o) => o.clsids.push(c),
            None => {
                out.push(OtherProcessor { name, kind: ProcessorKind::Apo, advice, clsids: vec![c] })
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Spatial sound

/// The endpoint property that holds the spatial sound format's CLSID
/// (MMDevices `Render\{guid}\Properties`). Absent, empty or the nil GUID
/// means spatial sound is off.
pub const PKEY_SPATIAL_FORMAT: &str = "{f8d2c69d-0989-4cc6-b197-a9e152f3b5d3},3";
/// Windows Sonic for Headphones' spatial format CLSID.
pub const WINDOWS_SONIC: &str = "{b53b4c27-1c42-4148-8533-507b42b6a0f7}";

/// Read the spatial format value. `name_of` names a CLSID from the registry
/// (Dolby Atmos and DTS register their own).
pub fn spatial_from_value(
    value: Option<&str>,
    name_of: impl Fn(&str) -> Option<String>,
) -> Option<OtherProcessor> {
    let clsid = guids_in(value?).into_iter().next()?;
    if clsid == NIL_GUID {
        return None;
    }
    let named = name_of(&clsid).unwrap_or_default();
    let lower = named.to_ascii_lowercase();
    let (name, advice) = if clsid == WINDOWS_SONIC {
        (
            "Windows Sonic for Headphones".to_owned(),
            "turn spatial sound off for this output in Windows Sound settings",
        )
    } else if lower.contains("dolby") {
        (
            "Dolby Atmos for Headphones".to_owned(),
            "turn it off in Dolby Access or Windows Sound settings",
        )
    } else if lower.contains("dts") {
        ("DTS Headphone:X".to_owned(), "turn it off in DTS Sound Unbound or Windows Sound settings")
    } else {
        let n = if named.trim().is_empty() {
            "Spatial sound".to_owned()
        } else {
            named.trim().to_owned()
        };
        (n, "turn spatial sound off for this output in Windows Sound settings")
    };
    Some(OtherProcessor {
        name,
        kind: ProcessorKind::Spatial,
        advice: advice.to_owned(),
        clsids: vec![clsid],
    })
}

// ---------------------------------------------------------------------------
// Vendor software, tied to the hardware it serves

/// A hardware vendor as seen on an endpoint: friendly-name words and USB
/// (or HD Audio) vendor ids in the device's hardware id.
pub struct DeviceVendor {
    pub name: &'static str,
    pub names: &'static [&'static str],
    pub vids: &'static [&'static str],
}

pub const DEVICE_VENDORS: &[DeviceVendor] = &[
    DeviceVendor { name: "Logitech", names: &["logitech", "logi "], vids: &["046D"] },
    DeviceVendor { name: "Astro", names: &["astro"], vids: &["9886"] },
    DeviceVendor { name: "Corsair", names: &["corsair"], vids: &["1B1C"] },
    DeviceVendor { name: "SteelSeries", names: &["steelseries", "arctis"], vids: &["1038"] },
    DeviceVendor { name: "Razer", names: &["razer"], vids: &["1532"] },
    DeviceVendor { name: "HyperX", names: &["hyperx"], vids: &["0951", "03F0"] },
    DeviceVendor { name: "Turtle Beach", names: &["turtle beach"], vids: &["10F5"] },
    DeviceVendor { name: "RODE", names: &["rode", "røde"], vids: &["19F7"] },
    DeviceVendor { name: "Realtek", names: &["realtek"], vids: &["0BDA", "10EC"] },
];

/// The hardware vendors an endpoint belongs to, by friendly name words and
/// by `VID_xxxx` / `VEN_xxxx` in its hardware id.
pub fn endpoint_vendors(name: &str, hw_id: &str) -> Vec<&'static str> {
    let name = name.to_lowercase();
    let hw = hw_id.to_ascii_uppercase();
    DEVICE_VENDORS
        .iter()
        .filter(|v| {
            v.names.iter().any(|n| {
                if n.ends_with(' ') || n.contains(' ') {
                    name.contains(n)
                } else {
                    name.split(|c: char| !c.is_alphanumeric()).any(|w| w == *n)
                }
            }) || v
                .vids
                .iter()
                .any(|id| hw.contains(&format!("VID_{id}")) || hw.contains(&format!("VEN_{id}")))
        })
        .map(|v| v.name)
        .collect()
}

/// Which outputs a vendor app can process.
pub enum Scope {
    /// Outputs of these hardware vendors ([`DEVICE_VENDORS`] names).
    Devices(&'static [&'static str]),
    /// The app's own virtual outputs, by a word in their friendly name.
    OwnEndpoints(&'static [&'static str]),
}

/// A known vendor audio app.
pub struct VendorApp {
    pub exe: &'static str,
    pub name: &'static str,
    pub advice: &'static str,
    pub scope: Scope,
}

const SONAR: Scope = Scope::OwnEndpoints(&["sonar"]);
const RAZER: Scope = Scope::Devices(&["Razer"]);
const NAHIMIC: Scope = Scope::OwnEndpoints(&["nahimic"]);
const DOLBY: Scope = Scope::OwnEndpoints(&["dolby"]);
const REALTEK: Scope = Scope::Devices(&["Realtek"]);
const LOGI: Scope = Scope::Devices(&["Logitech", "Astro"]);
const RODE: Scope = Scope::Devices(&["RODE"]);
const VOICEMEETER: Scope = Scope::OwnEndpoints(&["voicemeeter"]);

/// Kept small and checked by hand. Several exes may map to one product.
pub const VENDOR_APPS: &[VendorApp] = &[
    VendorApp {
        exe: "SteelSeriesSonar.exe",
        name: "SteelSeries Sonar",
        advice: "set Sonar's EQ flat",
        scope: SONAR,
    },
    VendorApp {
        exe: "SteelSeriesGG.exe",
        name: "SteelSeries Sonar",
        advice: "set Sonar's EQ flat",
        scope: SONAR,
    },
    VendorApp {
        exe: "RazerAppEngine.exe",
        name: "Razer Synapse",
        advice: "turn THX Spatial Audio and Synapse EQ off",
        scope: RAZER,
    },
    VendorApp {
        exe: "Razer Synapse 3.exe",
        name: "Razer Synapse",
        advice: "turn THX Spatial Audio and Synapse EQ off",
        scope: RAZER,
    },
    VendorApp {
        exe: "THXAudioService.exe",
        name: "THX Spatial Audio",
        advice: "turn THX Spatial Audio off",
        scope: RAZER,
    },
    VendorApp {
        exe: "NahimicSvc64.exe",
        name: "Nahimic",
        advice: "turn Nahimic's effects off",
        scope: NAHIMIC,
    },
    VendorApp {
        exe: "NahimicSvc32.exe",
        name: "Nahimic",
        advice: "turn Nahimic's effects off",
        scope: NAHIMIC,
    },
    VendorApp {
        exe: "DolbyAccess.exe",
        name: "Dolby Access",
        advice: "turn Dolby Atmos for Headphones off",
        scope: DOLBY,
    },
    VendorApp {
        exe: "DolbyDAX3API.exe",
        name: "Dolby Audio",
        advice: "turn Dolby's EQ off",
        scope: DOLBY,
    },
    VendorApp {
        exe: "RtkUWP.exe",
        name: "Realtek Audio Console",
        advice: "turn Realtek's effects off",
        scope: REALTEK,
    },
    VendorApp {
        exe: "lghub.exe",
        name: "Logitech G HUB",
        advice: "set G HUB's EQ flat",
        scope: LOGI,
    },
    VendorApp {
        exe: "lghub_agent.exe",
        name: "Logitech G HUB",
        advice: "set G HUB's EQ flat",
        scope: LOGI,
    },
    VendorApp {
        exe: "iCUE.exe",
        name: "Corsair iCUE",
        advice: "set iCUE's EQ flat",
        scope: Scope::Devices(&["Corsair"]),
    },
    VendorApp {
        exe: "RODE Central.exe",
        name: "RODE Central",
        advice: "set the interface's EQ flat on this output",
        scope: RODE,
    },
    VendorApp {
        exe: "RODECaster App.exe",
        name: "RODECaster app",
        advice: "set the RODECaster's EQ flat on this output",
        scope: RODE,
    },
    VendorApp {
        exe: "voicemeeter8x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
        scope: VOICEMEETER,
    },
    VendorApp {
        exe: "voicemeeterpro_x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
        scope: VOICEMEETER,
    },
    VendorApp {
        exe: "voicemeeter_x64.exe",
        name: "Voicemeeter",
        advice: "set Voicemeeter's EQ flat",
        scope: VOICEMEETER,
    },
    VendorApp {
        exe: "FxSound.exe",
        name: "FxSound",
        advice: "turn FxSound off",
        scope: Scope::OwnEndpoints(&["fxsound"]),
    },
];

/// Known vendor apps among `exes`, one per product, table order.
pub fn vendors_running<'a>(exes: impl IntoIterator<Item = &'a str>) -> Vec<&'static VendorApp> {
    let running: Vec<String> = exes.into_iter().map(|e| e.to_ascii_lowercase()).collect();
    let mut out: Vec<&'static VendorApp> = Vec::new();
    for app in VENDOR_APPS {
        if running.iter().any(|e| e == &app.exe.to_ascii_lowercase())
            && !out.iter().any(|o| o.name == app.name)
        {
            out.push(app);
        }
    }
    out
}

/// Does `app` process an output with this name and hardware id?
pub fn app_applies(app: &VendorApp, name: &str, hw_id: &str) -> bool {
    match app.scope {
        Scope::Devices(vendors) => {
            let mine = endpoint_vendors(name, hw_id);
            vendors.iter().any(|v| mine.contains(v))
        }
        Scope::OwnEndpoints(words) => {
            let n = name.to_lowercase();
            words.iter().any(|w| n.contains(w))
        }
    }
}

// ---------------------------------------------------------------------------
// Assembly

/// What the live scan read for one output.
#[derive(Debug, Clone, Default)]
pub struct EndpointFacts {
    pub apos: Vec<OtherProcessor>,
    pub spatial: Option<OtherProcessor>,
    /// The device's hardware id (`{1}.USB\VID_1532&PID_...`), may be empty.
    pub hw_id: String,
}

/// Put the findings together for every output in `report`: APO lines, then
/// spatial sound, then vendor apps for this output's hardware. An app whose
/// name already appears in an APO line (Realtek Audio Console) is not
/// repeated.
pub fn assemble(
    report: &super::ProbeReport,
    facts_for: impl Fn(&super::EndpointInfo) -> EndpointFacts,
    running: &[&'static VendorApp],
) -> Vec<EndpointProcessing> {
    let mut eps = report.endpoints.clone();
    super::listening::unique_endpoint_keys(&mut eps);
    eps.iter()
        .map(|ep| {
            let facts = facts_for(ep);
            let mut processors = facts.apos;
            processors.extend(facts.spatial);
            for app in running {
                if !app_applies(app, &ep.name, &facts.hw_id) {
                    continue;
                }
                if processors.iter().any(|p| p.name.contains(app.name)) {
                    continue;
                }
                processors.push(OtherProcessor {
                    name: app.name.to_owned(),
                    kind: ProcessorKind::Software,
                    advice: app.advice.to_owned(),
                    clsids: Vec::new(),
                });
            }
            EndpointProcessing { endpoint: super::listening::listening_key(&eps, ep), processors }
        })
        .filter(|e| !e.processors.is_empty())
        .collect()
}

/// The live scan. Registry and process list are only read.
#[cfg(windows)]
pub fn scan(report: &super::ProbeReport) -> Vec<EndpointProcessing> {
    let exes = crate::processes::list_all();
    let running = vendors_running(exes.iter().map(String::as_str));
    assemble(
        report,
        |ep| {
            if ep.fx_guid.is_empty() {
                return EndpointFacts::default();
            }
            let apos = match relay_apo::livereg::LiveRegistry::read_fx_store(&ep.fx_guid) {
                Ok(store) => third_party_apos(&store, win::apo_registration),
                Err(_) => Vec::new(),
            };
            let props = format!(r"{}\{}\Properties", relay_apo::ids::RENDER_ROOT, ep.fx_guid);
            let spatial = spatial_from_value(
                win::reg_sz(&props, Some(PKEY_SPATIAL_FORMAT)).as_deref(),
                |c| win::reg_sz(&relay_apo::ids::clsid_key(c), None),
            );
            let hw_id = win::reg_sz(&props, Some(PKEY_DEVICE_HW_ID)).unwrap_or_default();
            EndpointFacts { apos, spatial, hw_id }
        },
        &running,
    )
}

/// The endpoint's device instance (`{1}.USB\VID_..&PID_..`), in its
/// MMDevices `Properties`.
#[cfg(windows)]
const PKEY_DEVICE_HW_ID: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},2";

#[cfg(not(windows))]
pub fn scan(_report: &super::ProbeReport) -> Vec<EndpointProcessing> {
    Vec::new()
}

#[cfg(windows)]
mod win {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A REG_SZ under HKLM (`value` None = the default value). Read-only.
    pub fn reg_sz(subkey: &str, value: Option<&str>) -> Option<String> {
        let key = wide(subkey);
        let val = value.map(wide);
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: valid out-buffer and length; RegGetValueW NUL-terminates
        // and never writes past `len`.
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key.as_ptr()),
                val.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
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
        Some(String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').trim().to_owned())
    }

    fn key_exists(subkey: &str) -> bool {
        let key = wide(subkey);
        let mut h = HKEY::default();
        // SAFETY: opens read-only and closes the handle it got.
        unsafe {
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), None, KEY_READ, &mut h)
                .is_ok()
            {
                let _ = RegCloseKey(h);
                true
            } else {
                false
            }
        }
    }

    /// `Some` only when the CLSID is registered as an audio processing object.
    pub fn apo_registration(clsid: &str) -> Option<super::ApoRegistration> {
        let apo = format!(
            r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{}",
            clsid.to_ascii_uppercase()
        );
        if !key_exists(&apo) {
            return None;
        }
        Some(super::ApoRegistration {
            friendly: reg_sz(&apo, Some("FriendlyName")),
            com_name: reg_sz(&relay_apo::ids::clsid_key(clsid), None),
        })
    }
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

    fn sz(v: &str) -> RegValue {
        RegValue { kind: RegKind::Sz, data: sz_bytes(v) }
    }

    fn apo(friendly: &str) -> Option<ApoRegistration> {
        Some(ApoRegistration { friendly: Some(friendly.into()), com_name: None })
    }

    const NAHIMIC: &str = "{D2B6A7F4-3C51-4B9A-9E0D-1A2B3C4D5E6F}";
    const RTK_SFX: &str = "{e0a941a0-88a2-4df5-8d6b-dd20bb06e8fb}";
    const RTK_MFX: &str = "{d3993a3f-0000-4a1c-8d6b-dd20bb06e8fb}";
    const RTK_PROP: &str = "{7a1f4a1e-4a2b-4c3d-9e8f-0a1b2c3d4e5f}";
    const FX: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";

    #[test]
    fn clsids_classify_relay_microsoft_and_other() {
        assert_eq!(classify_clsid(relay_apo::ids::APO_CLSID), ApoClass::Relay);
        assert_eq!(classify_clsid("{62DC1A93-AE24-464C-A43E-452F824C4250}"), ApoClass::Microsoft);
        assert_eq!(classify_clsid(NAHIMIC), ApoClass::Other);
    }

    /// The second PC's Realtek HD Audio 6.0.1.8666 store: SFX and MFX each in
    /// the legacy and composite slots, plus the property page in the UI slot.
    fn realtek_store() -> FxStore {
        store(vec![
            (&format!("{FX},3") as &str, sz(RTK_PROP)),
            (&format!("{FX},5"), sz(RTK_SFX)),
            (&format!("{FX},6"), sz(RTK_MFX)),
            (&format!("{FX},13"), RegValue { kind: RegKind::MultiSz, data: multi_sz(&[RTK_SFX]) }),
            (&format!("{FX},14"), RegValue { kind: RegKind::MultiSz, data: multi_sz(&[RTK_MFX]) }),
        ])
    }

    fn realtek_reg(c: &str) -> Option<ApoRegistration> {
        match c {
            RTK_SFX => {
                Some(ApoRegistration { friendly: None, com_name: Some("RtkAPOSFX Class".into()) })
            }
            RTK_MFX => apo("Realtek MFX APO"),
            // RtkAdvPropPage Class is a COM class, not an APO.
            _ => None,
        }
    }

    #[test]
    fn realtek_store_is_one_line_without_the_property_page() {
        let s = realtek_store();
        let ids = effect_clsids(&s);
        assert_eq!(ids.len(), 2, "deduplicated, UI slot skipped: {ids:?}");
        assert!(!ids.contains(&RTK_PROP.to_owned()));
        let found = third_party_apos(&s, realtek_reg);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Realtek audio effects (Realtek Audio Console)");
        assert!(found[0].advice.contains("Realtek Audio Console"));
        assert_eq!(found[0].clsids.len(), 2);
    }

    /// S42b: Relay now registers under AudioEngine\AudioProcessingObjects
    /// like any other APO, so `reg_of` finds it — it must still read as
    /// Relay, never as somebody else's processing. Installed Relay store:
    /// legacy EFX + composite EFX both name our CLSID.
    #[test]
    fn relay_with_its_audio_engine_registration_is_not_other() {
        let plan = relay_apo::fxstore::plan_install(
            &FxStore::empty(),
            "{11111111-2222-3333-4444-555555555555}",
            r"C:\x.dll",
        );
        let registered = |c: &str| {
            c.eq_ignore_ascii_case(relay_apo::ids::APO_CLSID)
                .then(|| apo(relay_apo::ids::APO_FRIENDLY_NAME))
                .flatten()
        };
        assert!(effect_clsids(&plan.new_store)
            .contains(&relay_apo::ids::APO_CLSID.to_ascii_lowercase()));
        assert!(third_party_apos(&plan.new_store, registered).is_empty());
        assert_eq!(
            classify_clsid(&relay_apo::ids::APO_CLSID.to_ascii_lowercase()),
            ApoClass::Relay
        );
    }

    #[test]
    fn a_clsid_not_registered_as_an_apo_is_not_reported() {
        // Even in an effect slot: e.g. a property page a driver put there.
        let s = store(vec![(&format!("{FX},5") as &str, sz(RTK_PROP))]);
        assert!(third_party_apos(&s, |_| None).is_empty());
    }

    #[test]
    fn unknown_vendor_keeps_its_registered_name() {
        let s = store(vec![
            (
                relay_apo::ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID,
                RegValue {
                    kind: RegKind::MultiSz,
                    data: multi_sz(&[
                        relay_apo::ids::APO_CLSID,
                        "{11111111-2222-3333-4444-555555555555}",
                    ]),
                },
            ),
            (&format!("{FX},5"), sz("{637c490d-eee3-4c0a-973f-371958802da2}")),
            (
                relay_apo::ids::PKEY_EFX_MODES,
                RegValue {
                    kind: RegKind::MultiSz,
                    data: multi_sz(&[relay_apo::ids::MODE_DEFAULT]),
                },
            ),
        ]);
        let found = third_party_apos(&s, |_| apo("Acme Loudness"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Acme Loudness");
        let unnamed = third_party_apos(&s, |_| Some(ApoRegistration::default()));
        assert_eq!(unnamed[0].name, "Another audio effect");
    }

    #[test]
    fn vendor_table_names_apos() {
        let name = |n: &str| apo_vendor(&apo(n).unwrap()).map(|v| v.display);
        assert_eq!(name("RtkAPOSFX Class"), Some("Realtek audio effects"));
        assert_eq!(name("A-Volute Nh3 APO"), Some("Nahimic"));
        assert_eq!(name("Dolby DAX3 APO"), Some("Dolby audio effects"));
        assert_eq!(name("DTS APO4x"), Some("DTS audio effects"));
        assert_eq!(name("Waves MaxxAudio SFX"), Some("Waves MaxxAudio"));
        assert_eq!(name("THX Spatial"), Some("Razer THX Spatial Audio"));
        assert_eq!(name("B&O Audio Effects"), Some("Bang & Olufsen audio"));
        assert_eq!(name("Acme"), None);
        assert_eq!(name("Widths processor"), None, "dts only at a word start");
    }

    #[test]
    fn empty_nil_or_relay_only_stores_report_nothing() {
        assert!(third_party_apos(&FxStore::empty(), |_| apo("x")).is_empty());
        let s = store(vec![(
            relay_apo::ids::PKEY_FX_ENDPOINT_EFFECT_CLSID,
            sz(relay_apo::ids::APO_CLSID),
        )]);
        assert!(third_party_apos(&s, |_| apo("x")).is_empty());
        let s = store(vec![(&format!("{FX},5") as &str, sz(NIL_GUID))]);
        assert!(third_party_apos(&s, |_| apo("x")).is_empty());
    }

    #[test]
    fn spatial_value_parses() {
        assert!(spatial_from_value(None, |_| None).is_none());
        assert!(spatial_from_value(Some(""), |_| None).is_none());
        assert!(spatial_from_value(Some(NIL_GUID), |_| None).is_none());
        let sonic =
            spatial_from_value(Some("{B53B4C27-1C42-4148-8533-507B42B6A0F7}"), |_| None).unwrap();
        assert_eq!(sonic.name, "Windows Sonic for Headphones");
        assert_eq!(sonic.kind, ProcessorKind::Spatial);
        let atmos = spatial_from_value(Some("{11111111-2222-3333-4444-555555555555}"), |_| {
            Some("Dolby Atmos Spatial Sound".into())
        })
        .unwrap();
        assert_eq!(atmos.name, "Dolby Atmos for Headphones");
        let dts = spatial_from_value(Some("{11111111-2222-3333-4444-555555555555}"), |_| {
            Some("DTS Sound Unbound".into())
        })
        .unwrap();
        assert_eq!(dts.name, "DTS Headphone:X");
        let other =
            spatial_from_value(Some("{11111111-2222-3333-4444-555555555555}"), |_| None).unwrap();
        assert_eq!(other.name, "Spatial sound");
    }

    #[test]
    fn endpoint_vendors_by_name_and_vid() {
        assert_eq!(endpoint_vendors("Headset (Logitech PRO X)", ""), vec!["Logitech"]);
        assert_eq!(
            endpoint_vendors("Speakers (G735)", r"{1}.USB\VID_046D&PID_0AFE&MI_00\7&1"),
            vec!["Logitech"]
        );
        assert_eq!(
            endpoint_vendors("Headphones (VOID ELITE)", r"USB\VID_1B1C&PID_0A55"),
            vec!["Corsair"]
        );
        assert_eq!(endpoint_vendors("Arctis Nova 7", ""), vec!["SteelSeries"]);
        assert_eq!(endpoint_vendors("Cloud II", r"USB\VID_0951&PID_16A4"), vec!["HyperX"]);
        assert_eq!(endpoint_vendors("Cloud III", r"USB\VID_03F0&PID_0D84"), vec!["HyperX"]);
        assert_eq!(endpoint_vendors("Stealth 700", r"USB\VID_10F5&PID_0210"), vec!["Turtle Beach"]);
        assert_eq!(endpoint_vendors("A50", r"USB\VID_9886&PID_002C"), vec!["Astro"]);
        assert_eq!(endpoint_vendors("Kraken", r"USB\VID_1532&PID_0527"), vec!["Razer"]);
        assert_eq!(
            endpoint_vendors("Main (RODECaster Duo)", r"USB\VID_19F7&PID_0079"),
            vec!["RODE"]
        );
        assert_eq!(
            endpoint_vendors("Speakers", r"{1}.HDAUDIO\FUNC_01&VEN_10EC&DEV_0897"),
            vec!["Realtek"]
        );
        assert!(endpoint_vendors("Speakers (USB DAC)", r"USB\VID_262A&PID_9302").is_empty());
        assert!(endpoint_vendors("Episode speakers", "").is_empty(), "rode is a whole word");
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
        let names: Vec<&str> = found.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["SteelSeries Sonar", "Logitech G HUB"]);
        assert!(vendors_running(["chrome.exe", "relay-core.exe"]).is_empty());
    }

    #[test]
    fn vendor_table_has_no_duplicate_exes_and_known_vendors() {
        let mut seen = std::collections::HashSet::new();
        for a in VENDOR_APPS {
            assert!(seen.insert(a.exe.to_ascii_lowercase()), "duplicate {}", a.exe);
            assert!(!a.advice.is_empty());
            if let Scope::Devices(vs) = a.scope {
                for v in vs {
                    assert!(DEVICE_VENDORS.iter().any(|d| d.name == *v), "unknown vendor {v}");
                }
            }
        }
    }

    fn ep(key: &str, name: &str, fx: &str) -> super::super::EndpointInfo {
        super::super::EndpointInfo {
            key: key.into(),
            name: name.into(),
            default: false,
            fx_guid: fx.into(),
        }
    }

    /// The second PC: Realtek onboard, RODECaster Duo Main + Chat, G HUB and
    /// iCUE running with no Logitech/Corsair output, Realtek Audio Console.
    #[test]
    fn apps_attach_only_to_their_own_hardware() {
        use super::super::ProbeReport;
        let report = ProbeReport {
            endpoints: vec![
                ep("ep:c:rtk", "Speakers (Realtek(R) Audio)", "{r}"),
                ep("ep:c:rode", "Main (RODECaster Duo)", "{m}"),
                ep("ep:c:rode", "Chat (RODECaster Duo)", "{c}"),
                ep("ep:c:sonar", "SteelSeries Sonar - Gaming", "{s}"),
            ],
            monitors: vec![],
        };
        let running = vendors_running([
            "lghub.exe",
            "iCUE.exe",
            "RtkUWP.exe",
            "RODECaster App.exe",
            "SteelSeriesSonar.exe",
        ]);
        let out = assemble(
            &report,
            |e| match e.fx_guid.as_str() {
                "{r}" => EndpointFacts {
                    apos: third_party_apos(&realtek_store(), realtek_reg),
                    spatial: spatial_from_value(Some(WINDOWS_SONIC), |_| None),
                    hw_id: r"{1}.HDAUDIO\FUNC_01&VEN_10EC&DEV_0897".into(),
                },
                "{m}" | "{c}" => EndpointFacts {
                    hw_id: r"{1}.USB\VID_19F7&PID_0079".into(),
                    ..Default::default()
                },
                _ => EndpointFacts::default(),
            },
            &running,
        );
        let names = |k: &str| -> Vec<String> {
            out.iter()
                .find(|e| e.endpoint == k)
                .map(|e| e.processors.iter().map(|p| p.name.clone()).collect())
                .unwrap_or_default()
        };
        // Realtek: one APO line (Realtek Audio Console not repeated) + spatial.
        assert_eq!(
            names("ep:c:rtk"),
            vec![
                "Realtek audio effects (Realtek Audio Console)".to_owned(),
                "Windows Sonic for Headphones".to_owned()
            ]
        );
        // Unique keys per RODECaster output; G HUB / iCUE nowhere.
        assert_eq!(names("ep:c:rode#m"), vec!["RODECaster app".to_owned()]);
        assert_eq!(names("ep:c:rode#c"), vec!["RODECaster app".to_owned()]);
        assert_eq!(names("ep:c:sonar"), vec!["SteelSeries Sonar".to_owned()]);
        assert!(out
            .iter()
            .flat_map(|e| &e.processors)
            .all(|p| p.name != "Logitech G HUB" && p.name != "Corsair iCUE"));
    }

    #[test]
    fn guid_scan_ignores_malformed_groups() {
        assert!(guids_in("{not-a-guid}").is_empty());
        assert_eq!(guids_in("x{62DC1A93-AE24-464C-A43E-452F824C4250}y").len(), 1);
    }
}
