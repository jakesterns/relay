//! The audio-effects opt-in (S44): Windows' protected-audiodg switch.
//!
//! On stock Windows 11, audiodg only loads audio processing objects carrying
//! a Microsoft-issued signature. Relay's APO has none (no EV cert, ever — see
//! the S42c/S42d findings), so audiodg never calls its `DllMain`. The one
//! documented way round it is the machine-wide value
//!
//! ```text
//! HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio
//!     DisableProtectedAudioDG  REG_DWORD 1
//! ```
//!
//! which is what Equalizer APO's installer sets. It is a Windows security
//! setting for the whole PC, so Relay ships with it untouched and only ever
//! changes it on an explicit, confirmed opt-in through the elevated helper.
//!
//! # The rules this module keeps
//!
//! - **One value, one path, both constants.** Nothing here takes a key or a
//!   value name as input; [`vet_target`] is still checked before every live
//!   write so a future edit that tries cannot get past it.
//! - **Backup, then apply.** The value's prior state (absent / 0 / 1 / other)
//!   is written to `apo-backup\audiodg-protection.json` *before* the write.
//! - **Off puts back exactly what was there.** Not "delete the value": the
//!   recorded prior state. If it was already 1 before Relay (Equalizer APO,
//!   say), Relay records nothing, changes nothing, and "off" leaves it 1.
//! - **Never silently.** The record survives crashes and restarts; status
//!   reports it on every Settings visit and the uninstaller restores from it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The key, under HKLM. Fixed; never derived from input.
pub const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";
/// The one value Relay may write under [`KEY`].
pub const VALUE: &str = "DisableProtectedAudioDG";
/// Gate on the live write, like the APO / camera / firewall gates. Only the
/// elevated helper (and the elevated uninstaller) arms it.
pub const LIVE_WRITE_GATE: &str = "RELAY_AUDIODG_ALLOW_LIVE_WRITE";
/// Record file name inside the APO backup directory.
pub const RECORD_FILE: &str = "audiodg-protection.json";

/// Where the prior-state record lives.
pub fn record_file(apo_backup_dir: &Path) -> PathBuf {
    apo_backup_dir.join(RECORD_FILE)
}

/// Refuse any target other than the one value. Called before every live
/// write; the constants are the only callers today, which is the point.
pub fn vet_target(key: &str, value: &str) -> Result<(), String> {
    let k = key.trim().trim_start_matches(r"HKLM\").to_ascii_lowercase();
    if k != KEY.to_ascii_lowercase() || !value.eq_ignore_ascii_case(VALUE) {
        return Err(format!(r"HKLM\{key} :: {value} is not HKLM\{KEY} :: {VALUE}"));
    }
    Ok(())
}

/// What the value was before Relay changed it. `None` = absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub prior: Option<u32>,
    /// Unix seconds when Relay set it.
    pub changed_at: u64,
}

pub fn load_record(file: &Path) -> Result<Option<Record>> {
    match std::fs::read(file) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).with_context(|| format!("reading {}", file.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e).context(format!("reading {}", file.display()))),
    }
}

fn save_record(file: &Path, r: &Record) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(r)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("writing {}", file.display()))
}

/// The registry, narrowed to the one value. No method takes a path.
pub trait ProtectionValue {
    /// `None` when the value is absent.
    fn read(&self) -> Result<Option<u32>>;
    /// `None` deletes the value.
    fn write(&mut self, v: Option<u32>) -> Result<()>;
}

/// Effects are loadable when the value is exactly 1.
pub fn allows(v: Option<u32>) -> bool {
    v == Some(1)
}

/// What Settings shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The value now (`None` = absent; `unknown` set if unreadable).
    pub value: Option<u32>,
    /// Windows will load unsigned audio effects (value is 1).
    pub allowed: bool,
    /// Relay changed it and holds a record of the prior state.
    pub changed_by_relay: bool,
    /// The recorded prior state, when Relay changed it.
    pub prior: Option<Option<u32>>,
    /// Already 1 without Relay — another app (Equalizer APO, say) set it.
    pub set_elsewhere: bool,
    /// The registry could not be read; the fields above are defaults.
    pub unknown: bool,
}

