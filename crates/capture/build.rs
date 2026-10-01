//! Delay-load the Windows 11 media APIs so the binaries start on Windows 10.
//!
//! `MFCreateVirtualCamera` lives in `mfsensorgroup.dll` and only exists on
//! Windows 11 22H2 (build 22621) and later. Windows 10 ships that DLL, but
//! without the export — so a *static* import makes the loader refuse to start
//! the process at all:
//!
//! ```text
//! relay-share.exe - Entry Point Not Found
//! The procedure entry point MFCreateVirtualCamera could not be located in
//! the dynamic link library ...\relay-share.exe
//! ```
//!
//! That killed `relay-share.exe` on a real Windows 10 machine before `main()`
//! ran, which meant neither sending nor receiving worked there — regardless of
//! the virtual camera being opt-in and declined. The runtime error handling
//! around the call never got a chance to run.
//!
//! Delay-loading defers resolution to the first call. Nothing resolves the
//! export unless the camera is actually started, and `render.rs` refuses to do
//! that below `MIN_VCAM_BUILD`. Keep that guard: with delay-load a missing
//! export raises a structured exception rather than returning an error, so the
//! version check is what keeps it from ever being reached.
include!("../../build-support/versioninfo.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../build-support/versioninfo.rs");
    versioninfo::embed(
        versioninfo::Target::Bin("relay-share"),
        "relay-share.exe",
        "Relay share engine",
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    // delayimp.lib provides the helper the linker calls on first use.
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:mfsensorgroup.dll");
}
