//! VCP opcodes, the per-model quirks table and the pure profile-field →
//! opcode mapping.
//!
//! Standard opcodes come from MCCS 2.2. Vendor features (black equaliser,
//! response time / overdrive) live at vendor-reserved opcodes that differ per
//! model and that no vendor in this table publishes. Writing the wrong one
//! silently changes a setting the user never asked for, so this module makes
//! guessing *structurally impossible*:
//!
//! - every vendor opcode in [`QUIRKS`] carries its [`Evidence`];
//! - a vendor opcode only reaches a write as a [`VerifiedCode`], whose single
//!   constructor ([`VerifiedCode::attest`]) returns `None` for
//!   [`Evidence::Unverified`];
//! - [`VerifiedCode`]'s field is private to its module, so no other code in
//!   this crate — or any other — can mint one.
//!
//! An unmapped or unverified field is skipped and reported as
//! [`UnsupportedReason::NoKnownOpcode`], which the UI turns into a disabled
//! slider.
//!
//! Adding a model: see `docs/dev/vcp-verification.md` for the OSD runbook.

/// MCCS standard: luminance ("brightness").
pub const BRIGHTNESS: u8 = 0x10;
/// MCCS standard: contrast.
pub const CONTRAST: u8 = 0x12;
/// MCCS standard: sharpness.
pub const SHARPNESS: u8 = 0x87;
/// MCCS standard: input select. Never written by Relay; listed because the
/// capability parser and tests reference it.
pub const INPUT_SELECT: u8 = 0x60;

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// Why we believe a vendor opcode does what the table says it does.
///
/// There is no third option: either something authoritative says so, or a
/// named human watched the monitor's own OSD while the code was written, on a
/// named date. Set-and-read-back does not count — a monitor will happily
/// store and return a value for a control whose on-screen meaning is
/// something else entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Published documentation: an MCCS-style vendor spec or a vendor
    /// engineering note. `url` is where it was read.
    VendorDoc { title: &'static str, url: &'static str },
    /// A human watched the OSD change while Relay wrote the code. `date` is
    /// ISO-8601, `monitor` the exact panel it was seen on.
    Osd { observer: &'static str, date: &'static str, monitor: &'static str },
    /// A plausible candidate and nothing more — usually an opcode the panel
    /// advertises in the vendor-reserved range with a value list that happens
    /// to be the right length. Never written.
    Unverified { note: &'static str },
}

impl Evidence {
    pub const fn is_verified(&self) -> bool {
        !matches!(self, Evidence::Unverified { .. })
    }

    /// One line for logs, the docs table and the UI's "why is this off".
    pub fn describe(&self) -> String {
        match self {
            Evidence::VendorDoc { title, url } => format!("vendor doc: {title} ({url})"),
            Evidence::Osd { observer, date, monitor } => {
                format!("observed on the OSD of {monitor} by {observer} on {date}")
            }
            Evidence::Unverified { note } => format!("unverified: {note}"),
        }
    }
}

mod verified {
    use super::Evidence;

    /// A VCP opcode that may be written to a monitor.
    ///
    /// The inner `u8` is private to this module and there is exactly one
    /// constructor, so a code can only exist here if some [`Evidence`] other
    /// than `Unverified` backs it. This is the whole no-guessing guarantee;
    /// do not add a `From<u8>`, a public field or a `new(u8)`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VerifiedCode(u8);

    impl VerifiedCode {
        /// `Some` only when `evidence` is a vendor doc or an OSD observation.
        pub const fn attest(code: u8, evidence: &Evidence) -> Option<Self> {
            if evidence.is_verified() {
                Some(VerifiedCode(code))
            } else {
                None
            }
        }

        pub const fn get(self) -> u8 {
            self.0
        }
    }
}

pub use verified::VerifiedCode;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// A vendor opcode as it appears in the table: the code plus its evidence.
/// Whether it is ever written is decided by [`VerifiedCode::attest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub code: u8,
    pub evidence: Evidence,
}

