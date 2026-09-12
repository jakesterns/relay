//! Crash-restore against the *real* display backend, on real hardware.
//!
//! `cargo test -p relay-core --test crash_restore_display -- --ignored --nocapture`
//!
//! Same shape as `crash_restore.rs`, but with no recording backend: the core
//! runs its production `WinDisplay`, the profile carries a small real delta
//! (brightness −5 via DDC/CI, vibrance 60 via NvAPI, gamma 1.1 via ramp), and
//! the assertions read the actual monitor/GPU state between the phases:
//!
//! 1. before   — record the true current values
//! 2. applied  — values changed, snapshot pending on disk
//! 3. killed   — `taskkill /F`, values still changed (nobody restored)
//! 4. restart  — recovery runs before the pipe opens; values back to (1)
//!
//! Requires the physical monitor to be awake (DDC/CI answers). The test
//! changes real settings briefly; everything is restored by the core under
//! test, and a belt-and-braces restore runs in the harness on failure.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use relay_core::ipc::client::Client;
use relay_core::ipc::{Method, Reply};
use relay_core::types::{GameMatch, Profile, ProfileStatus};
use uuid::Uuid;

struct Core {
    child: Child,
    instance: String,
}

impl Core {
    fn spawn(root: &Path, instance: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_relay-core"))
            .args(["run", "--data-dir"])
            .arg(root)
            .env("RELAY_INSTANCE", instance)
            .env_remove("RELAY_RECORDING_BACKEND")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn relay-core");
        Self { child, instance: instance.to_string() }
    }

    fn pipe(&self) -> String {
        format!(r"\\.\pipe\relay-core-{}", self.instance)
    }

    async fn connect(&mut self) -> Client {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(c) = Client::connect_to(&self.pipe()).await {
                return c;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("relay-core exited early with {status}");
            }
            assert!(Instant::now() < deadline, "relay-core never opened its pipe");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn kill_hard(&mut self) {
        let status = Command::new("taskkill")
            .args(["/F", "/PID", &self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run taskkill");
        assert!(status.success(), "taskkill failed: {status}");
        let _ = self.child.wait();
    }

    async fn shutdown(mut self) {
        let mut c = self.connect().await;
        let _ = c.call(Method::Shutdown).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().expect("wait").is_none() {
            assert!(Instant::now() < deadline, "relay-core did not stop after shutdown");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The live values the profile below touches, read straight from hardware.
#[derive(Debug, Clone, PartialEq)]
struct LiveState {
    brightness: u32,
    ramp_mid: u16,
    dvc: Option<i32>,
}

fn read_live() -> LiveState {
    use relay_core::hardware::HardwareProbe;
    let mons = relay_core::hardware::probe_win::WindowsHardwareProbe.probe(false).monitors;
    let primary = mons.iter().find(|m| m.primary).expect("a primary monitor");
    let pm = relay_display::ddc::PhysicalMonitor::open(primary.hmonitor, 60)
        .expect("DDC/CI open (is the monitor awake?)");
    let (brightness, _max) = pm.get_vcp(relay_display::vcp::BRIGHTNESS).expect("read brightness");
    let ramp = relay_display::gamma::io::get_ramp(&primary.gdi_name).expect("read ramp");
    let dvc = relay_display::nvapi::NvApi::load().and_then(|api| {
        let d = api.display_by_gdi_name(&primary.gdi_name)?;
        api.get_vibrance(&d).ok().map(|v| v.current)
    });
    LiveState { brightness, ramp_mid: ramp.r[128], dvc }
}

fn snapshot_applied(root: &Path) -> Option<bool> {
    let bytes = std::fs::read(root.join("data").join("original-state.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["applied"].as_bool()
}

#[tokio::test]
#[ignore = "changes real monitor/GPU settings briefly; run by hand with the monitor awake"]
async fn killed_mid_apply_restores_real_display_on_restart() {
    let root: PathBuf = std::env::temp_dir().join(format!("relay-crash-real-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("crashreal-{}", Uuid::new_v4().simple());

    let before = read_live();
    println!("before:   {before:?}");

    // First life: a Ready profile with a small real delta, applied (pinned).
    let mut core = Core::spawn(&root, &instance);
    let mut c = core.connect().await;

    let mut profile = Profile::new("Crash test (real)", GameMatch::exe("relay-crash-real.exe"));
    profile.status = ProfileStatus::Ready;
    profile.display.follow_focus = true;
    profile.display.monitor.brightness = Some(before.brightness.saturating_sub(5) as u16);
    profile.display.gpu.gamma = 1.1;
    profile.display.gpu.vibrance = 60;
    let id = profile.id;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(profile) }).await.unwrap(),
        Reply::Ok
    ));
    assert!(matches!(c.call(Method::ApplyProfile { id }).await.unwrap(), Reply::Ok));
    drop(c);

    let applied = read_live();
    println!("applied:  {applied:?}");
    assert_eq!(snapshot_applied(&root), Some(true), "snapshot pending on disk");
    assert_eq!(applied.brightness, before.brightness.saturating_sub(5), "DDC/CI took effect");
    assert_ne!(applied.ramp_mid, before.ramp_mid, "gamma ramp took effect");
    if before.dvc.is_some() {
        assert_ne!(applied.dvc, before.dvc, "NvAPI vibrance took effect");
    }

    // Crash. Nothing restores; the machine is left in the applied state.
    core.kill_hard();
    let after_kill = read_live();
    println!("killed:   {after_kill:?}");
    assert_eq!(after_kill, applied, "hard kill leaves settings applied");
    assert_eq!(snapshot_applied(&root), Some(true));

    // Second life: recovery restores the real hardware before the pipe opens.
    let mut core = Core::spawn(&root, &instance);
    let _c = core.connect().await;
    let restored = read_live();
    println!("restored: {restored:?}");
    assert_eq!(snapshot_applied(&root), Some(false), "snapshot cleared after restore");
    assert_eq!(restored, before, "monitor and GPU back to original");

    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
