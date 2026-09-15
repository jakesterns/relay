//! Live-registry backend for [`crate::fxstore`] plans.
//!
//! Safety story, in order of importance:
//! - **No test executes this module.** All install/uninstall logic is
//!   proven against .reg fixtures in [`crate::fxstore`]; this file is the
//!   thin, dumb I/O layer that replays a plan. The first live run happens
//!   in a throwaway VM (M3b plan), never on a dev machine.
//! - The write paths ([`LiveRegistry::apply_install`],
//!   [`LiveRegistry::restore`]) are additionally gated behind the
//!   `RELAY_APO_ALLOW_LIVE_WRITE=1` environment variable so an accidental
//!   call from a test or a debug session fails loudly instead of touching
//!   HKLM.
//! - Scope: the only keys ever written are one endpoint's `FxProperties`
//!   subtree (paths come from [`FxStore`]'s *relative* keys, so nothing
//!   outside it is expressible) and the recorded CLSID keys under
//!   `HKLM\SOFTWARE\Classes\CLSID`.
//! - [`LiveRegistry::restore`] reconciles the live tree against the
//!   [`Backup`]: values and sub-keys we (or anyone since) added are
//!   deleted, prior values are rewritten, and the recorded COM keys are
//!   removed — restoring the export byte-for-byte.

use thiserror::Error;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW,
    RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_VALUE_TYPE,
};

use crate::fxstore::{Backup, FxStore, InstallPlan};
use crate::ids;
use crate::regfile::{KeyMap, RegKind, RegValue, ValueMap};

/// Errors from the live-registry backend.
#[derive(Debug, Error)]
pub enum LiveRegError {
    #[error("registry call failed with Win32 error {0}")]
    Win32(u32),
    #[error(
        "live registry writes are disabled (set RELAY_APO_ALLOW_LIVE_WRITE=1; VM / installer only)"
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

/// NUL-terminated UTF-16 for the Win32 W APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: self.0 came from RegOpenKeyExW/RegCreateKeyExW and is
        // closed exactly once, here.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

impl Key {
    /// Open `path` (relative to HKLM) with the given access mask.
    fn open(
        path: &str,
        sam: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> Result<Self, LiveRegError> {
        let path_w = wide(path);
        let mut hkey = HKEY::default();
        // SAFETY: path_w is NUL-terminated and outlives the call; hkey is a
        // valid out-pointer.
        let err = unsafe {
            RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path_w.as_ptr()), None, sam, &mut hkey)
        };
        check(err)?;
        Ok(Self(hkey))
    }

