//! Remembered peers (S35): the PCs this installation has paired with.
//!
//! One list, one file — `%LOCALAPPDATA%\Relay\data\peers.json` — used by both
//! ends of a share. The share engine reads and writes it (this crate is the
//! seam, because the core does not link the engine); the service reads it for
//! the UI and resolves a `peer_id` into what the engine needs.
//!
//! What a record *is*: the peer's DTLS certificate fingerprint. That is the
//! credential, and it only means something because `transport::identity`
//! gives each installation a lasting certificate — DTLS completes only if the
//! far end holds the matching private key. The name is display only. Names
//! come off the network and are trivially spoofed, so nothing here matches on
//! one. `docs/dev/trusted-peers.md` is the trust model; read it before
//! changing what gets stored or how it is matched.
//!
//! Two rules from the standing "an update never resets anything" rule:
//! the file is versioned and migrated (the pre-S35 shape is version 0), and
//! fields this build does not understand survive a load/save round trip.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

pub const STORE_VERSION: u32 = 1;
const FILE: &str = "peers.json";

/// Which way the last share with this peer ran, from this PC's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// We sent a share to them: they were the receiver.
    SentTo,
    /// They sent a share to us: they were the sender.
    ReceivedFrom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    /// Stable, random. What the UI and IPC refer to a peer by, so a rename on
    /// the far end does not orphan a favourite.
    pub id: String,
    /// Display only. Never used to authorise anything.
    pub name: String,
    /// The credential: the peer's DTLS certificate fingerprint as it appears
    /// in SDP (`sha-256 AA:BB:…`), normalised by [`norm`].
    pub fingerprint: String,
    pub first_paired_unix: u64,
    pub last_seen_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_direction: Option<Direction>,
    #[serde(default)]
    pub favourite: bool,
    /// Fields a newer Relay wrote that this one does not know. Kept, not
    /// dropped: an older build must not strip what a newer one recorded.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The pre-S35 shape: no version field, one row per name, and — the part that
/// made it unusable — a fingerprint that changed every run. Migrated for the
/// names and dates; the fingerprints it holds were already stale when written
/// and are replaced by the next code-verified pairing.
#[derive(Debug, Deserialize)]
struct PeerV0 {
    name: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    paired_unix: u64,
}
#[derive(Debug, Default, Deserialize)]
struct StoreV0 {
    #[serde(default)]
    peers: Vec<PeerV0>,
}

