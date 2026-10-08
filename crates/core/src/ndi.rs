//! NDI® output (S51): where the NDI runtime lives, and the names and notices
//! that go with it. NDI® is a registered trademark of Vizrt NDI AB.
//!
//! Relay never ships the NDI runtime and vendors no NDI SDK file
//! (`docs/dev/ndi-licensing.md`). The share engine loads the runtime the user
//! installed, by full path, only while NDI output is on. This module is the
//! shared, pure half: the core uses it to tell the UI whether the runtime is
//! there (a file-exists check, nothing loaded), the engine uses it to find the
//! DLL it then loads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The 64-bit Windows runtime library (`NDILIB_LIBRARY_NAME`).
pub const RUNTIME_DLL: &str = "Processing.NDI.Lib.x64.dll";
/// Set by the NDI 6 runtime installer to its folder (`NDILIB_REDIST_FOLDER`).
pub const RUNTIME_ENV: &str = "NDI_RUNTIME_DIR_V6";
/// Test and developer override: a folder holding [`RUNTIME_DLL`]. Checked
/// first, so a test can point the engine at an empty folder and prove the
/// "runtime missing" path on a PC that has NDI installed.
pub const OVERRIDE_ENV: &str = "RELAY_NDI_RUNTIME";
/// Where the NDI 6 runtime installer puts itself, under `%ProgramFiles%`.
const DEFAULT_RUNTIME_SUBDIR: &str = r"NDI\NDI 6 Runtime\v6";
/// NDI's own download for the runtime (`NDILIB_REDIST_URL`).
pub const RUNTIME_DOWNLOAD_URL: &str = "http://ndi.link/NDIRedistV6";
/// The link NDI's licence asks for next to every place NDI is selected.
pub const NDI_URL: &str = "https://ndi.video/";
/// The attribution NDI's licence asks for.
pub const TRADEMARK: &str = "NDI® is a registered trademark of Vizrt NDI AB.";

/// Longest source name we publish. NDI shows "MACHINE (name)"; the SDK does
/// not document a hard limit, so this keeps the name readable in a picker.
const MAX_NAME_CHARS: usize = 63;

/// Where the runtime is, or why it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NdiRuntime {
    /// The DLL exists at `path`.
    pub present: bool,
    /// Full path of the DLL that would be loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Folders looked in, in order, for the "needs the runtime" note.
    #[serde(default)]
    pub searched: Vec<String>,
    /// Where to get it.
    pub download_url: String,
    /// The NDI link and trademark line, so the UI never hard-codes them.
    pub ndi_url: String,
    pub trademark: String,
}

/// Find the runtime with the real environment and file system.
pub fn locate_runtime() -> NdiRuntime {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    locate_with(|k| std::env::var_os(k).map(PathBuf::from), exe_dir, |p| p.is_file())
}

