//! Update check and user-approved install (S45).
//!
//! The owner's rule (2026-10-01): Relay *checks* for updates on its own, but
//! *installs* one only when the user says so (or has turned on "Install
//! updates automatically", which is off by default).
//!
//! - **Check.** One HTTPS GET to the GitHub Releases API for this repository,
//!   through the same WinHTTP path the AutoEQ fetch uses (no new HTTP or TLS
//!   crate in the always-on core). First check ~60 s after the core starts,
//!   then at most once a day, never while a share/receive is up or a game
//!   profile is applied. The result is cached in `data\update.json`.
//! - **Install.** Download the installer and `SHA256SUMS.txt` from the same
//!   release (the names `release.yml` publishes) into `data\updates`, verify
//!   the SHA-256, and when the installer is Authenticode-signed require the
//!   signature to be valid and its subject to be [`EXPECTED_PUBLISHER`]. Then
//!   wait until no share and no profile is active and run the NSIS installer
//!   silently over the top (`/S /RELAUNCH`); its own hook stops this core,
//!   and its post-install hook starts the new one and reopens the window.
//!   The outcome is recorded and shown on the next start.
//!
//! Everything that decides something is a pure function here and is tested
//! with fixtures; only [`fetch_releases`], [`download`], [`signature`] and
//! [`launch_installer`] touch the network, disk or OS.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profiles::write_atomic;

/// The one host and repository the check talks to.
pub const RELEASES_URL: &str = "https://api.github.com/repos/jakesterns/relay/releases?per_page=10";
/// Downloads must come from this repository's release assets.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/jakesterns/relay/releases/download/";
/// The sums file `release.yml` publishes beside the installer
/// (`sha256sum` format: `<hex>  <name>`).
pub const SUMS_ASSET: &str = "SHA256SUMS.txt";
/// The installer name `release.yml` publishes: `Relay_<version>_x64-setup.exe`.
pub const INSTALLER_SUFFIX: &str = "_x64-setup.exe";
pub const INSTALLER_PREFIX: &str = "Relay_";
/// The subject (CN) a signed release must carry: SignPath Foundation signs
/// Relay's releases (docs/CODE_SIGNING_POLICY.md).
pub const EXPECTED_PUBLISHER: &str = "SignPath Foundation";
/// Wait this long after the core starts before the first check.
pub const START_DELAY: Duration = Duration::from_secs(60);
/// At most one automatic check per this interval.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Caps: the API listing and the sums file are small; the installer is tens
/// of megabytes.
pub const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SUMS_BYTES: usize = 64 * 1024;
pub const MAX_INSTALLER_BYTES: usize = 512 * 1024 * 1024;
/// Release notes shown in the UI are cut at this many characters.
const MAX_NOTES: usize = 4000;

/// The version this core was built as.
pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------- semver

/// A semantic version: `MAJOR.MINOR.PATCH[-PRE][+BUILD]`, with an optional
/// leading `v` as tags carry. Build metadata is ignored, as semver says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, p.split('.').map(str::to_string).collect()),
            Some(_) => return None,
            None => (s, Vec::new()),
        };
        let mut it = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            let p = p?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v =
            Version { major: num(it.next())?, minor: num(it.next())?, patch: num(it.next())?, pre };
        if it.next().is_some() || v.pre.iter().any(|p| p.is_empty()) {
            return None;
        }
        Some(v)
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release outranks any of its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------- release JSON

/// The fields of a GitHub release this module reads.
#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// A newer release this PC can install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Available {
    pub version: String,
    /// Release notes as plain text.
    pub notes: String,
    /// The release page, for "what's new" in full.
    pub url: String,
    pub prerelease: bool,
    pub installer_name: String,
    pub installer_url: String,
    pub installer_size: u64,
    pub sums_url: String,
}

pub fn parse_releases(json: &str) -> Result<Vec<GhRelease>> {
    serde_json::from_str(json).context("the GitHub release listing was not the expected JSON")
}

/// An asset name is a plain file name: nothing that could climb out of the
/// download folder.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn release_asset(a: &GhAsset) -> bool {
    safe_name(&a.name) && a.browser_download_url.starts_with(DOWNLOAD_PREFIX)
}