    /// Open `path` for setting values, creating it only if it is not there.
    ///
    /// The two-step matters, and it was measured: an endpoint's
    /// `FxProperties` key is owned by SYSTEM and grants `BUILTIN\Administrators`
    /// only **SetValue + ReadKey** — not `CreateSubKey`. `RegCreateKeyExW`
    /// with `KEY_WRITE` therefore fails `ERROR_ACCESS_DENIED` on an
    /// *existing* key, even from an elevated process, because `KEY_WRITE`
    /// includes rights we do not need. (Live proof, 2026-09-14: the first
    /// elevated install on this dev machine failed with Win32 error 5 and the
    /// FX store came back byte-identical.)
    ///
    /// The right answer is to ask for less, not to take ownership of a key
    /// Windows owns — an installer that rewrites that DACL leaves the machine
    /// permanently different, which is exactly what Relay promises not to do.
    /// So: open with `KEY_SET_VALUE`, and fall back to creating (which needs
    /// the parent's `CreateSubKey`) only for keys that do not exist yet — our
    /// own CLSID keys under `SOFTWARE\Classes`, where administrators do have
    /// it.
    fn create(path: &str) -> Result<Self, LiveRegError> {
        if let Ok(key) = Self::open(path, KEY_SET_VALUE) {
            return Ok(key);
        }
        let path_w = wide(path);
        let mut hkey = HKEY::default();
        // SAFETY: as above; all pointers are valid for the duration of the
        // call.
        let err = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
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

    /// Read every value of this key into a [`ValueMap`], preserving
    /// enumeration order (which is what `reg export` writes).
    fn read_values(&self) -> Result<ValueMap, LiveRegError> {
        let (max_name, max_data) = self.query_maxima()?;
        let mut values = ValueMap::new();
        let mut index = 0u32;
        loop {
            let mut name_buf = vec![0u16; max_name as usize + 1];
            let mut name_len = name_buf.len() as u32;
            let mut data = vec![0u8; max_data as usize];
            let mut data_len = data.len() as u32;
            let mut kind = REG_VALUE_TYPE::default();
            // SAFETY: buffers are sized from RegQueryInfoKeyW maxima and the
            // length in/out parameters match them.
            let err = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    Some(windows::core::PWSTR(name_buf.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind.0),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if err == ERROR_NO_MORE_ITEMS {
                break;
            }
            check(err)?;
            let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            data.truncate(data_len as usize);
            values.insert(name, RegValue { kind: RegKind::from_code(kind.0), data });
            index += 1;
        }
        Ok(values)
    }

    /// Enumerate direct sub-key names, in enumeration order.
    fn subkey_names(&self) -> Result<Vec<String>, LiveRegError> {
        let mut names = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buf = [0u16; 256]; // max registry key-name length is 255
            let mut len = buf.len() as u32;
            // SAFETY: buf/len match; the other out-params are unused.
            let err = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    Some(windows::core::PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if err == ERROR_NO_MORE_ITEMS {
                break;
            }
            check(err)?;
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            index += 1;
        }
        Ok(names)
    }

    /// (max value-name length in chars, max data length in bytes).
    fn query_maxima(&self) -> Result<(u32, u32), LiveRegError> {
        let mut max_name = 0u32;
        let mut max_data = 0u32;
        // SAFETY: only the two out-params we care about are passed.
        let err = unsafe {
            RegQueryInfoKeyW(
                self.0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&mut max_name),
                Some(&mut max_data),
                None,
                None,
            )
        };
        check(err)?;
        Ok((max_name, max_data))
    }

    fn set_value(&self, name: &str, value: &RegValue) -> Result<(), LiveRegError> {
        let name_w = wide(name);
        // SAFETY: name_w is NUL-terminated; data slice is valid for the call.
        let err = unsafe {
            RegSetValueExW(
                self.0,
                PCWSTR(name_w.as_ptr()),
                None,
                REG_VALUE_TYPE(value.kind.code()),
                Some(&value.data),
            )
        };
        check(err)
    }

    fn delete_value(&self, name: &str) -> Result<(), LiveRegError> {
        let name_w = wide(name);
        // SAFETY: name_w is NUL-terminated.
        let err = unsafe { RegDeleteValueW(self.0, PCWSTR(name_w.as_ptr())) };
        check(err)
    }
}

/// Delete the whole tree at `path` (relative to HKLM). Used only for keys
/// this installer created and recorded in the [`Backup`].
fn delete_tree(path: &str) -> Result<(), LiveRegError> {
    let path_w = wide(path);
    // SAFETY: path_w is NUL-terminated.
    let err = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(path_w.as_ptr())) };
    check(err)
}

/// Refuse to write unless explicitly armed. VM / installer only — never
/// call the write paths on a dev machine.
fn assert_writes_allowed() -> Result<(), LiveRegError> {
    if std::env::var("RELAY_APO_ALLOW_LIVE_WRITE").as_deref() == Ok("1") {
        Ok(())
    } else {
        Err(LiveRegError::WritesDisabled)
    }
}

/// The live-registry backend. Stateless; every fn takes explicit inputs.
pub struct LiveRegistry;

impl LiveRegistry {
    /// Read one endpoint's `FxProperties` tree into an [`FxStore`].
    /// Read-only — safe anywhere.
    pub fn read_fx_store(endpoint_guid: &str) -> Result<FxStore, LiveRegError> {
        let root_path = ids::fx_key(endpoint_guid);
        let mut keys = KeyMap::new();
        read_tree(&root_path, "", &mut keys)?;
        Ok(FxStore { keys })
    }

