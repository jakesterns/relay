//! S51: NDI® output with no usable NDI runtime is an answer, not a crash.
//! Runs the real loader in a child process (`relay-share ndi-probe`), pointed
//! by `RELAY_NDI_RUNTIME` at a folder of our making, so it means the same on a
//! PC with the NDI runtime installed and on one without.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("relay-ndi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn probe(runtime_dir: &PathBuf) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_relay-share"))
        .arg("ndi-probe")
        .env("RELAY_NDI_RUNTIME", runtime_dir)
        .output()
        .expect("spawn relay-share");
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().rev().find(|l| l.starts_with('{')).expect("a JSON line");
    serde_json::from_str(line).unwrap()
}

#[test]
fn absent_runtime_is_reported_with_the_download_link() {
    let dir = temp_dir("absent");
    let v = probe(&dir);
    assert_eq!(v["loaded"], false);
    assert_eq!(v["present"], false);
    assert_eq!(v["runtime_missing"], true);
    assert!(v["error"].as_str().unwrap().starts_with("NDI output needs the NDI runtime"));
    assert_eq!(v["download_url"], "http://ndi.link/NDIRedistV6");
    assert_eq!(v["searched"][0], dir.display().to_string());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_runtime_dll_fails_cleanly() {
    // A file with the runtime's name that is not a DLL: LoadLibrary refuses
    // it and the engine says so instead of dying.
    let dir = temp_dir("broken");
    std::fs::write(dir.join("Processing.NDI.Lib.x64.dll"), b"not a dll").unwrap();
    let v = probe(&dir);
    assert_eq!(v["loaded"], false);
    assert_eq!(v["present"], true);
    assert_eq!(v["runtime_missing"], false);
    assert!(v["error"].as_str().unwrap().contains("would not load"), "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}
