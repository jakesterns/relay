//! Install / uninstall planner for one endpoint's `FxProperties` store.
//!
//! Everything here operates on a registry *image* ([`FxStore`]), never the
//! live registry — the pure planner is unit-tested against exported .reg
//! fixtures before any code touches HKLM (brief risk #2). The live backend
//! is [`crate::livereg`]; it only executes plans produced here.
//!
//! Install semantics (decision recorded in the M3b plan — EFX placement):
//! - append [`ids::APO_CLSID`] to the REG_MULTI_SZ at
//!   [`ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID`] (create if absent,
//!   keep existing entries, never duplicate);
//! - set [`ids::PKEY_FX_ENDPOINT_EFFECT_CLSID`] to our CLSID **only if that
//!   value is absent** — never clobber a vendor EFX; the composite key is
//!   the chaining mechanism;
//! - ensure [`ids::MODE_DEFAULT`] is listed in the REG_MULTI_SZ at
//!   [`ids::PKEY_EFX_MODES`] (create/append, no duplicates).
//!
//! Nothing else is touched: every other key, value name and data byte of
//! the store is carried over verbatim, and [`plan_uninstall`] returns the
//! [`Backup`]'s store byte-for-byte. The fixture tests prove both by
//! serializing before/after back to .reg text and comparing strings.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids;
use crate::regfile::{
    multi_sz_bytes, parse_multi_sz, sz_bytes, KeyMap, RegKind, RegValue, ValueMap,
};

/// Errors from building an [`FxStore`] out of a parsed .reg map.
#[derive(Debug, Error)]
pub enum FxStoreError {
    #[error("no FxProperties key for endpoint {0} in the parsed data")]
    EndpointNotFound(String),
}

// ---------------------------------------------------------------------------
// FxStore

/// The FX property store of exactly one render endpoint: the `FxProperties`
/// key itself plus all of its sub-keys, addressed by path *relative* to
/// `FxProperties` (`""` is the key itself). Keeping paths relative is what
/// scopes every plan to a single endpoint — there is no way to express a
/// write outside its `FxProperties` subtree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FxStore {
    /// Relative key path → values, in registry enumeration order.
    pub keys: KeyMap,
}

impl FxStore {
    /// An empty store (endpoint whose `FxProperties` key has no values yet).
    pub fn empty() -> Self {
        let mut keys = KeyMap::new();
        keys.insert(String::new(), ValueMap::new());
        Self { keys }
    }

    /// Extract one endpoint's `FxProperties` subtree from a parsed .reg map
    /// (as produced by [`crate::regfile::parse`] on `reg export` output).
    /// Keys outside the subtree — the endpoint's other keys, sibling
    /// endpoints — are ignored; only the `FxProperties` tree is in scope.
    pub fn from_reg_map(map: &KeyMap, endpoint_guid: &str) -> Result<Self, FxStoreError> {
        let prefix = format!(r"HKEY_LOCAL_MACHINE\{}", ids::fx_key(endpoint_guid));
        let mut keys = KeyMap::new();
        for (path, values) in map.iter() {
            if let Some(rel) = strip_prefix_ci(path, &prefix) {
                keys.insert(rel.to_owned(), values.clone());
            }
        }
        if keys.is_empty() {
            return Err(FxStoreError::EndpointNotFound(endpoint_guid.to_owned()));
        }
        Ok(Self { keys })
    }

    /// Reattach the absolute prefix, yielding a map [`crate::regfile::serialize`]
    /// can turn back into `reg export` text (the before/after diff proof).
    pub fn to_reg_map(&self, endpoint_guid: &str) -> KeyMap {
        let prefix = format!(r"HKEY_LOCAL_MACHINE\{}", ids::fx_key(endpoint_guid));
        self.keys
            .iter()
            .map(|(rel, values)| {
                let path = if rel.is_empty() { prefix.clone() } else { format!(r"{prefix}\{rel}") };
                (path, values.clone())
            })
            .collect()
    }

    /// Values of the `FxProperties` key itself.
    fn root_mut(&mut self) -> &mut ValueMap {
        if !self.keys.contains_key("") {
            // The root key always exists in practice; create it defensively
            // (and put it first, matching export order) if it does not.
            let mut keys = KeyMap::new();
            keys.insert(String::new(), ValueMap::new());
            for (k, v) in self.keys.iter() {
                keys.insert(k.to_owned(), v.clone());
            }
            self.keys = keys;
        }
        // Just ensured above.
        self.keys.get_mut("").expect("root key exists")
    }
}

/// Case-insensitive (ASCII — registry semantics) prefix strip: returns the
/// path relative to `prefix`, `""` for the prefix itself.
fn strip_prefix_ci<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if path.len() < prefix.len() || !path[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = &path[prefix.len()..];
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('\\')
    }
}

// ---------------------------------------------------------------------------
// Serde mirror (values as base64 so the backup JSON is stable and readable)

#[derive(Serialize, Deserialize)]
struct StoreRepr {
    keys: Vec<KeyRepr>,
}