/// The newest installable release above `current`: not a draft, not a
/// pre-release unless asked for, with an installer and a sums file from
/// this repository's downloads.
pub fn pick(releases: &[GhRelease], current: &Version, include_pre: bool) -> Option<Available> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Some((Version::parse(&r.tag_name)?, r)))
        .filter(|(v, r)| include_pre || (!r.prerelease && !v.is_prerelease()))
        .filter(|(v, _)| v > current)
        .filter_map(|(v, r)| {
            let installer = r.assets.iter().find(|a| {
                release_asset(a)
                    && a.name.starts_with(INSTALLER_PREFIX)
                    && a.name.ends_with(INSTALLER_SUFFIX)
            })?;
            let sums = r.assets.iter().find(|a| release_asset(a) && a.name == SUMS_ASSET)?;
            Some((
                v.clone(),
                Available {
                    version: v.to_string(),
                    notes: plain_notes(r.body.as_deref().unwrap_or("")),
                    url: r.html_url.clone(),
                    prerelease: r.prerelease || v.is_prerelease(),
                    installer_name: installer.name.clone(),
                    installer_url: installer.browser_download_url.clone(),
                    installer_size: installer.size,
                    sums_url: sums.browser_download_url.clone(),
                },
            ))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, a)| a)
}

/// Release-note Markdown to plain text: headings and emphasis markers off,
/// links reduced to their text, bullets kept as "- ", length capped.
pub fn plain_notes(md: &str) -> String {
    let mut out = String::new();
    for line in md.replace("\r\n", "\n").lines() {
        let t = line.trim_end();
        let t = t.trim_start_matches('#').trim_start();
        let t =
            if let Some(rest) = t.strip_prefix("* ") { format!("- {rest}") } else { t.to_string() };
        let mut s = String::with_capacity(t.len());
        let mut chars = t.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' | '`' => {}
                '_' if chars.peek() == Some(&'_') => {
                    chars.next();
                }
                '[' => {
                    let mut text = String::new();
                    let mut closed = false;
                    for d in chars.by_ref() {
                        if d == ']' {
                            closed = true;
                            break;
                        }
                        text.push(d);
                    }
                    s.push_str(&text);
                    if closed && chars.peek() == Some(&'(') {
                        for d in chars.by_ref() {
                            if d == ')' {
                                break;
                            }
                        }
                    }
                }
                '<' => {
                    // Drop inline HTML tags (<!-- -->, <br>); keep the text.
                    for d in chars.by_ref() {
                        if d == '>' {
                            break;
                        }
                    }
                }
                _ => s.push(c),
            }
        }
        out.push_str(s.trim_end());
        out.push('\n');
    }
    let mut out = out.trim().to_string();
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    if out.chars().count() > MAX_NOTES {
        out = out.chars().take(MAX_NOTES).collect::<String>() + "…";
    }
    out
}

// ---------------------------------------------------------------- gates

/// Why an update must wait, or `None` when nothing is in the way. A check
/// and an install both wait for this; an install never interrupts a share
/// or a game.
pub fn busy_reason(sharing: bool, receiving: bool, profile_active: bool) -> Option<&'static str> {
    if sharing {
        Some("a share is running")
    } else if receiving {
        Some("Relay is receiving a share")
    } else if profile_active {
        Some("a game profile is applied")
    } else {
        None
    }
}

/// Whether the automatic check should run now. `now` and `last_check` are
/// Unix seconds; a `last_check` in the future (the clock moved back) counts
/// as due rather than silencing the check for however long the jump was.
pub fn check_due(auto: bool, uptime: Duration, now: u64, last_check: Option<u64>) -> bool {
    if !auto || uptime < START_DELAY {
        return false;
    }
    match last_check {
        None => true,
        Some(t) if t > now => true,
        Some(t) => now - t >= CHECK_INTERVAL_SECS,
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------ checksums

/// The SHA-256 for `name` in a `sha256sum`-format file (`<hex>  <name>`, or
/// `<hex> *<name>` for binary mode).
pub fn parse_sums(text: &str, name: &str) -> Option<[u8; 32]> {
    for line in text.lines() {
        let line = line.trim();
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else { continue };
        let file = rest.trim_start().trim_start_matches('*');
        if file == name {
            return decode_hex32(hex);
        }
    }
    None
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// Refuse unless the file's SHA-256 is the published one.
pub fn verify_sum(path: &Path, expected: &[u8; 32]) -> Result<(), String> {
    match sha256_file(path) {
        Ok(actual) if &actual == expected => Ok(()),
        Ok(_) => Err("The download does not match the checksum published with the release, so it \
                      was not installed. Nothing on your PC was changed."
            .into()),
        Err(e) => Err(format!("Could not read the download to check it: {e:#}")),
    }
}

// ------------------------------------------------------------ signature

/// What Windows says about an installer's Authenticode signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// No signature at all (releases before SignPath signing is live).
    Unsigned,
    /// A valid signature, with the signing certificate's subject name.
    Valid { subject: String },
    /// Signed, but the signature does not verify (tampered, revoked, expired
    /// without a timestamp, untrusted root).
    Invalid { reason: String },
}

/// Whether an installer may run, given its signature. An unsigned installer
/// is allowed (its SHA-256 has already been checked against the release);
/// a signed one must verify and name the expected publisher.
pub fn signature_decision(sig: &Signature, expected: &str) -> Result<(), String> {
    match sig {
        Signature::Unsigned => Ok(()),
        Signature::Valid { subject } if subject.trim().eq_ignore_ascii_case(expected) => Ok(()),
        Signature::Valid { subject } => Err(format!(
            "The installer is signed by \"{subject}\", not by {expected}, so it was not installed. \
             Nothing on your PC was changed."
        )),
        Signature::Invalid { reason } => Err(format!(
            "The installer's signature is not valid ({reason}), so it was not installed. Nothing on \
             your PC was changed."
        )),
    }
}

// ---------------------------------------------------------------- state

/// How the last install ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub version: String,
    pub ok: bool,
    pub message: String,
    pub at: u64,
}

/// `data\update.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCache {
    #[serde(default)]
    pub last_check: Option<u64>,
    #[serde(default)]
    pub available: Option<Available>,
    /// "Skip this version".
    #[serde(default)]
    pub skipped: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Set just before the installer is launched; read by the next core.
    #[serde(default)]
    pub pending_install: Option<String>,
    #[serde(default)]
    pub last_result: Option<InstallOutcome>,
    /// The version the tray was last told about, so it says so once.
    #[serde(default)]
    pub notified: Option<String>,
}

impl UpdateCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &json).with_context(|| format!("writing {}", path.display()))
    }

    /// On start: turn a pending install into a result, and forget an
    /// "available" release that is no longer newer than what is running.
    pub fn reconcile(&mut self, running: &Version, now: u64) {
        if let Some(v) = self.pending_install.take() {
            let ok = Version::parse(&v).is_some_and(|want| *running >= want);
            self.last_result = Some(InstallOutcome {
                version: v.clone(),
                ok,
                message: if ok {
                    format!("Relay {v} was installed. Your settings and profiles were kept.")
                } else {
                    format!("The update to Relay {v} did not finish. Relay {running} is still installed and nothing else was changed.")
                },
                at: now,
            });
        }
        if let Some(a) = &self.available {
            if Version::parse(&a.version).is_none_or(|v| v <= *running) {
                self.available = None;
            }
        }
    }

    /// Record a finished check.
    pub fn record_check(&mut self, now: u64, found: Result<Option<Available>, String>) {
        self.last_check = Some(now);
        match found {
            Ok(a) => {
                self.available = a;
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    /// The release to offer: available and not skipped.
    pub fn offer(&self) -> Option<&Available> {
        self.available.as_ref().filter(|a| self.skipped.as_deref() != Some(a.version.as_str()))
    }
}

/// What the updater is doing right now (in memory only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Downloading,
    /// Downloaded and verified; waiting for the share/profile to end.
    Waiting,
    Installing,
}

/// Everything the Settings card shows. Mirrored in `ui/src/lib/ipc.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current: String,
    pub phase: Phase,
    /// The release on offer: newer, not skipped, not put off with "Later".
    pub available: Option<Available>,
    pub last_check: Option<u64>,
    pub last_error: Option<String>,
    pub last_result: Option<InstallOutcome>,
    /// Why an install is waiting, when it is.
    pub waiting_for: Option<String>,
}

