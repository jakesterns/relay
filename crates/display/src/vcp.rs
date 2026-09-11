//! VCP opcodes, per-model quirks and the pure profile-field → opcode mapping.
//!
//! Standard opcodes come from MCCS 2.2. Vendor features (black equaliser,
//! response time) live at vendor-reserved opcodes that differ per model; the
//! quirks table records the ones that have been *verified on real hardware*.
//! An unmapped field is skipped and reported as unsupported — never guessed —
//! because writing an unknown vendor opcode can change something invisible in
//! the OSD that we would still faithfully back up and restore, but the user
//! never asked for.

/// MCCS standard: luminance ("brightness").
pub const BRIGHTNESS: u8 = 0x10;
/// MCCS standard: contrast.
pub const CONTRAST: u8 = 0x12;
/// MCCS standard: sharpness.
pub const SHARPNESS: u8 = 0x87;
/// MCCS standard: input select. Never written by Relay; listed because the
/// capability parser and tests reference it.
pub const INPUT_SELECT: u8 = 0x60;

/// How to drive one monitor model over DDC/CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quirks {
    /// Pause after every `SetVCPFeature` — cheap monitors drop back-to-back
    /// writes on the floor.
    pub write_delay_ms: u64,
    /// Verified vendor opcode for "black equaliser" / "black stabilizer".
    pub black_equalizer: Option<u8>,
    /// Verified vendor opcode for the response-time / overdrive setting, plus
    /// the value each named level maps to.
    pub response: Option<ResponseQuirk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseQuirk {
    pub code: u8,
    /// (profile name, wire value), e.g. ("fast", 2).
    pub levels: &'static [(&'static str, u32)],
}

impl Default for Quirks {
    fn default() -> Self {
        Self { write_delay_ms: 50, black_equalizer: None, response: None }
    }
}

/// Look up quirks by the monitor id's PNP prefix (`mon:GSM…` → `GSM`).
///
/// No LG vendor opcodes are listed yet: candidates exist in the captured
/// capability string (0xF4–0xFF region) but none has been verified against
/// the OSD on real hardware. Verification runbook lives in the M2 plan.
pub fn quirks_for(monitor_id: &str) -> Quirks {
    match pnp_of(monitor_id) {
        // LG: writes are reliable but slow to settle.
        Some("GSM") => Quirks { write_delay_ms: 60, ..Quirks::default() },
        _ => Quirks::default(),
    }
}

/// `mon:GSM5C7C:402NTCZ9E219` → `GSM`.
fn pnp_of(monitor_id: &str) -> Option<&str> {
    let rest = monitor_id.strip_prefix("mon:")?;
    rest.get(0..3).filter(|s| s.chars().all(|c| c.is_ascii_uppercase()))
}

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
    /// No verified vendor opcode is known for this model.
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
            Some(code) if advertises(code) => writes.push(VcpWrite { code, value: v as u32 }),
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
            Some(r) if advertises(r.code) => {
                match r.levels.iter().find(|(name, _)| name.eq_ignore_ascii_case(level)) {
                    Some((_, value)) => writes.push(VcpWrite { code: r.code, value: *value }),
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

    /// The LG ULTRAGEAR+'s advertised codes from the live M1 capture.
    const LG_CODES: &[u8] = &[0x02, 0x10, 0x12, 0x60, 0x62, 0xF6];

    fn input() -> MonitorPlanInput {
        MonitorPlanInput {
            brightness: Some(70),
            contrast: Some(55),
            black_equalizer: Some(12),
            response: Some("fast".into()),
            sharpness: Some(6),
        }
    }

    #[test]
    fn standard_codes_map_and_vendor_codes_report_unsupported() {
        let (writes, unsupported) =
            plan_writes(&input(), Some(LG_CODES), &quirks_for("mon:GSM5C7C:402NTCZ9E219"));
        assert_eq!(
            writes,
            vec![VcpWrite { code: BRIGHTNESS, value: 70 }, VcpWrite { code: CONTRAST, value: 55 },],
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
        let quirks = Quirks {
            black_equalizer: Some(0xF6),
            response: Some(ResponseQuirk { code: 0xF5, levels: &[("normal", 1), ("fast", 2)] }),
            ..Quirks::default()
        };
        let advertised = &[BRIGHTNESS, 0xF5, 0xF6][..];
        let (writes, unsupported) = plan_writes(&input(), Some(advertised), &quirks);
        assert!(writes.contains(&VcpWrite { code: 0xF6, value: 12 }));
        assert!(writes.contains(&VcpWrite { code: 0xF5, value: 2 }));
        // contrast/sharpness not advertised in this synthetic list.
        assert_eq!(
            unsupported.iter().filter(|u| u.reason == UnsupportedReason::NotAdvertised).count(),
            2
        );
    }

    #[test]
    fn quirks_lookup_is_keyed_on_pnp_prefix() {
        assert_eq!(quirks_for("mon:GSM5C7C:402NTCZ9E219").write_delay_ms, 60);
        assert_eq!(quirks_for("mon:DEL4099:XYZ").write_delay_ms, 50);
        assert_eq!(quirks_for("mon:path:x1234abcd"), Quirks::default());
        assert_eq!(quirks_for("garbage"), Quirks::default());
    }
}