pub fn status(reg: &dyn ProtectionValue, record: &Path) -> Status {
    let rec = load_record(record).ok().flatten();
    let (value, unknown) = match reg.read() {
        Ok(v) => (v, false),
        Err(_) => (None, true),
    };
    Status {
        value,
        allowed: allows(value),
        changed_by_relay: rec.is_some(),
        prior: rec.as_ref().map(|r| r.prior),
        set_elsewhere: rec.is_none() && allows(value),
        unknown,
    }
}

/// What an enable/disable did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The value was written (or removed).
    Changed(String),
    /// Nothing needed doing.
    Unchanged(String),
}

fn show(v: Option<u32>) -> String {
    match v {
        None => "absent".into(),
        Some(n) => n.to_string(),
    }
}

/// Turn audio effects on: record the prior state, then set 1.
pub fn enable(reg: &mut dyn ProtectionValue, record: &Path, now: u64) -> Result<Change> {
    let current = reg.read().context("reading the protection value")?;
    if let Some(rec) = load_record(record)? {
        // Relay already holds the original. Never overwrite it with our own 1.
        if allows(current) {
            return Ok(Change::Unchanged("already on (set by Relay)".into()));
        }
        reg.write(Some(1))?;
        return Ok(Change::Changed(format!(
            "set to 1 again (original state {} kept on record)",
            show(rec.prior)
        )));
    }
    if allows(current) {
        return Ok(Change::Unchanged(
            "already 1 — another app set it; Relay records nothing and changes nothing".into(),
        ));
    }
    save_record(record, &Record { prior: current, changed_at: now })?;
    reg.write(Some(1))?;
    Ok(Change::Changed(format!("set to 1 (was {}; recorded first)", show(current))))
}

/// Turn audio effects off: put back exactly the recorded prior state.
pub fn disable(reg: &mut dyn ProtectionValue, record: &Path) -> Result<Change> {
    let Some(rec) = load_record(record)? else {
        return Ok(Change::Unchanged("Relay did not change it — left as it is".into()));
    };
    reg.write(rec.prior)?;
    std::fs::remove_file(record).with_context(|| format!("removing {}", record.display()))?;
    Ok(Change::Changed(format!("put back to {}", show(rec.prior))))
}

/// The dry run shown before the prompt.
pub fn plan_lines(
    on: bool,
    current: Option<u32>,
    rec: Option<&Record>,
    restart: bool,
) -> Vec<String> {
    let target = format!(r"HKLM\{KEY} :: {VALUE}");
    let mut l = Vec::new();
    if on {
        if rec.is_none() && allows(current) {
            l.push(format!("{target} is already 1 (set by another app) — nothing to change"));
        } else {
            l.push(format!("{target}: {} → 1 (REG_DWORD)", show(current)));
            if rec.is_none() {
                l.push(format!(
                    r"backup: %LOCALAPPDATA%\Relay\apo-backup\{RECORD_FILE} records '{}' before anything is written",
                    show(current)
                ));
            }
        }
    } else {
        match rec {
            Some(r) => l.push(format!(
                "{target}: {} → {} (the state before Relay)",
                show(current),
                show(r.prior)
            )),
            None => l.push(format!("{target}: Relay did not change it — nothing to put back")),
        }
    }
    l.push(if restart {
        "Then Windows audio restarts (AudioEndpointBuilder and Audiosrv): sound cuts out for 2–3 seconds.".into()
    } else {
        "Takes effect after Windows audio restarts or the PC restarts.".into()
    });
    l
}

#[cfg(windows)]
pub use live::{restart_audio, LiveValue};

