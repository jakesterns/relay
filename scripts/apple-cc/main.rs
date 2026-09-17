//! C compiler shim for checking Relay against the Apple targets on a machine
//! with no Mac and no Xcode: forwards to `zig cc`, which carries the macOS
//! libc headers, and rewrites the clang target flags `cc-rs` passes into the
//! form zig understands.
//!
//! Only a few build scripts compile C for these targets — `ring` (webrtc-rs's
//! DTLS) and `objc2-exception-helper` (Tauri). Nothing is linked: this is for
//! `cargo check` / `cargo clippy`, which is the bar for macOS today. See
//! `docs/dev/porting.md`.
//!
//! Built by `scripts/check-macos.ps1` and by CI with plain `rustc`, so it is
//! not a workspace member. `ZIG` names the zig executable. A copy whose file
//! name ends in `-ar` forwards to `zig ar` instead, for `AR_<target>`.

use std::process::{exit, Command};

fn zig_target(rust_target: &str) -> Option<&'static str> {
    match rust_target {
        t if t.starts_with("aarch64-apple-darwin") || t.starts_with("arm64-apple-macos") => {
            Some("aarch64-macos")
        }
        t if t.starts_with("x86_64-apple-darwin") || t.starts_with("x86_64-apple-macos") => {
            Some("x86_64-macos")
        }
        _ => None,
    }
}

fn main() {
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let is_ar = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().ends_with("-ar")))
        .unwrap_or(false);
    if is_ar {
        let status = Command::new(&zig).arg("ar").args(std::env::args().skip(1)).status();
        exit(status.ok().and_then(|s| s.code()).unwrap_or(2));
    }
    let mut target = std::env::var("TARGET").ok().and_then(|t| zig_target(&t));
    let mut args = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if let Some(t) = a.strip_prefix("--target=") {
            target = zig_target(t).or(target);
        } else if a == "-target" {
            if let Some(t) = it.next() {
                target = zig_target(&t).or(target);
            }
        } else if a == "-arch" {
            it.next();
        } else if !a.starts_with("-mmacosx-version-min") {
            args.push(a);
        }
    }
    let Some(target) = target else {
        eprintln!("apple-cc: no Apple target in TARGET or the arguments");
        exit(2);
    };
    let status = Command::new(&zig)
        .args(["cc", "-target", target])
        .args(&args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("apple-cc: running {zig}: {e}");
            exit(2);
        });
    exit(status.code().unwrap_or(1));
}