/// A candidate for the response-time / overdrive control, with the wire value
/// each profile level maps to. The level map has to be verified along with the
/// code: knowing that 0xF5 is overdrive is no use if we do not know whether
/// `2` means "fast" or "off".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseCandidate {
    pub code: u8,
    pub levels: &'static [(&'static str, u32)],
    pub evidence: Evidence,
}

/// One row of the per-model table.
///
/// Matching is EDID-based: `pnp` is the three-letter manufacturer id and
/// `product` the four hex digits of the product code, which together name one
/// model. `models` lists the MCCS `model(...)` strings and EDID display names
/// the row has been seen under — for the docs and for humans reading a
/// capability dump. It does not decide a match, because one marketing name
/// covers several panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelQuirks {
    pub pnp: &'static str,
    /// `None` = every product from this manufacturer. A vendor-wide row
    /// carries timing only; see `vendor_wide_rows_carry_no_vendor_opcodes`.
    pub product: Option<&'static str>,
    pub models: &'static [&'static str],
    pub write_delay_ms: u64,
    pub black_equalizer: Option<Candidate>,
    pub response: Option<ResponseCandidate>,
}

impl ModelQuirks {
    const fn vendor(pnp: &'static str, write_delay_ms: u64) -> Self {
        Self {
            pnp,
            product: None,
            models: &[],
            write_delay_ms,
            black_equalizer: None,
            response: None,
        }
    }
}

/// Per-model quirks, most specific first. Rows with `product: None` are
/// manufacturer-wide fallbacks and carry timing only.
pub static QUIRKS: &[ModelQuirks] = &[
    // LG 32GS95UE / "LG ULTRAGEAR+", EDID GSM 5C7C, MCCS model WK95U. The
    // panel advertises F4, F5(01 02 03 04), F6(00 01 02), F7(00 01 02 03),
    // F8(00 01), F9, FA, FD, FE and FF in the vendor-reserved range. LG
    // publishes nothing about any of them, and a value-list length is exactly
    // the kind of hint that turns into a wrong write.
    ModelQuirks {
        pnp: "GSM",
        product: Some("5C7C"),
        models: &["WK95U", "LG ULTRAGEAR+", "32GS95UE"],
        write_delay_ms: 60,
        black_equalizer: Some(Candidate {
            code: 0xF6,
            evidence: Evidence::Unverified {
                note: "advertised as F6(00 01 02); three values, and Black Stabilizer has more",
            },
        }),
        response: Some(ResponseCandidate {
            code: 0xF5,
            levels: &[("off", 1), ("normal", 2), ("fast", 3), ("faster", 4)],
            evidence: Evidence::Unverified {
                note: "advertised as F5(01 02 03 04); four values matches the four \
                       response-time steps in the OSD, but the order is a guess",
            },
        }),
    },
    // Manufacturer-wide timing fallbacks. LG writes land reliably but settle
    // slowly; measured during the M2 live pass (2026-09-13).
    ModelQuirks::vendor("GSM", 60),
];

/// How to drive one monitor model over DDC/CI. Resolved from [`QUIRKS`]; the
/// vendor fields are `None` unless the table row's evidence held up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quirks {
    /// Pause after every `SetVCPFeature` — cheap monitors drop back-to-back
    /// writes on the floor.
    pub write_delay_ms: u64,
    /// Verified vendor opcode for "black equaliser" / "black stabilizer".
    pub black_equalizer: Option<VerifiedCode>,
    /// Verified vendor opcode for the response-time / overdrive setting, plus
    /// the value each named level maps to.
    pub response: Option<ResponseQuirk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseQuirk {
    pub code: VerifiedCode,
    /// (profile name, wire value), e.g. ("fast", 2).
    pub levels: &'static [(&'static str, u32)],
}

impl Default for Quirks {
    fn default() -> Self {
        Self { write_delay_ms: 50, black_equalizer: None, response: None }
    }
}

/// What identifies a monitor to the quirks table. `name` is the EDID display
/// name; it is carried for logging and never decides a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorKey<'a> {
    pub pnp: &'a str,
    pub product: Option<&'a str>,
    pub name: Option<&'a str>,
}

