//! Windows Firewall rules for `relay-share.exe` — the one Relay binary that
//! listens on a socket.
//!
//! # Why this module exists
//!
//! "Zero network config for the user" is a non-negotiable in the brief, and
//! without this module it is not true. Windows prompts the first time a given
//! *path* listens; dismissing that prompt writes a **Block** rule that never
//! expires and is never mentioned again. From then on Relay's share fails
//! with no visible cause — the symptom is "the other PC never connects",
//! which reads as a network fault and sends the user off to check their
//! router. On this dev machine the prompt-dismissal path had accumulated ten
//! Block rules and not one Allow rule.
//!
//! So Relay does three things, and this module is all three:
//!
//! 1. **Ask once, explicitly.** An inbound Allow rule is added through the
//!    existing elevated helper ([`crate::elevate`]) behind a normal UAC
//!    prompt the user can read. Relay never grabs elevation silently, and
//!    declining is a supported answer — see [`Verdict`] for what the app
//!    does afterwards.
//! 2. **Remove exactly what it added.** The rule is recorded in
//!    `firewall.json` and removed by the uninstaller
//!    ([`crate::uninstall::StepKind::RemoveFirewallRule`]), because a
//!    leftover firewall rule would fail the clean-VM diff like any other
//!    trace.
//! 3. **Name the failure.** [`status`] classifies the current state so the UI
//!    can say "Windows Firewall is blocking Relay" instead of letting it look
//!    like a dead network.
//!
//! # Reads are registry, writes are COM
//!
//! Detection runs in the always-on core and behind a UI banner, so it has to
//! be cheap and must not drag a COM enumeration into the footprint budget.
//! Windows stores every rule as one string value under
//! [`FIREWALL_RULES_KEY`], in a documented, stable, pipe-delimited format:
//!
//! ```text
//! v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|
//! App=C:\...\relay-share.exe|Name=Relay (relay-share)|Desc=...|
//! ```
//!
//! Reading needs no elevation and no COM, and parsing it is a pure function
//! ([`parse_rule`]) that is fixture-tested against strings captured from a
//! real machine. **Writes never go near that key** — hand-editing it does not
//! notify the firewall service and is unsupported. Mutation goes through
//! `INetFwPolicy2`, in the elevated helper only, and only ever `Add` one rule
//! or `Remove` rules *by name*. Nothing here enumerates rules over COM, so
//! the `IEnumVARIANT` dance never appears.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where Windows keeps the rule strings. Read-only, from here, always.
pub const FIREWALL_RULES_KEY: &str =
    r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules";

/// The display name Relay gives its own rule. This is the handle the
/// uninstaller removes by, so it has to be exact and it has to be ours
/// alone. It matches the name `scripts/firewall-rules.ps1` has always used,
/// so a machine cleaned up by hand before this shipped is still recognised.
pub const RULE_NAME: &str = "Relay (relay-share)";

/// Rule description, so the entry is self-explanatory in wf.msc.
pub const RULE_DESC: &str =
    "Relay LAN share (WebRTC + mDNS) on private and domain networks. Added by Relay's installer; removed when you uninstall Relay.";

/// The only binary that ever listens. `relay-core` talks over a named pipe
/// and opens no socket, so it gets no rule — see the brief.
pub const SHARE_EXE: &str = "relay-share.exe";

// Profile bits, matching `NET_FW_PROFILE_TYPE2`.
pub const PROFILE_DOMAIN: i32 = 0x1;
pub const PROFILE_PRIVATE: i32 = 0x2;
pub const PROFILE_PUBLIC: i32 = 0x4;

/// The profiles Relay's rule applies to: **private and domain, never public**.
///
/// Private is the obvious one — Relay is LAN-only by design. Domain is here
/// because a managed work machine reports its network as Domain, not Private,
/// and a private-only rule would leave exactly the silent failure this module
/// exists to remove. Public stays blocked on purpose: a coffee-shop network
/// has no business reaching a screen share, and Windows classifies unknown
/// networks as Public by default.
pub const RULE_PROFILES: i32 = PROFILE_DOMAIN | PROFILE_PRIVATE;

// ---------------------------------------------------------------------------
// The parsed rule. Pure model — no registry, no COM.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
}

/// One firewall rule, as far as Relay cares about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// The registry value name (a GUID for rules Windows generated). Carried
    /// so a report can point at the exact entry.
    pub key: String,
    /// `Name=` — the display name, and what `INetFwRules::Remove` takes.
    pub name: String,
    /// `App=` — the program the rule is scoped to, verbatim.
    pub app: Option<String>,
    pub action: Action,
    pub direction: Direction,
    /// `Active=TRUE`. A disabled rule is still listed but does not bite.
    pub enabled: bool,
    /// Profile bitmask. A rule with no `Profile=` applies to all of them.
    pub profiles: i32,
    /// The rule string exactly as Windows stored it. Kept so a rule Relay
    /// removes is backed up losslessly — the parsed fields above are only
    /// the ones the verdict needs, and a backup that drops the rest would
    /// not be a backup.
    pub raw: String,
}

