//! Crash-restore on the vendor colour path, both vendors, same assertions.
//!
//! This is `crash_restore.rs` with the display backend swapped from the
//! line-logging recorder to the simulated rig (`RELAY_DISPLAY_SIM`), so the
//! assertions can be about *values* rather than about which calls happened:
//! a real `relay-core` applies a profile, is killed with `taskkill /F`, and
//! the restart has to put brightness, the gamma ramp and the GPU colour back
//! to the exact numbers it found.
//!
//! Everything above `DisplayIo` is production code — planning,
//! backup-before-apply, snapshot format, vendor dispatch, the recovery that
//! runs before the pipe opens. What is simulated is the four primitive reads
//! and writes at the bottom. So this proves the AMD *path*; the AMD *driver*
//! needs a Radeon with a monitor on it, which this machine does not have
//! (`docs/plans/M2-display.md`).

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use relay_core::backup::GpuVendor;
use relay_core::display_sim::SimState;
use relay_core::ipc::client::Client;
use relay_core::ipc::{Method, Reply};
use relay_core::types::{GameMatch, Profile, ProfileStatus};
use uuid::Uuid;

struct Core {
    child: Child,
    instance: String,
}

impl Core {
    fn spawn(root: &Path, sim: &Path, instance: &str, vendor: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_relay-core"))
            .args(["run", "--data-dir"])
            .arg(root)
            .env("RELAY_INSTANCE", instance)
            .env("RELAY_DISPLAY_SIM", sim)
            .env("RELAY_DISPLAY_SIM_VENDOR", vendor)
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

fn snapshot_applied(root: &Path) -> Option<bool> {
    let bytes = std::fs::read(root.join("data").join("original-state.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["applied"].as_bool()
}

/// The vendor recorded in the pending snapshot — the field that decides which
/// API the restore will address.
fn snapshot_vendor(root: &Path) -> Option<String> {
    let bytes = std::fs::read(root.join("data").join("original-state.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["display"]["targets"][0]["nvapi"]["vendor"].as_str().map(|s| s.to_string())
}

/// A profile that drives all three paths at once: DDC/CI brightness, the
/// gamma ramp, and the vendor colour API.
fn profile() -> Profile {
    let mut p = Profile::new("AMD crash test", GameMatch::exe("relay-crash-test.exe"));
    p.status = ProfileStatus::Ready;
    p.display.follow_focus = true;
    p.display.monitor.brightness = Some(95);
    p.display.gpu.gamma = 1.2;
    p.display.gpu.vibrance = 75;
    p
}

async fn crash_and_restore(vendor: GpuVendor, label: &str) -> (Duration, (u32, u16, i32)) {
    let root: PathBuf =
        std::env::temp_dir().join(format!("relay-crash-{label}-{}", Uuid::new_v4()));
    let sim = root.join("sim-display.json");
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("crash-{label}-{}", Uuid::new_v4().simple());

    // Boot the simulated rig into a known state and remember it.
    let before = SimState::initial(vendor);
    std::fs::write(&sim, serde_json::to_vec_pretty(&before).unwrap()).unwrap();

    // First life: save a Ready profile and apply it.
    let mut core = Core::spawn(&root, &sim, &instance, label);
    let mut c = core.connect().await;
    let p = profile();
    let id = p.id;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(p) }).await.unwrap(),
        Reply::Ok
    ));
    assert!(matches!(c.call(Method::ApplyProfile { id }).await.unwrap(), Reply::Ok));
    drop(c);

    assert_eq!(snapshot_applied(&root), Some(true), "snapshot must be on disk and pending");
    assert_eq!(
        snapshot_vendor(&root).as_deref(),
        Some(label),
        "the snapshot records the vendor the restore must go back through"
    );

    let applied = SimState::read(&sim).unwrap();
    assert_ne!(applied.vcp[&0x10], before.vcp[&0x10], "[{label}] brightness moved over DDC/CI");
    assert_ne!(applied.ramp, before.ramp, "[{label}] gamma ramp moved");
    assert_ne!(applied.gpu.dvc, before.gpu.dvc, "[{label}] vendor colour moved");
    println!("  [{label}] before   {:?}", before.summary());
    println!("  [{label}] applied  {:?}", applied.summary());

    // Crash. Nothing gets a chance to clean up.
    core.kill_hard();
    let killed = SimState::read(&sim).unwrap();
    assert_eq!(killed, applied, "[{label}] a hard kill leaves the hardware changed");
    assert_eq!(snapshot_applied(&root), Some(true), "and the snapshot pending");
    println!("  [{label}] killed   {:?}", killed.summary());

    // Second life: recovery runs before the pipe opens, so the time from
    // launch to a connectable core is an upper bound on the restore.
    let start = Instant::now();
    let mut core = Core::spawn(&root, &sim, &instance, label);
    let mut c = core.connect().await;
    let elapsed = start.elapsed();

    let restored = SimState::read(&sim).unwrap();
    assert_eq!(restored, before, "[{label}] every value back to exactly what it was");
    assert_eq!(snapshot_applied(&root), Some(false), "[{label}] snapshot cleared after restore");
    println!(
        "  [{label}] restored {:?} in <= {:.1} ms",
        restored.summary(),
        elapsed.as_secs_f64() * 1e3
    );

    match c.call(Method::Status).await.unwrap() {
        Reply::Status { state } => assert!(state.active_profile.is_none()),
        other => panic!("unexpected reply {other:?}"),
    }
    drop(c);
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
    (elapsed, applied.summary())
}

/// The AMD path, through a real hard kill.
#[tokio::test]
async fn amd_killed_mid_apply_restores_on_restart() {
    crash_and_restore(GpuVendor::Amd, "amd").await;
}

/// And the NVIDIA path through the same harness, so "identical on both
/// vendors" is a test rather than a claim. The two differ only in the raw
/// numbers each driver's units produce.
#[tokio::test]
async fn nvidia_killed_mid_apply_restores_on_restart() {
    crash_and_restore(GpuVendor::Nvidia, "nvidia").await;
}

/// The vendor units are genuinely different — if these ever matched, the
/// mapping would have collapsed into one curve and the AMD range would be
/// going unused.
#[tokio::test]
async fn the_two_vendors_write_different_raw_values_for_the_same_profile() {
    let (_, amd) = crash_and_restore(GpuVendor::Amd, "amd").await;
    let (_, nvidia) = crash_and_restore(GpuVendor::Nvidia, "nvidia").await;
    assert_eq!(amd.0, nvidia.0, "DDC/CI brightness is vendor-neutral");
    assert_eq!(amd.1, nvidia.1, "the gamma ramp is vendor-neutral");
    assert_eq!(amd.2, 150, "75 % vibrance = halfway from the 100 default to 200");
    assert_eq!(nvidia.2, 32, "75 % vibrance = halfway up a 0..63 DVC range");
}