impl<'a> MonitorKey<'a> {
    /// Split a stable monitor id: `mon:GSM5C7C:402NTCZ9E219` → GSM / 5C7C.
    /// Ids that are not EDID-derived (`mon:path:…`) match nothing.
    pub fn from_id(monitor_id: &'a str) -> Option<Self> {
        let rest = monitor_id.strip_prefix("mon:")?;
        let head = rest.split(':').next()?;
        if head.len() < 3 {
            return None;
        }
        let (pnp, product) = head.split_at(3);
        if !pnp.chars().all(|c| c.is_ascii_uppercase()) {
            return None;
        }
        let product = (product.len() == 4 && product.chars().all(|c| c.is_ascii_hexdigit()))
            .then_some(product);
        Some(Self { pnp, product, name: None })
    }

    pub fn with_name(mut self, name: &'a str) -> Self {
        self.name = (!name.is_empty()).then_some(name);
        self
    }

    fn matches(&self, row: &ModelQuirks) -> bool {
        if !self.pnp.eq_ignore_ascii_case(row.pnp) {
            return false;
        }
        match row.product {
            None => true,
            Some(p) => self.product.is_some_and(|got| got.eq_ignore_ascii_case(p)),
        }
    }
}

/// The table row for a monitor, if any. Exposed so the verification runbook
/// and the docs generator can show the evidence behind a disabled control.
pub fn row_for(key: &MonitorKey) -> Option<&'static ModelQuirks> {
    QUIRKS.iter().find(|row| key.matches(row))
}

/// Resolve quirks for a monitor. Vendor opcodes survive only if their
/// evidence does.
pub fn quirks_for_key(key: &MonitorKey) -> Quirks {
    let Some(row) = row_for(key) else { return Quirks::default() };
    Quirks {
        write_delay_ms: row.write_delay_ms,
        black_equalizer: row
            .black_equalizer
            .and_then(|c| VerifiedCode::attest(c.code, &c.evidence)),
        response: row.response.and_then(|r| {
            VerifiedCode::attest(r.code, &r.evidence)
                .map(|code| ResponseQuirk { code, levels: r.levels })
        }),
    }
}

/// Look up quirks by stable monitor id alone.
pub fn quirks_for(monitor_id: &str) -> Quirks {
    MonitorKey::from_id(monitor_id).map(|k| quirks_for_key(&k)).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// One concrete write the profile asks for, produced by [`plan_writes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpWrite {
    pub code: u8,
    pub value: u32,
}

/// A profile field that cannot be applied on this monitor, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub field: &'static str,
    pub reason: UnsupportedReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// The monitor's capability string does not advertise the opcode.
    NotAdvertised,
    /// No verified vendor opcode is known for this model. Covers both "no
    /// table row" and "a row exists but its evidence is `Unverified`" — from
    /// the user's side those are the same thing: the control stays off.
    NoKnownOpcode,
}

/// Desired monitor-side values in profile units. Kept separate from the
/// core's `MonitorSettings` so this crate stays core-independent; the adapter
/// converts. `None` = leave as-is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorPlanInput {
    pub brightness: Option<u16>,
    pub contrast: Option<u16>,
    pub black_equalizer: Option<u16>,
    pub response: Option<String>,
    pub sharpness: Option<u16>,
}

/// Which vendor controls this monitor can actually offer, for the UI.
/// `response` is empty when the control is unavailable, otherwise it lists the
/// level names the verified map covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorControls {
    pub black_equalizer: bool,
    pub response: Vec<&'static str>,
}

/// What the UI should enable for one monitor: a control needs both a verified
/// opcode and — when the panel answered a capability request — that opcode in
/// the advertised list.
pub fn vendor_controls(quirks: &Quirks, advertised: Option<&[u8]>) -> VendorControls {
    let advertises = |code: u8| advertised.map(|c| c.contains(&code)).unwrap_or(true);
    VendorControls {
        black_equalizer: quirks.black_equalizer.is_some_and(|c| advertises(c.get())),
        response: match &quirks.response {
            Some(r) if advertises(r.code.get()) => r.levels.iter().map(|(n, _)| *n).collect(),
            _ => Vec::new(),
        },
    }
}

