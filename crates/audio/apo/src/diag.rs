//! Opt-in load diagnostics for the APO inside audiodg (S42c).
//!
//! Silent unless switched on. Switch: the file `%ProgramData%\Relay\apo-diag.on`
//! exists, or the env var `RELAY_APO_DIAG` is set (tests; audiodg has no way
//! to receive it). The switch is re-checked on every line, so creating or
//! deleting the file takes effect without restarting audiodg.
//!
//! Where lines go, first that opens for append wins:
//! 1. `%ProgramData%\Relay\apo-diag.log` — the directory is created if
//!    missing. audiodg runs as LOCAL SERVICE; `C:\ProgramData` lets
//!    Authenticated Users create folders, but a `Relay` folder an admin made
//!    first inherits CREATOR OWNER rights and may deny LOCAL SERVICE writes.
//! 2. `%TEMP%\relay-apo-diag.log` — for LOCAL SERVICE this is
//!    `C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp`.
//! 3. `%SystemRoot%\Temp\relay-apo-diag.log`.
//!
//! Every file is capped at [`CAP_BYTES`]; past that, lines are dropped.
//!
//! Real-time rule: nothing here is ever called from `APOProcess`. Callers are
//! DllMain, DllGetClassObject, the class factory and the config-path methods
//! (Initialize, format negotiation, LockForProcess), where allocation is
//! allowed.

use std::fmt::Arguments;
use std::io::Write;
use std::path::PathBuf;

/// Size cap per log file.
pub const CAP_BYTES: u64 = 256 * 1024;

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Relay")
}

/// The switch file path.
pub fn switch_path() -> PathBuf {
    program_data().join("apo-diag.on")
}

/// True when diagnostics are on.
pub fn enabled() -> bool {
    std::env::var_os("RELAY_APO_DIAG").is_some() || switch_path().exists()
}

/// Candidate log files in fallback order.
pub fn candidates() -> Vec<PathBuf> {
    let mut v = vec![program_data().join("apo-diag.log")];
    if let Some(t) = std::env::var_os("TEMP") {
        v.push(PathBuf::from(t).join("relay-apo-diag.log"));
    }
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    v.push(PathBuf::from(root).join("Temp").join("relay-apo-diag.log"));
    v
}

/// Append one line (pid, tid, uptime ms, message) to the first writable
/// candidate. Never panics, never returns an error.
pub fn log(args: Arguments<'_>) {
    if !enabled() {
        return;
    }
    let _ = std::fs::create_dir_all(program_data());
    let line =
        format!("pid={} tid={:?} {}\r\n", std::process::id(), std::thread::current().id(), args);
    for path in candidates() {
        let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
            continue;
        };
        if f.metadata().map(|m| m.len()).unwrap_or(0) >= CAP_BYTES {
            return;
        }
        let _ = f.write_all(line.as_bytes());
        return;
    }
}

/// `diag!("fmt", args..)` — formats only when diagnostics are on.
#[macro_export]
macro_rules! diag {
    ($($t:tt)*) => {
        if $crate::diag::enabled() {
            $crate::diag::log(format_args!($($t)*));
        }
    };
}
