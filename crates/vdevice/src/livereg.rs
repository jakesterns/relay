//! Live-registry backend for [`crate::reg`] plans.
//!
//! Safety story (same as `relay_apo::livereg`):
//! - **No test executes the write paths.** The planner is proven in
//!   [`crate::reg`]; this file is the thin I/O layer that replays a plan.
//! - Writes are gated behind `RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` so an
//!   accidental call fails loudly instead of touching HKLM; elevation is
//!   the second gate (HKLM ACLs reject a non-elevated writer).
//! - Scope: only the keys a plan names — two keys under
//!   `HKLM\SOFTWARE\Classes\CLSID\{our CLSID}` — are ever written or
//!   deleted, and the delete list comes from `installed.json`.

#![allow(unsafe_code)] // registry I/O; every block carries a SAFETY note

use thiserror::Error;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegOpenKeyExW, RegQueryInfoKeyW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};

use crate::reg::RegKeySpec;

#[derive(Debug, Error)]
pub enum LiveRegError {
    #[error("registry call failed with Win32 error {0}")]
    Win32(u32),
    #[error(
        "live registry writes are disabled (set RELAY_VDEVICE_ALLOW_LIVE_WRITE=1; \
         elevated install path only)"
    )]
    WritesDisabled,
}

fn check(err: WIN32_ERROR) -> Result<(), LiveRegError> {
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(LiveRegError::Win32(err.0))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// REG_SZ bytes: UTF-16LE with the terminating NUL.
fn sz_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect()
}

fn assert_writes_allowed() -> Result<(), LiveRegError> {
    if std::env::var("RELAY_VDEVICE_ALLOW_LIVE_WRITE").as_deref() == Ok("1") {
        Ok(())
    } else {
        Err(LiveRegError::WritesDisabled)
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: self.0 came from RegCreateKeyExW; closed exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

impl Key {
    fn create(root: HKEY, path: &str) -> Result<Self, LiveRegError> {
        let path_w = wide(path);
        let mut hkey = HKEY::default();
        // SAFETY: NUL-terminated path; valid out-pointer.
        let err = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(path_w.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
        };
        check(err)?;
        Ok(Self(hkey))
    }

    fn set_sz(&self, name: &str, data: &str) -> Result<(), LiveRegError> {
        let name_w = wide(name);
        let bytes = sz_bytes(data);
        // SAFETY: NUL-terminated name; data valid for the call.
        let err = unsafe {
            RegSetValueExW(
                self.0,
                PCWSTR(name_w.as_ptr()),
                None,
                REG_VALUE_TYPE(REG_SZ.0),
                Some(&bytes),
            )
        };
        check(err)
    }
}

/// Create the plan's keys and REG_SZ values. The caller must have persisted
/// the `installed.json` record first (backup-then-apply, same contract as
/// the APO installer). **Gated + elevated only.**
pub fn apply(keys: &[RegKeySpec]) -> Result<(), LiveRegError> {
    apply_under(HKEY_LOCAL_MACHINE, keys)
}

/// Per-user twin of [`apply`] for the DirectShow filter (HKCU, no
/// elevation). Same gate: nothing is written unless
/// RELAY_VDEVICE_ALLOW_LIVE_WRITE=1. The caller vets the keys
/// (`reg::vet_dshow_keys`) and records them first.
pub fn apply_user(keys: &[RegKeySpec]) -> Result<(), LiveRegError> {
    apply_under(HKEY_CURRENT_USER, keys)
}

/// Per-user twin of [`remove`]. Same gate.
pub fn remove_user(paths: &[String]) -> Result<(), LiveRegError> {
    remove_under(HKEY_CURRENT_USER, paths)
}

/// The product path for the Windows 10 camera (approved by Jake 2026-09-29):
/// HKCU only, no elevation, called by the core only after the user's recorded
/// consent and after `reg::vet_dshow_keys` accepted exactly these keys. The
/// environment gate guards against an accidental write in development; a
/// per-user camera the user asked for must work in the shipped app with no
/// setup, and the core cannot safely set an environment variable while other
/// threads run.
pub fn apply_user_consented(keys: &[RegKeySpec]) -> Result<(), LiveRegError> {
    write_keys(HKEY_CURRENT_USER, keys)
}

/// Counterpart of [`apply_user_consented`]: removes exactly the recorded,
/// vetted keys.
pub fn remove_user_consented(paths: &[String]) -> Result<(), LiveRegError> {
    delete_keys(HKEY_CURRENT_USER, paths)
}

/// Whether an HKCU key exists (read-only). Used at install to record which
/// parent keys the install itself creates (S43b).
pub fn user_key_exists(path: &str) -> bool {
    open_read(HKEY_CURRENT_USER, path).is_some()
}

fn open_read(root: HKEY, path: &str) -> Option<Key> {
    let path_w = wide(path);
    let mut hkey = HKEY::default();
    // SAFETY: NUL-terminated path; valid out-pointer.
    let err =
        unsafe { RegOpenKeyExW(root, PCWSTR(path_w.as_ptr()), None, KEY_READ, &mut hkey) };
    (err == ERROR_SUCCESS).then_some(Key(hkey))
}

/// Delete each HKCU key, in the order given, only if it exists and is
/// empty: no values (default included) and no subkeys. A key with anything
/// in it, or already gone, is left alone and is not an error. Callers pass
/// only vetted, recorded parents (`reg::plan_dshow_parent_cleanup`).
pub fn remove_user_empty_consented(paths: &[String]) -> Result<(), LiveRegError> {
    for path in paths {
        let Some(key) = open_read(HKEY_CURRENT_USER, path) else { continue };
        let (mut subkeys, mut values) = (0u32, 0u32);
        // SAFETY: open key; only the two counts are requested.
        let err = unsafe {
            RegQueryInfoKeyW(
                key.0,
                None,
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                None,
                None,
                None,
                None,
            )
        };
        check(err)?;
        drop(key);
        if subkeys != 0 || values != 0 {
            continue;
        }
        let path_w = wide(path);
        // SAFETY: NUL-terminated path. RegDeleteKeyW is not recursive: it
        // fails on a key that gained a subkey since the check.
        let err = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(path_w.as_ptr())) };
        if err != ERROR_FILE_NOT_FOUND {
            check(err)?;
        }
    }
    Ok(())
}

