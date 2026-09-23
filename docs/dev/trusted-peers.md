# Trusted peers: the trust model

**Status: approved. Jake answered §5 on 2026-09-22 — no: both PCs must be
ready. Implemented as Option A, and the implementation is described at the
end (§8).**

Jake's requirement: after two PCs have paired once, reconnecting should be one
click, not a fresh six-digit code read aloud between two machines. It must
survive updates and crashes.

This is a convenience feature whose whole substance is a security decision.
The shape to design against is *"any machine that once paired can reconnect
silently forever"*.

## 1. What exists today

`peers.json` already exists, and is **written but never read**:

- `receiver.rs:149` and `sender.rs:358` call `remember_peer(name, fingerprint)`
  after a successful pairing.
- Nothing anywhere loads it back. No comparison, no pinning, no decision.

There is a reason it was never read. `build_pc` in `transport/mod.rs` does not
supply a certificate, so webrtc-rs generates a fresh self-signed one per
`PeerConnection`. The DTLS fingerprint therefore changes on **every run**, and
a fingerprint recorded yesterday can never match today's. As an authorisation
anchor the file is not merely unused, it is unusable.

So the first piece of work is not storage. It is giving each installation a
**stable identity**.

## 2. Identity

Each installation generates one long-lived keypair and self-signed certificate
on first use, stored in the data root, and uses it for DTLS on every
connection thereafter.

- The **public identity** of a PC is its certificate fingerprint. It is already
  published in the SDP of every connection, so this reveals nothing new.
- The **private key** never leaves the machine and is what an impersonator
  would have to steal.

Pinning a fingerprint is then real authentication rather than a label: DTLS
will only complete if the far end holds the matching private key. This is the
same property the six-digit code establishes on first contact — it is how we
carry that property forward without asking again.

## 3. What is stored, and where

`%LOCALAPPDATA%\Relay\data\peers.json`, versioned, migrated, never reset by an
update (the standing rule). Per remembered peer:

| Field | Why |
|---|---|
| `id` | Stable random id for this pairing; the key we match on internally. |
| `name` | What the user sees. Advisory only — never authorises anything. |
| `fingerprint` | The peer's identity certificate. **This is the credential.** |
| `first_paired_unix`, `last_seen_unix` | "Last connected" in the UI, and lets old entries be found and pruned. |
| `favourite` | User ordering. No security meaning. |

Deliberately **not** stored: the six-digit code (it is single-use and must not
outlive the pairing), and any shared secret. There is no symmetric secret in
this design; the credential is an asymmetric key the peer already has.

`name` is advisory on purpose. Names come from the network and are trivially
spoofed; matching on a name rather than a fingerprint would make the whole
feature theatre.

## 4. What an attacker gets

**Someone who copies `%LOCALAPPDATA%\Relay\data`:**
- They get the *peer list* — the names and fingerprints of PCs this machine
  trusts. That is not a credential; it authorises nothing.
- They do **not** get the ability to impersonate a remembered peer, because the
  peer's private key is on the peer's machine.
- They **do** get this machine's own identity key, if it sits in the same
  folder — and with it they can impersonate *this* PC to its peers.

That last point is the real exposure, and it is bounded, not eliminated:

- The data root is per-user and protected by its ACL. Anyone who can read it
  can generally already run code as that user, at which point Relay's secrets
  are not the weak link.
- The identity key is stored separately from `peers.json`, so "send me your
  peers.json" style support requests never move the key.
- **On Windows the file is DPAPI-wrapped** (`CryptProtectData`, user scope,
  fixed application entropy) as `identity.key` — done 2026-09-23. A copied
  file is inert on another machine or under another account: the unwrap
  fails, Relay logs it and mints a fresh identity, and the copier holds
  nothing. Theft of the data folder is now a machine-bound problem. Not
  portable, so other platforms keep the plaintext `identity.pem` at mode
  0600 with this risk standing.
- Revocation is per-peer and immediate (§6), so a suspected compromise has an
  answer that is not "reinstall".

**Someone on the same LAN:** gains nothing. They cannot complete DTLS without a
remembered peer's private key, and an unknown fingerprint falls back to the
six-digit code exactly as today.

**A remembered peer that has itself been compromised** can reconnect. That is
inherent in remembering it, and is why revocation and visibility matter.

## 5. The decision — answered

**May a remembered sender connect while the receiving PC is _not_ in "Start
receiving"?**

**Jake, 2026-09-22: no.** "I would say no to the pairing without both devices
ready for streaming." Option A is what shipped. Option B is kept below as the
record of what was considered and why it was not the default.

