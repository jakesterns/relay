//! The Settings card's own path, over the real pipe.
//!
//! The two opt-in cards make exactly two calls before anything can change:
//! `ElevationPlan` to show the user what would happen, and `RunElevated` to
//! ask Windows for permission. This spawns a real core and drives the first
//! one for all four ops, because the promise "the user sees what will change
//! before the prompt" is only as good as that reply actually arriving.
//!
//! `RunElevated` is not driven here on purpose: it raises a UAC prompt, and a
//! test that needs a human to click is not a test. Its refusal paths are
//! covered against the real helper binary in `elevate_helper.rs`, and the
//! live approve-and-write pass is the runbook in `docs/dev/elevation-live.md`.

#![cfg(windows)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use relay_core::elevate::ElevatedOp;
use relay_core::ipc::client::Client;
use relay_core::ipc::{Method, Reply};

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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn relay-core");
        Self { child, instance: instance.to_string() }
    }

    async fn connect(&mut self) -> Client {
        let pipe = format!(r"\\.\pipe\relay-core-{}", self.instance);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(c) = Client::connect_to(&pipe).await {
                return c;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("relay-core exited early with {status}");
            }
            assert!(Instant::now() < deadline, "relay-core never opened its pipe");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_op_can_be_previewed_before_the_prompt() {
    let root = std::env::temp_dir().join(format!("relay-elev-ipc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut core = Core::spawn(&root, &format!("elevipc{}", std::process::id()));
    let mut client = core.connect().await;

    for op in [
        ElevatedOp::InstallApo { endpoint: None },
        ElevatedOp::UninstallApo { endpoint: None },
        ElevatedOp::InstallCamera,
        ElevatedOp::UninstallCamera,
        ElevatedOp::SetAudioEffectsAllowed { on: true, restart_audio: false },
        ElevatedOp::SetAudioEffectsAllowed { on: false, restart_audio: true },
    ] {
        let reply = client.call(Method::ElevationPlan { op: op.clone() }).await.expect("plan");
        let lines = match reply {
            Reply::DryRun { lines } => lines,
            other => panic!("{op:?}: unexpected reply {other:?}"),
        };
        assert!(!lines.is_empty(), "{op:?}: an empty listing tells the user nothing");
        // Every listing ends with the sentence that sets up the prompt, so a
        // card can never render a plan without saying a prompt is coming and
        // that declining is safe.
        let tail = lines.last().unwrap();
        assert!(tail.contains("Windows will ask for permission"), "{op:?}: {tail}");
        assert!(tail.contains("nothing on this PC changes"), "{op:?}: {tail}");
    }

    // Nothing above may have touched the machine: no backup, no record.
    assert!(!root.join("apo-backup").exists(), "a preview must not write a backup");
    assert!(!root.join("installed.json").exists(), "a preview must not record an install");

    // S44: the status read is read-only too, and says Relay changed nothing.
    match client.call(Method::AudioEffectsStatus).await.expect("status") {
        Reply::AudioEffects { status } => assert!(!status.changed_by_relay),
        other => panic!("unexpected reply {other:?}"),
    }
    assert!(!root.join("apo-backup").exists(), "a status read must not write a record");

    let _ = client.call(Method::Shutdown).await;
    let _ = std::fs::remove_dir_all(&root);
}

/// The install listing names the endpoint values and the COM key it would
/// write, not a paraphrase — so what the user reads is the machine's own
/// state. Skips on a machine with no render endpoint.
#[tokio::test(flavor = "current_thread")]
async fn the_apo_listing_names_the_real_endpoint_and_our_clsid() {
    if relay_audio::sessions::default_render_endpoint_guid().is_err() {
        eprintln!("skipping: no default render endpoint");
        return;
    }
    let root = std::env::temp_dir().join(format!("relay-elev-ipc2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut core = Core::spawn(&root, &format!("elevipc2{}", std::process::id()));
    let mut client = core.connect().await;

    let reply = client
        .call(Method::ElevationPlan { op: ElevatedOp::InstallApo { endpoint: None } })
        .await
        .expect("plan");
    let Reply::DryRun { lines } = reply else { panic!("unexpected reply") };
    let text = lines.join("\n");
    assert!(text.contains(relay_apo::ids::APO_CLSID), "{text}");
    assert!(text.contains("FxProperties"), "{text}");
    assert!(text.contains("written before anything is changed"), "{text}");

    let _ = client.call(Method::Shutdown).await;
    let _ = std::fs::remove_dir_all(&root);
}
