//! S44e: the applied chain survives audiodg rebuilding its graph.
//!
//! This process plays audiodg: it creates and drops the endpoint's section
//! the way APO instances do, while the real `ApoAudioControl` applies a
//! profile and the service's 1 s tick (`keep_alive`) runs. Covers:
//! - apply with nothing playing (no section yet) -> NotLoaded or
//!   NotInstalled, then a stream starts and keep_alive activates the chain;
//! - the creating instance dropping its handle: the held section keeps the
//!   name alive, and the next instance opens the same block, still active;
//! - a block rewritten or bypassed by someone else is re-asserted;
//! - restore bypasses and lets go of the hold, so the next instance comes
//!   up fresh in bypass.
//!
//! One test per file: the env vars and sections are process-wide.

#![cfg(windows)]

use relay_audio::shm::SharedParams;
use relay_core::apply::AudioControl;
use relay_core::audio_apo::ApoAudioControl;
use relay_core::types::{AudioChainState, AudioSettings, EqBand};

#[test]
fn applied_chain_survives_instance_rebuilds() {
    std::env::set_var("RELAY_APO_LOCAL_SECTION", "1");
    std::env::set_var("RELAY_INSTANCE", format!("keepalive{}", std::process::id()));
    let guid = match relay_audio::sessions::default_render_endpoint_guid() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipping: no default render endpoint ({e})");
            return;
        }
    };
    let name = relay_audio::shm::section_name(&guid, &std::env::var("RELAY_INSTANCE").unwrap())
        .replacen("Global\\", "Local\\", 1);

    let control = ApoAudioControl::default();
    let original = control.capture().expect("capture");
    let settings = AudioSettings {
        bands: vec![EqBand { freq_hz: 250.0, gain_db: -5.0, q: 1.2 }],
        ..AudioSettings::default()
    };

    // 1. Nothing playing: no APO instance, so no section.
    let state = control.apply(&settings, None).expect("apply");
    assert!(
        matches!(state, AudioChainState::NotLoaded | AudioChainState::NotInstalled),
        "{state:?}"
    );
    assert!(!control.holds_section());
    let idle = control.keep_alive();
    assert!(
        matches!(idle, Some(AudioChainState::NotLoaded | AudioChainState::NotInstalled)),
        "{idle:?}"
    );

    // 2. A stream starts: audiodg's instance creates the section (bypass).
    let first = SharedParams::create(&name).expect("create");
    assert!(first.created && first.block().bypass());
    assert_eq!(control.keep_alive(), Some(AudioChainState::Active));
    assert!(control.holds_section());
    assert!(!first.block().bypass(), "keep_alive activates the late section");
    let (_, p) = first.block().read_params().expect("params");
    assert_eq!(p.bands.len(), 1);
    assert_eq!(p.bands[0].gain_db, -5.0);

    // Steady state: nothing rewritten, sequence unchanged.
    let seq = first.block().sequence();
    assert_eq!(control.keep_alive(), Some(AudioChainState::Active));
    assert_eq!(first.block().sequence(), seq, "no rewrite when nothing changed");

    // 3. audiodg rebuilds: the instance drops its handle, a new one creates.
    drop(first);
    let second = SharedParams::create(&name).expect("recreate");
    assert!(!second.created, "the held section kept the name alive");
    assert!(!second.block().bypass(), "the new instance starts active");
    assert_eq!(second.block().read_params().unwrap().1.bands[0].gain_db, -5.0);

    // 4. Someone else bypasses / rewrites the block: re-asserted.
    second.block().set_bypass(true);
    assert_eq!(control.keep_alive(), Some(AudioChainState::Active));
    assert!(!second.block().bypass());
    second.block().write_params(&relay_audio::params::ChainParams::default());
    control.keep_alive();
    assert_eq!(second.block().read_params().unwrap().1.bands.len(), 1, "params re-asserted");

    // 5. Restore: bypass, and the hold is released.
    control.restore(&original).expect("restore");
    assert!(second.block().bypass());
    assert!(!control.holds_section());
    assert_eq!(control.keep_alive(), None, "nothing to keep alive after restore");
    drop(second);
    let third = SharedParams::create(&name).expect("fresh");
    assert!(third.created && third.block().bypass(), "no leftover chain after restore");
}