*Option A — no (recommended; implemented).*
The receiver must be receiving; remembering removes the **code**, not the
consent. One click on the receiving PC, then the sender connects with no code.

- Nothing can put a picture on someone's screen while they are doing something
  else.
- The failure mode of a stolen identity key is bounded: an impersonator still
  cannot connect to a PC that is not listening.
- Costs one click on the receiver — which is the click Jake already makes.

*Option B — yes, auto-accept.* A remembered sender can connect any time Relay
is running.

- Genuinely more convenient: the receiving PC needs no interaction at all.
- But it is exactly the shape we set out to design against. Anyone holding a
  remembered peer's key, or that peer's machine, can start a stream to this PC
  unprompted.
- If Jake wants this, it should be **per peer**, off by default, obvious in the
  UI, and paired with an on-screen indication that a share has begun.

I recommend A, and if B is wanted later it should be an explicit per-peer
setting rather than the default behaviour.

## 6. Forgetting

**Forget** removes the entry and takes effect immediately, including mid-share.
It is not a hidden row: a forgotten peer needs a fresh six-digit code, exactly
like a stranger.

Both ends should be forgettable independently; there is no protocol message to
tell the other side it has been forgotten, and pretending otherwise would be a
lie. The next connection attempt simply falls back to the code.

## 7. What this does not change

- The six-digit code stays for first contact and anything not remembered.
- No zero-configuration promise is broken: no ports, no accounts, no relays.
- LAN only, as today.
- Nothing is written to the registry; the data root only.

## 8. How it is built

- **Identity**: `crates/capture/src/transport/identity.rs`. One ECDSA P-256
  self-signed **certificate** (rtc's PEM: key + certificate), DPAPI-wrapped
  as `identity.key` on Windows and plaintext `identity.pem` elsewhere, used
  for every `PeerConnection` via `RTCConfigurationBuilder::with_certificates`.
  It must be the certificate, not the key: the fingerprint hashes the
  certificate, and `RTCCertificate::from_key_pair` mints a new one (random
  serial and subject) every call. The first cut of S35 stored only the key
  and so had a different fingerprint every share — found 2026-09-23 by a
  test that compares fingerprints across two loads rather than file bytes.
  A key-only file is upgraded in place on first read. The same day,
  `signal::sdp_fingerprint` was found to have never matched on the wire
  (it was given the JSON description, not raw SDP); it now takes either,
  and `scripts/trusted-check.sh` is the one-PC proof of the whole path.
- **Store**: `crates/core/src/peers.rs`, `peers.json` version 1. Migrates the
  pre-S35 file (version 0) on first load; keeps fields it does not know;
  moves an unreadable file aside rather than overwriting it. Matches on
  fingerprint only. `remember` is the code-verified path and the only one
  that creates an entry; `touch` (the trusted path) can only update one.
- **Wire**: `SigMsg::Offer` gained `trusted: bool` (serde default `false`).
  A trusted offer carries an empty MAC. A receiver that predates the field
  fails the MAC and says `Bye`, and the sender turns that into "pair with
  its code once and it will" — the right fallback, by construction.
- **Receiver** (`receiver.rs`): a trusted offer is accepted only if
  `peers::recognise(fingerprint)` hits, and **"paired" is not reported until
  DTLS has completed**, because the fingerprint in an SDP is public and
  only the key-holder can finish the handshake with it. A stranger who
  presents a remembered fingerprint is refused and named as such in the log.
  A code-verified offer is remembered, as before.
- **Sender** (`sender.rs`): given `--trusted <fingerprint>`, it sends no MAC
  and checks the receiver's *answer* carries that fingerprint — mutual
  authentication, so a LAN device calling itself by a remembered name gets
  nowhere. Both ends `touch` the store only after DTLS connects.
- **Service** (`service.rs`): the client names a peer by *id*.
  `resolve_share_peer` turns it into name + fingerprint; `ShareRequest::trusted`
  is `#[serde(skip)]`, so nothing over IPC can set it. A missing peer and a
  missing code are both refused before the engine starts, in words.
- **UI**: the Share screen lists remembered PCs (favourites, last connected)
  above scanned ones and hides the code box when one is chosen, saying the one
  thing that can still fail — the other PC must be on Start receiving. The
  Receive screen lists remembered PCs with a real Forget, and says
  "remembered, no code" when a sender got in that way.
- **DPAPI wrapping** (§4): done 2026-09-23, Windows only. On first run of a
  build that has it, an existing plaintext `identity.pem` is wrapped into
  `identity.key` and removed; the fingerprint is unchanged by the migration.