pub fn path() -> Result<PathBuf> {
    Ok(crate::config::Paths::default_for_user()?.data_dir().join(FILE))
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fingerprints compared as the same thing whatever the case or spacing.
/// SDP emits `sha-256 AA:BB`; nothing should depend on that exact form.
pub fn norm(fingerprint: &str) -> String {
    fingerprint.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl Store {
    /// Load the store. Never fails: a missing file is an empty store, and a
    /// file that cannot be parsed is moved aside rather than overwritten, so a
    /// bug here costs the user a re-pair, not their history.
    pub fn load(path: &Path) -> Store {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Store { version: STORE_VERSION, ..Default::default() },
        };
        match Self::parse(&text) {
            Ok(s) => s,
            Err(e) => {
                let aside = path.with_extension("json.unreadable");
                warn!(error = %e, from = %path.display(), to = %aside.display(),
                    "peers.json unreadable; moved aside and starting empty");
                let _ = std::fs::rename(path, &aside);
                Store { version: STORE_VERSION, ..Default::default() }
            }
        }
    }

    fn parse(text: &str) -> Result<Store> {
        let v: serde_json::Value = serde_json::from_str(text)?;
        let versioned = v.get("version").and_then(|x| x.as_u64()).is_some();
        if !versioned {
            let v0: StoreV0 = serde_json::from_value(v)?;
            let migrated = Store {
                version: STORE_VERSION,
                peers: v0
                    .peers
                    .into_iter()
                    .map(|p| Peer {
                        id: new_id(),
                        name: p.name,
                        fingerprint: norm(&p.fingerprint),
                        first_paired_unix: p.paired_unix,
                        last_seen_unix: p.paired_unix,
                        last_direction: None,
                        favourite: false,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            };
            info!(peers = migrated.peers.len(), "migrated peers.json from version 0");
            return Ok(migrated);
        }
        let mut s: Store = serde_json::from_value(v)?;
        if s.version > STORE_VERSION {
            // Written by a newer Relay. We read what we understand and keep
            // the rest in `extra`; saving stamps *our* version, which is the
            // honest statement of what this build guarantees about the file.
            warn!(found = s.version, ours = STORE_VERSION, "peers.json is from a newer Relay");
        }
        for p in &mut s.peers {
            p.fingerprint = norm(&p.fingerprint);
        }
        s.version = STORE_VERSION;
        Ok(s)
    }

    /// Atomic: written beside the file and renamed over it, so a crash
    /// mid-write leaves the old list rather than half of the new one.
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

    pub fn get(&self, id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// The peer with this fingerprint, if we remember it. This is the only
    /// question the trusted-connect path asks.
    pub fn recognise(&self, fingerprint: &str) -> Option<&Peer> {
        let fp = norm(fingerprint);
        self.peers.iter().find(|p| p.fingerprint == fp)
    }

    /// Record a pairing that a six-digit code just verified.
    ///
    /// Matches on fingerprint first. Failing that, a peer we already know by
    /// *name* with a different fingerprint is the same PC with a new identity
    /// (reinstalled, or its key file lost); the code was verified, so the user
    /// has consented to this fingerprint under this name, and the entry — id,
    /// favourite, first-paired date — carries over rather than duplicating.
    /// That name-match is only safe here, behind the code; `touch` never does
    /// it.
    pub fn remember(
        &mut self,
        name: &str,
        fingerprint: &str,
        direction: Direction,
        now: u64,
    ) -> &Peer {
        let fp = norm(fingerprint);
        let idx = self
            .peers
            .iter()
            .position(|p| p.fingerprint == fp)
            .or_else(|| self.peers.iter().position(|p| p.name.eq_ignore_ascii_case(name)));
        match idx {
            Some(i) => {
                let p = &mut self.peers[i];
                if p.fingerprint != fp {
                    info!(%name, "known PC has a new identity; replacing its fingerprint");
                    p.fingerprint = fp;
                }
                p.name = name.to_string();
                p.last_seen_unix = now;
                p.last_direction = Some(direction);
                &self.peers[i]
            }
            None => {
                self.peers.push(Peer {
                    id: new_id(),
                    name: name.to_string(),
                    fingerprint: fp,
                    first_paired_unix: now,
                    last_seen_unix: now,
                    last_direction: Some(direction),
                    favourite: false,
                    extra: Default::default(),
                });
                self.peers.last().expect("just pushed")
            }
        }
    }

    /// A trusted connection succeeded: note when. Creates nothing — a PC that
    /// is not remembered cannot become remembered without a code.
    pub fn touch(&mut self, fingerprint: &str, direction: Direction, now: u64) -> bool {
        let fp = norm(fingerprint);
        match self.peers.iter_mut().find(|p| p.fingerprint == fp) {
            Some(p) => {
                p.last_seen_unix = now;
                p.last_direction = Some(direction);
                true
            }
            None => false,
        }
    }

    /// Remove a peer. It needs a code again, like a stranger.
    pub fn forget(&mut self, id: &str) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| p.id != id);
        self.peers.len() != before
    }

    pub fn set_favourite(&mut self, id: &str, favourite: bool) -> bool {
        match self.peers.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.favourite = favourite;
                true
            }
            None => false,
        }
    }

    /// For display: favourites first, then most recently connected.
    pub fn list(&self) -> Vec<Peer> {
        let mut v = self.peers.clone();
        v.sort_by(|a, b| {
            b.favourite
                .cmp(&a.favourite)
                .then(b.last_seen_unix.cmp(&a.last_seen_unix))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        v
    }
}

// ---- convenience over the default path, for the engine ------------------

/// Record a code-verified pairing in the user's store.
pub fn remember(name: &str, fingerprint: &str, direction: Direction) -> Result<Peer> {
    let path = path()?;
    let mut s = Store::load(&path);
    let p = s.remember(name, fingerprint, direction, now_unix()).clone();
    s.save(&path)?;
    Ok(p)
}

/// Is this fingerprint one we remember?
pub fn recognise(fingerprint: &str) -> Result<Option<Peer>> {
    Ok(Store::load(&path()?).recognise(fingerprint).cloned())
}

/// Note a successful trusted connection.
pub fn touch(fingerprint: &str, direction: Direction) -> Result<bool> {
    let path = path()?;
    let mut s = Store::load(&path);
    let hit = s.touch(fingerprint, direction, now_unix());
    if hit {
        s.save(&path)?;
    }
    Ok(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("relay-peers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    const FP_A: &str = "sha-256 AA:BB:CC";
    const FP_B: &str = "sha-256 DD:EE:FF";

    #[test]
    fn missing_file_is_an_empty_current_store() {
        let s = Store::load(&tmp("missing"));
        assert_eq!(s.version, STORE_VERSION);
        assert!(s.peers.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let p = tmp("roundtrip");
        let mut s = Store::load(&p);
        s.remember("studio-pc", FP_A, Direction::SentTo, 100);
        s.save(&p).unwrap();
        let back = Store::load(&p);
        assert_eq!(back.peers, s.peers);
        assert_eq!(back.version, STORE_VERSION);
    }

    #[test]
    fn migrates_the_pre_s35_file() {
        // Exactly what `signal::remember_peer_at` used to write.
        let p = tmp("v0");
        std::fs::write(
            &p,
            r#"{"peers":[{"name":"den-pc","fingerprint":"sha-256 11:22","paired_unix":1700000000}]}"#,
        )
        .unwrap();
        let s = Store::load(&p);
        assert_eq!(s.version, STORE_VERSION);
        assert_eq!(s.peers.len(), 1);
        let peer = &s.peers[0];
        assert_eq!(peer.name, "den-pc");
        assert_eq!(peer.first_paired_unix, 1_700_000_000);
        assert_eq!(peer.last_seen_unix, 1_700_000_000);
        assert!(!peer.id.is_empty());
        assert!(!peer.favourite);
        // And it stays migrated once saved: the next load takes the v1 path.
        s.save(&p).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"version\": 1"));
    }

    #[test]
    fn keeps_fields_it_does_not_understand() {
        // The standing rule: an older Relay must not strip what a newer one
        // wrote. A future field on a peer and on the store both survive.
        let p = tmp("extra");
        std::fs::write(
            &p,
            r#"{"version":1,"future_setting":true,"peers":[{"id":"x1","name":"a","fingerprint":"sha-256 aa",
                "first_paired_unix":1,"last_seen_unix":2,"nickname":"the den"}]}"#,
        )
        .unwrap();
        let s = Store::load(&p);
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"nickname\": \"the den\""), "{text}");
        assert!(text.contains("\"future_setting\": true"), "{text}");
    }

    #[test]
    fn a_newer_files_version_is_read_and_restamped() {
        let p = tmp("newer");
        std::fs::write(&p, r#"{"version":7,"peers":[]}"#).unwrap();
        let s = Store::load(&p);
        assert_eq!(s.version, STORE_VERSION);
    }

    #[test]
    fn unreadable_file_is_moved_aside_not_destroyed() {
        let p = tmp("corrupt");
        std::fs::write(&p, "{ this is not json").unwrap();
        let s = Store::load(&p);
        assert!(s.peers.is_empty());
        assert!(!p.exists(), "corrupt file should have been moved");
        assert!(p.with_extension("json.unreadable").exists());
    }

    #[test]
    fn recognises_by_fingerprint_whatever_the_case_or_spacing() {
        let mut s = Store::default();
        s.remember("studio-pc", "sha-256 AA:BB:CC", Direction::ReceivedFrom, 1);
        assert!(s.recognise("SHA-256   aa:bb:cc").is_some());
        assert!(s.recognise(" sha-256 AA:BB:CC ").is_some());
        assert!(s.recognise(FP_B).is_none());
    }

    #[test]
    fn a_name_never_recognises_anything() {
        // The name is display only; a PC calling itself by a remembered name
        // with an unknown fingerprint is a stranger.
        let mut s = Store::default();
        s.remember("studio-pc", FP_A, Direction::ReceivedFrom, 1);
        assert!(s.recognise("studio-pc").is_none());
    }

    #[test]
    fn re_pairing_the_same_pc_updates_rather_than_duplicates() {
        let mut s = Store::default();
        let id = s.remember("studio-pc", FP_A, Direction::SentTo, 1).id.clone();
        s.set_favourite(&id, true);
        // Same PC, renamed, seen again the other way round.
        let again = s.remember("Studio PC", FP_A, Direction::ReceivedFrom, 5);
        assert_eq!(again.id, id);
        assert_eq!(again.name, "Studio PC");
        assert_eq!(again.last_seen_unix, 5);
        assert_eq!(again.last_direction, Some(Direction::ReceivedFrom));
        assert!(again.favourite, "favourite survives a re-pair");
        assert_eq!(s.peers.len(), 1);
    }

    #[test]
    fn a_known_name_with_a_new_identity_replaces_behind_the_code() {
        // Reinstalled PC: same name, new key. The code was verified, so the
        // user consented; the entry carries over with the new credential.
        let mut s = Store::default();
        let id = s.remember("den-pc", FP_A, Direction::SentTo, 1).id.clone();
        let p = s.remember("den-pc", FP_B, Direction::SentTo, 2);
        assert_eq!(p.id, id);
        assert_eq!(p.fingerprint, norm(FP_B));
        assert_eq!(s.peers.len(), 1);
        assert!(s.recognise(FP_A).is_none(), "the old identity is gone");
    }

    #[test]
    fn touch_never_creates() {
        // The trusted path cannot mint a remembered peer; only a code can.
        let mut s = Store::default();
        assert!(!s.touch(FP_A, Direction::ReceivedFrom, 9));
        assert!(s.peers.is_empty());
        s.remember("x", FP_A, Direction::ReceivedFrom, 1);
        assert!(s.touch(FP_A, Direction::ReceivedFrom, 9));
        assert_eq!(s.peers[0].last_seen_unix, 9);
    }

    #[test]
    fn forget_is_real() {
        let mut s = Store::default();
        let id = s.remember("x", FP_A, Direction::ReceivedFrom, 1).id.clone();
        assert!(s.forget(&id));
        assert!(!s.forget(&id));
        assert!(s.recognise(FP_A).is_none());
    }

    #[test]
    fn list_puts_favourites_first_then_most_recent() {
        let mut s = Store::default();
        s.remember("old", "sha-256 01", Direction::SentTo, 10);
        s.remember("new", "sha-256 02", Direction::SentTo, 30);
        let fav = s.remember("fav", "sha-256 03", Direction::SentTo, 20).id.clone();
        s.set_favourite(&fav, true);
        let names: Vec<_> = s.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["fav", "new", "old"]);
    }
}
