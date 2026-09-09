//! Opt-in start at login. Touches exactly one registry value:
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Relay`.
//! Off by default; the uninstaller removes it too.

use anyhow::{Context, Result};

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const VALUE_NAME: &str = "Relay";

/// The command line the Run key launches: this executable in `run` mode.
pub fn command_line() -> Result<String> {
    let exe = std::env::current_exe().context("locating relay-core.exe")?;
    Ok(format!("\"{}\" run", exe.display()))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Key(HKEY);

    impl Key {
        fn open(access: REG_SAM_FLAGS) -> Result<Self> {
            let sub = wide(RUN_KEY);
            let mut h = HKEY::default();
            // SAFETY: `sub` is NUL-terminated; `h` receives the handle.
            let r = unsafe {
                RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, access, &mut h)
            };
            r.ok().context("opening HKCU Run key")?;
            Ok(Self(h))
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: handle came from RegOpenKeyExW.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    pub fn is_enabled() -> Result<bool> {
        let key = Key::open(KEY_QUERY_VALUE)?;
        let name = wide(VALUE_NAME);
        // SAFETY: existence check only; every out-pointer is None.
        let r = unsafe { RegQueryValueExW(key.0, PCWSTR(name.as_ptr()), None, None, None, None) };
        if r == ERROR_SUCCESS {
            Ok(true)
        } else if r == ERROR_FILE_NOT_FOUND {
            Ok(false)
        } else {
            Err(anyhow::Error::from(windows::core::Error::from(r.to_hresult()))
                .context("reading Run value"))
        }
    }

    pub fn set(enabled: bool) -> Result<()> {
        let key = Key::open(KEY_SET_VALUE)?;
        let name = wide(VALUE_NAME);
        if enabled {
            let cmd = wide(&command_line()?);
            let bytes: Vec<u8> = cmd.iter().flat_map(|c| c.to_le_bytes()).collect();
            // SAFETY: `bytes` is a NUL-terminated UTF-16 string as REG_SZ requires.
            let r =
                unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)) };
            r.ok().context("writing Run value")?;
        } else {
            // SAFETY: `name` is NUL-terminated.
            let r = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
            if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
                return Err(anyhow::Error::from(windows::core::Error::from(r.to_hresult()))
                    .context("removing Run value"));
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn is_enabled() -> Result<bool> {
        Ok(false)
    }
    pub fn set(_enabled: bool) -> Result<()> {
        anyhow::bail!("autostart is Windows-only")
    }
}

pub use imp::{is_enabled, set};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_exe_and_uses_run() {
        let c = command_line().unwrap();
        assert!(c.starts_with('"'));
        assert!(c.ends_with("\" run"));
    }
}
