//! Which process a stored PID meant (r54 privacy fix).
//!
//! A PID alone is not an identity: Windows reuses them. A receive request
//! kept in `active-stream.json` carried `return_pid` across days and
//! restarts; on PC2 the test process behind it had exited, the PID was
//! reused, and the receiver captured another app's audio and sent it to the
//! sender, unasked. So a PID Relay will capture is always stored with the
//! process's image path and creation time, and checked against the live
//! process every time before it is used: on every receive or share spawn,
//! on every resume, and again in the engine right before process loopback is
//! activated (activation itself accepts a dead or recycled PID without
//! failing, so it cannot be the check).
//!
//! The probe uses `PROCESS_QUERY_LIMITED_INFORMATION` only: a call app or a
//! game is never opened with more than that, and nothing is read from it
//! except its image name and its times.

use serde::{Deserialize, Serialize};

/// A process, pinned down well enough that a reused PID is not mistaken for
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcIdentity {
    pub pid: u32,
    /// Full Win32 image path (`QueryFullProcessImageNameW`).
    pub image: String,
    /// Creation time, FILETIME (100 ns since 1601), from `GetProcessTimes`.
    pub created: u64,
}

impl ProcIdentity {
    /// `discord.exe` for display.
    pub fn exe(&self) -> String {
        crate::winloop::exe_name(&self.image)
    }
}

/// Why a stored identity no longer names a live process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    /// No process with that PID (or it exited and only a handle keeps it).
    Gone,
    /// A process with that PID exists, but it is another program, or the
    /// same program started again: a different process either way.
    Reused,
}

/// Looks processes up. The live one is [`LiveProbe`]; tests fake it.
pub trait ProcessProbe {
    fn identity(&self, pid: u32) -> Option<ProcIdentity>;
}

/// Compare a stored identity with what the PID is now.
pub fn verify(stored: &ProcIdentity, probe: &dyn ProcessProbe) -> Result<(), Stale> {
    let Some(live) = probe.identity(stored.pid) else { return Err(Stale::Gone) };
    if live.created == stored.created && live.image.eq_ignore_ascii_case(&stored.image) {
        Ok(())
    } else {
        Err(Stale::Reused)
    }
}

/// What to do with a request's PID before anything captures it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// No PID asked for.
    None,
    /// Use it: this is the identity to store and to hand the engine.
    Keep(ProcIdentity),
    /// Drop the route; the process it named is not there any more.
    Drop(Stale),
}

/// Settle a request's `pid` and stored identity.
///
/// - A stored identity is always checked against the live process, whoever
///   asks: a hotkey replaying the last share hours later is not a fresh pick.
/// - No stored identity and `fresh` (the user picked the PID just now, a
///   Start from the UI): it is pinned to whatever process holds it now, and
///   one that is already gone is dropped.
/// - No stored identity and not fresh (a resume of a record written before
///   this check existed): it cannot be verified, so it is dropped.
pub fn settle(
    pid: Option<u32>,
    stored: Option<&ProcIdentity>,
    fresh: bool,
    probe: &dyn ProcessProbe,
) -> Settled {
    let Some(pid) = pid.filter(|p| *p != 0) else { return Settled::None };
    match stored {
        Some(id) if id.pid == pid => match verify(id, probe) {
            Ok(()) => Settled::Keep(id.clone()),
            Err(why) => Settled::Drop(why),
        },
        // An identity for another PID is a tampered or confused record.
        Some(_) => Settled::Drop(Stale::Reused),
        None if fresh => match probe.identity(pid) {
            Some(id) => Settled::Keep(id),
            None => Settled::Drop(Stale::Gone),
        },
        None => Settled::Drop(Stale::Gone),
    }
}

/// The real process table.
pub struct LiveProbe;

impl ProcessProbe for LiveProbe {
    fn identity(&self, pid: u32) -> Option<ProcIdentity> {
        live(pid)
    }
}

/// The live identity of `pid`, or `None` when no running process has it.
#[cfg(windows)]
pub fn live(pid: u32) -> Option<ProcIdentity> {
    if pid == 0 {
        return None;
    }
    // SAFETY: limited-information access only; the handle is closed on
    // every path; out buffers are ours and sized.
    unsafe { imp::identity(pid) }
}

#[cfg(not(windows))]
pub fn live(_pid: u32) -> Option<ProcIdentity> {
    None
}

/// Check `pid` against an image path and creation time handed over on a
/// command line (the engine side of the check). `Err` says why not.
pub fn check_live(pid: u32, image: &str, created: u64) -> Result<(), Stale> {
    verify(&ProcIdentity { pid, image: image.to_string(), created }, &LiveProbe)
}

