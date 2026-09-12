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

use crate::installed::{iso_now, Component, CAMERA_MEDIA_SOURCE};

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

#[cfg(test)]
mod tests {
    use super::*;

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
