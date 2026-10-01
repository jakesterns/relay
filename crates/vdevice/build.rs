include!("../../build-support/versioninfo.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../build-support/versioninfo.rs");
    versioninfo::embed(versioninfo::Target::Cdylib, "relay_vdevice.dll", "Relay virtual camera");
    // COM entry points under their canonical names, on the cdylib only.
    // The Rust symbols carry unique names (RelayVdevice*) so the rlib can
    // coexist with relay-apo's identical exports inside one binary; the
    // alias below puts the standard names into the DLL's export table.
    // Only when the COM media source is compiled in (feature "com"); the
    // no-default-features dependency build has no symbols to alias.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() && std::env::var("CARGO_FEATURE_COM").is_ok() {
        println!(
            "cargo:rustc-cdylib-link-arg=/EXPORT:DllGetClassObject=RelayVdeviceDllGetClassObject"
        );
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:DllCanUnloadNow=RelayVdeviceDllCanUnloadNow");
    }
}
