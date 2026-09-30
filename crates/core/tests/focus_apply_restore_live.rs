//! Apply-on-focus and restore-on-focus-loss, against real hardware.
//!
//! `cargo test -p relay-core --test focus_apply_restore_live -- --ignored --nocapture`
//!
//! This is the product's central safety promise, live: a profile applies only
//! while its app has focus, and the machine goes back the moment focus leaves.
//! `crash_restore_display.rs` covers the crash path; this covers the ordinary
//! one, which is the path that runs thousands of times a day.
//!
//! The app under test is Notepad, because it is on every Windows install and
//! its exe name is whatever this Windows calls it — the test asks the core
//! what it saw in the foreground rather than assuming, so it works whether
//! the machine has the Win32 Notepad or the Store one.
//!
//! No `SetForegroundWindow` anywhere: stealing focus from a test process is
//! subject to Windows' foreground lock and is flaky. Launching a windowed app
//! and closing it again produces two genuine foreground changes, which is
//! what the core's hook is watching for.
//!
//! Timing comes from the core's own log rather than from hardware reads: a
//! DDC/CI round trip is tens of milliseconds on its own and would swamp the
//! measurement. The core runs with `--verbose` so the foreground-change line
//! is there to measure from.
//!
//! Requires the physical monitor to be awake (DDC/CI answers). Settings are
//! changed briefly and restored by the core under test.

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
    root: PathBuf,
}

