//! B8: a share that ends while a stdin read is pending must still exit at
//! once. The parent here plays the core: it holds the child's stdin open and
//! never writes to it.
#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Time from spawn to exit, or `None` if the child had to be killed.
fn exit_time(old_drop: bool, limit: Duration) -> Option<Duration> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_relay-share"));
    cmd.arg("stdin-exit-check").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if old_drop {
        cmd.env("RELAY_RUNTIME_DROP", "1");
    }
    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn relay-share");
    let _held_open = child.stdin.take();
    while started.elapsed() < limit {
        if child.try_wait().unwrap().is_some() {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().ok();
    child.wait().ok();
    None
}

#[test]
fn exits_promptly_with_a_stdin_read_pending() {
    let took = exit_time(false, Duration::from_secs(5)).expect("the process never exited");
    println!("exit with stdin held open: {:.0} ms", took.as_secs_f64() * 1e3);
    // 200 ms of work + 100 ms shutdown grace + process start.
    assert!(took < Duration::from_millis(1500), "took {took:?}");
}

#[test]
fn the_old_runtime_drop_is_what_hung() {
    // Keeps the diagnosis honest: if this ever starts exiting on its own, the
    // explanation in `run_async` no longer holds and should be revisited.
    let took = exit_time(true, Duration::from_secs(4));
    println!("old drop, stdin held open: {took:?} (None = still running after 4 s)");
    assert!(took.is_none(), "the plain runtime drop exited in {took:?}");
}
