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
//! So: generate one certificate on first use, keep it in the data root, and
//! use that same certificate on every connection. The fingerprint then
//! identifies *this installation* and pinning it is real authentication — DTLS
//! completes only if the far end holds the matching private key.
//!
//! It has to be the *certificate*, not just the key. The first cut of S35
//! stored the keypair and rebuilt the certificate from it each run, and
//! `RTCCertificate::from_key_pair` mints a fresh self-signed certificate with
//! a random serial and subject every time — so the fingerprint, which hashes
//! the certificate, changed on every share and a remembered peer could never
//! match. `the_same_key_comes_back_across_runs` compares fingerprints, not
//! file bytes, so this cannot quietly come back.
//!
//! On Windows the key at rest is wrapped with DPAPI in user scope
//! (`identity.key`), so a copy of the file is inert on any other machine or
//! account: theft of the data folder no longer carries the credential. Other
//! platforms keep the plaintext PEM (`identity.pem`, mode 0600) until the
//! portability seam grows a keychain equivalent.
//!
//! The trust model this serves, including what an attacker who copies the file
//! can do, is `docs/dev/trusted-peers.md`. Read that before changing anything
//! here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::KeyPair;
use rtc::peer_connection::certificate::RTCCertificate;

/// The stored form: rtc's own PEM (EXPIRES + CERTIFICATE + PRIVATE KEY
/// blocks), read back with `RTCCertificate::from_pem`.
type StoredPem = String;

/// The plaintext PEM file: the only format before DPAPI wrapping landed, still
/// the format on non-Windows, and read on Windows exactly once to migrate.
/// Kept separate from `peers.json` on purpose: the peer list is the sort of
/// thing that gets attached to a support request, and the private key must
/// never travel with it.
const PEM_FILE: &str = "identity.pem";

/// The DPAPI-wrapped key (Windows only). Same PEM inside, unreadable outside
/// this user on this machine.
#[cfg(windows)]
const KEY_FILE: &str = "identity.key";

/// Mixed into the DPAPI wrap so another program running as this user cannot
/// unwrap the blob by accident with a bare `CryptUnprotectData`. Not a secret
/// — anyone with the binary has it — so it raises the bar from "any process"
/// to "a process that went looking", which is what optional entropy is for.
#[cfg(windows)]
const ENTROPY: &[u8] = b"relay-identity-v1";

/// Where this installation's identity key lives: the wrapped file on Windows,
/// the PEM elsewhere.
pub fn key_path() -> Result<PathBuf> {
    let dir = relay_core::config::Paths::default_for_user()?.data_dir();
    #[cfg(windows)]
    {
        Ok(dir.join(KEY_FILE))
    }
    #[cfg(not(windows))]
    {
        Ok(dir.join(PEM_FILE))
    }
}

/// The certificate for this installation, creating the key on first use.
pub fn certificate() -> Result<RTCCertificate> {
    certificate_at(&key_path()?)
}

/// As [`certificate`], against an explicit path, so tests need no data root.
///
/// On Windows `path` is the wrapped file; a plaintext `identity.pem` beside
/// it (from a build before wrapping) is read, wrapped and removed, so an
/// update keeps the identity — the standing rule that an update never resets
/// anything.
pub fn certificate_at(path: &Path) -> Result<RTCCertificate> {
    load_or_create(path)
}

/// Read the stored key, or generate and persist one.
///
/// A key that exists but cannot be read is a genuine dilemma: replacing it
/// silently would forget every remembered peer with no explanation, while
/// refusing to start would leave the user unable to share at all. Sharing is
/// the more important of the two, so we replace it — but loudly, and the
/// failure surfaces as "this PC is no longer recognised", which is at least a
/// symptom that matches the cause. On Windows a wrapped file copied from
/// another machine or account fails the same way, by design.
fn load_or_create(path: &Path) -> Result<RTCCertificate> {
    if let Some(pem) = read_stored(path) {
        match parse(&pem) {
            Ok(Parsed::Certificate(c)) => {
                // Evidence for "survives an update": the same identity, reused.
                tracing::info!(path = %path.display(), "loaded this PC's DTLS identity");
                return Ok(c);
            }
            Ok(Parsed::KeyOnly(key)) => {
                // A file from the first cut of S35: key only. Build the
                // certificate once and keep it, so the fingerprint is fixed
                // from here on.
                let cert = RTCCertificate::from_key_pair(key)
                    .context("build DTLS certificate from the stored key")?;
                match store(path, cert.serialize_pem().as_bytes()) {
                    Ok(()) => tracing::info!(
                        path = %path.display(),
                        "identity upgraded from key to certificate"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "could not persist the identity certificate; \
                         the fingerprint will change next run"
                    ),
                }
                return Ok(cert);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e, path = %path.display(),
                    "identity unreadable; generating a new one. \
                     Peers that remembered this PC will ask for a pairing code again."
                );
            }
        }
    }

    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("generate a DTLS identity key")?;
    let cert = RTCCertificate::from_key_pair(key).context("build DTLS identity certificate")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    store(path, cert.serialize_pem().as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    tracing::info!(path = %path.display(), "generated this PC's DTLS identity");
    Ok(cert)
}

