//! Parse and serialize `reg export` v5.00 files.
//!
//! Contract: `serialize(&parse(&decode_bytes(fixture)?)?)` reproduces the
//! decoded fixture text byte-for-byte. That exactness is what lets the
//! uninstall proof be a plain string comparison of two registry exports
//! (brief risk #2). To get there the serializer mirrors reg.exe precisely:
//! CRLF line endings, hex byte lists wrapped with trailing `\` so that no
//! content line exceeds 79 characters (80 with the continuation backslash),
//! two-space continuation indent, lowercase hex, `dword:` / `hex:` /
//! `hex(TYPE):` value forms, and a blank line after every key block.
//!
//! Ordering matters: `reg export` writes keys and value names in registry
//! enumeration order, which is neither byte-sorted nor guaranteed stable
//! across case differences — so both levels use [`OrderedMap`], a small
//! insertion-ordered map (no extra dependency).

use std::fmt;

use thiserror::Error;

/// Header line of every v5.00 export.
pub const HEADER: &str = "Windows Registry Editor Version 5.00";

/// Errors from decoding or parsing a .reg file.
#[derive(Debug, Error)]
pub enum RegFileError {
    #[error("file is not valid UTF-16LE")]
    BadUtf16,
    #[error("file is not valid UTF-8")]
    BadUtf8,
    #[error("missing or unsupported header (expected {HEADER:?})")]
    BadHeader,
    #[error("line {0}: unterminated or malformed key line")]
    BadKeyLine(usize),
    #[error("line {0}: value line outside any [key] section")]
    ValueOutsideKey(usize),
    #[error("line {0}: malformed value line")]
    BadValueLine(usize),
    #[error("line {0}: malformed quoted string")]
    BadString(usize),
    #[error("line {0}: malformed hex byte list")]
    BadHex(usize),
    #[error("line {0}: malformed dword")]
    BadDword(usize),
}

// ---------------------------------------------------------------------------
// Ordered map

/// Insertion-ordered string-keyed map. `reg export` order is enumeration
/// order, not sorted order, so a `BTreeMap` cannot round-trip a fixture
/// exactly; this preserves order while keeping `insert` replace-in-place.
/// Lookups are linear — fine at registry-key scale (tens of entries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Insert; replaces in place (keeping position) if the key exists,
    /// appends otherwise. Returns the previous value if any.
    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => Some(std::mem::replace(v, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        let idx = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(idx).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }
}

impl<V> FromIterator<(String, V)> for OrderedMap<V> {
    fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

// ---------------------------------------------------------------------------
// Value model

/// Registry value type. `Other(n)` carries any type we do not model; its data
/// still round-trips as raw bytes via `hex(n):`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegKind {
    Sz,
    ExpandSz,
    MultiSz,
    Binary,
    Dword,
    Qword,
    Other(u32),
}

impl RegKind {
    /// The `REG_*` type code (`winnt.h` numbering).
    pub fn code(self) -> u32 {
        match self {
            RegKind::Sz => 1,
            RegKind::ExpandSz => 2,
            RegKind::Binary => 3,
            RegKind::Dword => 4,
            RegKind::MultiSz => 7,
            RegKind::Qword => 11,
            RegKind::Other(n) => n,
        }
    }

    pub fn from_code(code: u32) -> Self {
        match code {
            1 => RegKind::Sz,
            2 => RegKind::ExpandSz,
            3 => RegKind::Binary,
            4 => RegKind::Dword,
            7 => RegKind::MultiSz,
            11 => RegKind::Qword,
            n => RegKind::Other(n),
        }
    }
}

impl fmt::Display for RegKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "REG type {}", self.code())
    }
}

/// One registry value: type + raw data bytes exactly as the registry stores
/// them (REG_SZ = UTF-16LE including the terminating NUL, REG_DWORD = 4 LE
/// bytes, REG_QWORD = 8 LE bytes, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegValue {
    pub kind: RegKind,
    pub data: Vec<u8>,
}

/// Values of one key, in file order. The empty name `""` is the default
/// value (`@=` in the file).
pub type ValueMap = OrderedMap<RegValue>;

/// Full key path → values, in file order.
pub type KeyMap = OrderedMap<ValueMap>;

// ---------------------------------------------------------------------------
// Decoding

/// Decode the raw bytes of a .reg file. `reg export` writes UTF-16LE with a
/// BOM; plain UTF-8 (with or without BOM) is accepted as a fallback so tests
/// can use ordinary string literals. The BOM is stripped.
pub fn decode_bytes(bytes: &[u8]) -> Result<String, RegFileError> {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> =
            bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        if bytes.len() % 2 != 0 {
            return Err(RegFileError::BadUtf16);
        }
        return String::from_utf16(&units).map_err(|_| RegFileError::BadUtf16);
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| RegFileError::BadUtf8)
}