impl Rule {
    /// Does this rule govern `program`? Windows stores the path as it was
    /// registered, so compare case-insensitively — but never loosely: a rule
    /// for a different `relay-share.exe` (another worktree, an old install)
    /// must not be mistaken for this one's.
    pub fn governs(&self, program: &str) -> bool {
        self.app.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(program))
    }

    /// Same executable *name*, any location. Used only for reporting the
    /// stale rules a dev machine accumulates — never for the verdict.
    pub fn is_share_exe(&self) -> bool {
        self.app
            .as_deref()
            .and_then(|a| a.rsplit(['\\', '/']).next())
            .is_some_and(|f| f.eq_ignore_ascii_case(SHARE_EXE))
    }

    /// Rule applies on at least one of the currently-connected profiles.
    pub fn applies_on(&self, active: i32) -> bool {
        self.profiles & active != 0
    }

    /// This is the rule Relay itself added.
    pub fn is_ours(&self) -> bool {
        self.name == RULE_NAME
    }
}

/// Parse one registry rule string. Returns `None` for anything that is not a
/// rule we can reason about (a malformed value, or one with no action).
///
/// The format is `v2.xx|Key=Value|Key=Value|…`; keys may repeat, which is how
/// a multi-profile rule is spelled (`Profile=Domain|Profile=Private|`).
/// Values are not escaped by Windows, so a `Name=` containing a literal `|`
/// is not representable and cannot occur.
pub fn parse_rule(key: &str, raw: &str) -> Option<Rule> {
    let mut name = String::new();
    let mut app = None;
    let mut action = None;
    let mut direction = None;
    let mut enabled = true;
    let mut profiles = 0i32;

    for field in raw.split('|') {
        let Some((k, v)) = field.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = v.to_string(),
            "App" => app = Some(v.to_string()),
            "Action" => {
                action = match v {
                    "Allow" => Some(Action::Allow),
                    "Block" => Some(Action::Block),
                    _ => None,
                }
            }
            "Dir" => {
                direction = match v {
                    "In" => Some(Direction::In),
                    "Out" => Some(Direction::Out),
                    _ => None,
                }
            }
            "Active" => enabled = v.eq_ignore_ascii_case("TRUE"),
            "Profile" => {
                profiles |= match v {
                    "Domain" => PROFILE_DOMAIN,
                    "Private" => PROFILE_PRIVATE,
                    "Public" => PROFILE_PUBLIC,
                    _ => 0,
                }
            }
            _ => {}
        }
    }

    Some(Rule {
        key: key.to_string(),
        name,
        app,
        action: action?,
        direction: direction?,
        enabled,
        // No `Profile=` at all means every profile.
        profiles: if profiles == 0 {
            PROFILE_DOMAIN | PROFILE_PRIVATE | PROFILE_PUBLIC
        } else {
            profiles
        },
        raw: raw.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Policy state + verdict. Still pure.
// ---------------------------------------------------------------------------

/// The firewall's own settings, as far as the verdict needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Profiles of the networks this PC is connected to right now.
    pub active_profiles: i32,
    /// The firewall is switched on for at least one active profile.
    pub enabled: bool,
    /// Unsolicited inbound traffic is dropped by default on the active
    /// profiles — the normal Windows setting, and the one that makes an
    /// explicit Allow rule necessary.
    pub default_inbound_block: bool,
}

impl Default for Policy {
    fn default() -> Self {
        // The safe assumption when we cannot read policy: a normal, enabled,
        // default-deny firewall. Guessing "permissive" would suppress the
        // warning on exactly the machines that need it.
        Self { active_profiles: PROFILE_PRIVATE, enabled: true, default_inbound_block: true }
    }
}

/// What the current rules mean for a share on this PC, right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Verdict {
    /// An Allow rule covers this exact binary on a connected profile.
    /// Sharing will work; nothing to say to the user.
    Allowed,
    /// A Block rule matches. **This is the state that looks like a broken
    /// network and is not.** Windows applies deny before allow, so this wins
    /// even when an Allow rule also exists — which is why the installer
    /// removes the Block rules for our own binary rather than only adding an
    /// Allow rule on top of them.
    Blocked,
    /// No rule either way, and the firewall drops unsolicited inbound by
    /// default: Windows will raise its prompt the first time a share starts,
    /// and one wrong click there is what creates [`Verdict::Blocked`].
    WillPrompt,
    /// No rule, but inbound is not being dropped — an already-permissive
    /// network. Sharing works; this is the "declined the prompt and it is
    /// fine anyway" case the DoD asks for.
    Permissive,
    /// The only connected network is Public. Relay does not add rules there
    /// on purpose, so a share will not be reachable until the user marks the
    /// network private — which is a Windows setting, not a Relay one.
    PublicNetwork,
    /// The firewall is switched off entirely. Nothing to do.
    FirewallOff,
}

