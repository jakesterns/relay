//! DLLs that other programs load from Relay's folder, and what an update does
//! when one of them is still loaded (PC2, r54-r56).
//!
//! Three Relay-folder DLLs run inside processes Relay does not control:
//!
//! * `relay_vdevice.dll`, the Relay Camera. The DirectShow camera is
//!   in-process, so Discord, Zoom or a browser that opened it keeps it loaded.
//! * `relay_apo.dll`, the audio effect, loaded by Windows audio (`audiodg`).
//! * `Processing.NDI.Lib.x64.dll`, the bundled NDI® runtime, loaded by
//!   `relay-share` while NDI output is on.
//!
//! A loaded DLL cannot be overwritten or deleted, so an NSIS install over the
//! top used to skip it silently and leave the old version in place under the
//! new app. A loaded DLL *can* be renamed, though. So the installer's
//! pre-install hook renames a locked one aside to `<name>.old<N>`, and the new
//! file then goes in at the original path. Every registration (the camera's
//! `InprocServer32`, the APO's CLSID) names that path, so the next program to
//! open the camera or the next audio-engine start loads the new DLL, with no
//! registry write and no elevation. The programs that still have the old copy
//! open keep it until they close it: this module finds them by name so the
//! installer (and the core, on start) can say which apps to close, and it
//! deletes each `.old<N>` file once nothing holds it.
//!
//! If even the rename fails, the installer stops with a message instead of
//! installing a mix of versions (`ui/src-tauri/installer/hooks.nsh`).

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The DLLs the installer moves aside when they are locked, with the
/// plain-English name of what loads them.
pub const SWAPPABLE: &[(&str, &str)] = &[
    ("relay_vdevice.dll", "the Relay Camera"),
    ("relay_apo.dll", "Relay's audio effect"),
    (crate::ndi::RUNTIME_DLL, "the NDI® runtime"),
];

/// One moved-aside copy that could not be deleted yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pending {
    /// The moved-aside file.
    pub path: String,
    /// The DLL's real name (`relay_vdevice.dll`).
    pub dll: String,
    /// What it is, for the message.
    pub what: String,
    /// Programs that still have it loaded (exe names, deduplicated).
    pub holders: Vec<String>,
}

/// `relay_vdevice.dll.old3` -> `relay_vdevice.dll`. Only names the installer
/// produces: one of [`SWAPPABLE`], `.old`, then digits.
pub fn aside_of(file_name: &str) -> Option<&'static str> {
    SWAPPABLE.iter().map(|(n, _)| *n).find(|n| {
        file_name.len() > n.len()
            && file_name[..n.len()].eq_ignore_ascii_case(n)
            && file_name[n.len()..]
                .strip_prefix(".old")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The moved-aside copies in `dir`.
pub fn asides(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            aside_of(&name).map(|dll| (e.path(), dll))
        })
        .collect();
    out.sort();
    out
}

/// Delete every moved-aside copy that nothing holds any more; return the
/// rest, with the programs holding them. `delete` and `holders` are injected
/// for tests.
pub fn sweep_with(
    dir: &Path,
    delete: impl Fn(&Path) -> bool,
    holders: impl Fn(&Path, &str) -> Vec<String>,
) -> Vec<Pending> {
    let mut out = Vec::new();
    for (path, dll) in asides(dir) {
        if delete(&path) {
            continue;
        }
        let what = SWAPPABLE.iter().find(|(n, _)| *n == dll).map_or("", |(_, w)| *w);
        out.push(Pending {
            path: path.display().to_string(),
            dll: dll.to_string(),
            what: what.to_string(),
            holders: holders(dir, dll),
        });
    }
    out
}

/// [`sweep_with`] on the real file system and process list.
pub fn sweep(dir: &Path) -> Vec<Pending> {
    sweep_with(dir, |p| std::fs::remove_file(p).is_ok(), holders)
}

