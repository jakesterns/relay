//! Stream resilience (S38): the intent record and the reconnect schedule.
//!
//! Two things live here, both pure so they can be tested without a process:
//!
//! - The **intent record** (`active-stream.json`): "the user started this and
//!   has not stopped it". Written when a share or receive starts by the
//!   user's hand, removed when it stops by the user's hand or when the
//!   supervisor gives up. Its presence is what turns an engine exit from
//!   "it stopped" into "it died; bring it back" — a deliberate Stop clears it
//!   *before* killing the engine, so the exit that follows finds nothing.
//!   It is also what a fresh core acts on after a reboot or a power cut.
//! - The **schedule**: how long to wait before each attempt, and when to stop
//!   trying. Deliberately not clever. A receiver that is rebooting will not
//!   answer mDNS for a while, and hammering it changes nothing.
//!
//! The record keeps the receiver's *name*, not a peer id: ids are resolved
//! through `peers::Store` at resume time, so a share that began with a code
//! resumes with none (the code pairing remembered the peer), and a peer
//! Forgotten in the meantime simply cannot be resumed. `ShareRequest::trusted`
//! is serde-skipped and is never in the file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::share::{ReceiveRequest, ShareRequest};

pub const VERSION: u32 = 1;
const FILE: &str = "active-stream.json";

/// Wait before attempt *n* (1-based). Then it repeats the last value.
const DELAYS_SECS: [u64; 4] = [1, 2, 5, 10];
/// Stop trying this long after the first failure. Three minutes covers a
/// receiver rebooting; anything longer is a share nobody is watching.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub version: u32,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive: Option<ReceiveRequest>,
    /// The receiver's name, for a send. Resolved to a remembered peer at
    /// resume time; see the module docs for why not an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    pub started_unix: u64,
    /// Reconnect attempts so far in the current episode. Zero while the
    /// stream is up.
    #[serde(default)]
    pub attempts: u32,
    /// Fields a newer Relay wrote. Kept, never dropped (the standing rule).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub fn path() -> Result<PathBuf> {
    Ok(path_in(&crate::config::Paths::default_for_user()?))
}

/// The record's place under a given data root (`--data-dir` moves it).
pub fn path_in(paths: &crate::config::Paths) -> PathBuf {
    paths.data_dir().join(FILE)
}

impl Record {
    /// A share the user just started, to `peer`.
    pub fn for_send(request: &ShareRequest, peer: &str, now_unix: u64) -> Self {
        Record {
            version: VERSION,
            kind: Kind::Send,
            share: Some(request.clone()),
            receive: None,
            peer: Some(peer.to_string()),
            started_unix: now_unix,
            attempts: 0,
            extra: Default::default(),
        }
    }

    /// A receive the user just started. `host` is dropped: the window that
    /// embedded the stream may not exist by the time this is replayed, and
    /// the shell knows how to embed a stream window it finds running.
    pub fn for_receive(request: &ReceiveRequest, now_unix: u64) -> Self {
        let mut request = request.clone();
        request.host = None;
        Record {
            version: VERSION,
            kind: Kind::Receive,
            share: None,
            receive: Some(request),
            peer: None,
            started_unix: now_unix,
            attempts: 0,
            extra: Default::default(),
        }
    }

    /// The record on disk, if any. A file that cannot be parsed is moved
    /// aside and reported as absent: a broken record must not stop the core
    /// from starting, and must not be silently overwritten either.
    pub fn load(path: &Path) -> Option<Record> {
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<Record>(&text) {
            Ok(r) => Some(r),
            Err(e) => {
                let aside = path.with_extension("json.unreadable");
                warn!(error = %e, from = %path.display(), to = %aside.display(),
                    "active-stream.json unreadable; moved aside");
                let _ = std::fs::rename(path, &aside);
                None
            }
        }
    }

    /// Atomic, like every other store: written beside and renamed over.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }

    /// Remove the record. Absent is fine — "no intent" is the normal state.
    pub fn clear(path: &Path) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                warn!(error = %e, path = %path.display(), "could not clear active-stream.json")
            }
        }
    }
}

/// How long to wait before attempt `n` (1-based). Attempt 0 is not an attempt.
pub fn delay_for(attempt: u32) -> Duration {
    let i = (attempt.max(1) as usize - 1).min(DELAYS_SECS.len() - 1);
    Duration::from_secs(DELAYS_SECS[i])
}

/// One reconnect episode: from the first failure until the stream is back
/// or we stop trying. Times are injected so the schedule is testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    /// When the stream was first seen to be gone.
    pub since: std::time::Instant,
    /// Attempts made so far in this episode.
    pub attempts: u32,
    /// When the next attempt is due.
    pub next_at: std::time::Instant,
}

impl Episode {
    /// Start an episode at `now`; the first attempt is due after
    /// `delay_for(1)`.
    pub fn begin(now: std::time::Instant) -> Self {
        Episode { since: now, attempts: 0, next_at: now + delay_for(1) }
    }