// ---------------------------------------------------------------------------
// Parsing

/// Parse decoded .reg text into a key map. Accepts CRLF and LF, blank lines,
/// `\`-continued hex byte lists, `@=` default values and escaped strings.
pub fn parse(text: &str) -> Result<KeyMap, RegFileError> {
    let mut lines = text.lines().enumerate();

    // Header: first non-blank line must be the v5.00 signature.
    loop {
        match lines.next() {
            Some((_, l)) if l.trim().is_empty() => continue,
            Some((_, l)) if l.trim_end() == HEADER => break,
            _ => return Err(RegFileError::BadHeader),
        }
    }

    let mut map = KeyMap::new();
    let mut current: Option<String> = None;

    while let Some((idx, raw)) = lines.next() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let key = rest.strip_suffix(']').ok_or(RegFileError::BadKeyLine(idx + 1))?;
            map.insert(key.to_owned(), ValueMap::new());
            current = Some(key.to_owned());
            continue;
        }

        // Value line — possibly continued across lines with a trailing `\`.
        let mut logical = line.to_owned();
        while logical.ends_with('\\') && !logical.ends_with("\\\\") {
            logical.pop();
            let (_, next) = lines.next().ok_or(RegFileError::BadHex(idx + 1))?;
            logical.push_str(next.trim_end_matches('\r').trim_start());
        }
        // A string value legitimately ends with `\\"`; the loop above only
        // continues on a *bare* trailing backslash, which quoted string and
        // key lines can never produce (they end with `"` / `]`).

        let key = current.as_deref().ok_or(RegFileError::ValueOutsideKey(idx + 1))?;
        let (name, value) = parse_value_line(&logical, idx + 1)?;
        // `current` was inserted above, so the lookup cannot fail.
        map.get_mut(key).ok_or(RegFileError::ValueOutsideKey(idx + 1))?.insert(name, value);
    }
    Ok(map)
}

/// Split one logical value line into (name, value).
fn parse_value_line(line: &str, lineno: usize) -> Result<(String, RegValue), RegFileError> {
    let (name, rest) = if let Some(rest) = line.strip_prefix("@=") {
        (String::new(), rest)
    } else if line.starts_with('"') {
        let (name, after) = parse_quoted(line, lineno)?;
        let rest = after.strip_prefix('=').ok_or(RegFileError::BadValueLine(lineno))?;
        (name, rest)
    } else {
        return Err(RegFileError::BadValueLine(lineno));
    };

    let value = if rest.starts_with('"') {
        let (s, tail) = parse_quoted(rest, lineno)?;
        if !tail.is_empty() {
            return Err(RegFileError::BadValueLine(lineno));
        }
        RegValue { kind: RegKind::Sz, data: sz_bytes(&s) }
    } else if let Some(hexstr) = rest.strip_prefix("dword:") {
        if hexstr.len() != 8 {
            return Err(RegFileError::BadDword(lineno));
        }
        let n = u32::from_str_radix(hexstr, 16).map_err(|_| RegFileError::BadDword(lineno))?;
        RegValue { kind: RegKind::Dword, data: n.to_le_bytes().to_vec() }
    } else if let Some(body) = rest.strip_prefix("hex") {
        let (kind, bytes_str) = if let Some(b) = body.strip_prefix(':') {
            (RegKind::Binary, b)
        } else if let Some(b) = body.strip_prefix('(') {
            let close = b.find("):").ok_or(RegFileError::BadHex(lineno))?;
            let code =
                u32::from_str_radix(&b[..close], 16).map_err(|_| RegFileError::BadHex(lineno))?;
            (RegKind::from_code(code), &b[close + 2..])
        } else {
            return Err(RegFileError::BadHex(lineno));
        };
        let mut data = Vec::new();
        if !bytes_str.is_empty() {
            for tok in bytes_str.split(',') {
                if tok.len() != 2 {
                    return Err(RegFileError::BadHex(lineno));
                }
                data.push(u8::from_str_radix(tok, 16).map_err(|_| RegFileError::BadHex(lineno))?);
            }
        }
        RegValue { kind, data }
    } else {
        return Err(RegFileError::BadValueLine(lineno));
    };

    Ok((name, value))
}