enum Parsed {
    Certificate(RTCCertificate),
    KeyOnly(KeyPair),
}

/// rtc's certificate PEM, or the pre-certificate key-only PEM.
fn parse(pem: &StoredPem) -> Result<Parsed> {
    if let Ok(c) = RTCCertificate::from_pem(pem) {
        return Ok(Parsed::Certificate(c));
    }
    let key = KeyPair::from_pem(pem).context("neither a certificate nor a key")?;
    Ok(Parsed::KeyOnly(key))
}

/// The PEM text held at `path`, if there is one we can open.
///
/// Windows: unwrap the DPAPI blob; failing that, look for the pre-wrapping
/// plaintext file next to it and migrate it in place (wrap, write, delete the
/// plaintext). A failed unwrap is logged and treated as no key.
#[cfg(windows)]
fn read_stored(path: &Path) -> Option<String> {
    if let Ok(blob) = std::fs::read(path) {
        match dpapi::unprotect(&blob) {
            Ok(bytes) => return String::from_utf8(bytes).ok(),
            Err(e) => {
                tracing::warn!(
                    error = %e, path = %path.display(),
                    "identity key could not be unwrapped (copied from another PC or user?)"
                );
                return None;
            }
        }
    }
    let legacy = path.with_file_name(PEM_FILE);
    let pem = std::fs::read_to_string(&legacy).ok()?;
    // Only migrate a key that parses: a corrupt plaintext file gets the same
    // replace-loudly treatment as before, from the caller.
    if parse(&pem).is_err() {
        return Some(pem);
    }
    match store(path, pem.as_bytes()) {
        Ok(()) => {
            if let Err(e) = std::fs::remove_file(&legacy) {
                tracing::warn!(error = %e, "wrapped the identity key but could not remove the plaintext copy");
            } else {
                tracing::info!(path = %path.display(), "identity key wrapped with DPAPI");
            }
        }
        Err(e) => {
            // Keep working from the plaintext rather than lose the identity.
            tracing::warn!(error = %e, "could not wrap the identity key; leaving it as PEM");
        }
    }
    Some(pem)
}