    /// Is an attempt due, and are we still within the budget? `Some(n)` is
    /// "make attempt n now"; `None` is "not yet" or "give up" — the caller
    /// tells them apart with [`Episode::gave_up`].
    pub fn due(&mut self, now: std::time::Instant) -> Option<u32> {
        if self.gave_up(now) || now < self.next_at {
            return None;
        }
        self.attempts += 1;
        self.next_at = now + delay_for(self.attempts + 1);
        Some(self.attempts)
    }

    pub fn gave_up(&self, now: std::time::Instant) -> bool {
        now.duration_since(self.since) >= GIVE_UP_AFTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relay-resilience-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    fn share() -> ShareRequest {
        serde_json::from_str(r#"{"code":"123456","peer":"studio-pc"}"#).unwrap()
    }

    #[test]
    fn the_schedule_is_one_two_five_ten_and_then_ten() {
        let s: Vec<u64> = (1..=7).map(|n| delay_for(n).as_secs()).collect();
        assert_eq!(s, [1, 2, 5, 10, 10, 10, 10]);
        // Zero is not an attempt; it reads as the first.
        assert_eq!(delay_for(0), delay_for(1));
    }

    #[test]
    fn an_episode_paces_attempts_and_gives_up_at_three_minutes() {
        let t0 = Instant::now();
        let mut ep = Episode::begin(t0);
        assert_eq!(ep.due(t0), None, "nothing is due the instant it dropped");
        assert_eq!(ep.due(t0 + Duration::from_millis(999)), None);
        assert_eq!(ep.due(t0 + Duration::from_secs(1)), Some(1));
        // The second wait starts from the first attempt, not from t0.
        assert_eq!(ep.due(t0 + Duration::from_secs(2)), None);
        assert_eq!(ep.due(t0 + Duration::from_secs(3)), Some(2));
        assert_eq!(ep.due(t0 + Duration::from_secs(8)), Some(3));
        assert_eq!(ep.due(t0 + Duration::from_secs(18)), Some(4));
        assert_eq!(ep.due(t0 + Duration::from_secs(28)), Some(5));

        let late = t0 + GIVE_UP_AFTER;
        assert!(ep.gave_up(late));
        assert_eq!(ep.due(late), None, "past the budget nothing is ever due again");
        assert_eq!(ep.attempts, 5, "giving up does not count as an attempt");
    }

    #[test]
    fn a_send_record_round_trips_and_never_carries_the_fingerprint() {
        let p = tmp("send");
        let mut req = share();
        req.trusted = Some("sha-256 aa".into());
        let r = Record::for_send(&req, "studio-pc", 1_700_000_000);
        r.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("sha-256"), "trusted is serde-skipped: {text}");
        assert!(text.contains("\"version\": 1"));

        let back = Record::load(&p).expect("present");
        assert_eq!(back.kind, Kind::Send);
        assert_eq!(back.peer.as_deref(), Some("studio-pc"));
        assert_eq!(back.share.as_ref().unwrap().code, "123456");
        assert_eq!(back.share.as_ref().unwrap().trusted, None);
        assert_eq!(back.attempts, 0);
    }

    #[test]
    fn a_receive_record_drops_the_window_it_was_embedded_in() {
        let p = tmp("recv");
        let req: ReceiveRequest =
            serde_json::from_str(r#"{"code":"418254","host":123456,"vcam":true}"#).unwrap();
        Record::for_receive(&req, 1).save(&p).unwrap();
        let back = Record::load(&p).unwrap();
        let rr = back.receive.unwrap();
        assert_eq!(rr.code.as_deref(), Some("418254"), "the same code, so a sender can return");
        assert_eq!(rr.host, None, "a stale HWND must not be replayed");
        assert!(rr.vcam, "the service-set routing is kept");
    }

    #[test]
    fn clear_is_idempotent_and_absent_is_normal() {
        let p = tmp("clear");
        assert!(Record::load(&p).is_none());
        Record::clear(&p);
        Record::for_receive(&serde_json::from_str::<ReceiveRequest>("{}").unwrap(), 1)
            .save(&p)
            .unwrap();
        assert!(Record::load(&p).is_some());
        Record::clear(&p);
        assert!(Record::load(&p).is_none());
        Record::clear(&p);
    }

    #[test]
    fn an_unreadable_record_is_moved_aside_not_replayed() {
        let p = tmp("corrupt");
        std::fs::write(&p, "{ nope").unwrap();
        assert!(Record::load(&p).is_none());
        assert!(!p.exists());
        assert!(p.with_extension("json.unreadable").exists());
    }

    #[test]
    fn fields_from_a_newer_relay_survive() {
        let p = tmp("extra");
        std::fs::write(
            &p,
            r#"{"version":1,"kind":"receive","receive":{},"started_unix":5,"future":"yes"}"#,
        )
        .unwrap();
        let r = Record::load(&p).unwrap();
        r.save(&p).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"future\": \"yes\""));
    }
}