impl Core {
    fn spawn(root: &Path, instance: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_relay-core"))
            // --verbose so the DEBUG "foreground changed" line reaches the
            // log; the restore line is INFO and would be there regardless.
            .args(["run", "--verbose", "--data-dir"])
            .arg(root)
            .env("RELAY_INSTANCE", instance)
            .env_remove("RELAY_RECORDING_BACKEND")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn relay-core");
        Self { child, instance: instance.to_string(), root: root.to_path_buf() }
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

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("logs").join("core.log")).unwrap_or_default()
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

/// The exe name the core currently reports in the foreground, if any.
async fn foreground_exe(c: &mut Client) -> Option<String> {
    match c.call(Method::Status).await.ok()? {
        Reply::Status { state } => state.foreground.map(|f| f.exe),
        _ => None,
    }
}

/// Wait until the core reports `want` (or, with `want = None`, anything but
/// `other`) in the foreground.
async fn wait_for_foreground(c: &mut Client, matches: impl Fn(&str) -> bool) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(exe) = foreground_exe(c).await {
            if matches(&exe) {
                return Some(exe);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// Seconds-since-midnight from a tracing timestamp (`...THH:MM:SS.ffffffZ`),
/// which is all the precision this measurement needs and avoids pulling in a
/// date library for one line.
fn log_time(line: &str) -> Option<f64> {
    let t = line.split('T').nth(1)?;
    let t = t.split('Z').next()?;
    let mut parts = t.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Milliseconds from the last `foreground changed` line to the first
/// `original state restored` after it.
fn restore_latency_ms(log: &str) -> Option<f64> {
    let lines: Vec<&str> = log.lines().collect();
    let restored = lines.iter().rposition(|l| l.contains("original state restored"))?;
    // Focus leaving is a foreground event; the app exiting with nothing else
    // taking focus fires none, and the core's exit watch logs this instead.
    let blurred = lines[..restored]
        .iter()
        .rposition(|l| l.contains("foreground changed") || l.contains("the profiled app exited"))?;
    Some((log_time(lines[restored])? - log_time(lines[blurred])?) * 1000.0)
}

fn spawn_notepad() {
    // Fire and forget: on Windows 11 `notepad.exe` is an app-execution alias,
    // so the process this starts exits immediately and the real Notepad runs
    // under a different pid. Holding the `Child` would only let us kill a
    // process that is already gone — which is exactly how the first version
    // of this test failed, silently leaving the profile applied.
    let _ = Command::new("cmd")
        .args(["/C", "start", "", "notepad.exe"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Close every process with this image name. By name, not pid, for the
/// app-execution-alias reason above.
fn kill_by_name(exe: &str) {
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", exe])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Puts the machine back if the test unwinds with the profile still applied.
///
/// Not belt and braces: the first run of this test panicked between apply and
/// restore and left the monitor dimmed. The repair is the product's own
/// recovery path — start a core on the same data root and it restores from
/// the pending snapshot before it opens its pipe — so that is what this does,
/// rather than writing display values from the test.
struct RestoreGuard {
    root: PathBuf,
    before: LiveState,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if read_live() == self.before {
            return;
        }
        eprintln!("RestoreGuard: settings still applied, running the core's recovery path");
        let instance = format!("focusrepair-{}", Uuid::new_v4().simple());
        let mut child = Command::new(env!("CARGO_BIN_EXE_relay-core"))
            .args(["run", "--data-dir"])
            .arg(&self.root)
            .env("RELAY_INSTANCE", &instance)
            .env_remove("RELAY_RECORDING_BACKEND")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn relay-core for repair");
        // Recovery runs before the pipe opens, so a short wait is enough.
        std::thread::sleep(Duration::from_secs(3));
        let _ = child.kill();
        let _ = child.wait();
        let now = read_live();
        eprintln!("RestoreGuard: {now:?} (wanted {:?})", self.before);
    }
}

#[tokio::test]
#[ignore = "changes real monitor/GPU settings briefly and opens Notepad; run by hand with the monitor awake"]
async fn profile_applies_on_focus_and_restores_when_focus_leaves() {
    let root: PathBuf = std::env::temp_dir().join(format!("relay-focus-live-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let instance = format!("focuslive-{}", Uuid::new_v4().simple());

    // Start from a known foreground: a Notepad left over from an earlier run
    // means the relaunch below produces no foreground *change*, and the test
    // times out waiting for one. Both spellings, because which one exists
    // depends on whether this Windows has the Store app.
    kill_by_name("Notepad.exe");
    kill_by_name("notepad.exe");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let before = read_live();
    println!("before:     {before:?}");
    let _guard = RestoreGuard { root: root.clone(), before: before.clone() };

    let mut core = Core::spawn(&root, &instance);
    let mut c = core.connect().await;

    // Discover what this Windows calls Notepad rather than assuming: on a
    // Store-app install the foreground exe is not necessarily "notepad.exe".
    spawn_notepad();
    let exe = wait_for_foreground(&mut c, |e| e.to_ascii_lowercase().contains("notepad"))
        .await
        .expect("Notepad never reached the foreground");
    println!("target exe: {exe}");
    kill_by_name(&exe);
    // Let the foreground settle back before the profile exists, so the apply
    // below is caused by the relaunch and nothing else. A Store app takes its
    // time going away; rushing this is what made the first version flaky.
    wait_for_foreground(&mut c, |e| !e.eq_ignore_ascii_case(&exe)).await;
    tokio::time::sleep(Duration::from_millis(750)).await;

    let mut profile = Profile::new("Focus test (live)", GameMatch::exe(&exe));
    profile.status = ProfileStatus::Ready;
    profile.display.follow_focus = true;
    profile.display.monitor.brightness = Some(before.brightness.saturating_sub(5) as u16);
    profile.display.gpu.gamma = 1.1;
    profile.display.gpu.vibrance = 60;
    assert!(matches!(
        c.call(Method::SaveProfile { profile: Box::new(profile) }).await.unwrap(),
        Reply::Ok
    ));

    // --- focus: the profile must apply on its own, with no IPC nudge -------
    spawn_notepad();
    wait_for_foreground(&mut c, |e| e.eq_ignore_ascii_case(&exe))
        .await
        .expect("Notepad never reached the foreground on relaunch");
    // The apply happens on the focus event; give the DDC/gamma/NvAPI writes a
    // moment to land before reading the hardware back.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let applied = read_live();
    println!("applied:    {applied:?}");
    assert_eq!(
        applied.brightness,
        before.brightness.saturating_sub(5),
        "focus should have applied the profile over DDC/CI"
    );
    assert_ne!(applied.ramp_mid, before.ramp_mid, "gamma ramp took effect");
    if before.dvc.is_some() {
        assert_ne!(applied.dvc, before.dvc, "NvAPI vibrance took effect");
    }

    // --- focus leaves: the machine must go back --------------------------
    kill_by_name(&exe);
    wait_for_foreground(&mut c, |e| !e.eq_ignore_ascii_case(&exe))
        .await
        .expect("foreground never moved off Notepad");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let restored = read_live();
    println!("restored:   {restored:?}");
    assert_eq!(restored, before, "monitor and GPU back to original after focus left");

    let latency = restore_latency_ms(&core.log());
    match latency {
        Some(ms) => {
            println!("restore-on-blur latency: {ms:.1} ms (from the core's own log)");
            // The M2 Definition of Done. Generous by an order of magnitude
            // against what the hardware actually costs, so this fails only if
            // something is genuinely wrong rather than merely slow.
            assert!(ms <= 200.0, "restore-on-blur took {ms:.1} ms, budget is 200 ms");
        }
        None => panic!("could not find the foreground-change / restore pair in the log"),
    }

    core.shutdown().await;
    let final_state = read_live();
    println!("after stop: {final_state:?}");
    assert_eq!(final_state, before, "nothing left changed after the core stopped");
    let _ = std::fs::remove_dir_all(&root);
}
