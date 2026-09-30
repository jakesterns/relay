//! Pure registration planner for the camera media source COM DLL.
//!
//! Same discipline as `relay_apo::fxstore`: everything here operates on a
//! plan (paths + REG_SZ values), never the live registry, and is unit-tested
//! before any code touches HKLM. The live backend is [`crate::livereg`]; it
//! only executes plans produced here.
//!
//! Unlike the APO there is no prior state to preserve: the camera adds two
//! brand-new keys under `HKLM\SOFTWARE\Classes\CLSID` and uninstall deletes
//! exactly those (recorded in `installed.json`). HKLM because the Windows
//! Camera Frame Server service hosts the media source as LOCAL SERVICE and
//! cannot see per-user classes — the same decision as the APO (M3b plan).
//!
//! That is measured, not assumed: an HKCU-only registration resolves in the
//! calling process but `IMFVirtualCamera::Start` fails 0x80070003 and the
//! Frame Server never loads the DLL. Evidence, control arms and the probe
//! (`examples/vcam_reg_probe.rs`) are in `docs/dev/vcam-live.md`; do not
//! re-litigate the hive without reading it.

use crate::installed::{iso_now, Component, CAMERA_DSHOW_FILTER, CAMERA_MEDIA_SOURCE};

/// CLSID of the Relay camera media source. Generated once for this project;
/// never reuse or change it — uninstall and `installed.json` match on it.
pub const VCAM_CLSID: &str = "{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}";

/// The camera name apps show in their device pickers.
pub const VCAM_FRIENDLY_NAME: &str = "Relay Camera";

/// COM class name in the registry (distinct from the camera name so the
/// registry entry is identifiable).
pub const VCAM_CLASS_NAME: &str = "Relay Camera Source";

/// One registry key to create, with its REG_SZ values (name → data; the
/// empty name is the default value). Paths are relative to HKLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegKeySpec {
    pub path: String,
    pub values: Vec<(String, String)>,
}

/// The full recipe for one camera install: the keys to create and the
/// `installed.json` record to persist *before* creating them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraInstallPlan {
    pub keys: Vec<RegKeySpec>,
    pub record: Component,
}

pub fn clsid_key() -> String {
    format!(r"SOFTWARE\Classes\CLSID\{VCAM_CLSID}")
}

/// Plan a camera registration. Pure — touches nothing.
pub fn plan_camera_install(dll_path: &str) -> CameraInstallPlan {
    let clsid = clsid_key();
    let inproc = format!(r"{clsid}\InprocServer32");
    let keys = vec![
        RegKeySpec {
            path: clsid.clone(),
            values: vec![(String::new(), VCAM_CLASS_NAME.to_owned())],
        },
        RegKeySpec {
            path: inproc.clone(),
            values: vec![
                (String::new(), dll_path.to_owned()),
                ("ThreadingModel".to_owned(), "Both".to_owned()),
            ],
        },
    ];
    let record = Component {
        id: CAMERA_MEDIA_SOURCE.to_owned(),
        installed_at: iso_now(),
        dll_path: dll_path.to_owned(),
        hklm_keys: keys.iter().map(|k| k.path.clone()).collect(),
        hkcu_keys: Vec::new(),
    };
    CameraInstallPlan { keys, record }
}

/// The keys to delete for an uninstall, deepest first (so `InprocServer32`
/// goes before its parent; `RegDeleteTree` on the parent would also work,
/// but deleting exactly the recorded list is the contract).
pub fn plan_camera_uninstall(record: &Component) -> Vec<String> {
    let mut keys = record.hklm_keys.clone();
    keys.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
    keys
}

/// How "Relay Camera" is provided on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPath {
    /// Windows 11 22H2+: `MFCreateVirtualCamera` + the media source the
    /// Frame Server hosts (HKLM registration, elevated helper).
    FrameServer,
    /// Everything older (Windows 10): the DirectShow source filter the app
    /// loads itself (per-user registration, no elevation) — S43.
    DirectShow,
}

impl CameraPath {
    /// Pure choice, so the rule is testable without the OS.
    pub fn for_support(frameserver: bool) -> Self {
        if frameserver {
            CameraPath::FrameServer
        } else {
            CameraPath::DirectShow
        }
    }
}