/// The in-memory updater the service holds.
#[derive(Debug)]
pub struct Updater {
    pub cache: UpdateCache,
    pub file: PathBuf,
    pub dir: PathBuf,
    pub phase: Phase,
    /// "Later": hide this version until the next check finds it again.
    pub later: Option<String>,
    /// A verified installer and its version, waiting to run.
    pub ready: Option<(PathBuf, String)>,
    pub waiting_for: Option<String>,
    /// When this core started; the first check waits [`START_DELAY`].
    pub started: std::time::Instant,
}

impl Updater {
    pub fn load(paths: &crate::config::Paths) -> Self {
        let file = paths.update_file();
        let mut cache = UpdateCache::load(&file);
        if let Some(running) = Version::parse(running_version()) {
            let before = cache.clone();
            cache.reconcile(&running, now_unix());
            if cache != before {
                let _ = cache.save(&file);
            }
        }
        Updater {
            cache,
            file,
            dir: paths.updates_dir(),
            phase: Phase::Idle,
            later: None,
            ready: None,
            waiting_for: None,
            started: std::time::Instant::now(),
        }
    }

    pub fn save(&self) {
        if let Err(e) = self.cache.save(&self.file) {
            tracing::warn!(error = %e, "could not save the update cache");
        }
    }

    pub fn status(&self) -> UpdateStatus {
        UpdateStatus {
            current: running_version().to_string(),
            phase: self.phase,
            available: self
                .cache
                .offer()
                .filter(|a| self.later.as_deref() != Some(a.version.as_str()))
                .cloned(),
            last_check: self.cache.last_check,
            last_error: self.cache.last_error.clone(),
            last_result: self.cache.last_result.clone(),
            waiting_for: self.waiting_for.clone(),
        }
    }