/// Map profile fields to concrete VCP writes for one monitor.
///
/// `advertised`: the opcode list from the capability probe; `None` means the
/// monitor never answered a capability request — standard codes are then
/// attempted anyway (many panels support 0x10/0x12 while failing the
/// capabilities string), but vendor codes are not.
pub fn plan_writes(
    input: &MonitorPlanInput,
    advertised: Option<&[u8]>,
    quirks: &Quirks,
) -> (Vec<VcpWrite>, Vec<Unsupported>) {
    let mut writes = Vec::new();
    let mut unsupported = Vec::new();
    let advertises = |code: u8| advertised.map(|codes| codes.contains(&code)).unwrap_or(true);

    let mut standard = |field: &'static str, code: u8, value: Option<u16>| {
        let Some(v) = value else { return };
        if advertises(code) {
            writes.push(VcpWrite { code, value: v as u32 });
        } else {
            unsupported.push(Unsupported { field, reason: UnsupportedReason::NotAdvertised });
        }
    };
    standard("brightness", BRIGHTNESS, input.brightness);
    standard("contrast", CONTRAST, input.contrast);
    standard("sharpness", SHARPNESS, input.sharpness);

    if let Some(v) = input.black_equalizer {
        match quirks.black_equalizer {
            Some(code) if advertises(code.get()) => {
                writes.push(VcpWrite { code: code.get(), value: v as u32 })
            }
            Some(_) => unsupported.push(Unsupported {
                field: "black_equalizer",
                reason: UnsupportedReason::NotAdvertised,
            }),
            None => unsupported.push(Unsupported {
                field: "black_equalizer",
                reason: UnsupportedReason::NoKnownOpcode,
            }),
        }
    }
    if let Some(level) = &input.response {
        match &quirks.response {
            Some(r) if advertises(r.code.get()) => {
                match r.levels.iter().find(|(name, _)| name.eq_ignore_ascii_case(level)) {
                    Some((_, value)) => writes.push(VcpWrite { code: r.code.get(), value: *value }),
                    None => unsupported.push(Unsupported {
                        field: "response",
                        reason: UnsupportedReason::NoKnownOpcode,
                    }),
                }
            }
            Some(_) => unsupported
                .push(Unsupported { field: "response", reason: UnsupportedReason::NotAdvertised }),
            None => unsupported
                .push(Unsupported { field: "response", reason: UnsupportedReason::NoKnownOpcode }),
        }
    }

    (writes, unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LG_ID: &str = "mon:GSM5C7C:402NTCZ9E219";
    /// The LG ULTRAGEAR+'s advertised codes from the live M1 capture, trimmed
    /// to the ones these tests care about.
    const LG_CODES: &[u8] = &[0x02, 0x10, 0x12, 0x60, 0x62, 0xF5, 0xF6];

    fn input() -> MonitorPlanInput {
        MonitorPlanInput {
            brightness: Some(70),
            contrast: Some(55),
            black_equalizer: Some(12),
            response: Some("fast".into()),
            sharpness: Some(6),
        }
    }

    /// A verified row, built the only way one can be built.
    fn verified_quirks() -> Quirks {
        const DOC: Evidence =
            Evidence::VendorDoc { title: "test fixture", url: "https://example.invalid" };
        Quirks {
            write_delay_ms: 60,
            black_equalizer: VerifiedCode::attest(0xF6, &DOC),
            response: VerifiedCode::attest(0xF5, &DOC)
                .map(|code| ResponseQuirk { code, levels: &[("normal", 1), ("fast", 2)] }),
        }
    }

    // -- the no-guessing guarantee ------------------------------------------

    #[test]
    fn unverified_evidence_cannot_produce_a_writable_code() {
        let e = Evidence::Unverified { note: "looks about right" };
        assert!(VerifiedCode::attest(0xF6, &e).is_none());
        assert!(!e.is_verified());
    }

    /// The headline guarantee: the LG row *has* candidate opcodes for both
    /// vendor controls, the panel *advertises* both, and the profile asks for
    /// both — and still nothing is written and no slider is enabled, because
    /// the evidence is `Unverified`.
    #[test]
    fn unverified_table_entry_does_not_enable_the_slider() {
        let row = row_for(&MonitorKey::from_id(LG_ID).unwrap()).unwrap();
        assert_eq!(row.black_equalizer.map(|c| c.code), Some(0xF6));
        assert_eq!(row.response.map(|r| r.code), Some(0xF5));
        assert!(LG_CODES.contains(&0xF5) && LG_CODES.contains(&0xF6));

        let quirks = quirks_for(LG_ID);
        assert_eq!(quirks.black_equalizer, None);
        assert_eq!(quirks.response, None);

        let controls = vendor_controls(&quirks, Some(LG_CODES));
        assert!(!controls.black_equalizer, "unverified opcode must not enable the slider");
        assert!(controls.response.is_empty());

        let (writes, unsupported) = plan_writes(&input(), Some(LG_CODES), &quirks);
        assert!(
            writes.iter().all(|w| w.code < 0xE0),
            "no vendor-reserved code may be written: {writes:?}"
        );
        for field in ["black_equalizer", "response"] {
            let u = unsupported.iter().find(|u| u.field == field).expect(field);
            assert_eq!(u.reason, UnsupportedReason::NoKnownOpcode);
        }
    }

    /// Every unverified row must be inert, not just the LG one.
    #[test]
    fn no_unverified_row_ever_resolves_to_a_code() {
        for row in QUIRKS {
            let key = MonitorKey { pnp: row.pnp, product: row.product, name: None };
            let q = quirks_for_key(&key);
            if !row.black_equalizer.is_some_and(|c| c.evidence.is_verified()) {
                assert_eq!(q.black_equalizer, None, "{} {:?}", row.pnp, row.product);
            }
            if !row.response.is_some_and(|r| r.evidence.is_verified()) {
                assert_eq!(q.response, None, "{} {:?}", row.pnp, row.product);
            }
        }
    }

    /// A vendor-wide row (`product: None`) matches panels nobody has ever
    /// looked at, so it may only carry timing.
    #[test]
    fn vendor_wide_rows_carry_no_vendor_opcodes() {
        for row in QUIRKS.iter().filter(|r| r.product.is_none()) {
            assert!(row.black_equalizer.is_none() && row.response.is_none(), "{}", row.pnp);
        }
    }

    /// Verified evidence must name who saw it and when, so a wrong entry can
    /// be traced back to an observation rather than to a hunch.
    #[test]
    fn verified_entries_carry_traceable_evidence() {
        let all = QUIRKS
            .iter()
            .flat_map(|r| [r.black_equalizer.map(|c| c.evidence), r.response.map(|r| r.evidence)])
            .flatten();
        for e in all {
            match e {
                Evidence::VendorDoc { title, url } => {
                    assert!(!title.is_empty() && url.starts_with("http"), "{e:?}")
                }
                Evidence::Osd { observer, date, monitor } => {
                    assert!(!observer.is_empty() && !monitor.is_empty(), "{e:?}");
                    assert_eq!(date.len(), 10, "ISO-8601 date wanted, got {date:?}");
                }
                Evidence::Unverified { note } => assert!(!note.is_empty()),
            }
        }
    }

    /// Rows are searched in order, so a vendor-wide row must never shadow a
    /// model row for the same manufacturer.
    #[test]
    fn model_rows_sort_before_their_vendor_wide_row() {
        for (i, row) in QUIRKS.iter().enumerate() {
            if row.product.is_none() {
                assert!(
                    QUIRKS[i..].iter().all(|later| later.pnp != row.pnp || later.product.is_none()),
                    "{} model row is shadowed by the vendor-wide row",
                    row.pnp
                );
            }
        }
    }

    // -- matching -----------------------------------------------------------

    #[test]
    fn key_splits_pnp_and_product() {
        let k = MonitorKey::from_id(LG_ID).unwrap();
        assert_eq!((k.pnp, k.product), ("GSM", Some("5C7C")));
        assert_eq!(MonitorKey::from_id("mon:path:x1234abcd"), None);
        assert_eq!(MonitorKey::from_id("garbage"), None);
        // Short/odd product codes still identify the manufacturer.
        assert_eq!(MonitorKey::from_id("mon:DEL:abc").unwrap().product, None);
    }

    #[test]
    fn model_row_beats_the_vendor_wide_row() {
        let lg = row_for(&MonitorKey::from_id(LG_ID).unwrap()).unwrap();
        assert_eq!(lg.product, Some("5C7C"));
        let other_lg = row_for(&MonitorKey::from_id("mon:GSM7654:311NDX55X942").unwrap()).unwrap();
        assert_eq!(other_lg.product, None, "unknown LG falls back to the timing-only row");
        assert!(other_lg.black_equalizer.is_none());
    }

    #[test]
    fn quirks_lookup_is_keyed_on_pnp_and_product() {
        assert_eq!(quirks_for(LG_ID).write_delay_ms, 60);
        assert_eq!(quirks_for("mon:GSM7654:311NDX55X942").write_delay_ms, 60);
        assert_eq!(quirks_for("mon:DEL4099:XYZ").write_delay_ms, 50);
        assert_eq!(quirks_for("mon:path:x1234abcd"), Quirks::default());
        assert_eq!(quirks_for("garbage"), Quirks::default());
    }

    // -- planning -----------------------------------------------------------

    #[test]
    fn standard_codes_map_and_unverified_vendor_codes_report_unsupported() {
        let (writes, unsupported) = plan_writes(&input(), Some(LG_CODES), &quirks_for(LG_ID));
        assert_eq!(
            writes,
            vec![VcpWrite { code: BRIGHTNESS, value: 70 }, VcpWrite { code: CONTRAST, value: 55 }],
            "sharpness not advertised on this panel; vendor codes unverified"
        );
        let fields: Vec<_> = unsupported.iter().map(|u| u.field).collect();
        assert_eq!(fields, vec!["sharpness", "black_equalizer", "response"]);
        assert_eq!(unsupported[0].reason, UnsupportedReason::NotAdvertised);
        assert_eq!(unsupported[1].reason, UnsupportedReason::NoKnownOpcode);
    }

    #[test]
    fn no_capability_string_still_attempts_standard_codes_only() {
        let (writes, unsupported) = plan_writes(&input(), None, &Quirks::default());
        let codes: Vec<_> = writes.iter().map(|w| w.code).collect();
        assert_eq!(codes, vec![BRIGHTNESS, CONTRAST, SHARPNESS]);
        assert!(unsupported.iter().all(|u| u.reason == UnsupportedReason::NoKnownOpcode));
    }

    #[test]
    fn empty_input_plans_nothing() {
        let (writes, unsupported) =
            plan_writes(&MonitorPlanInput::default(), Some(LG_CODES), &Quirks::default());
        assert!(writes.is_empty());
        assert!(unsupported.is_empty());
    }

    #[test]
    fn verified_vendor_quirk_maps_named_levels() {
        let quirks = verified_quirks();
        let advertised = &[BRIGHTNESS, 0xF5, 0xF6][..];
        let (writes, unsupported) = plan_writes(&input(), Some(advertised), &quirks);
        assert!(writes.contains(&VcpWrite { code: 0xF6, value: 12 }));
        assert!(writes.contains(&VcpWrite { code: 0xF5, value: 2 }));
        // contrast/sharpness not advertised in this synthetic list.
        assert_eq!(
            unsupported.iter().filter(|u| u.reason == UnsupportedReason::NotAdvertised).count(),
            2
        );
        let controls = vendor_controls(&quirks, Some(advertised));
        assert!(controls.black_equalizer);
        assert_eq!(controls.response, vec!["normal", "fast"]);
    }

    #[test]
    fn verified_but_unadvertised_opcode_stays_off() {
        let controls = vendor_controls(&verified_quirks(), Some(&[BRIGHTNESS, CONTRAST]));
        assert_eq!(controls, VendorControls::default());
    }

    #[test]
    fn a_level_outside_the_verified_map_is_never_written() {
        let mut input = input();
        input.response = Some("ultra".into());
        let (writes, unsupported) =
            plan_writes(&input, Some(&[BRIGHTNESS, 0xF5, 0xF6]), &verified_quirks());
        assert!(writes.iter().all(|w| w.code != 0xF5));
        assert!(unsupported
            .iter()
            .any(|u| u.field == "response" && u.reason == UnsupportedReason::NoKnownOpcode));
    }
}