// ---------------------------------------------------------------------------
// DirectShow camera filter (Windows 10 path, S43)
// ---------------------------------------------------------------------------
//
// Before Windows 11 22H2 there is no frame-server virtual camera, so "Relay
// Camera" is a user-mode DirectShow source filter that the app itself loads
// (the OBS VirtualCam approach). The app runs as the user, so the class and
// its entry in the video-capture category can live in the user's own hive:
// `HKCU\Software\Classes` is merged into `HKCR`, which is where COM and the
// system device enumerator look. No elevation, no HKLM, no driver.
//
// Three keys, all brand new, all recorded; uninstall deletes exactly these
// and `vet_dshow_keys` refuses anything else, so a tampered installed.json
// cannot point the delete at the category itself or another filter.

/// CLSID of the Relay DirectShow camera filter. Generated once for this
/// project; never reuse or change it.
pub const DSHOW_CLSID: &str = "{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}";

/// `CLSID_VideoInputDeviceCategory` — the category every webcam picker
/// enumerates through `ICreateDevEnum`.
pub const VIDEO_INPUT_CATEGORY: &str = "{860BB310-5D01-11D0-BD3B-00A0C911CE86}";

/// COM class name of the filter (the device name is [`VCAM_FRIENDLY_NAME`]).
pub const DSHOW_CLASS_NAME: &str = "Relay Camera (DirectShow)";

/// `Software\Classes\CLSID\{filter}` (HKCU-relative).
pub fn dshow_clsid_key() -> String {
    format!(r"Software\Classes\CLSID\{DSHOW_CLSID}")
}

/// `Software\Classes\CLSID\{category}\Instance\{filter}` (HKCU-relative).
pub fn dshow_category_key() -> String {
    format!(r"Software\Classes\CLSID\{VIDEO_INPUT_CATEGORY}\Instance\{DSHOW_CLSID}")
}

/// Every key the filter may ever own, parent before child.
pub fn dshow_allowed_keys() -> [String; 3] {
    [dshow_clsid_key(), format!(r"{}\InprocServer32", dshow_clsid_key()), dshow_category_key()]
}

/// Plan the per-user registration of the DirectShow filter. Pure.
pub fn plan_dshow_install(dll_path: &str) -> CameraInstallPlan {
    let [clsid, inproc, category] = dshow_allowed_keys();
    let keys = vec![
        RegKeySpec { path: clsid, values: vec![(String::new(), DSHOW_CLASS_NAME.to_owned())] },
        RegKeySpec {
            path: inproc,
            values: vec![
                (String::new(), dll_path.to_owned()),
                ("ThreadingModel".to_owned(), "Both".to_owned()),
            ],
        },
        RegKeySpec {
            path: category,
            values: vec![
                ("FriendlyName".to_owned(), VCAM_FRIENDLY_NAME.to_owned()),
                ("CLSID".to_owned(), DSHOW_CLSID.to_owned()),
            ],
        },
    ];
    let record = Component {
        id: CAMERA_DSHOW_FILTER.to_owned(),
        installed_at: iso_now(),
        dll_path: dll_path.to_owned(),
        hklm_keys: Vec::new(),
        hkcu_keys: keys.iter().map(|k| k.path.clone()).collect(),
    };
    CameraInstallPlan { keys, record }
}

/// Refuse any key list that is not a subset of [`dshow_allowed_keys`]
/// (case-insensitive, as the registry is). Run before every live write or
/// delete — the list comes from a file on disk.
pub fn vet_dshow_keys(keys: &[String]) -> Result<(), String> {
    let allowed = dshow_allowed_keys();
    for k in keys {
        if !allowed.iter().any(|a| a.eq_ignore_ascii_case(k)) {
            return Err(format!("refusing to touch {k}: not one of the Relay Camera filter keys"));
        }
    }
    Ok(())
}