#[derive(Serialize, Deserialize)]
struct KeyRepr {
    path: String,
    values: Vec<ValueRepr>,
}

#[derive(Serialize, Deserialize)]
struct ValueRepr {
    name: String,
    /// `REG_*` type code.
    kind: u32,
    /// Raw data bytes, base64.
    data: String,
}

impl Serialize for FxStore {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let repr = StoreRepr {
            keys: self
                .keys
                .iter()
                .map(|(path, values)| KeyRepr {
                    path: path.to_owned(),
                    values: values
                        .iter()
                        .map(|(name, v)| ValueRepr {
                            name: name.to_owned(),
                            kind: v.kind.code(),
                            data: b64.encode(&v.data),
                        })
                        .collect(),
                })
                .collect(),
        };
        repr.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FxStore {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let repr = StoreRepr::deserialize(deserializer)?;
        let mut keys = KeyMap::new();
        for key in repr.keys {
            let mut values = ValueMap::new();
            for v in key.values {
                let data = b64.decode(&v.data).map_err(serde::de::Error::custom)?;
                values.insert(v.name, RegValue { kind: RegKind::from_code(v.kind), data });
            }
            keys.insert(key.path, values);
        }
        Ok(Self { keys })
    }
}

// ---------------------------------------------------------------------------
// Backup

/// Everything needed to undo an install, written to disk *before* any live
/// change (brief: original state on disk first; uninstall restores
/// byte-for-byte). JSON round-trips losslessly — proven in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    /// Endpoint device GUID (the `{...}` key name under MMDevices\Render).
    pub endpoint_guid: String,
    /// ISO-8601 UTC timestamp of when the backup was taken.
    pub timestamp: String,
    /// The complete prior FxProperties store, verbatim.
    pub store: FxStore,
    /// The CLSID the installer registered (matches [`ids::APO_CLSID`]).
    pub installed_clsid: String,
    /// Machine-wide keys the installer adds under HKLM — the COM class and
    /// (S42b) the audio-engine APO registration (recorded diff — uninstall
    /// deletes exactly these, deepest first; kept while another endpoint
    /// still carries the APO). Backups from before S42b lack the
    /// audio-engine key; nothing wrote it then, so nothing is left behind.
    pub com_keys: Vec<String>,
}

// ---------------------------------------------------------------------------
// Plans

/// The full recipe for one install: the backup to persist first, the store
/// image to write, and the COM registration tree to create.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPlan {
    pub backup: Backup,
    pub new_store: FxStore,
    /// CLSID registration keys (paths relative to HKLM):
    /// `SOFTWARE\Classes\CLSID\{clsid}` with the friendly name, and its
    /// `InprocServer32` with the DLL path and `ThreadingModel = "Both"`.
    pub com_keys: KeyMap,
}

/// Plan an install against the current store image. Pure — touches nothing.
pub fn plan_install(current: &FxStore, endpoint_guid: &str, apo_dll_path: &str) -> InstallPlan {
    let clsid_key = ids::clsid_key(ids::APO_CLSID);
    let inproc_key = format!(r"{clsid_key}\InprocServer32");

    let backup = Backup {
        endpoint_guid: endpoint_guid.to_owned(),
        timestamp: iso_now(),
        store: current.clone(),
        installed_clsid: ids::APO_CLSID.to_owned(),
        com_keys: vec![
            clsid_key.clone(),
            inproc_key.clone(),
            ids::audio_engine_key(ids::APO_CLSID),
        ],
    };

    let mut new_store = current.clone();
    let root = new_store.root_mut();

    // 1. Composite EFX chain: append our CLSID, preserving vendor entries.
    append_to_multi_sz(root, ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID, ids::APO_CLSID);

    // 2. Legacy single-EFX slot: fill only if empty — never evict a vendor.
    if !root.contains_key(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID) {
        root.insert(
            ids::PKEY_FX_ENDPOINT_EFFECT_CLSID.to_owned(),
            RegValue { kind: RegKind::Sz, data: sz_bytes(ids::APO_CLSID) },
        );
    }

    // 3. Processing modes: make sure DEFAULT is offered for streaming.
    append_to_multi_sz(root, ids::PKEY_EFX_MODES, ids::MODE_DEFAULT);

    // COM registration tree.
    let mut com_keys = KeyMap::new();
    let mut clsid_values = ValueMap::new();
    clsid_values.insert(
        String::new(),
        RegValue { kind: RegKind::Sz, data: sz_bytes(ids::APO_FRIENDLY_NAME) },
    );
    com_keys.insert(clsid_key, clsid_values);
    let mut inproc_values = ValueMap::new();
    inproc_values
        .insert(String::new(), RegValue { kind: RegKind::Sz, data: sz_bytes(apo_dll_path) });
    inproc_values.insert(
        "ThreadingModel".to_owned(),
        RegValue { kind: RegKind::Sz, data: sz_bytes("Both") },
    );
    com_keys.insert(inproc_key, inproc_values);

    // Audio-engine registration — what audiodg actually looks up (S42b).
    com_keys.insert(ids::audio_engine_key(ids::APO_CLSID), audio_engine_values());

    InstallPlan { backup, new_store, com_keys }
}