    /// Write an [`InstallPlan`]: set the diffed FxProperties values and
    /// create the COM registration keys.
    ///
    /// **VM / installer only — never call on a dev machine.** The caller
    /// must have persisted `plan.backup` to disk first (same
    /// backup-then-apply contract as relay-core's `Applier`).
    pub fn apply_install(plan: &InstallPlan) -> Result<(), LiveRegError> {
        assert_writes_allowed()?;

        // Only the values the planner actually changed are written; every
        // untouched value stays physically untouched.
        let changed = crate::fxstore::diff(&plan.backup.store, &plan.new_store);
        let root_path = ids::fx_key(&plan.backup.endpoint_guid);
        for (rel_key, name) in &changed {
            let path = join(&root_path, rel_key);
            let key = Key::create(&path)?;
            match plan.new_store.keys.get(rel_key).and_then(|values| values.get(name)) {
                Some(value) => key.set_value(name, value)?,
                // Present before, absent in the plan — the installer never
                // produces this, but honour it for completeness.
                None => key.delete_value(name)?,
            }
        }

        // COM registration (recorded in the backup for uninstall).
        for (path, values) in plan.com_keys.iter() {
            let key = Key::create(path)?;
            for (name, value) in values.iter() {
                key.set_value(name, value)?;
            }
        }
        Ok(())
    }

    /// Restore from a [`Backup`]: reconcile the live `FxProperties` tree to
    /// the recorded store (delete values and sub-keys that were not there,
    /// rewrite everything that was), then delete the recorded COM keys.
    ///
    /// **VM / installer only — never call on a dev machine.**
    pub fn restore(backup: &Backup) -> Result<(), LiveRegError> {
        assert_writes_allowed()?;

        let root_path = ids::fx_key(&backup.endpoint_guid);

        // Read what is live now so we can delete anything the backup does
        // not contain (our values, or strays added since).
        let mut live = KeyMap::new();
        read_tree(&root_path, "", &mut live)?;

        for (rel_key, live_values) in live.iter() {
            match backup.store.keys.get(rel_key) {
                Some(prior_values) => {
                    let key = Key::open(&join(&root_path, rel_key), KEY_READ | KEY_SET_VALUE)?;
                    for (name, _) in live_values.iter() {
                        if !prior_values.contains_key(name) {
                            key.delete_value(name)?;
                        }
                    }
                }
                // A sub-key that did not exist at backup time: remove it
                // wholesale. (Direct children only — deeper strays go with
                // their parent tree.)
                None if !rel_key.contains('\\') || parent_in(&backup.store, rel_key) => {
                    delete_tree(&join(&root_path, rel_key))?;
                }
                None => {} // ancestor already deleted by delete_tree above
            }
        }

        // Rewrite the prior state verbatim, creating any missing keys.
        for (rel_key, prior_values) in backup.store.keys.iter() {
            let key = Key::create(&join(&root_path, rel_key))?;
            for (name, value) in prior_values.iter() {
                key.set_value(name, value)?;
            }
        }

        // Delete the COM keys we registered, deepest first.
        let mut com_keys = backup.com_keys.clone();
        com_keys.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
        for path in &com_keys {
            delete_tree(path)?;
        }
        Ok(())
    }
}

/// True if the direct parent of `rel_key` exists in the backup — meaning
/// `rel_key` itself is the topmost stray and should be tree-deleted.
fn parent_in(store: &FxStore, rel_key: &str) -> bool {
    match rel_key.rsplit_once('\\') {
        Some((parent, _)) => store.keys.contains_key(parent),
        None => true,
    }
}

fn join(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_owned()
    } else {
        format!(r"{root}\{rel}")
    }
}

/// Recursively read the key at `root\rel` and its sub-keys into `out`,
/// keyed by relative path (`""` = the root itself).
fn read_tree(root: &str, rel: &str, out: &mut KeyMap) -> Result<(), LiveRegError> {
    let key = Key::open(&join(root, rel), KEY_READ)?;
    out.insert(rel.to_owned(), key.read_values()?);
    for name in key.subkey_names()? {
        let child = if rel.is_empty() { name } else { format!(r"{rel}\{name}") };
        read_tree(root, &child, out)?;
    }
    Ok(())
}
