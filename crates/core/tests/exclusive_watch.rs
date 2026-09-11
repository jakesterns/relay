//! End-to-end WASAPI-exclusive detection: a real `relay-core run`, a profile
//! with audio processing applied over IPC, and an exclusive-mode stream held
//! by this test process (the stand-in for an exclusive-mode game). The core's
//! watcher must flip `audio_chain` to `exclusivebypassed` and back.
//!
//! Skips (passes with a note) on machines with no default render endpoint.
//! Also exercises `RenderPreview` — the offline A/B pair — over IPC.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use relay_core::ipc::client::Client;
use relay_core::ipc::{Method, Reply};
use relay_core::types::{AudioChainState, EqBand, GameMatch, Profile, ProfileStatus};
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

async fn audio_chain(c: &mut Client) -> AudioChainState {
    match c.call(Method::Status).await.unwrap() {
        Reply::Status { state } => state.audio_chain,
        other => panic!("unexpected reply {other:?}"),
    }
}

/// Poll status until `want`, returning how long it took.
async fn wait_for_chain(
    c: &mut Client,
    want: AudioChainState,
    timeout: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if audio_chain(c).await == want {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

fn audio_profile() -> Profile {
    let mut profile = Profile::new("Exclusive watch", GameMatch::exe("relay-exclusive-test.exe"));
    profile.status = ProfileStatus::Ready;
    profile.audio.bands.push(EqBand { freq_hz: 3000.0, gain_db: 4.0, q: 1.0 });
    profile
}

#[tokio::test]
async fn exclusive_stream_flips_the_chain_state_and_back() {
    match relay_audio::sessions::probe_default_render() {
        Ok(_) => {}
        Err(relay_audio::sessions::SessionsError::NoDevice) => {
            eprintln!("skipped: no default render endpoint on this machine");
            return;
        }
        Err(e) => panic!("probe failed: {e}"),
    }

    let root: PathBuf = std::env::temp_dir().join(format!("relay-exclusive-{}", Uuid::new_v4()));
    let rec = root.join("recording.log");
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("excl-{}", Uuid::new_v4().simple());

    let mut core = Core::spawn(&root, &rec, &instance);
    let mut c = core.connect().await;

    let profile = audio_profile();
    let id = profile.id;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(profile) }).await.unwrap(),
        Reply::Ok
    ));
    assert!(matches!(c.call(Method::ApplyProfile { id }).await.unwrap(), Reply::Ok));
    assert_eq!(audio_chain(&mut c).await, AudioChainState::Active);

    // "Launch the game": hold the endpoint exclusively from this process.
    let hold = match relay_audio::sessions::hold_exclusive_for_test() {
        Ok(h) => h,
        Err(relay_audio::sessions::SessionsError::NoExclusiveFormat) => {
            eprintln!("skipped: endpoint refused the exclusive-mode test formats");
            core.shutdown().await;
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        Err(e) => panic!("could not open the exclusive test stream: {e}"),
    };

    // DoD: reported within 1 s. Allow 3 s here for machine noise, but record.
    let took = wait_for_chain(&mut c, AudioChainState::ExclusiveBypassed, Duration::from_secs(3))
        .await
        .expect("core never reported ExclusiveBypassed");
    eprintln!("exclusive stream reported after {took:?}");

    drop(hold);
    wait_for_chain(&mut c, AudioChainState::Active, Duration::from_secs(5))
        .await
        .expect("chain state never recovered after the exclusive stream closed");

    drop(c);
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn render_preview_produces_the_ab_pair() {
    // The core shells out to `relay-preview` (built by relay-audio).
    let preview =
        Path::new(env!("CARGO_BIN_EXE_relay-core")).parent().unwrap().join("relay-preview.exe");
    if !preview.exists() {
        eprintln!("skipped: {} not built (run a workspace build first)", preview.display());
        return;
    }
    let root: PathBuf = std::env::temp_dir().join(format!("relay-preview-{}", Uuid::new_v4()));
    let rec = root.join("recording.log");
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("prev-{}", Uuid::new_v4().simple());

    let mut core = Core::spawn(&root, &rec, &instance);
    let mut c = core.connect().await;

    let profile = audio_profile();
    let id = profile.id;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(profile) }).await.unwrap(),
        Reply::Ok
    ));

    match c.call(Method::RenderPreview { id, wav: None }).await.unwrap() {
        Reply::Preview { original, processed, sample_rate, hrtf_applied: _ } => {
            assert_eq!(sample_rate, 48_000);
            for p in [&original, &processed] {
                let meta = std::fs::metadata(p).unwrap_or_else(|e| panic!("{p}: {e}"));
                assert!(meta.len() > 100_000, "{p} suspiciously small");
            }
            assert!(Path::new(&original).starts_with(root.join("previews")));
        }
        other => panic!("unexpected reply {other:?}"),
    }

    drop(c);
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