#[cfg(windows)]
mod live {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_DWORD,
        REG_OPEN_CREATE_OPTIONS, REG_VALUE_TYPE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The live HKLM value. Reads are ungated; writes need [`LIVE_WRITE_GATE`].
    pub struct LiveValue;

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: handle came from RegCreateKeyExW.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(write: bool) -> Result<Key> {
        let sub = wide(KEY);
        let mut h = HKEY::default();
        let access = if write { KEY_SET_VALUE | KEY_QUERY_VALUE } else { KEY_QUERY_VALUE };
        // SAFETY: `sub` is NUL-terminated; `h` receives the handle. The Audio
        // key exists on every Windows install, so create == open in practice.
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(sub.as_ptr()),
                None,
                None,
                REG_OPEN_CREATE_OPTIONS(0),
                access | KEY_WOW64_64KEY,
                None,
                &mut h,
                None,
            )
        };
        r.ok().with_context(|| format!(r"opening HKLM\{KEY}"))?;
        Ok(Key(h))
    }

    impl ProtectionValue for LiveValue {
        fn read(&self) -> Result<Option<u32>> {
            let key = open(false)?;
            let name = wide(VALUE);
            let mut ty = REG_VALUE_TYPE::default();
            let mut data = [0u8; 4];
            let mut len = 4u32;
            // SAFETY: buffers are sized and outlive the call.
            let r = unsafe {
                RegQueryValueExW(
                    key.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    Some(&mut ty),
                    Some(data.as_mut_ptr()),
                    Some(&mut len),
                )
            };
            if r == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if r != ERROR_SUCCESS {
                return Err(anyhow::Error::from(windows::core::Error::from(r.to_hresult()))
                    .context("reading DisableProtectedAudioDG"));
            }
            if ty != REG_DWORD || len != 4 {
                anyhow::bail!("DisableProtectedAudioDG is not a REG_DWORD");
            }
            Ok(Some(u32::from_le_bytes(data)))
        }

        fn write(&mut self, v: Option<u32>) -> Result<()> {
            vet_target(KEY, VALUE).map_err(anyhow::Error::msg)?;
            if std::env::var_os(LIVE_WRITE_GATE).is_none() {
                anyhow::bail!("live write refused: {LIVE_WRITE_GATE} is not set");
            }
            let key = open(true)?;
            let name = wide(VALUE);
            let r = match v {
                // SAFETY: 4-byte little-endian DWORD, as REG_DWORD requires.
                Some(n) => unsafe {
                    RegSetValueExW(
                        key.0,
                        PCWSTR(name.as_ptr()),
                        None,
                        REG_DWORD,
                        Some(&n.to_le_bytes()),
                    )
                },
                // SAFETY: `name` is NUL-terminated.
                None => match unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) } {
                    e if e == ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                    e => e,
                },
            };
            r.ok().context("writing DisableProtectedAudioDG")
        }
    }

    /// Restart the Windows audio engine so the value takes effect. Fixed
    /// command, no input. `-Force` on AudioEndpointBuilder also restarts its
    /// dependant Audiosrv. Sound cuts out for 2–3 seconds.
    pub fn restart_audio() -> Result<()> {
        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Restart-Service -Name AudioEndpointBuilder -Force; Start-Service -Name Audiosrv",
            ])
            .status()
            .context("restarting Windows audio")?;
        anyhow::ensure!(status.success(), "restarting Windows audio exited with {status}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake registry holding just the one value, logging every write.
    struct Fake {
        v: Option<u32>,
        writes: Vec<Option<u32>>,
    }
    impl ProtectionValue for Fake {
        fn read(&self) -> Result<Option<u32>> {
            Ok(self.v)
        }
        fn write(&mut self, v: Option<u32>) -> Result<()> {
            self.v = v;
            self.writes.push(v);
            Ok(())
        }
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp() -> (Dir, PathBuf) {
        let d = std::env::temp_dir().join(format!("relay-s44-{}", uuid::Uuid::new_v4()));
        let f = record_file(&d);
        (Dir(d), f)
    }

    #[test]
    fn only_the_one_value_is_a_valid_target() {
        assert!(vet_target(KEY, VALUE).is_ok());
        assert!(vet_target(&format!(r"HKLM\{KEY}"), &VALUE.to_lowercase()).is_ok());
        for (k, v) in [
            (KEY, "DisableProtectedAudio"),
            (KEY, ""),
            (r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\Sub", VALUE),
            (r"SOFTWARE\Microsoft\Windows\CurrentVersion", VALUE),
            (r"SYSTEM\CurrentControlSet\Services\Audiosrv", VALUE),
            (r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\..\Run", VALUE),
        ] {
            assert!(vet_target(k, v).is_err(), "{k} :: {v} should be refused");
        }
    }

    /// The prior-state matrix: absent / 0 / 1 / other, on then off.
    #[test]
    fn on_then_off_restores_exactly_the_prior_state() {
        for prior in [None, Some(0), Some(2)] {
            let (_d, f) = tmp();
            let mut r = Fake { v: prior, writes: vec![] };
            assert!(matches!(enable(&mut r, &f, 7).unwrap(), Change::Changed(_)));
            assert_eq!(r.v, Some(1));
            assert_eq!(load_record(&f).unwrap(), Some(Record { prior, changed_at: 7 }));
            // The record shares apo-backup with the per-endpoint backups and is
            // never mistaken for one.
            assert!(crate::audio_apo::recorded_endpoints(f.parent().unwrap()).is_empty());
            let s = status(&r, &f);
            assert!(s.allowed && s.changed_by_relay && !s.set_elsewhere);
            assert_eq!(s.prior, Some(prior));

            assert!(matches!(disable(&mut r, &f).unwrap(), Change::Changed(_)));
            assert_eq!(r.v, prior, "prior {prior:?} not restored");
            assert_eq!(r.writes, vec![Some(1), prior]);
            assert!(!f.exists(), "record must go once restored");
        }
    }

    #[test]
    fn already_one_before_relay_is_left_alone_both_ways() {
        let (_d, f) = tmp();
        let mut r = Fake { v: Some(1), writes: vec![] };
        let s = status(&r, &f);
        assert!(s.allowed && s.set_elsewhere && !s.changed_by_relay);
        assert!(matches!(enable(&mut r, &f, 1).unwrap(), Change::Unchanged(_)));
        assert!(!f.exists(), "nothing to record when Relay changed nothing");
        assert!(matches!(disable(&mut r, &f).unwrap(), Change::Unchanged(_)));
        assert_eq!(r.v, Some(1));
        assert!(r.writes.is_empty());
    }

    #[test]
    fn a_second_enable_never_overwrites_the_original_record() {
        let (_d, f) = tmp();
        let mut r = Fake { v: None, writes: vec![] };
        enable(&mut r, &f, 1).unwrap();
        // Something reset it to 0 behind our back; on again keeps "absent".
        r.v = Some(0);
        enable(&mut r, &f, 2).unwrap();
        assert_eq!(load_record(&f).unwrap().unwrap().prior, None);
        assert!(matches!(enable(&mut r, &f, 3).unwrap(), Change::Unchanged(_)));
        disable(&mut r, &f).unwrap();
        assert_eq!(r.v, None);
    }

    #[test]
    fn the_record_is_written_before_the_value() {
        struct Failing;
        impl ProtectionValue for Failing {
            fn read(&self) -> Result<Option<u32>> {
                Ok(Some(0))
            }
            fn write(&mut self, _: Option<u32>) -> Result<()> {
                anyhow::bail!("write failed")
            }
        }
        let (_d, f) = tmp();
        assert!(enable(&mut Failing, &f, 1).is_err());
        // The backup is on disk even though the write failed, so a later
        // "off" or the uninstaller puts back 0 rather than guessing.
        assert_eq!(load_record(&f).unwrap().unwrap().prior, Some(0));
    }

    #[test]
    fn off_without_a_record_changes_nothing() {
        let (_d, f) = tmp();
        let mut r = Fake { v: Some(0), writes: vec![] };
        assert!(matches!(disable(&mut r, &f).unwrap(), Change::Unchanged(_)));
        assert!(r.writes.is_empty());
    }

    #[test]
    fn the_plan_names_the_value_and_the_restart() {
        let on = plan_lines(true, None, None, false).join("\n");
        assert!(on.contains(
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio :: DisableProtectedAudioDG"
        ));
        assert!(on.contains("absent → 1") && on.contains("backup:") && on.contains("after"));
        let off = plan_lines(false, Some(1), Some(&Record { prior: Some(0), changed_at: 0 }), true)
            .join("\n");
        assert!(off.contains("1 → 0") && off.contains("2–3 seconds"));
        assert!(plan_lines(true, Some(1), None, false)[0].contains("another app"));
    }
}