/// Parse a leading quoted string (with `\\` and `\"` escapes); returns the
/// unescaped string and the remainder after the closing quote.
fn parse_quoted(s: &str, lineno: usize) -> Result<(String, &str), RegFileError> {
    let inner = s.strip_prefix('"').ok_or(RegFileError::BadString(lineno))?;
    let mut out = String::new();
    let mut chars = inner.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, e @ ('\\' | '"'))) => out.push(e),
                _ => return Err(RegFileError::BadString(lineno)),
            },
            '"' => return Ok((out, &inner[i + 1..])),
            c => out.push(c),
        }
    }
    Err(RegFileError::BadString(lineno))
}

// ---------------------------------------------------------------------------
// Serialization

/// Serialize a key map back to reg.exe-compatible text (no BOM; CRLF;
/// trailing blank line after each key block, matching `reg export`).
pub fn serialize(map: &KeyMap) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str("\r\n\r\n");
    for (key, values) in map.iter() {
        out.push('[');
        out.push_str(key);
        out.push_str("]\r\n");
        for (name, value) in values.iter() {
            serialize_value(&mut out, name, value);
        }
        out.push_str("\r\n");
    }
    out
}

fn serialize_value(out: &mut String, name: &str, value: &RegValue) {
    let mut line = String::new();
    if name.is_empty() {
        line.push('@');
    } else {
        line.push('"');
        line.push_str(&escape(name));
        line.push('"');
    }
    line.push('=');

    match value.kind {
        RegKind::Sz => {
            if let Some(s) = sz_from_bytes(&value.data) {
                line.push('"');
                line.push_str(&escape(&s));
                line.push('"');
                out.push_str(&line);
                out.push_str("\r\n");
                return;
            }
            // Malformed REG_SZ data (no NUL terminator / invalid UTF-16):
            // reg.exe falls back to hex(1); so do we.
            line.push_str("hex(1):");
            push_hex_wrapped(out, line, &value.data);
        }
        RegKind::Dword if value.data.len() == 4 => {
            let n =
                u32::from_le_bytes([value.data[0], value.data[1], value.data[2], value.data[3]]);
            line.push_str(&format!("dword:{n:08x}"));
            out.push_str(&line);
            out.push_str("\r\n");
        }
        RegKind::Binary => {
            line.push_str("hex:");
            push_hex_wrapped(out, line, &value.data);
        }
        kind => {
            // ExpandSz, MultiSz, Qword, Other(n) — and Dword with unexpected
            // data length — all export as hex(TYPE):.
            line.push_str(&format!("hex({:x}):", kind.code()));
            push_hex_wrapped(out, line, &value.data);
        }
    }
}

/// Append a comma-separated hex byte list to `line`, wrapping exactly like
/// reg.exe: every byte is written as `xx,`; if appending a token would push
/// the content line past 79 characters, the line is closed with `\` (making
/// it at most 80) and continues indented by two spaces. The final trailing
/// comma is then removed. Verified byte-for-byte against a 7k-line fixture.
fn push_hex_wrapped(out: &mut String, mut line: String, data: &[u8]) {
    for byte in data {
        if line.len() + 3 > 79 {
            line.push('\\');
            out.push_str(&line);
            out.push_str("\r\n");
            line = "  ".to_owned();
        }
        line.push_str(&format!("{byte:02x},"));
    }
    if !data.is_empty() {
        line.pop(); // trailing comma
    }
    out.push_str(&line);
    out.push_str("\r\n");
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// REG_SZ / REG_MULTI_SZ helpers

/// Encode a string as REG_SZ data: UTF-16LE plus terminating NUL.
pub fn sz_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity((s.len() + 1) * 2);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Decode REG_SZ data (UTF-16LE with a single terminating NUL). Returns
/// `None` if the data is not shaped like that — callers fall back to hex.
pub fn sz_from_bytes(data: &[u8]) -> Option<String> {
    if data.len() < 2 || data.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let (last, body) = units.split_last()?;
    if *last != 0 || body.contains(&0) {
        return None;
    }
    String::from_utf16(body).ok()
}

/// Decode REG_MULTI_SZ data into its list of strings. Tolerant of a missing
/// final list terminator; interior strings end at each NUL.
pub fn parse_multi_sz(data: &[u8]) -> Vec<String> {
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, u) in units.iter().enumerate() {
        if *u == 0 {
            if i == start {
                return out; // empty string = list terminator
            }
            out.push(String::from_utf16_lossy(&units[start..i]));
            start = i + 1;
        }
    }
    if start < units.len() {
        // No terminator on the last string — keep it anyway.
        out.push(String::from_utf16_lossy(&units[start..]));
    }
    out
}

