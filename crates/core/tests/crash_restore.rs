//! Crash-restore harness.
//!
//! Spawns a real `relay-core run` with a temp data root and the file-backed
//! recording backend, applies a profile over IPC, kills the process with
//! `taskkill /F` (no chance to clean up), restarts it, and asserts that the
//! restart restored the original state and cleared the snapshot. Also checks
//! the saved profile survived the restart.

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
    fn spawn(root: &Path, recording: &Path, instance: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_relay-core"))
            .args(["run", "--data-dir"])
            .arg(root)
            .env("RELAY_INSTANCE", instance)
            .env("RELAY_RECORDING_BACKEND", recording)
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

fn recording(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[tokio::test]
async fn killed_mid_apply_restores_on_restart() {
    let root: PathBuf = std::env::temp_dir().join(format!("relay-crash-{}", Uuid::new_v4()));
    let rec = root.join("recording.log");
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("crashtest-{}", Uuid::new_v4().simple());

    // First life: save a Ready profile and apply it.
    let mut core = Core::spawn(&root, &rec, &instance);
    let mut c = core.connect().await;
    assert!(matches!(c.call(Method::Ping).await.unwrap(), Reply::Pong));

    let mut profile = Profile::new("Crash test", GameMatch::exe("relay-crash-test.exe"));
    profile.status = ProfileStatus::Ready;
    profile.display.follow_focus = true;
    let id = profile.id;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(profile) }).await.unwrap(),
        Reply::Ok
    ));
    assert!(matches!(c.call(Method::ApplyProfile { id }).await.unwrap(), Reply::Ok));
    drop(c);

    assert_eq!(snapshot_applied(&root), Some(true), "snapshot must be on disk and pending");
    let before = recording(&rec);
    assert!(before.contains(&"display.apply".to_string()), "apply recorded: {before:?}");
    assert!(!before.contains(&"display.restore".to_string()), "nothing restored yet");

    // Crash.
    core.kill_hard();
    assert_eq!(snapshot_applied(&root), Some(true), "a hard kill leaves the snapshot pending");

    // Second life: recovery happens before the pipe opens.
    let mut core = Core::spawn(&root, &rec, &instance);
    let mut c = core.connect().await;

    let after = recording(&rec);
    let restores = after.iter().filter(|l| *l == "display.restore").count();
    let audio_restores = after.iter().filter(|l| *l == "audio.restore").count();
    assert_eq!(restores, 1, "display restored exactly once on restart: {after:?}");
    assert_eq!(audio_restores, 1, "audio restored exactly once on restart: {after:?}");
    assert_eq!(snapshot_applied(&root), Some(false), "snapshot cleared after restore");

    // The profile written before the crash is still there.
    match c.call(Method::ListProfiles).await.unwrap() {
        Reply::Profiles { profiles } => {
            assert!(profiles.iter().any(|p| p.id == id), "profile survived restart");
        }
        other => panic!("unexpected reply {other:?}"),
    }

    // Status shows nothing applied.
    match c.call(Method::Status).await.unwrap() {
        Reply::Status { state } => assert!(state.active_profile.is_none()),
        other => panic!("unexpected reply {other:?}"),
    }
    drop(c);

    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn second_instance_exits_immediately() {
    let root: PathBuf = std::env::temp_dir().join(format!("relay-single-{}", Uuid::new_v4()));
    let rec = root.join("recording.log");
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("single-{}", Uuid::new_v4().simple());

    let mut core = Core::spawn(&root, &rec, &instance);
    let _ = core.connect().await;

    let out = Command::new(env!("CARGO_BIN_EXE_relay-core"))
        .args(["run", "--data-dir"])
        .arg(&root)
        .env("RELAY_INSTANCE", &instance)
        .output()
        .expect("spawn second instance");
    assert!(out.status.success(), "second instance must exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("already running"), "got: {text}");

    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