/// The message for the user, or `None` when nothing is pending.
pub fn notice(pending: &[Pending]) -> Option<String> {
    if pending.is_empty() {
        return None;
    }
    let mut lines =
        vec!["Relay was updated, but some programs still have the previous version of a Relay \
         component open. They keep using it until they are closed; nothing else was changed."
            .to_string()];
    let mut seen: Vec<&str> = Vec::new();
    for p in pending {
        if seen.contains(&p.dll.as_str()) {
            continue;
        }
        seen.push(&p.dll);
        let mut holders: Vec<&str> = pending
            .iter()
            .filter(|q| q.dll == p.dll)
            .flat_map(|q| q.holders.iter().map(String::as_str))
            .collect();
        holders.sort_unstable();
        holders.dedup();
        let line = if p.dll == "relay_apo.dll" {
            // audiodg runs as LOCAL SERVICE: an unelevated process cannot see
            // its modules, and the user cannot close it like an app.
            format!(
                "- {}: Windows audio still has the old one loaded. It switches over the next \
                 time Windows audio restarts (sign out and back in, or restart the PC).",
                p.what
            )
        } else if holders.is_empty() {
            format!(
                "- {}: a program that cannot be identified still has the old one open. Close \
                 the apps that use it (or restart the PC) to switch over.",
                p.what
            )
        } else {
            format!("- {}: close {} to switch over.", p.what, join_and(&holders))
        };
        lines.push(line);
    }
    Some(lines.join("\n"))
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Programs with `dll` from `dir` loaded, by exe name. Processes this user
/// cannot open (services, other users) are not listed.
#[cfg(windows)]
pub fn holders(dir: &Path, dll: &str) -> Vec<String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Process32FirstW, Process32NextW,
        MODULEENTRY32W, PROCESSENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        TH32CS_SNAPPROCESS,
    };
    fn wstr(w: &[u16]) -> String {
        let n = w.iter().position(|c| *c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..n])
    }
    let want = dir.join(dll).display().to_string();
    let mut out: Vec<String> = Vec::new();
    // SAFETY: toolhelp snapshots closed by `Owned`; the entry structs are
    // zeroed with dwSize set, as the API requires.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let snap = windows::core::Owned::new(snap);
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(*snap, &mut pe).is_err() {
            return out;
        }
        loop {
            let pid = pe.th32ProcessID;
            if pid != 0 {
                if let Ok(ms) =
                    CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)
                {
                    let ms = windows::core::Owned::new(ms);
                    let mut me = MODULEENTRY32W {
                        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
                        ..Default::default()
                    };
                    let mut ok = Module32FirstW(*ms, &mut me).is_ok();
                    while ok {
                        if wstr(&me.szExePath).eq_ignore_ascii_case(&want) {
                            let exe = wstr(&pe.szExeFile);
                            if !out.iter().any(|e| e.eq_ignore_ascii_case(&exe)) {
                                out.push(exe);
                            }
                            break;
                        }
                        ok = Module32NextW(*ms, &mut me).is_ok();
                    }
                }
            }
            if Process32NextW(*snap, &mut pe).is_err() {
                break;
            }
        }
    }
    out.sort_by_key(|e| e.to_ascii_lowercase());
    out
}