/// The values `RegisterAPO` writes for Relay's `APO_REG_PROPERTIES`, in the
/// order and with the types it writes them (REG_SZ strings, REG_DWORD
/// numbers; `APOInterface<n>` REG_SZ braced IIDs) — matched against a
/// third-party registration on the dev PC. Every number mirrors
/// `com::GetRegistrationProperties`.
pub fn audio_engine_values() -> ValueMap {
    let sz = |s: &str| RegValue { kind: RegKind::Sz, data: sz_bytes(s) };
    let dword = |n: u32| RegValue { kind: RegKind::Dword, data: n.to_le_bytes().to_vec() };
    let mut v = ValueMap::new();
    v.insert("FriendlyName".into(), sz(ids::APO_FRIENDLY_NAME));
    v.insert("Copyright".into(), sz(ids::APO_COPYRIGHT));
    v.insert("MajorVersion".into(), dword(ids::APO_MAJOR_VERSION));
    v.insert("MinorVersion".into(), dword(ids::APO_MINOR_VERSION));
    v.insert("Flags".into(), dword(ids::APO_REG_FLAGS));
    v.insert("MinInputConnections".into(), dword(1));
    v.insert("MaxInputConnections".into(), dword(1));
    v.insert("MinOutputConnections".into(), dword(1));
    v.insert("MaxOutputConnections".into(), dword(1));
    v.insert("MaxInstances".into(), dword(u32::MAX));
    v.insert("NumAPOInterfaces".into(), dword(1));
    v.insert("APOInterface0".into(), sz(ids::IID_IAUDIO_PROCESSING_OBJECT));
    v
}

/// Plan an uninstall: the store to restore is the backup, verbatim. Trivial
/// by construction — the byte-for-byte fixture tests are the point.
pub fn plan_uninstall(backup: &Backup) -> FxStore {
    backup.store.clone()
}

/// Append `entry` to the REG_MULTI_SZ value `name` (create with just
/// `entry` if absent). Idempotent: CLSIDs compare case-insensitively, so a
/// second install never duplicates.
fn append_to_multi_sz(values: &mut ValueMap, name: &str, entry: &str) {
    let mut list = match values.get(name) {
        Some(v) => parse_multi_sz(&v.data),
        None => Vec::new(),
    };
    if !list.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
        list.push(entry.to_owned());
        values.insert(
            name.to_owned(),
            RegValue { kind: RegKind::MultiSz, data: multi_sz_bytes(&list) },
        );
    }
}

// ---------------------------------------------------------------------------
// Diff

/// The `(relative key, value name)` pairs whose data or presence differs
/// between two stores. Used by the tests to prove the install touches
/// exactly the three PKEYs, and by the live backend to write only the diff.
pub fn diff(before: &FxStore, after: &FxStore) -> Vec<(String, String)> {
    let mut changed = Vec::new();
    for (path, before_values) in before.keys.iter() {
        match after.keys.get(path) {
            Some(after_values) => {
                for (name, v) in before_values.iter() {
                    if after_values.get(name) != Some(v) {
                        changed.push((path.to_owned(), name.to_owned()));
                    }
                }
                for (name, _) in after_values.iter() {
                    if !before_values.contains_key(name) {
                        changed.push((path.to_owned(), name.to_owned()));
                    }
                }
            }
            None => {
                for (name, _) in before_values.iter() {
                    changed.push((path.to_owned(), name.to_owned()));
                }
            }
        }
    }
    for (path, after_values) in after.keys.iter() {
        if !before.keys.contains_key(path) {
            for (name, _) in after_values.iter() {
                changed.push((path.to_owned(), name.to_owned()));
            }
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Timestamp (no chrono dependency; Howard Hinnant's civil-from-days)

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso_from_unix(secs)
}

fn iso_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days, shifted so the era starts 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formatting_is_correct() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(951_827_696), "2000-02-29T12:34:56Z");
        assert_eq!(iso_from_unix(1_772_064_000), "2026-02-26T00:00:00Z");
    }

    #[test]
    fn diff_reports_added_removed_changed() {
        let mut a = FxStore::empty();
        a.root_mut()
            .insert("keep".to_owned(), RegValue { kind: RegKind::Dword, data: vec![1, 0, 0, 0] });
        a.root_mut()
            .insert("gone".to_owned(), RegValue { kind: RegKind::Dword, data: vec![2, 0, 0, 0] });
        let mut b = a.clone();
        b.root_mut().remove("gone");
        b.root_mut()
            .insert("new".to_owned(), RegValue { kind: RegKind::Dword, data: vec![3, 0, 0, 0] });
        let mut changed = diff(&a, &b);
        changed.sort();
        assert_eq!(
            changed,
            vec![(String::new(), "gone".to_owned()), (String::new(), "new".to_owned()),]
        );
    }
}