/// HKCU keys to delete for the filter, deepest first. Vetted.
pub fn plan_dshow_uninstall(record: &Component) -> Result<Vec<String>, String> {
    vet_dshow_keys(&record.hkcu_keys)?;
    let mut keys = record.hkcu_keys.clone();
    keys.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dshow_plan_is_per_user_and_names_the_category_instance() {
        let plan = plan_dshow_install(r"C:\Users\u\AppData\Local\Relay\relay_vdevice.dll");
        assert_eq!(plan.keys.len(), 3);
        assert!(plan.keys.iter().all(|k| k.path.starts_with(r"Software\Classes\CLSID\")));
        assert!(plan.record.hklm_keys.is_empty(), "no HKLM: no elevation");
        assert_eq!(plan.record.id, CAMERA_DSHOW_FILTER);
        let cat = &plan.keys[2];
        assert!(cat.path.contains(VIDEO_INPUT_CATEGORY) && cat.path.ends_with(DSHOW_CLSID));
        assert!(cat.values.contains(&("FriendlyName".into(), "Relay Camera".into())));
        assert!(cat.values.contains(&("CLSID".into(), DSHOW_CLSID.into())));
        assert!(plan.keys[1].values.contains(&("ThreadingModel".into(), "Both".into())));
        assert_eq!(
            plan.record.hkcu_keys,
            plan.keys.iter().map(|k| k.path.clone()).collect::<Vec<_>>()
        );
        vet_dshow_keys(&plan.record.hkcu_keys).expect("own plan passes the vet");
    }

    #[test]
    fn dshow_uninstall_is_deepest_first_and_exactly_the_record() {
        let plan = plan_dshow_install("x.dll");
        let del = plan_dshow_uninstall(&plan.record).expect("vetted");
        assert_eq!(del.len(), 3);
        // Both 7-deep keys (InprocServer32 and the category instance) come
        // before the 4-deep CLSID key.
        assert_eq!(del[2], dshow_clsid_key());
        assert!(del[..2].iter().any(|k| k.ends_with("InprocServer32")));
        assert!(del[..2].contains(&dshow_category_key()));
    }

    #[test]
    fn dshow_vet_refuses_the_category_and_foreign_keys() {
        for bad in [
            format!(r"Software\Classes\CLSID\{VIDEO_INPUT_CATEGORY}"),
            format!(r"Software\Classes\CLSID\{VIDEO_INPUT_CATEGORY}\Instance"),
            r"Software\Classes\CLSID\{A3FCE0F5-3493-419F-958A-ABA1250EC20B}".to_owned(),
            r"Software\Microsoft\Windows\CurrentVersion\Run".to_owned(),
            clsid_key(),
        ] {
            assert!(vet_dshow_keys(std::slice::from_ref(&bad)).is_err(), "{bad}");
        }
        let mut rec = plan_dshow_install("x.dll").record;
        rec.hkcu_keys.push(r"Software\Classes\CLSID".into());
        assert!(plan_dshow_uninstall(&rec).is_err(), "a tampered record deletes nothing");
        // Case-insensitive like the registry.
        assert!(vet_dshow_keys(&[dshow_clsid_key().to_lowercase()]).is_ok());
    }

    #[test]
    fn dshow_clsid_differs_from_the_media_source() {
        assert_ne!(DSHOW_CLSID, VCAM_CLSID);
    }

    #[test]
    fn install_plan_covers_clsid_and_inproc() {
        let plan = plan_camera_install(r"C:\Relay\relay_vdevice.dll");
        assert_eq!(plan.keys.len(), 2);
        assert!(plan.keys[0].path.ends_with(VCAM_CLSID));
        assert_eq!(plan.keys[0].values, vec![(String::new(), VCAM_CLASS_NAME.to_owned())]);
        assert!(plan.keys[1].path.ends_with("InprocServer32"));
        assert!(plan.keys[1]
            .values
            .contains(&(String::new(), r"C:\Relay\relay_vdevice.dll".to_owned())));
        assert!(plan.keys[1].values.contains(&("ThreadingModel".into(), "Both".into())));
        // The record lists exactly the created keys.
        assert_eq!(
            plan.record.hklm_keys,
            vec![plan.keys[0].path.clone(), plan.keys[1].path.clone()]
        );
        assert_eq!(plan.record.id, CAMERA_MEDIA_SOURCE);
    }

    #[test]
    fn uninstall_deletes_recorded_keys_deepest_first() {
        let plan = plan_camera_install("x.dll");
        let del = plan_camera_uninstall(&plan.record);
        assert_eq!(del.len(), 2);
        assert!(del[0].ends_with("InprocServer32"));
        assert!(del[1].ends_with(VCAM_CLSID));
    }
}