fn apply_under(root: HKEY, keys: &[RegKeySpec]) -> Result<(), LiveRegError> {
    assert_writes_allowed()?;
    write_keys(root, keys)
}

fn write_keys(root: HKEY, keys: &[RegKeySpec]) -> Result<(), LiveRegError> {
    for spec in keys {
        let key = Key::create(root, &spec.path)?;
        for (name, data) in &spec.values {
            key.set_sz(name, data)?;
        }
    }
    Ok(())
}

/// Delete the recorded keys, in the order given (the planner already sorts
/// deepest first). A key that is already gone is not an error — uninstall
/// must be idempotent. **Gated + elevated only.**
pub fn remove(paths: &[String]) -> Result<(), LiveRegError> {
    remove_under(HKEY_LOCAL_MACHINE, paths)
}

fn remove_under(root: HKEY, paths: &[String]) -> Result<(), LiveRegError> {
    assert_writes_allowed()?;
    delete_keys(root, paths)
}

fn delete_keys(root: HKEY, paths: &[String]) -> Result<(), LiveRegError> {
    for path in paths {
        let path_w = wide(path);
        // SAFETY: NUL-terminated path.
        let err = unsafe { RegDeleteTreeW(root, PCWSTR(path_w.as_ptr())) };
        if err != ERROR_FILE_NOT_FOUND {
            check(err)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_gated() {
        // The gate env var is never set in tests; both paths must refuse
        // before touching any registry API.
        assert!(matches!(apply(&[]), Err(LiveRegError::WritesDisabled)));
        assert!(matches!(remove(&[]), Err(LiveRegError::WritesDisabled)));
        assert!(matches!(apply_user(&[]), Err(LiveRegError::WritesDisabled)));
        assert!(matches!(remove_user(&[]), Err(LiveRegError::WritesDisabled)));
    }
}
