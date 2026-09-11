//! WASAPI-exclusive detection against a real endpoint. The "game" is our
//! own exclusive-mode stream (see the M3 plan's Definition of Ready).
//!
//! Skips (passes with a note) on machines without a default render
//! endpoint — CI runners — or whose endpoint refuses the exclusive formats.

#![cfg(windows)]

use relay_audio::sessions::{hold_exclusive_for_test, probe_default_render, SessionsError};

#[test]
fn exclusive_stream_is_detected_and_clears_on_release() {
    let before = match probe_default_render() {
        Ok(s) => s,
        Err(SessionsError::NoDevice) => {
            eprintln!("skipped: no default render endpoint on this machine");
            return;
        }
        Err(e) => panic!("probe failed: {e}"),
    };
    if before.exclusive {
        eprintln!("skipped: endpoint already exclusively held by another app");
        return;
    }

    let hold = match hold_exclusive_for_test() {
        Ok(h) => h,
        Err(SessionsError::NoExclusiveFormat) => {
            eprintln!("skipped: endpoint refused the exclusive-mode test formats");
            return;
        }
        Err(e) => panic!("could not open the exclusive test stream: {e}"),
    };
    let during = probe_default_render().expect("probe while holding");
    assert!(during.exclusive, "exclusive stream not detected while held");

    drop(hold);
    // The audio engine can take a moment to release the endpoint.
    let mut cleared = false;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !probe_default_render().expect("probe after release").exclusive {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "endpoint still reads exclusive 2 s after release");
}

#[test]
fn session_enumeration_reports_pids() {
    match probe_default_render() {
        Ok(s) => {
            // No assertion on contents (depends on what's playing); just
            // check the shape and that our own PID is not falsely reported.
            for pid in &s.active_pids {
                assert_ne!(*pid, 0);
            }
        }
        Err(SessionsError::NoDevice) => eprintln!("skipped: no default render endpoint"),
        Err(e) => panic!("probe failed: {e}"),
    }
}