#[cfg(not(windows))]
fn read_stored(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Persist the PEM: DPAPI-wrapped on Windows, mode-0600 plaintext elsewhere.
#[cfg(windows)]
fn store(path: &Path, pem: &[u8]) -> Result<()> {
    let blob = dpapi::protect(pem)?;
    std::fs::write(path, blob)?;
    Ok(())
}

#[cfg(not(windows))]
fn store(path: &Path, pem: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(pem)?;
    Ok(())
}

/// `CryptProtectData` / `CryptUnprotectData` in user scope with fixed entropy
/// and no UI. The blob is bound to this user on this machine (roaming with the
/// user's master key, which is Windows' business, not ours).
#[cfg(windows)]
mod dpapi {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr().cast_mut() }
    }

    /// Copy the output blob out and free the LocalAlloc'd buffer.
    ///
    /// SAFETY: `out` was filled by a successful DPAPI call, so `pbData` is a
    /// LocalAlloc buffer of `cbData` bytes that we own.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        }
        v
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        let input = blob(plain);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: every pointer is to a live local; the output is taken and
        // freed by `take`.
        unsafe {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .context("CryptProtectData")?;
            Ok(take(out))
        }
    }

    pub fn unprotect(wrapped: &[u8]) -> Result<Vec<u8>> {
        let input = blob(wrapped);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as `protect`.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .context("CryptUnprotectData")?;
            Ok(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const STORED: &str = KEY_FILE;
    #[cfg(not(windows))]
    const STORED: &str = PEM_FILE;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("relay-identity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p.join(STORED)
    }

    fn fingerprint(c: &RTCCertificate) -> String {
        c.get_fingerprints()[0].value.clone()
    }

    #[test]
    fn the_same_key_comes_back_across_runs() {
        let path = tmp("stable");
        assert!(!path.exists(), "fixture should start empty");

        let a = certificate_at(&path).unwrap();
        let first = std::fs::read(&path).unwrap();
        let b = certificate_at(&path).unwrap();
        let second = std::fs::read(&path).unwrap();

        // This is the whole point: a second run must not mint a new identity,
        // or every remembered peer is forgotten on every restart.
        assert_eq!(first, second);
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn a_corrupt_key_is_replaced_rather_than_fatal() {
        let path = tmp("corrupt");
        std::fs::write(&path, b"this is not a key").unwrap();
        // Sharing matters more than remembering: this must still produce a
        // usable certificate rather than refusing to start.
        certificate_at(&path).expect("should recover by generating a new key");
        let stored = std::fs::read(&path).unwrap();
        assert_ne!(stored, b"this is not a key");
    }

    #[test]
    fn the_key_is_created_with_its_directory() {
        let dir = std::env::temp_dir()
            .join(format!("relay-identity-nested-{}", std::process::id()))
            .join("data");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(STORED);
        certificate_at(&path).unwrap();
        assert!(path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn the_key_at_rest_is_not_plaintext() {
        let path = tmp("wrapped");
        certificate_at(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("-----BEGIN"),
            "the wrapped file must not carry the PEM in the clear"
        );
        // And it is not merely obfuscated: DPAPI round-trips it to a
        // certificate rtc can load.
        let pem = String::from_utf8(dpapi::unprotect(&bytes).unwrap()).unwrap();
        assert!(pem.contains("-----BEGIN"));
        RTCCertificate::from_pem(&pem).unwrap();
    }

    #[test]
    fn a_key_only_file_from_the_first_cut_is_upgraded_and_its_fingerprint_then_holds() {
        // The pre-certificate format. Its fingerprint was never stable, so
        // nothing is owed to it beyond keeping the key; what matters is that
        // the second and third runs agree.
        let path = tmp("keyonly");
        let pem = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap().serialize_pem();
        store(&path, pem.as_bytes()).unwrap();
        let a = certificate_at(&path).unwrap();
        let b = certificate_at(&path).unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let stored = read_stored(&path).unwrap();
        assert!(stored.contains("CERTIFICATE"), "upgraded to a certificate on disk");
    }

    #[cfg(windows)]
    #[test]
    fn a_plaintext_key_from_before_wrapping_is_migrated_not_replaced() {
        let path = tmp("migrate");
        let legacy = path.with_file_name(PEM_FILE);
        let cert = RTCCertificate::from_key_pair(
            KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap(),
        )
        .unwrap();
        let expected = fingerprint(&cert);
        std::fs::write(&legacy, cert.serialize_pem()).unwrap();

        // An update must not reset the identity: the wrapped file carries the
        // old key, the plaintext copy is gone, and the fingerprint is unchanged.
        let c = certificate_at(&path).unwrap();
        assert_eq!(fingerprint(&c), expected);
        assert!(path.exists(), "wrapped file written");
        assert!(!legacy.exists(), "plaintext removed after wrapping");

        let again = certificate_at(&path).unwrap();
        assert_eq!(fingerprint(&again), expected);
    }

    #[cfg(windows)]
    #[test]
    fn a_blob_wrapped_with_other_entropy_is_treated_as_no_key() {
        // Stands in for "copied from another PC": the unwrap fails, and the
        // outcome is a fresh key, not a crash.
        let path = tmp("foreign");
        let cert = RTCCertificate::from_key_pair(
            KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap(),
        )
        .unwrap();
        let mut blob = dpapi::protect(cert.serialize_pem().as_bytes()).unwrap();
        let mid = blob.len() / 2;
        blob[mid] ^= 0xFF;
        std::fs::write(&path, &blob).unwrap();

        let c = certificate_at(&path).unwrap();
        assert_ne!(fingerprint(&c), fingerprint(&cert));
        let stored = std::fs::read(&path).unwrap();
        assert_ne!(stored, blob, "replaced with a fresh wrapped key");
    }
}