impl Verdict {
    /// Will an inbound share connection reach `relay-share.exe`?
    pub fn can_share(self) -> bool {
        matches!(self, Verdict::Allowed | Verdict::Permissive | Verdict::FirewallOff)
    }

    /// Is this worth interrupting the user about? `WillPrompt` deliberately
    /// is: it is the last moment before the mistake, not after it.
    pub fn needs_attention(self) -> bool {
        !matches!(self, Verdict::Allowed | Verdict::FirewallOff)
    }

    /// One plain-English sentence. The UI uses its own longer copy; this is
    /// for `relay-core firewall` and the logs.
    pub fn summary(self) -> &'static str {
        match self {
            Verdict::Allowed => "Relay is allowed through Windows Firewall.",
            Verdict::Blocked => {
                "Windows Firewall is blocking Relay's share. This is a firewall rule, not a network fault."
            }
            Verdict::WillPrompt => {
                "Windows will ask whether to allow Relay the first time you share. Declining that prompt blocks Relay permanently."
            }
            Verdict::Permissive => {
                "No firewall rule for Relay, but this network is not blocking inbound connections."
            }
            Verdict::PublicNetwork => {
                "This network is set to Public, so Relay is not reachable on it."
            }
            Verdict::FirewallOff => "Windows Firewall is off for this network.",
        }
    }
}

/// Everything the UI and the CLI are told about the firewall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallStatus {
    #[serde(flatten)]
    pub verdict: Verdict,
    /// The `relay-share.exe` the verdict is about, so a report can never be
    /// ambiguous about *which* binary is allowed.
    pub program: String,
    /// Relay's own Allow rule is present for this binary.
    pub rule_present: bool,
    /// Block rules matching this exact binary, on a connected profile.
    pub blocking_rules: usize,
    /// Rules for a `relay-share.exe` somewhere *else* — another worktree or
    /// an old install. Harmless, but worth showing on a dev machine.
    pub stale_rules: usize,
    pub policy: Policy,
    /// The probe could not read firewall state; every field above is a
    /// conservative default and should not be reported as fact.
    pub unknown: bool,
}

/// Classify. Pure, so every branch is unit-tested without a firewall.
///
/// Order matters: a Block rule beats an Allow rule (that is how Windows
/// evaluates), and an explicit rule beats the default action.
pub fn classify(rules: &[Rule], program: &str, policy: &Policy) -> Verdict {
    if !policy.enabled {
        return Verdict::FirewallOff;
    }

    let relevant = |r: &&Rule| {
        r.enabled
            && r.direction == Direction::In
            && r.governs(program)
            && r.applies_on(policy.active_profiles)
    };
    let mut blocked = false;
    let mut allowed = false;
    for r in rules.iter().filter(relevant) {
        match r.action {
            Action::Block => blocked = true,
            Action::Allow => allowed = true,
        }
    }

    if blocked {
        return Verdict::Blocked;
    }
    if allowed {
        return Verdict::Allowed;
    }
    if !policy.default_inbound_block {
        return Verdict::Permissive;
    }
    // No rule and inbound is dropped. If the only network we are on is one
    // Relay will not add a rule for, say so specifically rather than
    // promising a prompt that will not help.
    if policy.active_profiles & RULE_PROFILES == 0 && policy.active_profiles & PROFILE_PUBLIC != 0 {
        return Verdict::PublicNetwork;
    }
    Verdict::WillPrompt
}

/// Build the full status from parsed rules. Pure counterpart to [`status`].
pub fn status_from(rules: &[Rule], program: &str, policy: Policy) -> FirewallStatus {
    let verdict = classify(rules, program, &policy);
    FirewallStatus {
        verdict,
        program: program.to_string(),
        rule_present: rules.iter().any(|r| {
            r.is_ours()
                && r.action == Action::Allow
                && r.direction == Direction::In
                && r.governs(program)
        }),
        blocking_rules: rules
            .iter()
            .filter(|r| {
                r.enabled
                    && r.action == Action::Block
                    && r.direction == Direction::In
                    && r.governs(program)
            })
            .count(),
        stale_rules: rules.iter().filter(|r| r.is_share_exe() && !r.governs(program)).count(),
        policy,
        unknown: false,
    }
}

// ---------------------------------------------------------------------------
// The record of what we added, so uninstall removes exactly that.
// ---------------------------------------------------------------------------