    /// Apply a finished check. Returns the version to announce in the tray,
    /// once per version.
    pub fn finish_check(&mut self, found: Result<Option<Available>, String>) -> Option<String> {
        self.phase = Phase::Idle;
        self.cache.record_check(now_unix(), found);
        let announce = self
            .cache
            .offer()
            .map(|a| a.version.clone())
            .filter(|v| self.cache.notified.as_deref() != Some(v.as_str()));
        if let Some(v) = &announce {
            self.cache.notified = Some(v.clone());
        }
        self.save();
        announce
    }

    pub fn skip(&mut self, version: &str) {
        self.cache.skipped = Some(version.to_string());
        self.save();
    }

    pub fn record_failure(&mut self, version: &str, message: String) {
        self.phase = Phase::Idle;
        self.ready = None;
        self.waiting_for = None;
        self.cache.last_result = Some(InstallOutcome {
            version: version.to_string(),
            ok: false,
            message,
            at: now_unix(),
        });
        self.save();
    }
}

// -------------------------------------------------------------- effects

/// GET the release listing (Windows: WinHTTP).
pub fn fetch_releases() -> Result<Vec<GhRelease>> {
    #[cfg(windows)]
    {
        let mut body = Vec::new();
        crate::hardware::catalog::https_get(
            RELEASES_URL,
            &["Accept: application/vnd.github+json", "X-GitHub-Api-Version: 2022-11-28"],
            MAX_LISTING_BYTES,
            &mut |c| {
                body.extend_from_slice(c);
                Ok(())
            },
        )?;
        parse_releases(std::str::from_utf8(&body).context("the listing was not UTF-8")?)
    }
    #[cfg(not(windows))]
    bail!("the update check is Windows-only")
}

/// The full check: fetch, then pick against the running version.
pub fn check(include_pre: bool) -> Result<Option<Available>, String> {
    let current = Version::parse(running_version()).ok_or("the running version is not semver")?;
    let releases = fetch_releases().map_err(|e| format!("Could not check for updates: {e:#}"))?;
    Ok(pick(&releases, &current, include_pre))
}

/// Download one URL to `dest`, returning its SHA-256.
pub fn download(url: &str, dest: &Path, max: usize) -> Result<[u8; 32]> {
    if !url.starts_with(DOWNLOAD_PREFIX) {
        bail!("refusing to download from outside this repository's releases: {url}");
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        let part = dest.with_extension("part");
        let mut f =
            std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        let mut h = Sha256::new();
        crate::hardware::catalog::https_get(url, &[], max, &mut |c| {
            h.update(c);
            f.write_all(c).context("writing the download")
        })?;
        f.sync_all().ok();
        drop(f);
        std::fs::rename(&part, dest).with_context(|| format!("moving {}", dest.display()))?;
        Ok(h.finalize().into())
    }
    #[cfg(not(windows))]
    {
        let _ = (dest, max);
        bail!("downloading updates is Windows-only")
    }
}