#[cfg(not(windows))]
pub fn holders(_dir: &Path, _dll: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_installer_aside_names_match() {
        assert_eq!(aside_of("relay_vdevice.dll.old1"), Some("relay_vdevice.dll"));
        assert_eq!(aside_of("RELAY_APO.DLL.old12"), Some("relay_apo.dll"));
        assert_eq!(aside_of("Processing.NDI.Lib.x64.dll.old2"), Some("Processing.NDI.Lib.x64.dll"));
        for no in [
            "relay_vdevice.dll",
            "relay_vdevice.dll.old",
            "relay_vdevice.dll.oldx",
            "relay_vdevice.dll.old1.bak",
            "relay-core.exe.old1",
            "profiles.json.old1",
        ] {
            assert_eq!(aside_of(no), None, "{no}");
        }
    }

    #[test]
    fn sweep_deletes_free_copies_and_reports_held_ones() {
        let dir = std::env::temp_dir().join(format!("relay-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["relay_vdevice.dll.old1", "relay_vdevice.dll.old2", "relay_apo.dll.old1"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(dir.join("relay_vdevice.dll"), b"new").unwrap();
        let held =
            |p: &Path| p.ends_with("relay_vdevice.dll.old2") || p.ends_with("relay_apo.dll.old1");
        let pending = sweep_with(
            &dir,
            |p| !held(p) && std::fs::remove_file(p).is_ok(),
            |_, dll| if dll == "relay_vdevice.dll" { vec!["Discord.exe".into()] } else { vec![] },
        );
        assert!(!dir.join("relay_vdevice.dll.old1").exists(), "a free copy is deleted");
        assert!(dir.join("relay_vdevice.dll").exists(), "the live DLL is never touched");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].dll, "relay_apo.dll");
        assert_eq!(pending[1].holders, vec!["Discord.exe".to_string()]);
        let msg = notice(&pending).unwrap();
        assert!(msg.contains("the Relay Camera: close Discord.exe"), "{msg}");
        assert!(msg.contains("Windows audio"), "{msg}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notice_names_every_holder_once() {
        let p = |path: &str, h: &[&str]| Pending {
            path: path.into(),
            dll: "relay_vdevice.dll".into(),
            what: "the Relay Camera".into(),
            holders: h.iter().map(|s| s.to_string()).collect(),
        };
        let msg =
            notice(&[p("a", &["Discord.exe", "Zoom.exe"]), p("b", &["Discord.exe", "chrome.exe"])])
                .unwrap();
        assert!(msg.contains("close Discord.exe, Zoom.exe and chrome.exe"), "{msg}");
        assert_eq!(msg.matches("Relay Camera").count(), 1);
        assert!(notice(&[]).is_none());
    }

    /// The mechanism the installer relies on, on a real loaded DLL: it cannot
    /// be deleted, it can be renamed aside, a new file can then take its
    /// path, the process holding it is named, and the aside copy is swept
    /// once the DLL is unloaded.
    #[cfg(windows)]
    #[test]
    #[allow(unsafe_code)]
    fn a_loaded_dll_is_moved_aside_and_its_holder_named() {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::FreeLibrary;
        use windows::Win32::System::LibraryLoader::LoadLibraryW;

        let dir = std::env::temp_dir().join(format!("relay-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("relay_vdevice.dll");
        // Any harmless DLL will do; this one has no DllMain side effects.
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::copy(Path::new(&sys).join(r"System32\version.dll"), &live).unwrap();
        let wide: Vec<u16> = live.as_os_str().encode_wide_nul();
        // SAFETY: loading a copy of a system DLL by full path; freed below.
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.expect("load");

        assert!(std::fs::remove_file(&live).is_err(), "a loaded DLL cannot be deleted");
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_string_lossy().into_owned();
        assert!(holders(&dir, "relay_vdevice.dll").iter().any(|h| h.eq_ignore_ascii_case(&me)));

        let aside = dir.join("relay_vdevice.dll.old1");
        std::fs::rename(&live, &aside).expect("a loaded DLL can be renamed");
        std::fs::write(&live, b"the new version").unwrap();

        let pending = sweep(&dir);
        assert_eq!(pending.len(), 1, "still loaded, so kept: {pending:?}");
        assert!(notice(&pending).unwrap().contains("Relay Camera"));

        // SAFETY: the module loaded above.
        unsafe { FreeLibrary(module) }.unwrap();
        assert!(sweep(&dir).is_empty());
        assert!(!aside.exists(), "swept once unloaded");
        assert_eq!(std::fs::read(&live).unwrap(), b"the new version");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    trait WideNul {
        fn encode_wide_nul(&self) -> Vec<u16>;
    }
    #[cfg(windows)]
    impl WideNul for std::ffi::OsStr {
        fn encode_wide_nul(&self) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().chain(Some(0)).collect()
        }
    }
}
