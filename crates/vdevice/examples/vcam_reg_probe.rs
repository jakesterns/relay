//! Registration-hive probe for `MFCreateVirtualCamera` (session S5).
//!
//! The question this answers: does the Windows Camera Frame Server resolve a
//! virtual-camera media source registered under `HKCU\Software\Classes\CLSID`,
//! so Relay could register per-user and drop its one elevated step?
//!
//! Usage (no elevation, writes nothing):
//!
//! ```powershell
//! cargo run -p relay-vdevice --release --example vcam_reg_probe -- <CLSID>
//! ```
//!
//! `<CLSID>` is a braced GUID string; pass Relay's own or a throwaway one to
//! run the control arms (registered in HKCU / not registered at all). The
//! probe prints the HRESULT of `MFCreateVirtualCamera`, what `CoGetClassObject`
//! makes of the same CLSID in *this* process (which does honour HKCU via
//! HKCR), and the Frame Server service state before and after — so a failure
//! can be attributed to the lookup rather than to the service never starting.
//!
//! **Answer (2026-09-14, Win11 26200): no.** A per-user registration gets
//! past the *client* side - `MFCreateVirtualCamera` and the class lookup in
//! the calling process both succeed, because HKCR merges HKCU over HKLM -
//! but `IMFVirtualCamera::Start` then fails 0x80070003 inside the Frame
//! Server, which runs as NT AUTHORITY\LocalService and never loads the
//! DLL. Full write-up and the control arms: `docs/dev/vcam-live.md`.

#[cfg(windows)]
#[path = "vcam_reg_probe/win.rs"]
mod win;

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vcam_reg_probe probes the Windows Frame Server; there is nothing to probe here.");
}