/// Download the installer and its sums into `dir`, verify both, and return
/// the installer's path. Every refusal is worded for the user.
pub fn fetch_and_verify(dir: &Path, a: &Available) -> Result<PathBuf, String> {
    if !safe_name(&a.installer_name) {
        return Err("The release's installer has an unexpected name; not downloading it.".into());
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

    let sums_path = dir.join(SUMS_ASSET);
    download(&a.sums_url, &sums_path, MAX_SUMS_BYTES)
        .map_err(|e| format!("Could not download the checksums: {e:#}"))?;
    let sums = std::fs::read_to_string(&sums_path)
        .map_err(|e| format!("Could not read the checksums: {e}"))?;
    let expected = parse_sums(&sums, &a.installer_name).ok_or_else(|| {
        format!(
            "The release's {SUMS_ASSET} does not list {}, so it was not installed.",
            a.installer_name
        )
    })?;

    let installer = dir.join(&a.installer_name);
    let actual = download(&a.installer_url, &installer, MAX_INSTALLER_BYTES)
        .map_err(|e| format!("Could not download the installer: {e:#}"))?;
    if actual != expected {
        let _ = std::fs::remove_file(&installer);
        return Err("The download does not match the checksum published with the release, so it \
                    was not installed. Nothing on your PC was changed."
            .into());
    }
    // Re-hash from disk: the file that runs is the file that was checked.
    verify_sum(&installer, &expected)?;
    signature_decision(&signature(&installer), EXPECTED_PUBLISHER).inspect_err(|_| {
        let _ = std::fs::remove_file(&installer);
    })?;
    Ok(installer)
}

/// Start the NSIS installer silently, detached, so it outlives this core
/// (its pre-install hook stops the core; its post-install hook starts the
/// new one and, with `/RELAUNCH`, reopens the window).
pub fn launch_installer(installer: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(installer);
    cmd.args(["/S", "/RELAUNCH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().with_context(|| format!("starting {}", installer.display()))?;
    Ok(())
}

/// Ask Windows about the installer's Authenticode signature.
pub fn signature(path: &Path) -> Signature {
    #[cfg(windows)]
    {
        sig_win::check(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Signature::Unsigned
    }
}

#[cfg(windows)]
mod sig_win {
    use super::Signature;
    use std::path::Path;
    use windows::core::{GUID, HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::Cryptography::{
        CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE,
    };
    use windows::Win32::Security::WinTrust::{
        WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE,
    };

    const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100_u32 as i32;
    const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003_u32 as i32;
    const TRUST_E_PROVIDER_UNKNOWN: i32 = 0x800B_0001_u32 as i32;

    pub fn check(path: &Path) -> Signature {
        let file = HSTRING::from(path.as_os_str());
        let mut info = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(file.as_ptr()),
            ..Default::default()
        };
        let mut data = WINTRUST_DATA {
            cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
            dwStateAction: WTD_STATEACTION_VERIFY,
            ..Default::default()
        };
        let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: `data` and `info` outlive both calls; the state handle is
        // released with WTD_STATEACTION_CLOSE on every path.
        unsafe {
            let rc =
                WinVerifyTrust(HWND(-1isize as *mut _), &mut action, &mut data as *mut _ as *mut _);
            let result = match rc {
                0 => {
                    let subject = signer_subject(data.hWVTStateData);
                    match subject {
                        Some(subject) => Signature::Valid { subject },
                        None => Signature::Invalid { reason: "no signer certificate".into() },
                    }
                }
                TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
                    Signature::Unsigned
                }
                other => {
                    Signature::Invalid { reason: format!("WinVerifyTrust 0x{:08X}", other as u32) }
                }
            };
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            let _ =
                WinVerifyTrust(HWND(-1isize as *mut _), &mut action, &mut data as *mut _ as *mut _);
            result
        }
    }

    unsafe fn signer_subject(state: windows::Win32::Foundation::HANDLE) -> Option<String> {
        let prov = WTHelperProvDataFromStateData(state);
        if prov.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if signer.is_null() || (*signer).csCertChain == 0 || (*signer).pasCertChain.is_null() {
            return None;
        }
        let cert = (*(*signer).pasCertChain).pCert;
        if cert.is_null() {
            return None;
        }
        let mut buf = [0u16; 256];
        let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf));
        if n <= 1 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(n as usize - 1)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/github-releases.json");

    #[test]
    fn semver_orders_numbers_numerically_and_prereleases_below_releases() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("v1.2.3") == v("1.2.3"));
        assert!(v("1.0.0") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.9"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0+build.5") == v("1.0.0"));
        assert_eq!(v("v0.2.0-rc.1").to_string(), "0.2.0-rc.1");
    }

    #[test]
    fn semver_rejects_what_is_not_a_version() {
        for bad in ["", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "1.2.3-", "1..2", "latest"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_fixture_parses_and_the_newest_stable_release_is_picked() {
        let rels = parse_releases(FIXTURE).unwrap();
        assert_eq!(rels.len(), 5);
        let a = pick(&rels, &v("0.1.0"), false).unwrap();
        // 0.4.0 is a draft, 0.3.0-rc.1 a pre-release: 0.2.1 wins.
        assert_eq!(a.version, "0.2.1");
        assert_eq!(a.installer_name, "Relay_0.2.1_x64-setup.exe");
        assert!(a.installer_url.starts_with(DOWNLOAD_PREFIX));
        assert!(a.sums_url.ends_with("/SHA256SUMS.txt"));
        assert!(!a.prerelease);
        assert!(a.notes.contains("- Faster reconnect"));
        assert!(!a.notes.contains("**"));
        assert!(!a.notes.contains("](http"));
    }

    #[test]
    fn prereleases_are_offered_only_when_opted_in() {
        let rels = parse_releases(FIXTURE).unwrap();
        let a = pick(&rels, &v("0.1.0"), true).unwrap();
        assert_eq!(a.version, "0.3.0-rc.1");
        assert!(a.prerelease);
    }

    #[test]
    fn nothing_is_offered_when_up_to_date_or_assets_are_missing() {
        let rels = parse_releases(FIXTURE).unwrap();
        assert!(pick(&rels, &v("0.2.1"), false).is_none());
        assert!(pick(&rels, &v("9.0.0"), true).is_none());
        // 0.2.5 in the fixture has no sums file; 0.2.2 points off-repo.
        let only_broken: Vec<_> = rels
            .iter()
            .filter(|r| r.tag_name == "v0.2.5" || r.tag_name == "v0.2.2")
            .cloned()
            .collect();
        assert!(pick(&only_broken, &v("0.1.0"), true).is_none());
    }

    #[test]
    fn notes_lose_markdown_but_keep_the_words() {
        let n = plain_notes(
            "## What's Changed\r\n* **Fix** the [share](https://x/y) drop by `@a`\n\n\n\nok",
        );
        assert_eq!(n, "What's Changed\n- Fix the share drop by @a\n\nok");
    }

    #[test]
    fn the_check_waits_a_minute_after_start_and_then_once_a_day() {
        let day = CHECK_INTERVAL_SECS;
        let up = Duration::from_secs(61);
        assert!(!check_due(true, Duration::from_secs(30), 10 * day, None));
        assert!(check_due(true, up, 10 * day, None));
        assert!(!check_due(true, up, 10 * day, Some(10 * day - 3600)));
        assert!(check_due(true, up, 10 * day, Some(9 * day)));
        // Clock went backwards: check rather than go quiet.
        assert!(check_due(true, up, 10 * day, Some(11 * day)));
        // Switched off: never.
        assert!(!check_due(false, up, 10 * day, None));
    }

    #[test]
    fn nothing_happens_during_a_share_a_receive_or_a_game() {
        assert_eq!(busy_reason(false, false, false), None);
        assert!(busy_reason(true, false, false).is_some());
        assert!(busy_reason(false, true, false).is_some());
        assert!(busy_reason(false, false, true).is_some());
    }

    #[test]
    fn sums_parse_both_sha256sum_forms() {
        let h = "a".repeat(64);
        let text = format!("{}  licenses.html\n{h} *Relay_0.2.1_x64-setup.exe\n", "b".repeat(64));
        assert_eq!(parse_sums(&text, "Relay_0.2.1_x64-setup.exe"), Some([0xaa; 32]));
        assert_eq!(parse_sums(&text, "licenses.html"), Some([0xbb; 32]));
        assert_eq!(parse_sums(&text, "missing.exe"), None);
        assert_eq!(parse_sums("zz  x.exe", "x.exe"), None);
    }

    #[test]
    fn a_good_sum_passes_and_a_bad_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("relay-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("setup.exe");
        std::fs::write(&f, b"abc").unwrap();
        // SHA-256("abc")
        let good = decode_hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .unwrap();
        assert!(verify_sum(&f, &good).is_ok());
        let mut bad = good;
        bad[0] ^= 1;
        let e = verify_sum(&f, &bad).unwrap_err();
        assert!(e.contains("does not match"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signature_decisions() {
        let p = EXPECTED_PUBLISHER;
        assert!(signature_decision(&Signature::Unsigned, p).is_ok());
        assert!(signature_decision(&Signature::Valid { subject: "SignPath Foundation".into() }, p)
            .is_ok());
        let e = signature_decision(&Signature::Valid { subject: "Someone Else Ltd".into() }, p)
            .unwrap_err();
        assert!(e.contains("Someone Else Ltd"));
        assert!(signature_decision(&Signature::Invalid { reason: "x".into() }, p).is_err());
    }

    #[test]
    fn downloads_are_refused_from_outside_the_repository() {
        let e = download("https://evil.example/Relay_9.9.9_x64-setup.exe", Path::new("x"), 10)
            .unwrap_err();
        assert!(e.to_string().contains("refusing"));
    }

    #[test]
    fn asset_names_cannot_escape_the_download_folder() {
        assert!(safe_name("Relay_0.2.1_x64-setup.exe"));
        for bad in ["..\\x.exe", "../x.exe", "a/b.exe", ".hidden", "", "a b.exe"] {
            assert!(!safe_name(bad), "{bad}");
        }
    }

    fn avail(ver: &str) -> Available {
        Available {
            version: ver.into(),
            notes: String::new(),
            url: String::new(),
            prerelease: false,
            installer_name: format!("Relay_{ver}_x64-setup.exe"),
            installer_url: String::new(),
            installer_size: 0,
            sums_url: String::new(),
        }
    }

    #[test]
    fn a_pending_install_becomes_a_result_on_the_next_start() {
        let mut c = UpdateCache {
            pending_install: Some("0.2.0".into()),
            available: Some(avail("0.2.0")),
            ..Default::default()
        };
        c.reconcile(&v("0.2.0"), 5);
        let r = c.last_result.clone().unwrap();
        assert!(r.ok && r.version == "0.2.0");
        assert!(c.pending_install.is_none());
        assert!(c.available.is_none(), "an installed version is no longer on offer");

        let mut c = UpdateCache { pending_install: Some("0.2.0".into()), ..Default::default() };
        c.reconcile(&v("0.1.0"), 5);
        assert!(!c.last_result.unwrap().ok);
    }

    #[test]
    fn skip_and_later_hide_the_offer_and_the_tray_says_it_once() {
        let dir = std::env::temp_dir().join(format!("relay-upd-state-{}", std::process::id()));
        let paths = crate::config::Paths::at(&dir);
        std::fs::create_dir_all(paths.data_dir()).unwrap();
        let mut u = Updater::load(&paths);
        assert_eq!(u.finish_check(Ok(Some(avail("9.0.0")))).as_deref(), Some("9.0.0"));
        assert_eq!(u.finish_check(Ok(Some(avail("9.0.0")))), None, "announced once");
        assert!(u.status().available.is_some());
        u.later = Some("9.0.0".into());
        assert!(u.status().available.is_none());
        u.later = None;
        u.skip("9.0.0");
        assert!(u.status().available.is_none());
        // A newer one is offered again despite the skip.
        u.finish_check(Ok(Some(avail("9.1.0"))));
        assert_eq!(u.status().available.unwrap().version, "9.1.0");
        // The cache survives a restart.
        let again = Updater::load(&paths);
        assert_eq!(again.cache.skipped.as_deref(), Some("9.0.0"));
        assert!(again.cache.last_check.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_check_keeps_the_last_known_offer() {
        let mut c = UpdateCache { available: Some(avail("9.0.0")), ..Default::default() };
        c.record_check(10, Err("offline".into()));
        assert_eq!(c.last_check, Some(10));
        assert!(c.available.is_some());
        assert_eq!(c.last_error.as_deref(), Some("offline"));
    }
}
