//! End-to-end shared-memory parameter path: this process stands in for the
//! APO (creates the section for the default render endpoint, like audiodg
//! would), then drives the real `ApoAudioControl` backend through the
//! `AudioControl` trait and watches the block from the "APO" side.
//!
//! Requires a default render endpoint; skips (cleanly) on machines without
//! one, mirroring the other live-endpoint tests in this crate.

#![cfg(windows)]

use relay_core::apply::AudioControl;
use relay_core::audio_apo::ApoAudioControl;
use relay_core::types::{AudioSettings, EqBand};

#[test]
fn apply_writes_params_and_restore_bypasses() {
    // Serialise the whole scenario in one test: the env vars and the section
    // are process-wide.
    std::env::set_var("RELAY_APO_LOCAL_SECTION", "1");
    std::env::set_var("RELAY_INSTANCE", format!("apotest{}", std::process::id()));

    let guid = match relay_audio::sessions::default_render_endpoint_guid() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipping: no default render endpoint ({e})");
            return;
        }
    };
    let name = relay_audio::shm::section_name(&guid, &std::env::var("RELAY_INSTANCE").unwrap())
        .replacen("Global\\", "Local\\", 1);
    let apo_side = relay_audio::shm::SharedParams::create(&name).expect("create section");
    assert!(apo_side.created, "test must own a fresh section");
    assert!(apo_side.block().bypass(), "resting state is bypass");

    let control = ApoAudioControl::default();

    // capture() before anything: honest snapshot of the resting state.
    let original = control.capture().expect("capture");
    assert!(original.bypass);

    // apply() with real settings: chain goes live.
    let settings = AudioSettings {
        bands: vec![EqBand { freq_hz: 1000.0, gain_db: 4.0, q: 1.0 }],
        hrtf: false,
        limiter: None,
        apply_to_share: false,
        headset_correction: false,
        ..AudioSettings::default()
    };
    let state = control.apply(&settings, None).expect("apply");
    assert_eq!(state, relay_core::types::AudioChainState::Active);
    assert!(!apo_side.block().bypass(), "apply clears the bypass word");
    let (_, params) = apo_side.block().read_params().expect("params readable");
    assert_eq!(params.bands.len(), 1);
    assert_eq!(params.bands[0].freq_hz, 1000.0);
    assert_eq!(params.bands[0].gain_db, 4.0);

    // Empty settings never activate anything.
    let state = control.apply(&AudioSettings::default(), None).expect("apply empty");
    assert_eq!(state, relay_core::types::AudioChainState::Bypass);

    // restore() = bypass, whatever was captured.
    control.restore(&original).expect("restore");
    assert!(apo_side.block().bypass(), "restore sets the bypass word");
}
