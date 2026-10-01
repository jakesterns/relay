// Shared Windows VERSIONINFO generator, `include!`d by each crate's build.rs.
//
// SignPath requires a version resource on every signed file, and the values
// must agree across all of them: ProductName "Relay", CompanyName "Relay
// contributors" (also `bundle.publisher` in ui/src-tauri/tauri.conf.json),
// the workspace version, the MIT copyright line. A few kilobytes of `.rsrc`
// the loader maps and nothing reads, so the footprint gate is unaffected.
//
// No description may contain "install", "setup" or "update": Windows'
// installer detection keys on those words in an unmanifested binary and would
// raise a UAC prompt on launch that nobody asked for.

#[allow(dead_code)]
mod versioninfo {
    pub const PRODUCT: &str = "Relay";
    pub const COMPANY: &str = "Relay contributors";
    pub const COPYRIGHT: &str = "MIT License, Relay contributors";

    /// Where the resource is linked.
    pub enum Target<'a> {
        /// One `[[bin]]` of this package, by name.
        Bin(&'a str),
        /// The package's cdylib.
        Cdylib,
    }

    /// Compiles and links a version resource for `file` (the output file
    /// name, e.g. `relay-share.exe`). No-op off Windows.
    pub fn embed(target: Target, file: &str, description: &str) {
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
        let (stem, file_type) = match file.rsplit_once('.') {
            Some((s, "dll")) => (s, "VFT_DLL"),
            Some((s, _)) => (s, "VFT_APP"),
            None => (file, "VFT_APP"),
        };
        let rc_text = format!(
            r#"#include <winver.h>
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS VOS_NT_WINDOWS32
FILETYPE {file_type}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "{COMPANY}"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{stem}"
      VALUE "LegalCopyright", "{COPYRIGHT}"
      VALUE "OriginalFilename", "{file}"
      VALUE "ProductName", "{PRODUCT}"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
        );
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
        let rc = out.join(format!("{stem}.rc"));
        std::fs::write(&rc, rc_text).expect("writing the version resource script");
        let res = match target {
            Target::Bin(bin) => embed_resource::compile_for(&rc, [bin], embed_resource::NONE),
            Target::Cdylib => embed_resource::compile_for_cdylib(&rc, embed_resource::NONE),
        };
        res.manifest_optional().expect("compiling the version resource");
    }
}