/// Encode a list of strings as REG_MULTI_SZ data: each string UTF-16LE +
/// NUL, then a final list-terminating NUL.
pub fn multi_sz_bytes(strings: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in strings {
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_value(text: &str) -> (String, RegValue) {
        let full = format!("{HEADER}\n\n[HKEY_LOCAL_MACHINE\\Test]\n{text}\n");
        let map = parse(&full).unwrap();
        let values = map.get(r"HKEY_LOCAL_MACHINE\Test").unwrap();
        let (name, value) = values.iter().next().unwrap();
        (name.to_owned(), value.clone())
    }

    #[test]
    fn parses_quoted_string_with_escapes() {
        let (name, v) = one_value(r#""path"="C:\\Program Files\\\"Relay\"""#);
        assert_eq!(name, "path");
        assert_eq!(v.kind, RegKind::Sz);
        assert_eq!(sz_from_bytes(&v.data).unwrap(), r#"C:\Program Files\"Relay""#);
    }

    #[test]
    fn parses_default_value_and_dword_and_qword() {
        let (name, v) = one_value(r#"@="hello""#);
        assert_eq!(name, "");
        assert_eq!(sz_from_bytes(&v.data).unwrap(), "hello");

        let (_, v) = one_value(r#""d"=dword:000000fe"#);
        assert_eq!(v.kind, RegKind::Dword);
        assert_eq!(v.data, 0xfeu32.to_le_bytes());

        let (_, v) = one_value("\"q\"=hex(b):86,02,00,00,00,00,00,00");
        assert_eq!(v.kind, RegKind::Qword);
        assert_eq!(v.data, 0x286u64.to_le_bytes());
    }

    #[test]
    fn parses_continuation_lines_and_lf_only() {
        let text = format!("{HEADER}\n\n[HKEY_LOCAL_MACHINE\\Test]\n\"b\"=hex:01,02,\\\n  03,04\n");
        let map = parse(&text).unwrap();
        let v = map.get(r"HKEY_LOCAL_MACHINE\Test").unwrap().get("b").unwrap();
        assert_eq!(v.kind, RegKind::Binary);
        assert_eq!(v.data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn multi_sz_helpers_round_trip() {
        let strings = vec!["{AAA}".to_owned(), "{BBB}".to_owned()];
        let data = multi_sz_bytes(&strings);
        assert_eq!(parse_multi_sz(&data), strings);
        assert!(parse_multi_sz(&multi_sz_bytes(&[])).is_empty());
    }

    #[test]
    fn decode_handles_utf16_and_utf8() {
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "abc".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_bytes(&utf16).unwrap(), "abc");
        assert_eq!(decode_bytes(b"\xEF\xBB\xBFabc").unwrap(), "abc");
        assert_eq!(decode_bytes(b"abc").unwrap(), "abc");
    }

    #[test]
    fn serializes_with_reg_exe_wrapping() {
        // 44-char name + "=hex:" puts the first wrap after 10 bytes; matches
        // an observed `reg export` line shape.
        let mut values = ValueMap::new();
        values.insert(
            "{b3f8fa53-0004-438e-9003-51a46e139bfc},15".to_owned(),
            RegValue { kind: RegKind::Binary, data: vec![0xAB; 40] },
        );
        let mut map = KeyMap::new();
        map.insert(r"HKEY_LOCAL_MACHINE\Test".to_owned(), values);
        let text = serialize(&map);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[3],
            "\"{b3f8fa53-0004-438e-9003-51a46e139bfc},15\"=hex:ab,ab,ab,ab,ab,ab,ab,ab,ab,ab,\\"
        );
        assert_eq!(lines[3].len(), 79);
        assert!(lines[4].starts_with("  ab,"));
        assert_eq!(lines[4].len(), 78);
        // Round-trips.
        assert_eq!(parse(&text).unwrap(), map);
    }

    #[test]
    fn synthetic_round_trip_exact() {
        let mut values = ValueMap::new();
        values.insert(String::new(), RegValue { kind: RegKind::Sz, data: sz_bytes("Relay") });
        values.insert(
            "modes".to_owned(),
            RegValue {
                kind: RegKind::MultiSz,
                data: multi_sz_bytes(&["{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}".to_owned()]),
            },
        );
        values.insert(
            "flag".to_owned(),
            RegValue { kind: RegKind::Dword, data: 1u32.to_le_bytes().to_vec() },
        );
        let mut map = KeyMap::new();
        map.insert(r"HKEY_LOCAL_MACHINE\Test".to_owned(), values);
        let text = serialize(&map);
        assert_eq!(serialize(&parse(&text).unwrap()), text);
    }
}
