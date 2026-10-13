//! The camera's diagnostic log (r54).
//!
//! The camera code runs inside someone else's process (the call app on
//! Windows 10, the Frame Server on Windows 11), so it has no tracing
//! subscriber and must never write to stdout. Each line goes to the
//! debugger stream (`OutputDebugStringW`, visible in DebugView) and, when
//! Relay is installed for this user (`%LOCALAPPDATA%\Relay` exists), is
//! appended to `logs\camera.log` there, capped at 256 KB with one rotation.
//! Nothing is created on a PC without Relay's own folder, and a failed write
//! is dropped: a log line must never cost the call a frame.
//!
//! What goes in: which media type the app negotiated (once per connection),
//! and when the picture switches between the live ring and the waiting still
//! — the gaps testers ask about. Never pixel data, never anything from the
//! app beyond its exe name.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const CAP_BYTES: u64 = 256 * 1024;

static LOCK: Mutex<()> = Mutex::new(());

/// The log file, if Relay's data folder exists for this user.
/// `RELAY_CAMERA_LOG` names another file (tests); a namespaced test instance
/// (`RELAY_INSTANCE`) without it writes no file at all, so test runs never
/// touch the user's real log.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RELAY_CAMERA_LOG") {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("RELAY_INSTANCE").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Relay");
    if !root.is_dir() {
        return None;
    }
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs).ok()?;
    Some(logs.join("camera.log"))
}

/// The host process's exe name, e.g. `Discord.exe`.
pub fn host_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// One line: `<unix seconds.ms> [<exe> <pid>] <text>`.
pub fn line(text: &str) {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let full = format!(
        "{}.{:03} [{} {}] {}\n",
        now.as_secs(),
        now.subsec_millis(),
        host_exe(),
        std::process::id(),
        text
    );
    debug_out(&full);
    let _guard = LOCK.lock();
    let Some(path) = path() else { return };
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > CAP_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(full.as_bytes());
    }
}

#[cfg(windows)]
fn debug_out(s: &str) {
    let w: Vec<u16> = format!("Relay Camera: {s}").encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated wide string that outlives the call.
    #[allow(unsafe_code)]
    unsafe {
        windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(windows::core::PCWSTR(
            w.as_ptr(),
        ))
    };
}

#[cfg(not(windows))]
fn debug_out(_: &str) {}
