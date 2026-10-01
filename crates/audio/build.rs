include!("../../build-support/versioninfo.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../build-support/versioninfo.rs");
    versioninfo::embed(
        versioninfo::Target::Bin("relay-preview"),
        "relay-preview.exe",
        "Relay audio preview renderer",
    );
}
