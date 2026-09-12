//! `installed.json` — the single record of every virtual-device component
//! Relay has registered on this machine, plus the user's consent decision.
//!
//! Contract (brief + M5 plan):
//! - Nothing is registered before the consent decision says so.
//! - Every registered component appears here with the exact registry keys
//!   (and, later, driver INF) it added, so opt-out / uninstall can remove
//!   precisely that and nothing else.
//! - After a full opt-out the `components` list is empty.
//!
//! The file lives at `%LOCALAPPDATA%\Relay\installed.json` (the core owns
//! the path via `config::Paths`); writes are tmp-then-rename atomic.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const VERSION: u32 = 1;

/// Component id of the camera media source COM DLL.
pub const CAMERA_MEDIA_SOURCE: &str = "camera-media-source";

/// The user's first-run decision. `None` = not asked yet (the UI shows the
/// consent screen). A recorded `false` is as final as a `true`: the screen
/// does not come back, Settings does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    /// ISO-8601 UTC of the decision.
    pub decided_at: String,
    /// Endpoint APO opt-in (the other first-run component; its install
    /// path and backups live in relay-apo, this is only the recorded
    /// decision).
    #[serde(default)]
    pub apo: bool,
    /// Virtual camera opt-in.
    pub camera: bool,
    /// Virtual microphone opt-in (the signed driver later; the interim
    /// VB-Cable route also honours this flag even though it installs
    /// nothing).
    pub microphone: bool,
}

/// One registered component and exactly what it added to the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// Stable id, e.g. [`CAMERA_MEDIA_SOURCE`].
    pub id: String,
    /// ISO-8601 UTC of the registration.
    pub installed_at: String,
    /// The DLL the registration points at.
    pub dll_path: String,
    /// HKLM-relative registry keys created — uninstall deletes exactly
    /// these, deepest first.
    pub hklm_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
    #[serde(default)]
    pub components: Vec<Component>,
}

impl Default for InstalledFile {
    fn default() -> Self {
        Self { version: VERSION, consent: None, components: Vec::new() }
    }
}

impl InstalledFile {
    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    /// Add or replace the record for `component.id`.
    pub fn record(&mut self, component: Component) {
        self.components.retain(|c| c.id != component.id);
        self.components.push(component);
    }

    pub fn remove(&mut self, id: &str) {
        self.components.retain(|c| c.id != id);
    }
}

/// Load the record; a missing file is the default (no consent, nothing
/// installed). A corrupt file is an error — never guess about what is
/// registered on the machine.
pub fn load(path: &Path) -> std::io::Result<InstalledFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(InstalledFile::default()),
        Err(e) => Err(e),
    }
}

/// Atomic save: write `.tmp` beside the target, fsync, rename over.
pub fn save(path: &Path, file: &InstalledFile) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ` (no chrono dependency; same
/// civil-from-days routine as relay-apo's backup timestamps).
pub fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
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
    fn missing_file_is_default() {
        let dir = std::env::temp_dir().join(format!("relay-inst-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("missing.json");
        let f = load(&path).expect("load missing");
        assert_eq!(f, InstalledFile::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_round_trip_and_record_remove() {
        let dir = std::env::temp_dir().join(format!("relay-inst-rt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("installed.json");

        let mut f = InstalledFile {
            consent: Some(Consent {
                decided_at: iso_now(),
                apo: false,
                camera: true,
                microphone: false,
            }),
            ..Default::default()
        };
        f.record(Component {
            id: CAMERA_MEDIA_SOURCE.into(),
            installed_at: iso_now(),
            dll_path: r"C:\somewhere\relay_vdevice.dll".into(),
            hklm_keys: vec![r"SOFTWARE\Classes\CLSID\{X}".into()],
        });
        save(&path, &f).expect("save");
        assert_eq!(load(&path).expect("load"), f);

        // Replace, then remove — the DoD: after opt-out the list is empty.
        f.record(Component {
            id: CAMERA_MEDIA_SOURCE.into(),
            installed_at: iso_now(),
            dll_path: "elsewhere.dll".into(),
            hklm_keys: vec![],
        });
        assert_eq!(f.components.len(), 1);
        f.remove(CAMERA_MEDIA_SOURCE);
        assert!(f.components.is_empty());
        save(&path, &f).expect("save 2");
        assert!(load(&path).expect("load 2").components.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("relay-inst-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("installed.json");
        std::fs::write(&path, "not json").expect("write");
        assert!(load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
