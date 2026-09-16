//! Windows version resources for relay-core's three binaries.
//!
//! Without one, Task Manager lists the always-on process as a bare
//! `relay-core.exe`, file properties show no publisher, and once the binaries
//! are signed the UAC prompt raised for `relay-elevate.exe` would have no
//! program name to show beside the verified publisher. The publisher string
//! matches `bundle.publisher` in `ui/src-tauri/tauri.conf.json`, which is what
//! Add/Remove Programs and `relay-ui.exe` carry.
//!
//! A few kilobytes of `.rsrc`; the loader maps it, nothing reads it at run
//! time, so it costs the footprint gate nothing.
//!
//! No description may contain "install", "setup" or "update": Windows'
//! installer detection keys on those words in an unmanifested binary and
//! would raise a UAC prompt on launch that nobody asked for.

const PUBLISHER: &str = "Relay";

const BINS: &[(&str, &str)] = &[
    ("relay-core", "Relay"),
    ("relay-svc", "Relay launcher"),
    ("relay-elevate", "Relay administrator helper"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let mut parts: Vec<u16> = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(3)
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    parts.resize(4, 0);
    let numeric = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    for (bin, description) in BINS {
        let rc = out.join(format!("{bin}.rc"));
        std::fs::write(&rc, resource_script(bin, description, &version, &numeric))
            .expect("writing the version resource script");
        embed_resource::compile_for(&rc, [bin], embed_resource::NONE)
            .manifest_optional()
            .expect("compiling the version resource");
    }
}

fn resource_script(bin: &str, description: &str, version: &str, numeric: &str) -> String {
    format!(
        r#"#include <winver.h>
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "{PUBLISHER}"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{bin}"
      VALUE "OriginalFilename", "{bin}.exe"
      VALUE "ProductName", "Relay"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    )
}
