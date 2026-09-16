//! Starting the core without a console window flashing on screen.
//!
//! `relay-core.exe` is a console-subsystem binary because it is also the CLI:
//! `relay-core status` has to print into a shell's pipe, and Windows decides
//! whether to capture a process' output from the PE subsystem field, not from
//! anything the process does at runtime. (Building it for the GUI subsystem
//! was tried in M7 and reverted — PowerShell silently captured nothing, which
//! is a far worse failure than a flash because no script notices it.)
//!
//! But a console-subsystem process gets a console allocated *before* `main`
//! runs, so anything that launches the service — the Run key at login, the
//! installer — flashes a black window. `hide_own_console` only shortens it.
//!
//! So the service is launched through `relay-svc.exe`: a GUI-subsystem binary
//! that never gets a console of its own, spawns `relay-core.exe` with
//! `CREATE_NO_WINDOW`, and exits. Nothing stays resident — the launcher is
//! gone before the core finishes starting, and the core is reparented to the
//! shell as any detached service would be.
//!
//! This is the inverse of the usual split (GUI service + console CLI shim),
//! and it is the right way round here: it leaves `relay-core`'s command line,
//! its output and every script and test that reads it completely untouched.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Name of the launcher the Run key and the installer invoke.
pub const LAUNCHER_EXE: &str = "relay-svc.exe";
/// Name of the service/CLI binary the launcher starts.
pub const CORE_EXE: &str = "relay-core.exe";
/// Name of the Tauri window. The core launches it from the tray; it is never
/// required for the core to run.
pub const UI_EXE: &str = "relay-ui.exe";

/// `CREATE_NO_WINDOW` — run a console application without giving it a visible
/// console window. The child still has valid standard handles, so the core's
/// "log to stderr when attached" path keeps working under a debugger.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolve a sibling of `exe` by file name.
///
/// Pure so the layout assumption — everything Relay ships lives in one
/// directory — is testable without a filesystem.
pub fn sibling_of(exe: &Path, name: &str) -> Option<PathBuf> {
    exe.parent().map(|dir| dir.join(name))
}

/// The launcher to point the Run key at: `relay-svc.exe` beside the running
/// binary when it is there, otherwise the running binary itself.
///
/// The fallback is for development, where `cargo run -p relay-core` may not
/// have built the launcher. It costs a console flash at login and nothing
/// else, which is the right trade for a dev machine — silently failing to set
/// up autostart would not be.
pub fn autostart_target(exe: &Path) -> PathBuf {
    sibling_of(exe, LAUNCHER_EXE).filter(|p| p.exists()).unwrap_or_else(|| exe.to_path_buf())
}

/// Start `relay-core.exe` with no console window and do not wait for it.
///
/// `args` is forwarded verbatim, so `relay-svc run --data-dir X` starts
/// `relay-core run --data-dir X` and the Run key's command line keeps the
/// shape it always had.
#[cfg(windows)]
pub fn spawn_core(args: &[String]) -> Result<u32> {
    use std::os::windows::process::CommandExt;

    let me = std::env::current_exe().context("locating relay-svc.exe")?;
    let core = sibling_of(&me, CORE_EXE)
        .filter(|p| p.exists())
        .with_context(|| format!("{CORE_EXE} not found next to {}", me.display()))?;

    let child = std::process::Command::new(&core)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("starting {}", core.display()))?;
    Ok(child.id())
}

#[cfg(not(windows))]
pub fn spawn_core(_args: &[String]) -> Result<u32> {
    anyhow::bail!("the windowless launcher is Windows-only")
}

/// Bring the Relay window up: focus the one that is already running, or start
/// it if there is none. Used by the tray's "Open Relay".
///
/// Focusing first saves a process start: the window has its own
/// single-instance guard now (a second `relay-ui.exe` hands focus to the first
/// and exits), but there is no reason to launch one just for that.
#[cfg(windows)]
pub fn open_ui() -> Result<()> {
    if focus_ui() {
        return Ok(());
    }
    let me = std::env::current_exe().context("locating relay-core.exe")?;
    let ui = sibling_of(&me, UI_EXE)
        .filter(|p| p.exists())
        .with_context(|| format!("{UI_EXE} not found next to {}", me.display()))?;
    std::process::Command::new(&ui)
        .spawn()
        .with_context(|| format!("starting {}", ui.display()))?;
    Ok(())
}

/// Bring an existing Relay window to the front. `false` if none is up.
///
/// Also what a second launch of `relay-ui.exe` calls before exiting. It works
/// from there because the process the user just started is allowed to take
/// the foreground, and it is handing it straight to the window they wanted.
#[cfg(windows)]
pub fn focus_ui() -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let Some(p) =
        crate::processes::list_windowed().into_iter().find(|p| p.exe.eq_ignore_ascii_case(UI_EXE))
    else {
        return false;
    };
    let hwnd = HWND(p.hwnd as usize as *mut std::ffi::c_void);
    // SAFETY: a window handle the enumeration just produced. Both calls are
    // no-ops on a handle that died in between.
    unsafe {
        // Only un-minimise. SW_RESTORE on a maximised window would also
        // un-maximise it, which is not what "show me Relay" means.
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

#[cfg(not(windows))]
pub fn focus_ui() -> bool {
    false
}

#[cfg(not(windows))]
pub fn open_ui() -> Result<()> {
    anyhow::bail!("the Relay window is Windows-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_resolves_within_the_install_directory() {
        let exe = Path::new(r"C:\Users\u\AppData\Local\Relay\relay-svc.exe");
        assert_eq!(
            sibling_of(exe, CORE_EXE).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Local\Relay\relay-core.exe")
        );
    }

    #[test]
    fn autostart_falls_back_to_the_running_exe_when_the_launcher_is_absent() {
        // A path that cannot exist, so the `.exists()` filter fails and the
        // fallback is what comes back.
        let exe = Path::new(r"C:\nonexistent-relay-dir\relay-core.exe");
        assert_eq!(autostart_target(exe), exe.to_path_buf());
    }

    #[test]
    fn autostart_prefers_the_launcher_when_it_is_there() {
        // Build a real directory holding a file named like the launcher, so
        // the existence check has something to find.
        let dir = std::env::temp_dir().join(format!("relay-launcher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join(LAUNCHER_EXE);
        std::fs::write(&launcher, b"").unwrap();
        let core = dir.join(CORE_EXE);

        assert_eq!(autostart_target(&core), launcher);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
