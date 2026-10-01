//! Windows version resources for relay-core's three binaries; the shared
//! generator and its rules live in `build-support/versioninfo.rs`.
//!
//! Without one, Task Manager lists the always-on process as a bare
//! `relay-core.exe`, and the UAC prompt raised for `relay-elevate.exe` would
//! have no program name to show beside the verified publisher.

include!("../../build-support/versioninfo.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../build-support/versioninfo.rs");
    use versioninfo::{embed, Target::Bin};
    embed(Bin("relay-core"), "relay-core.exe", "Relay");
    embed(Bin("relay-svc"), "relay-svc.exe", "Relay launcher");
    embed(Bin("relay-elevate"), "relay-elevate.exe", "Relay administrator helper");
}