pub const RECORD_VERSION: u32 = 1;

/// `firewall.json` — written when the rule goes in, deleted when it comes
/// out. Same contract as `installed.json`: the uninstaller plans from what
/// this says was done, not from a hard-coded guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallRecord {
    pub version: u32,
    /// ISO-8601 UTC.
    pub installed_at: String,
    /// The rule's display name — the handle the uninstaller removes by.
    pub rule_name: String,
    /// The binary the rule was scoped to.
    pub program: String,
    pub profiles: i32,
    /// Block rules for this binary that the install removed, captured
    /// verbatim *before* the change. Relay's rule is "original state is
    /// written to disk before any change", and a Block rule the user created
    /// by dismissing a prompt is still state we are touching.
    #[serde(default)]
    pub removed_blocks: Vec<Rule>,
}

impl FirewallRecord {
    pub fn new(program: &str, removed_blocks: Vec<Rule>) -> Self {
        Self {
            version: RECORD_VERSION,
            installed_at: now_iso8601(),
            rule_name: RULE_NAME.to_string(),
            program: program.to_string(),
            profiles: RULE_PROFILES,
            removed_blocks,
        }
    }
}

/// Load the record; a missing file means "we never added a rule". A corrupt
/// one is an error — never guess about what is on the machine.
pub fn load_record(path: &Path) -> anyhow::Result<Option<FirewallRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write the record atomically (tmp-then-rename), like every other record
/// Relay keeps.
pub fn save_record(path: &Path, record: &FirewallRecord) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn clear_record(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn now_iso8601() -> String {
    // Same shape as relay-vdevice's record, without pulling in a date crate.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Howard Hinnant's `civil_from_days`, for a date string without a crate.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The `relay-share.exe` this process should be reasoning about: the one
/// sitting beside the running binary. Derived, never passed in — the same
/// rule the elevated helper follows for every other install path, so a
/// tampered request cannot point a firewall rule at somebody else's exe.
pub fn share_program() -> anyhow::Result<PathBuf> {
    let me = std::env::current_exe()?;
    let dir = me.parent().ok_or_else(|| anyhow::anyhow!("the running binary has no parent"))?;
    Ok(dir.join(SHARE_EXE))
}

// ---------------------------------------------------------------------------
// Windows: the registry read and the two COM writes.
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub use imp::{
    add_allow_rule, install_dry_run, install_live, read_policy, read_rules, remove_rules_named,
    status, uninstall_live, LIVE_WRITE_GATE,
};

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::{Context, Result};

    use windows::core::BSTR;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule, NetFwPolicy2, NetFwRule, NET_FW_ACTION_ALLOW,
        NET_FW_IP_PROTOCOL_ANY, NET_FW_PROFILE_TYPE2, NET_FW_RULE_DIR_IN,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    /// Read and parse every firewall rule. Read-only, unelevated, no COM.
    ///
    /// A machine can have a few thousand rules; each is a short string and we
    /// keep only the parsed struct, so this is a few milliseconds and a few
    /// hundred KB transiently. It runs on demand (a screen opening), never on
    /// the core's 1 s tick.
    pub fn read_rules() -> Result<Vec<Rule>> {
        let mut out = Vec::new();
        let sub: Vec<u16> = FIREWALL_RULES_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key = HKEY::default();
        // SAFETY: `sub` is NUL-terminated and outlives the call; `key` is
        // closed on every path below.
        unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                windows::core::PCWSTR(sub.as_ptr()),
                None,
                KEY_READ,
                &mut key,
            )
            .ok()
            .context("opening the firewall rule store")?;
        }

        let mut index = 0u32;
        loop {
            // Rule names are GUIDs or short strings; the data is the rule
            // string. Both are generously sized rather than probed twice.
            let mut name = vec![0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; 8192];
            let mut data_len = data.len() as u32;
            let mut ty = 0u32;

            // SAFETY: every buffer and length is live for the call, and the
            // lengths are re-read afterwards before the buffers are sliced.
            let rc = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    Some(windows::core::PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut ty),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if rc.is_err() {
                // ERROR_NO_MORE_ITEMS ends the walk; anything else (a value
                // too big for the buffers) skips just that rule.
                if rc == windows::Win32::Foundation::ERROR_NO_MORE_ITEMS {
                    break;
                }
                index += 1;
                if index > 100_000 {
                    break;
                }
                continue;
            }
            index += 1;
            // Every rule is a REG_SZ. Anything else under this key is not a
            // rule and is not ours to interpret.
            if ty != REG_SZ.0 {
                continue;
            }

            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            // REG_SZ data is UTF-16 with a trailing NUL.
            let units: Vec<u16> = data[..data_len as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            let raw = String::from_utf16_lossy(&units);
            if let Some(rule) = parse_rule(&name, &raw) {
                out.push(rule);
            }
        }

        // SAFETY: `key` came from a successful RegOpenKeyExW.
        unsafe { RegCloseKey(key).ok().ok() };
        Ok(out)
    }

    thread_local! {
        /// COM is initialised at most once per thread that asks the firewall
        /// anything. We deliberately never `CoUninitialize`: this runs on
        /// threads the core owns for their whole life, and tearing down an
        /// apartment the caller may also be using would be worse than
        /// leaving it up.
        static COM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Join an apartment if this thread has none. Multi-threaded, to match
    /// the rest of the process; a thread that already picked STA keeps it
    /// (`RPC_E_CHANGED_MODE`), which is fine — the firewall objects are
    /// happy either way.
    fn ensure_com() {
        COM_READY.with(|ready| {
            if ready.get() {
                return;
            }
            // SAFETY: no pointers; every outcome, including "already in an
            // apartment", is acceptable and checked by the call that follows.
            let _ = unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                )
            };
            ready.set(true);
        });
    }

    fn policy2() -> Result<INetFwPolicy2> {
        ensure_com();
        // SAFETY: standard in-proc CoCreateInstance for a documented CLSID.
        unsafe {
            CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)
                .context("creating the firewall policy object")
        }
    }

    /// Read the profile/default-action settings the verdict depends on.
    pub fn read_policy() -> Result<Policy> {
        let p = policy2()?;
        // SAFETY: `p` is a live COM object; all three calls are read-only
        // property gets that take and return plain values.
        unsafe {
            let active = p.CurrentProfileTypes().context("reading the active network profiles")?;
            let mut enabled = false;
            let mut block = false;
            for bit in [PROFILE_DOMAIN, PROFILE_PRIVATE, PROFILE_PUBLIC] {
                if active & bit == 0 {
                    continue;
                }
                let ty = NET_FW_PROFILE_TYPE2(bit);
                if p.get_FirewallEnabled(ty).map(|b| b.as_bool()).unwrap_or(true) {
                    enabled = true;
                    // Default-deny on any active profile is enough to need a
                    // rule: the share has to work on all of them.
                    if p.get_DefaultInboundAction(ty)
                        .map(|a| a != NET_FW_ACTION_ALLOW)
                        .unwrap_or(true)
                    {
                        block = true;
                    }
                }
            }
            Ok(Policy { active_profiles: active, enabled, default_inbound_block: block })
        }
    }

    /// The full read-only probe behind `Method::FirewallStatus`.
    ///
    /// Never fails: a machine whose firewall state cannot be read is reported
    /// as `unknown` with conservative defaults, because a probe error is not
    /// a reason to tell the user everything is fine.
    pub fn status(program: &Path) -> FirewallStatus {
        let program = program.to_string_lossy().to_string();
        let read = read_rules().and_then(|r| Ok((r, read_policy()?)));
        match read {
            Ok((rules, policy)) => status_from(&rules, &program, policy),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not read firewall state");
                FirewallStatus {
                    verdict: Verdict::WillPrompt,
                    program,
                    rule_present: false,
                    blocking_rules: 0,
                    stale_rules: 0,
                    policy: Policy::default(),
                    unknown: true,
                }
            }
        }
    }

    /// Add the inbound Allow rule for `program` on [`RULE_PROFILES`].
    ///
    /// Elevated helper only — modifying firewall policy needs an
    /// administrator token, which is exactly why this sits behind the same
    /// UAC prompt as the other install ops rather than being attempted
    /// silently by the installer.
    pub fn add_allow_rule(program: &Path) -> Result<()> {
        let rule: INetFwRule =
            // SAFETY: documented CLSID, in-proc, no aggregation.
            unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }
                .context("creating a firewall rule object")?;
        // SAFETY: `rule` is live and every BSTR outlives its call.
        unsafe {
            rule.SetName(&BSTR::from(RULE_NAME))?;
            rule.SetDescription(&BSTR::from(RULE_DESC))?;
            rule.SetApplicationName(&BSTR::from(program.to_string_lossy().as_ref()))?;
            rule.SetDirection(NET_FW_RULE_DIR_IN)?;
            rule.SetAction(NET_FW_ACTION_ALLOW)?;
            rule.SetProtocol(NET_FW_IP_PROTOCOL_ANY.0)?;
            rule.SetProfiles(RULE_PROFILES)?;
            rule.SetEnabled(windows::Win32::Foundation::VARIANT_TRUE)?;

            let policy = policy2()?;
            policy.Rules().context("opening the rule collection")?.Add(&rule).context(
                "adding the rule — this needs administrator rights, which the elevated helper holds",
            )?;
        }
        Ok(())
    }

    /// The live-write gate, matching `RELAY_APO_ALLOW_LIVE_WRITE` and
    /// `RELAY_VDEVICE_ALLOW_LIVE_WRITE`. Only [`crate::elevate`] arms it, and
    /// only around one vetted call, so an accidental call anywhere else in
    /// the product fails loudly instead of changing firewall policy.
    pub const LIVE_WRITE_GATE: &str = "RELAY_FIREWALL_ALLOW_LIVE_WRITE";

    fn gate_open() -> Result<()> {
        anyhow::ensure!(
            std::env::var(LIVE_WRITE_GATE).is_ok_and(|v| v == "1"),
            "refusing to change firewall policy: {LIVE_WRITE_GATE} is not set. \
             Firewall rules are written by the elevated helper only."
        );
        anyhow::ensure!(
            crate::processes::is_elevated(),
            "changing firewall policy needs administrator rights"
        );
        Ok(())
    }

    /// What an install would change, for the listing shown *before* the UAC
    /// prompt. Read-only.
    pub fn install_dry_run(program: &Path) -> Vec<String> {
        let mut lines = vec![format!(
            "[ ] Allow inbound connections to — {} (private and domain networks; never public)",
            program.display()
        )];
        match read_rules() {
            Ok(rules) => {
                let p = program.to_string_lossy();
                let blocks = block_rules_for(&rules, &p);
                for b in &blocks {
                    lines.push(format!(
                        "[x] Remove a firewall rule that blocks Relay — \"{}\"",
                        b.name
                    ));
                }
                let dupes = rules.iter().filter(|r| r.is_ours()).count();
                if dupes > 0 {
                    lines.push(format!(
                        "[x] Replace {dupes} existing rule(s) named \"{RULE_NAME}\""
                    ));
                }
            }
            Err(_) => lines.push("[ ] (could not read the current firewall rules)".into()),
        }
        lines
    }

    /// Inbound Block rules scoped to exactly this binary — the ones that make
    /// an Allow rule useless, because Windows applies deny first.
    fn block_rules_for(rules: &[Rule], program: &str) -> Vec<Rule> {
        rules
            .iter()
            .filter(|r| {
                r.action == Action::Block && r.direction == Direction::In && r.governs(program)
            })
            .cloned()
            .collect()
    }

    /// Add Relay's Allow rule, after backing up and removing the Block rules
    /// that would override it.
    ///
    /// Backup-then-apply, like every other install path in Relay: the record
    /// (including each removed rule's verbatim string) is on disk *before*
    /// the first policy change.
    ///
    /// Idempotent. Existing rules named [`RULE_NAME`] are removed before the
    /// new one is added, so re-running this — or reinstalling over the top —
    /// leaves exactly one rule rather than accumulating duplicates. Note that
    /// this removes rules with Relay's name for *other* copies of
    /// `relay-share.exe` too (`INetFwRules::Remove` matches on name alone);
    /// on a dev machine that means the ones
    /// `scripts/firewall-rules.ps1 -Allow` added for other worktrees, which
    /// that script can re-add.
    pub fn install_live(paths: &crate::config::Paths, program: &Path) -> Result<FirewallStatus> {
        gate_open()?;
        anyhow::ensure!(
            program.exists(),
            "{} is not on disk — refusing to add a firewall rule for a path that does not exist",
            program.display()
        );
        let p = program.to_string_lossy().to_string();

        let rules = read_rules()?;
        let blocks = block_rules_for(&rules, &p);

        // Backup first. If anything below fails, this file is what says what
        // was on the machine beforehand.
        let record = FirewallRecord::new(&p, blocks.clone());
        save_record(&paths.firewall_file(), &record)
            .context("writing the firewall record before changing anything")?;

        let mut names: Vec<String> = blocks.iter().map(|r| r.name.clone()).collect();
        names.push(RULE_NAME.to_string());
        names.sort();
        names.dedup();
        let removed = remove_rules_named(&names)?;

        add_allow_rule(program)?;
        tracing::info!(program = %p, removed, "added the Relay firewall rule");
        Ok(status(program))
    }

    /// Remove Relay's rule and forget the record.
    ///
    /// The Block rules the install removed are **not** re-created. They named
    /// a binary that the uninstall is deleting, so restoring them would put
    /// back a rule blocking a program that no longer exists — and would
    /// silently re-break Relay for anyone who reinstalls. Their verbatim
    /// strings stay in `firewall.json` until that file is deleted, so the
    /// decision is auditable rather than lossy. A clean VM has no such rules,
    /// so the uninstall diff is unaffected either way.
    pub fn uninstall_live(paths: &crate::config::Paths) -> Result<usize> {
        gate_open()?;
        let removed = remove_rules_named(&[RULE_NAME.to_string()])?;
        clear_record(&paths.firewall_file())?;
        tracing::info!(removed, "removed the Relay firewall rule");
        Ok(removed)
    }

    /// Remove every rule with this display name. `Remove` is by name, which
    /// is why Relay's rule has a distinctive one — and why the Block rules
    /// Windows generates (named after the program) are removed by *their*
    /// recorded names rather than by a wildcard.
    ///
    /// Returns how many names were removed without error.
    pub fn remove_rules_named(names: &[String]) -> Result<usize> {
        let policy = policy2()?;
        // SAFETY: `policy` is live; `Rules` is a property get and `Remove`
        // takes one BSTR that outlives the call.
        let rules = unsafe { policy.Rules() }.context("opening the rule collection")?;
        let mut removed = 0;
        for name in names {
            // Removing a name that is not there is not an error worth
            // failing an uninstall over — it is the desired end state.
            // SAFETY: as above.
            if unsafe { rules.Remove(&BSTR::from(name.as_str())) }.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(not(windows))]
pub fn status(program: &Path) -> FirewallStatus {
    FirewallStatus {
        verdict: Verdict::FirewallOff,
        program: program.to_string_lossy().to_string(),
        rule_present: false,
        blocking_rules: 0,
        stale_rules: 0,
        policy: Policy { active_profiles: 0, enabled: false, default_inbound_block: false },
        unknown: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a dev machine's rule store (user path replaced).
    const REAL_ALLOW: &str = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=%LOCALAPPDATA%\Relay\relay-share.exe|Name=Relay (relay-share)|Desc=Relay LAN share (WebRTC + mDNS). Added by scripts/firewall-rules.ps1.|";
    /// The shape Windows writes when the user dismisses the prompt.
    const REAL_BLOCK: &str = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=%LOCALAPPDATA%\Relay\relay-share.exe|Name=relay-share.exe|Desc=|";

    const INSTALLED: &str = r"%LOCALAPPDATA%\Relay\relay-share.exe";

    fn deny_policy() -> Policy {
        Policy { active_profiles: PROFILE_PRIVATE, enabled: true, default_inbound_block: true }
    }

    #[test]
    fn parses_a_real_allow_rule() {
        let r = parse_rule("{guid}", REAL_ALLOW).unwrap();
        assert_eq!(r.name, RULE_NAME);
        assert_eq!(r.action, Action::Allow);
        assert_eq!(r.direction, Direction::In);
        assert!(r.enabled);
        assert_eq!(r.profiles, PROFILE_PRIVATE);
        assert!(r.governs(INSTALLED));
        assert!(r.is_ours());
    }

    /// The description field contains `. Added by…` — a value with dots and
    /// spaces must not confuse the splitter, and an empty `Desc=` must parse.
    #[test]
    fn parses_a_real_block_rule_with_an_empty_description() {
        let r = parse_rule("{guid}", REAL_BLOCK).unwrap();
        assert_eq!(r.action, Action::Block);
        assert_eq!(r.name, "relay-share.exe");
        assert!(!r.is_ours());
        assert!(r.governs(INSTALLED));
    }

    #[test]
    fn a_rule_without_a_profile_applies_everywhere() {
        let raw = r"v2.33|Action=Allow|Active=TRUE|Dir=In|App=C:\x\relay-share.exe|Name=x|";
        let r = parse_rule("{g}", raw).unwrap();
        assert!(r.applies_on(PROFILE_PUBLIC));
        assert!(r.applies_on(PROFILE_DOMAIN));
        assert!(r.applies_on(PROFILE_PRIVATE));
    }

    #[test]
    fn multiple_profile_fields_are_combined() {
        let raw = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Profile=Domain|Profile=Private|App=C:\x\relay-share.exe|Name=x|";
        let r = parse_rule("{g}", raw).unwrap();
        assert_eq!(r.profiles, PROFILE_DOMAIN | PROFILE_PRIVATE);
        assert!(!r.applies_on(PROFILE_PUBLIC));
    }

    #[test]
    fn a_rule_with_no_action_is_not_a_rule_we_reason_about() {
        assert!(parse_rule("{g}", "v2.33|Active=TRUE|Dir=In|").is_none());
        assert!(parse_rule("{g}", "").is_none());
    }

    /// The whole point: deny beats allow, so adding an Allow rule on top of
    /// a Block rule would leave the user just as broken.
    #[test]
    fn a_block_rule_wins_over_an_allow_rule() {
        let rules =
            vec![parse_rule("{a}", REAL_ALLOW).unwrap(), parse_rule("{b}", REAL_BLOCK).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::Blocked);
    }

    #[test]
    fn an_allow_rule_alone_is_allowed() {
        let rules = vec![parse_rule("{a}", REAL_ALLOW).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::Allowed);
        assert!(classify(&rules, INSTALLED, &deny_policy()).can_share());
    }

    /// A Block rule for a *different* `relay-share.exe` — another worktree —
    /// must not be reported as blocking this one.
    #[test]
    fn a_rule_for_another_copy_of_the_exe_is_stale_not_blocking() {
        let other = REAL_BLOCK.replace(INSTALLED, r"D:\worktree\target\debug\relay-share.exe");
        let rules = vec![parse_rule("{b}", &other).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::WillPrompt);
        let s = status_from(&rules, INSTALLED, deny_policy());
        assert_eq!(s.blocking_rules, 0);
        assert_eq!(s.stale_rules, 1);
    }

    #[test]
    fn a_disabled_block_rule_does_not_bite() {
        let off = REAL_BLOCK.replace("Active=TRUE", "Active=FALSE");
        let rules = vec![parse_rule("{b}", &off).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::WillPrompt);
    }

    /// A Block rule that only applies on Public does not block a share on
    /// the private network the user is actually on.
    #[test]
    fn a_block_rule_on_an_inactive_profile_does_not_bite() {
        let pub_only = REAL_BLOCK.replace("Profile=Private", "Profile=Public");
        let rules = vec![parse_rule("{b}", &pub_only).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::WillPrompt);
    }

    #[test]
    fn an_outbound_rule_is_irrelevant() {
        let out = REAL_BLOCK.replace("Dir=In", "Dir=Out");
        let rules = vec![parse_rule("{b}", &out).unwrap()];
        assert_eq!(classify(&rules, INSTALLED, &deny_policy()), Verdict::WillPrompt);
    }

    #[test]
    fn no_rule_on_a_permissive_network_is_fine() {
        let policy = Policy { default_inbound_block: false, ..deny_policy() };
        let v = classify(&[], INSTALLED, &policy);
        assert_eq!(v, Verdict::Permissive);
        assert!(v.can_share());
    }

    #[test]
    fn a_disabled_firewall_needs_nothing() {
        let policy = Policy { enabled: false, ..deny_policy() };
        assert_eq!(classify(&[], INSTALLED, &policy), Verdict::FirewallOff);
        assert!(!classify(&[], INSTALLED, &policy).needs_attention());
    }

    /// Relay adds no Public rule on purpose, so on a public-only network the
    /// honest answer names that rather than promising a prompt.
    #[test]
    fn a_public_only_network_is_called_out_by_name() {
        let policy = Policy { active_profiles: PROFILE_PUBLIC, ..deny_policy() };
        assert_eq!(classify(&[], INSTALLED, &policy), Verdict::PublicNetwork);
    }

    /// Domain is in scope, so a work machine gets the same Allow rule.
    #[test]
    fn a_domain_network_is_in_scope() {
        assert_ne!(RULE_PROFILES & PROFILE_DOMAIN, 0);
        assert_eq!(RULE_PROFILES & PROFILE_PUBLIC, 0);
        let domain = REAL_ALLOW.replace("Profile=Private", "Profile=Domain");
        let rules = vec![parse_rule("{a}", &domain).unwrap()];
        let policy = Policy { active_profiles: PROFILE_DOMAIN, ..deny_policy() };
        assert_eq!(classify(&rules, INSTALLED, &policy), Verdict::Allowed);
    }

    #[test]
    fn the_default_policy_assumes_the_worst_not_the_best() {
        let p = Policy::default();
        assert!(p.enabled && p.default_inbound_block);
        assert!(classify(&[], INSTALLED, &p).needs_attention());
    }

    #[test]
    fn program_matching_is_case_insensitive_but_not_loose() {
        let r = parse_rule("{a}", REAL_ALLOW).unwrap();
        assert!(r.governs(&INSTALLED.to_uppercase()));
        assert!(!r.governs(r"%LOCALAPPDATA%\Relay\relay-core.exe"));
    }

    #[test]
    fn the_record_round_trips() {
        let dir = std::env::temp_dir().join(format!("relay-fw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("firewall.json");
        assert!(load_record(&path).unwrap().is_none());

        let blocks = vec![parse_rule("{b}", REAL_BLOCK).unwrap()];
        let rec = FirewallRecord::new(INSTALLED, blocks.clone());
        save_record(&path, &rec).unwrap();
        let back = load_record(&path).unwrap().unwrap();
        assert_eq!(back, rec);
        assert_eq!(back.removed_blocks, blocks);
        assert_eq!(back.profiles, RULE_PROFILES);

        clear_record(&path).unwrap();
        assert!(load_record(&path).unwrap().is_none());
        // Clearing twice is fine — the uninstaller must be re-runnable.
        clear_record(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_timestamp_looks_like_a_timestamp() {
        let t = now_iso8601();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z'));
        assert!(t.starts_with("20"), "{t}");
    }
}
