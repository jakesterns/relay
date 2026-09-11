//! EDID parsing and the [`MonitorId`] scheme.
//!
//! The id is a pure function of the EDID base block — manufacturer PNP id,
//! product code, and serial — and deliberately excludes everything positional
//! (device path, adapter, output index, GDI name). Two facts make that the
//! right key: the EDID travels with the panel, so the id survives reboots,
//! cable swaps and port changes; and the serial disambiguates two units of the
//! same model. Panels that ship no serial at all fall back to a hash of the
//! full base block — still stable per unit-as-flashed, though two identical
//! serial-less units would collide (accepted for v1; profiles then treat them
//! as the same monitor, which is also what the user usually wants).

use crate::types::MonitorId;

/// Parsed fields of an EDID base block (128 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edid {
    /// Three-letter PNP manufacturer id, e.g. "GSM" (LG).
    pub manufacturer: String,
    /// Little-endian product code from bytes 10..12.
    pub product: u16,
    /// 32-bit serial from bytes 12..16 (0 = not provided).
    pub serial32: u32,
    /// Serial *string* from an 0xFF display descriptor, if present.
    pub serial_text: Option<String>,
    /// Display name from an 0xFC descriptor, e.g. "LG ULTRAGEAR+".
    pub name: Option<String>,
    /// Native resolution from the preferred (first) detailed timing.
    pub native: Option<(u32, u32)>,
}

/// Parse an EDID base block. `None` if the header magic is wrong or the block
/// is short. Extension blocks are ignored: identity lives in the base block.
pub fn parse(bytes: &[u8]) -> Option<Edid> {
    const MAGIC: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if bytes.len() < 128 || bytes[..8] != MAGIC {
        return None;
    }

    // Manufacturer: three 5-bit letters packed big-endian into bytes 8..10.
    let m = u16::from_be_bytes([bytes[8], bytes[9]]);
    let letter = |shift: u16| ((m >> shift) & 0x1F) as u8;
    let manufacturer: String = [letter(10), letter(5), letter(0)]
        .iter()
        .map(|&c| if (1..=26).contains(&c) { (b'A' + c - 1) as char } else { '?' })
        .collect();

    let product = u16::from_le_bytes([bytes[10], bytes[11]]);
    let serial32 = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);

    let mut serial_text = None;
    let mut name = None;
    let mut native = None;
    for i in 0..4 {
        let d = &bytes[54 + i * 18..54 + (i + 1) * 18];
        if d[0] != 0 || d[1] != 0 {
            // Detailed timing descriptor (pixel clock != 0). The first one is
            // the preferred/native mode.
            if native.is_none() {
                let h = d[2] as u32 | (((d[4] >> 4) as u32) << 8);
                let v = d[5] as u32 | (((d[7] >> 4) as u32) << 8);
                if h > 0 && v > 0 {
                    native = Some((h, v));
                }
            }
            continue;
        }
        let text = || {
            let s: String = d[5..18]
                .iter()
                .take_while(|&&c| c != 0x0A)
                .map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' })
                .collect();
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        match d[3] {
            0xFF if serial_text.is_none() => serial_text = text(),
            0xFC if name.is_none() => name = text(),
            _ => {}
        }
    }

    Some(Edid { manufacturer, product, serial32, serial_text, name, native })
}

/// Stable monitor id: `mon:<PNP><PRODUCT-hex>:<serial>`.
///
/// Serial preference: the serial-string descriptor (what vendors print on the
/// label), else the 32-bit serial, else `x` + a hash of the whole base block.
pub fn monitor_id(edid: &Edid, raw: &[u8]) -> MonitorId {
    let serial = match (&edid.serial_text, edid.serial32) {
        (Some(s), _) => s.clone(),
        (None, n) if n != 0 => n.to_string(),
        _ => format!("x{:08x}", fnv1a(raw)),
    };
    MonitorId(format!("mon:{}{:04X}:{}", edid.manufacturer, edid.product, serial))
}

/// FNV-1a, enough to fingerprint a 128-byte block without a new dependency.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real EDID from the dev PC's LG ULTRAGEAR+ (registry dump, see plan).
    const GSM5C7C: &[u8] = include_bytes!("../../tests/fixtures/edid-gsm5c7c.bin");

    #[test]
    fn parses_the_real_lg_block() {
        let e = parse(GSM5C7C).unwrap();
        assert_eq!(e.manufacturer, "GSM");
        assert_eq!(e.product, 0x5C7C);
        assert_eq!(e.serial_text.as_deref(), Some("402NTCZ9E219"));
        assert_eq!(e.name.as_deref(), Some("LG ULTRAGEAR+"));
        let (w, h) = e.native.unwrap();
        assert!(w >= 1920 && h >= 1080, "native {w}x{h}");
    }

    #[test]
    fn id_is_a_pure_function_of_the_edid() {
        // Identity must not involve where the monitor is plugged in: deriving
        // the id twice from the same block — as if enumerated via different
        // device paths / ports / adapters — gives the same key.
        let e = parse(GSM5C7C).unwrap();
        let id_port_a = monitor_id(&e, GSM5C7C);
        let id_port_b = monitor_id(&parse(GSM5C7C).unwrap(), GSM5C7C);
        assert_eq!(id_port_a, id_port_b);
        assert_eq!(id_port_a.0, "mon:GSM5C7C:402NTCZ9E219");
    }

    #[test]
    fn serial_fallbacks_stay_stable_and_distinct() {
        let mut no_text = GSM5C7C.to_vec();
        // Blank the 0xFF descriptor tag so only the 32-bit serial remains.
        for i in 0..4 {
            if no_text[54 + i * 18] == 0 && no_text[54 + i * 18 + 3] == 0xFF {
                no_text[54 + i * 18 + 3] = 0x10; // dummy descriptor
            }
        }
        let e = parse(&no_text).unwrap();
        assert!(e.serial_text.is_none());
        let id = monitor_id(&e, &no_text);
        assert_eq!(id.0, format!("mon:GSM5C7C:{}", e.serial32));

        // No serial at all → hash fallback, still deterministic.
        let mut no_serial = no_text.clone();
        no_serial[12..16].fill(0);
        let e2 = parse(&no_serial).unwrap();
        let id_a = monitor_id(&e2, &no_serial);
        let id_b = monitor_id(&e2, &no_serial);
        assert_eq!(id_a, id_b);
        assert!(id_a.0.starts_with("mon:GSM5C7C:x"));
        assert_ne!(id_a, id);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 128]).is_none());
        assert!(parse(&GSM5C7C[..64]).is_none());
    }
}
