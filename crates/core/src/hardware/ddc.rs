//! MCCS capabilities-string parsing. The monitor answers
//! `CapabilitiesRequestAndCapabilitiesReply` with a lisp-ish string like
//! `(prot(monitor)type(lcd)model(27GP850)cmds(01 02 03)vcp(02 10 12 60(0F 11 12) F0)…)`;
//! we extract the top-level `vcp(…)` group and return the supported VCP
//! opcodes. Values in nested parens are the allowed *settings* of the
//! preceding code, not codes, and are skipped.

/// Parse the `vcp(...)` group out of a raw capabilities string.
/// Returns a sorted, de-duplicated opcode list; empty if there is no group.
pub fn parse_vcp_codes(caps: &str) -> Vec<u8> {
    let Some(group) = group_body(caps, "vcp") else { return Vec::new() };
    let mut codes = Vec::new();
    let mut depth = 0usize;
    for token in tokens(group) {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = depth.saturating_sub(1),
            Token::Word(w) if depth == 0 => {
                if let Ok(code) = u8::from_str_radix(w, 16) {
                    codes.push(code);
                }
            }
            Token::Word(_) => {}
        }
    }
    codes.sort_unstable();
    codes.dedup();
    codes
}

/// The `model(...)` body, if present — a nicer display name than EDID gives.
pub fn parse_model(caps: &str) -> Option<String> {
    group_body(caps, "model").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Body of the top-level group `name(...)`, balanced-paren aware.
fn group_body<'a>(caps: &'a str, name: &str) -> Option<&'a str> {
    let bytes = caps.as_bytes();
    let mut i = 0;
    while let Some(off) = caps[i..].find(name) {
        let start = i + off;
        let after = start + name.len();
        // Must be a group: preceded by nothing/space/paren, followed by '('.
        let prev_ok = start == 0 || matches!(bytes[start - 1], b'(' | b')' | b' ');
        if prev_ok && bytes.get(after) == Some(&b'(') {
            let mut depth = 0usize;
            for (j, &b) in bytes.iter().enumerate().skip(after) {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&caps[after + 1..j]);
                        }
                    }
                    _ => {}
                }
            }
            return None; // unbalanced
        }
        i = after;
    }
    None
}

enum Token<'a> {
    Open,
    Close,
    Word(&'a str),
}

fn tokens(s: &str) -> impl Iterator<Item = Token<'_>> {
    let mut out = Vec::new();
    let mut word_start = None;
    for (i, c) in s.char_indices() {
        match c {
            '(' | ')' | ' ' | '\t' | '\r' | '\n' => {
                if let Some(ws) = word_start.take() {
                    out.push(Token::Word(&s[ws..i]));
                }
                match c {
                    '(' => out.push(Token::Open),
                    ')' => out.push(Token::Close),
                    _ => {}
                }
            }
            _ => {
                if word_start.is_none() {
                    word_start = Some(i);
                }
            }
        }
    }
    if let Some(ws) = word_start {
        out.push(Token::Word(&s[ws..]));
    }
    out.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative of what LG gaming monitors return (structure matches the
    /// MCCS spec; a live-captured string is added to the plan during the
    /// hardware pass).
    const LG_STYLE: &str = "(prot(monitor)type(lcd)model(27GP850)cmds(01 02 03 0C E3 F3)\
vcp(02 04 05 08 10 12 14(05 08 0B) 16 18 1A 52 60(0F 10 11 12) AC AE B2 B6 C0 C6 C8 C9 CA CC(01 02 03) D6(01 04 05) DF E4 E5 E6 E7 E8 E9 EA EB EF F0(00 01) FD)\
mswhql(1)asset_eep(40)mccs_ver(2.1))";

    /// Captured live from the dev PC's LG ULTRAGEAR+ over DDC/CI
    /// (`live_ddc_caps`, 2026-09-10) — note the trailing spaces inside nested
    /// value lists and codes listed out of numeric order.
    const LG_ULTRAGEAR_LIVE: &str = "(prot(monitor)type(lcd)model(WK95U)cmds(01 02 03 0C E3 F3)\
vcp(02 04 05 08 10 12 14(05 08 0B ) 16 18 1A 52 60(11 12 0F 10 ) AC AE B2 B6 C0 C6 C8 C9 D6(01 04) \
DF 62 8D F4 F5(01 02 03 04) F6(00 01 02) 4D 4E 4F 15(01 06 11 13 14 15 18 19 20 22 23 24 28 29 32 48) \
F7(00 01 02 03) F8(00 01) F9 E4 E5 E6 E7 E8 E9 EA EB EF FA(00 01) FD(00 01) FE(00 01 02) FF)\
mccs_ver(2.1)mswhql(1))";

    #[test]
    fn parses_the_live_lg_capabilities_string() {
        let codes = parse_vcp_codes(LG_ULTRAGEAR_LIVE);
        assert_eq!(codes.len(), 47);
        // Brightness, contrast, input select, volume must be advertised.
        for want in [0x10, 0x12, 0x60, 0x62] {
            assert!(codes.contains(&want), "missing {want:02X}");
        }
        // Values of 15(…)/60(…) are settings, not codes.
        for not in [0x01, 0x06, 0x0F, 0x11, 0x13] {
            assert!(!codes.contains(&not), "leaked nested value {not:02X}");
        }
        assert_eq!(parse_model(LG_ULTRAGEAR_LIVE).as_deref(), Some("WK95U"));
    }

    #[test]
    fn extracts_top_level_vcp_codes_only() {
        let codes = parse_vcp_codes(LG_STYLE);
        for want in [0x02, 0x10, 0x12, 0x60, 0xF0, 0xFD] {
            assert!(codes.contains(&want), "missing {want:02X}");
        }
        // Nested values are settings, not codes: 0F/11 only appear inside 60(…).
        assert!(!codes.contains(&0x0F));
        assert!(!codes.contains(&0x11));
        // Sorted + deduplicated.
        assert!(codes.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn model_and_missing_groups() {
        assert_eq!(parse_model(LG_STYLE).as_deref(), Some("27GP850"));
        assert!(parse_vcp_codes("(prot(monitor)type(lcd))").is_empty());
        assert!(parse_vcp_codes("").is_empty());
        assert!(parse_vcp_codes("garbage no parens").is_empty());
        // `vcp` appearing as a value must not be mistaken for the group.
        assert!(parse_vcp_codes("(model(vcp)cmds(01))").is_empty());
    }

    #[test]
    fn survives_unbalanced_parens() {
        assert!(parse_vcp_codes("(vcp(10 12").is_empty());
        assert_eq!(parse_vcp_codes("(vcp(10 12)"), vec![0x10, 0x12]);
    }
}
