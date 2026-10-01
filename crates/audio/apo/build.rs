include!("../../../build-support/versioninfo.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../../build-support/versioninfo.rs");
    versioninfo::embed(versioninfo::Target::Cdylib, "relay_apo.dll", "Relay audio effects");
}
