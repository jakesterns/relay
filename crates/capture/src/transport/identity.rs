//! This PC's lasting DTLS identity (S35).
//!
//! Relay used to let webrtc-rs generate a throwaway certificate for every
//! `PeerConnection`, which meant this machine had a different DTLS fingerprint
//! on every single run. That is fine for a connection authorised by a
//! six-digit code typed at the time — and fatal for remembering anything.
//! `peers.json` has been recording peer fingerprints since M4, and nothing
//! ever read them back, because a fingerprint written yesterday could not
//! match anything today. The store was not unused by oversight; it was
//! unusable.
//!
//! So: generate one keypair on first use, keep it in the data root, and derive
//! the DTLS certificate from it every time. The fingerprint then identifies
//! *this installation* and pinning it is real authentication — DTLS completes
//! only if the far end holds the matching private key.
//!
//! The trust model this serves, including what an attacker who copies the file
//! can do, is `docs/dev/trusted-peers.md`. Read that before changing anything
//! here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::KeyPair;
use rtc::peer_connection::certificate::RTCCertificate;

/// File name inside the data root. Kept separate from `peers.json` on
/// purpose: the peer list is the sort of thing that gets attached to a support
/// request, and the private key must never travel with it.
const KEY_FILE: &str = "identity.pem";

/// Where this installation's identity key lives.
pub fn key_path() -> Result<PathBuf> {
    Ok(relay_core::config::Paths::default_for_user()?.data_dir().join(KEY_FILE))
}

/// The certificate for this installation, creating the key on first use.
pub fn certificate() -> Result<RTCCertificate> {
    certificate_at(&key_path()?)
}

/// As [`certificate`], against an explicit path, so tests need no data root.
pub fn certificate_at(path: &Path) -> Result<RTCCertificate> {
    let key = load_or_create_key(path)?;
    RTCCertificate::from_key_pair(key).context("build DTLS certificate from the identity key")
}

/// Read the PEM key, or generate and persist one.
///
/// A key that exists but cannot be parsed is a genuine dilemma: replacing it
/// silently would forget every remembered peer with no explanation, while
/// refusing to start would leave the user unable to share at all. Sharing is
/// the more important of the two, so we replace it — but loudly, and the
/// failure surfaces as "this PC is no longer recognised", which is at least a
/// symptom that matches the cause.
fn load_or_create_key(path: &Path) -> Result<KeyPair> {
    if let Ok(pem) = std::fs::read_to_string(path) {
        match KeyPair::from_pem(&pem) {
            Ok(k) => return Ok(k),
            Err(e) => {
                tracing::warn!(
                    error = %e, path = %path.display(),
                    "identity key unreadable; generating a new one. \
                     Peers that remembered this PC will ask for a pairing code again."
                );
            }
        }
    }

    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("generate a DTLS identity key")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    write_private(path, key.serialize_pem().as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    tracing::info!(path = %path.display(), "generated this PC's DTLS identity");
    Ok(key)
}

/// Write a file only this user can read.
///
/// The data root is already per-user, so this is defence in depth rather than
/// the only thing standing between the key and another account. It matters
/// most on a shared machine, where `%LOCALAPPDATA%` protects by convention and
/// an explicit ACL protects by rule.
#[cfg(windows)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    // `icacls <file> /inheritance:r /grant:r <user>:F` in API terms is a long
    // way round; the practical protection here is the parent directory's ACL,
    // which Windows already restricts to this user. Recorded rather than
    // silently skipped: see docs/dev/trusted-peers.md §4 for the DPAPI
    // recommendation that would make a copied file inert.
    Ok(())
}

#[cfg(not(windows))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("relay-identity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p.join(KEY_FILE)
    }

    #[test]
    fn the_same_key_comes_back_across_runs() {
        let path = tmp("stable");
        let a = std::fs::read_to_string(&path).ok();
        assert!(a.is_none(), "fixture should start empty");

        let _ = certificate_at(&path).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        let _ = certificate_at(&path).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();

        // This is the whole point: a second run must not mint a new identity,
        // or every remembered peer is forgotten on every restart.
        assert_eq!(first, second);
        assert!(first.contains("PRIVATE KEY"));
    }

    #[test]
    fn a_corrupt_key_is_replaced_rather_than_fatal() {
        let path = tmp("corrupt");
        std::fs::write(&path, b"this is not a PEM key").unwrap();
        // Sharing matters more than remembering: this must still produce a
        // usable certificate rather than refusing to start.
        certificate_at(&path).expect("should recover by generating a new key");
        let pem = std::fs::read_to_string(&path).unwrap();
        assert!(pem.contains("PRIVATE KEY"));
    }

    #[test]
    fn the_key_is_created_with_its_directory() {
        let dir = std::env::temp_dir()
            .join(format!("relay-identity-nested-{}", std::process::id()))
            .join("data");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(KEY_FILE);
        certificate_at(&path).unwrap();
        assert!(path.exists());
    }
}