/// The search, with the environment and file system injected for tests.
///
/// Order: [`OVERRIDE_ENV`] (and only it, when set), then the NDI 6 runtime
/// folder its installer names, then that installer's default folder, then
/// the engine's own folder. Only full paths are produced; the
/// engine never asks the loader to search `PATH`.
pub fn locate_with(
    env: impl Fn(&str) -> Option<PathBuf>,
    exe_dir: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> NdiRuntime {
    let mut dirs: Vec<PathBuf> = Vec::new();
    match env(OVERRIDE_ENV).filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => dirs.push(d),
        None => {
            if let Some(d) = env(RUNTIME_ENV).filter(|d| !d.as_os_str().is_empty()) {
                dirs.push(d);
            }
            // The installer's default folder. A Relay that was already
            // running when the runtime was installed has an environment
            // without NDI_RUNTIME_DIR_V6 (and so do the engines it spawns),
            // so without this the runtime would only be found after a
            // restart.
            if let Some(pf) = env("ProgramFiles").filter(|d| !d.as_os_str().is_empty()) {
                let d = pf.join(DEFAULT_RUNTIME_SUBDIR);
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
            if let Some(d) = exe_dir {
                dirs.push(d);
            }
        }
    }
    let found = dirs.iter().map(|d| d.join(RUNTIME_DLL)).find(|p| exists(p));
    NdiRuntime {
        present: found.is_some(),
        path: found.map(|p| p.display().to_string()),
        searched: dirs.iter().map(|d| d.display().to_string()).collect(),
        download_url: RUNTIME_DOWNLOAD_URL.into(),
        ndi_url: NDI_URL.into(),
        trademark: TRADEMARK.into(),
    }
}

/// The receiver's source name: "Relay (from <sender>)". Control characters
/// are dropped and the result is capped, so whatever the sending PC calls
/// itself, the name is one readable line in an NDI source list.
pub fn receive_source_name(sender: &str) -> String {
    let clean: String = sender.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    let base = if clean.is_empty() { "Relay".to_string() } else { format!("Relay (from {clean})") };
    cap(base)
}

/// The sender's own share, when it publishes too.
pub fn share_source_name() -> String {
    "Relay share".to_string()
}

fn cap(s: String) -> String {
    if s.chars().count() <= MAX_NAME_CHARS {
        return s;
    }
    // Keep the closing parenthesis so a long name still reads as one.
    let mut out: String = s.chars().take(MAX_NAME_CHARS - 2).collect();
    out.push('…');
    if s.ends_with(')') {
        out.push(')');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let pairs: Vec<(String, PathBuf)> =
            pairs.iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        move |k| pairs.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn runtime_found_through_the_v6_variable() {
        let dir = PathBuf::from(r"C:\Program Files\NDI\NDI 6 Runtime\v6");
        let want = dir.join(RUNTIME_DLL);
        let r = locate_with(
            env_of(&[(RUNTIME_ENV, r"C:\Program Files\NDI\NDI 6 Runtime\v6")]),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local\Relay")),
            |p| p == want,
        );
        assert!(r.present);
        assert_eq!(r.path.as_deref(), Some(want.display().to_string().as_str()));
        assert_eq!(r.searched.len(), 2);
    }

    #[test]
    fn runtime_found_in_the_default_folder_without_the_variable() {
        // Relay started before the runtime was installed: no variable.
        let want =
            PathBuf::from(r"C:\Program Files").join(DEFAULT_RUNTIME_SUBDIR).join(RUNTIME_DLL);
        let r = locate_with(env_of(&[("ProgramFiles", r"C:\Program Files")]), None, |p| p == want);
        assert!(r.present);
        // Named by the variable too: looked in once, not twice.
        let r = locate_with(
            env_of(&[
                ("ProgramFiles", r"C:\Program Files"),
                (RUNTIME_ENV, r"C:\Program Files\NDI\NDI 6 Runtime\v6"),
            ]),
            None,
            |_| false,
        );
        assert_eq!(r.searched.len(), 1);
    }

    #[test]
    fn missing_runtime_says_where_it_looked_and_where_to_get_it() {
        let r = locate_with(env_of(&[]), Some(PathBuf::from(r"C:\Relay")), |_| false);
        assert!(!r.present);
        assert!(r.path.is_none());
        assert_eq!(r.searched, vec![r"C:\Relay".to_string()]);
        assert_eq!(r.download_url, "http://ndi.link/NDIRedistV6");
        assert!(r.trademark.contains("Vizrt NDI AB"));
        assert_eq!(r.ndi_url, "https://ndi.video/");
    }

    #[test]
    fn override_replaces_every_other_location() {
        // A test pointing at an empty folder must see "missing" even on a PC
        // with the runtime installed.
        let r = locate_with(
            env_of(&[(OVERRIDE_ENV, r"C:\empty"), (RUNTIME_ENV, r"C:\ndi")]),
            Some(PathBuf::from(r"C:\Relay")),
            |p| p.starts_with(r"C:\ndi") || p.starts_with(r"C:\Relay"),
        );
        assert!(!r.present);
        assert_eq!(r.searched, vec![r"C:\empty".to_string()]);
    }

    #[test]
    fn empty_variables_are_ignored() {
        let r = locate_with(env_of(&[(RUNTIME_ENV, ""), (OVERRIDE_ENV, "")]), None, |_| true);
        assert!(!r.present);
        assert!(r.searched.is_empty());
    }

    #[test]
    fn source_names() {
        assert_eq!(receive_source_name("GAMING-PC"), "Relay (from GAMING-PC)");
        assert_eq!(receive_source_name("  "), "Relay");
        assert_eq!(receive_source_name("a\u{7}b\n"), "Relay (from ab)");
        let long = receive_source_name(&"x".repeat(200));
        assert_eq!(long.chars().count(), MAX_NAME_CHARS);
        assert!(long.ends_with("…)"));
        assert_eq!(share_source_name(), "Relay share");
    }
}