/// The engine's check right before it activates process loopback on `pid`.
/// With the identity the core pinned (`image`, `created`) the live process
/// must match it exactly. Without one (a hand-typed `relay-share` line, never
/// the core) the process must at least exist. `Err` is the reason, in words
/// for the log.
pub fn target_ok(pid: u32, image: Option<&str>, created: Option<u64>) -> Result<(), &'static str> {
    let verdict = match (image, created) {
        (Some(image), Some(created)) => check_live(pid, image, created),
        _ => live(pid).map(|_| ()).ok_or(Stale::Gone),
    };
    verdict.map_err(|why| match why {
        Stale::Gone => "the app has closed",
        Stale::Reused => "the PID now belongs to another process",
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use super::ProcIdentity;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub(super) unsafe fn identity(pid: u32) -> Option<ProcIdentity> {
        // SAFETY: see `live`.
        unsafe {
            let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let out = read(h, pid);
            let _ = CloseHandle(h);
            out
        }
    }

    unsafe fn read(h: HANDLE, pid: u32) -> Option<ProcIdentity> {
        // SAFETY: valid handle with limited query rights; locals as outs.
        unsafe {
            // A process that exited but whose object another handle keeps
            // alive still opens; it is gone for our purposes.
            let mut code = 0u32;
            GetExitCodeProcess(h, &mut code).ok()?;
            if code != STILL_ACTIVE.0 as u32 {
                return None;
            }
            let (mut c, mut e, mut k, mut u) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).ok()?;
            let created = (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime);
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .ok()?;
            Some(ProcIdentity {
                pid,
                image: String::from_utf16_lossy(&buf[..len as usize]),
                created,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake(HashMap<u32, ProcIdentity>);
    impl ProcessProbe for Fake {
        fn identity(&self, pid: u32) -> Option<ProcIdentity> {
            self.0.get(&pid).cloned()
        }
    }
    fn id(pid: u32, image: &str, created: u64) -> ProcIdentity {
        ProcIdentity { pid, image: image.into(), created }
    }
    fn table(ids: &[ProcIdentity]) -> Fake {
        Fake(ids.iter().map(|i| (i.pid, i.clone())).collect())
    }

    const DISCORD: &str = r"C:\Users\u\AppData\Local\Discord\app-1.0\Discord.exe";

    #[test]
    fn a_valid_pid_is_kept() {
        let stored = id(2772, DISCORD, 1_000);
        let probe = table(std::slice::from_ref(&stored));
        assert_eq!(verify(&stored, &probe), Ok(()));
        assert_eq!(settle(Some(2772), Some(&stored), false, &probe), Settled::Keep(stored));
    }

    #[test]
    fn image_comparison_ignores_case() {
        let stored = id(10, DISCORD, 5);
        let probe = table(&[id(10, &DISCORD.to_lowercase(), 5)]);
        assert_eq!(verify(&stored, &probe), Ok(()));
    }

    #[test]
    fn a_reused_pid_is_refused() {
        // PC2, r54: the test process behind 2772 exited and another program
        // got the PID.
        let stored = id(2772, r"C:\Tools\tone-test.exe", 1_000);
        let probe = table(&[id(2772, DISCORD, 9_000)]);
        assert_eq!(verify(&stored, &probe), Err(Stale::Reused));
        assert_eq!(settle(Some(2772), Some(&stored), false, &probe), Settled::Drop(Stale::Reused));
    }

    #[test]
    fn the_same_exe_started_again_is_refused() {
        let stored = id(2772, DISCORD, 1_000);
        let probe = table(&[id(2772, DISCORD, 1_001)]);
        assert_eq!(verify(&stored, &probe), Err(Stale::Reused));
    }

    #[test]
    fn a_gone_pid_is_refused() {
        let stored = id(2772, DISCORD, 1_000);
        let probe = table(&[]);
        assert_eq!(verify(&stored, &probe), Err(Stale::Gone));
        assert_eq!(settle(Some(2772), Some(&stored), false, &probe), Settled::Drop(Stale::Gone));
    }

    #[test]
    fn a_resumed_pid_without_identity_is_dropped() {
        // The pre-fix record: a bare PID cannot be verified, even when some
        // process holds it now.
        let probe = table(&[id(2772, DISCORD, 9_000)]);
        assert_eq!(settle(Some(2772), None, false, &probe), Settled::Drop(Stale::Gone));
    }

    #[test]
    fn an_identity_for_another_pid_is_refused() {
        let probe = table(&[id(2772, DISCORD, 1), id(3000, DISCORD, 1)]);
        assert_eq!(
            settle(Some(3000), Some(&id(2772, DISCORD, 1)), false, &probe),
            Settled::Drop(Stale::Reused)
        );
    }

    #[test]
    fn a_fresh_pick_is_pinned_to_the_live_process() {
        let live = id(4242, DISCORD, 77);
        let probe = table(std::slice::from_ref(&live));
        assert_eq!(settle(Some(4242), None, true, &probe), Settled::Keep(live));
        assert_eq!(settle(Some(4243), None, true, &probe), Settled::Drop(Stale::Gone));
        // A stored identity is checked even on a fresh start (the hotkey
        // replaying the last share): a mismatch is never re-pinned.
        let other = id(4242, r"C:\other.exe", 1);
        assert_eq!(settle(Some(4242), Some(&other), true, &probe), Settled::Drop(Stale::Reused));
    }

    #[test]
    fn no_pid_is_nothing_to_settle() {
        let probe = table(&[]);
        assert_eq!(settle(None, None, true, &probe), Settled::None);
        assert_eq!(settle(Some(0), None, false, &probe), Settled::None);
    }

    #[cfg(windows)]
    #[test]
    fn this_process_has_a_live_identity_and_a_bogus_creation_time_fails() {
        let me = live(std::process::id()).expect("own identity");
        assert!(me.created > 0 && me.image.to_ascii_lowercase().ends_with(".exe"));
        assert_eq!(check_live(me.pid, &me.image, me.created), Ok(()));
        assert_eq!(check_live(me.pid, &me.image, me.created + 1), Err(Stale::Reused));
        assert_eq!(live(0), None);
        // The engine-side gate.
        assert_eq!(target_ok(me.pid, Some(&me.image), Some(me.created)), Ok(()));
        assert!(target_ok(me.pid, Some(&me.image), Some(me.created + 1)).is_err());
        assert!(target_ok(me.pid, Some(r"C:\Windows\notepad.exe"), Some(me.created)).is_err());
        assert_eq!(target_ok(me.pid, None, None), Ok(()), "no identity: must exist");
    }
}
