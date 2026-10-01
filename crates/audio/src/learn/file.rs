//! The game-layer EQ as stored in a profile, and the versioned file format
//! users import and export. Spec: `docs/eq-file-format.md`.
//!
//! A file carries the game's identity and the game layer only — never the
//! headphone correction (that belongs to the listener's hardware, not the
//! game) and never anything about the person or PC that made it.

use serde::{Deserialize, Serialize};

use super::derive::Goal;
use crate::params::{BandParams, FilterKind};

pub const FORMAT: &str = "relay-game-eq";
pub const SCHEMA: u32 = 1;
/// Largest file accepted, bytes.
pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const MAX_CURVE_POINTS: usize = 64;
pub const MAX_FILE_BANDS: usize = 8;
pub const MIN_HZ: f32 = 20.0;
pub const MAX_HZ: f32 = 20_000.0;
/// Game-layer gain range. Boosts are capped lower than cuts: hearing first.
pub const MAX_BOOST_DB: f32 = super::derive::MAX_BOOST_DB;
pub const MAX_CUT_DB: f32 = super::derive::MAX_CUT_DB;
pub const MAX_NAME: usize = 128;
pub const MAX_VERSION: usize = 64;
pub const MAX_NOTE: usize = 500;
pub const MAX_EXE: usize = 128;
/// "Keep learning to fine-tune": an imported curve moves this far towards
/// the locally learned one (0 = imported as-is, 1 = learned).
pub const IMPORT_BLEND: f32 = 0.5;

/// Where a profile's game layer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LayerSource {
    #[default]
    Learned,
    Imported,
    /// Imported, then fine-tuned towards what Relay learned on this PC.
    Tuned,
}

/// The game layer a profile applies, stacked after the headset correction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEqLayer {
    /// Ascending `(hz, db)` points.
    pub curve: Vec<(f32, f32)>,
    #[serde(default)]
    pub source: LayerSource,
    /// The exe version it was learned on, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe_version: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// For an imported or tuned layer: the curve as imported, so fine-tuning
    /// always blends from the original rather than drifting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Vec<(f32, f32)>>,
}

impl GameEqLayer {
    /// A learned layer.
    pub fn learned(curve: Vec<(f32, f32)>, exe_version: Option<String>) -> Self {
        Self { curve, source: LayerSource::Learned, exe_version, note: String::new(), base: None }
    }

    /// Imported or tuned: the learned result blends into this one.
    pub fn is_imported(&self) -> bool {
        matches!(self.source, LayerSource::Imported | LayerSource::Tuned)
    }

    /// What taking a learned `candidate` would make of this layer: the
    /// candidate itself for a learned layer; for an imported one, the
    /// original import moved [`IMPORT_BLEND`] of the way towards it.
    pub fn offer(&self, candidate: &[(f32, f32)]) -> Vec<(f32, f32)> {
        if !self.is_imported() {
            return candidate.to_vec();
        }
        let base = self.base.as_deref().unwrap_or(&self.curve);
        blend(base, candidate, IMPORT_BLEND)
    }
}

/// `a + t (b - a)`, on `b`'s frequencies (with `a` interpolated in log
/// frequency). Rounded to 0.1 dB like learned curves.
pub fn blend(a: &[(f32, f32)], b: &[(f32, f32)], t: f32) -> Vec<(f32, f32)> {
    b.iter()
        .map(|&(hz, db)| {
            let base = crate::fit::interp_db(a, hz as f64) as f32;
            (hz, ((base + t * (db - base)) * 10.0).round() / 10.0)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameIdentity {
    /// Executable file name, e.g. `game.exe`. No directories.
    pub exe: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Game version the curve was made on, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBand {
    pub kind: FilterKind,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// The exchange file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameEqFile {
    pub format: String,
    pub schema: u32,
    pub game: GameIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<(f32, f32)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<FileBand>>,
    /// The goal the curve was made for, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Goal>,
    /// Creator's note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FileError {
    #[error("the file is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge,
    #[error("not a Relay game EQ file: {0}")]
    Parse(String),
    #[error("not a Relay game EQ file (format is {0:?})")]
    Format(String),
    #[error("made by a newer Relay (schema {0}); this one reads schema {SCHEMA}")]
    Schema(u32),
    #[error("{0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> FileError {
    FileError::Invalid(msg.into())
}

/// A plain exe file name: `name.exe`, no path, no control characters.
pub fn valid_exe(exe: &str) -> bool {
    !exe.is_empty()
        && exe.len() <= MAX_EXE
        && exe.to_ascii_lowercase().ends_with(".exe")
        && exe.len() > 4
        && !exe.chars().any(|c| matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'))
        && !exe.chars().any(char::is_control)
        && !exe.starts_with('.')
}

fn text_ok(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(|c| c.is_control() && c != '\n')
}

fn check_curve(curve: &[(f32, f32)]) -> Result<(), FileError> {
    if curve.len() < 2 || curve.len() > MAX_CURVE_POINTS {
        return Err(invalid(format!("the curve needs 2 to {MAX_CURVE_POINTS} points")));
    }
    let mut last = 0.0f32;
    for &(hz, db) in curve {
        if !hz.is_finite() || !db.is_finite() {
            return Err(invalid("the curve has a non-numeric point"));
        }
        if !(MIN_HZ..=MAX_HZ).contains(&hz) {
            return Err(invalid(format!("curve frequency {hz} Hz is outside 20 Hz to 20 kHz")));
        }
        if hz <= last {
            return Err(invalid("curve frequencies must ascend"));
        }
        if !(-MAX_CUT_DB..=MAX_BOOST_DB).contains(&db) {
            return Err(invalid(format!(
                "curve gain {db} dB is outside -{MAX_CUT_DB} to +{MAX_BOOST_DB} dB"
            )));
        }
        last = hz;
    }
    Ok(())
}

impl GameEqFile {
    /// Parse and validate. Nothing is trusted until this returns Ok.
    pub fn parse(text: &str) -> Result<Self, FileError> {
        if text.len() > MAX_FILE_BYTES {
            return Err(FileError::TooLarge);
        }
        let f: GameEqFile =
            serde_json::from_str(text).map_err(|e| FileError::Parse(e.to_string()))?;
        f.validate()?;
        Ok(f)
    }

    pub fn validate(&self) -> Result<(), FileError> {
        if self.format != FORMAT {
            return Err(FileError::Format(self.format.chars().take(40).collect()));
        }
        if self.schema != SCHEMA {
            return Err(FileError::Schema(self.schema));
        }
        if !valid_exe(&self.game.exe) {
            return Err(invalid("game.exe must be a plain .exe file name"));
        }
        if !text_ok(&self.game.name, MAX_NAME) {
            return Err(invalid("game.name is too long or has control characters"));
        }
        if let Some(v) = &self.game.version {
            if v.is_empty() || !text_ok(v, MAX_VERSION) || v.contains('\n') {
                return Err(invalid("game.version is empty, too long or malformed"));
            }
        }
        if !text_ok(&self.note, MAX_NOTE) {
            return Err(invalid("note is too long or has control characters"));
        }
        if self.curve.is_none() && self.bands.is_none() {
            return Err(invalid("the file has neither a curve nor bands"));
        }
        if let Some(c) = &self.curve {
            check_curve(c)?;
        }
        if let Some(bands) = &self.bands {
            if bands.is_empty() || bands.len() > MAX_FILE_BANDS {
                return Err(invalid(format!("bands needs 1 to {MAX_FILE_BANDS} entries")));
            }
            for b in bands {
                if !matches!(
                    b.kind,
                    FilterKind::Peaking | FilterKind::LowShelf | FilterKind::HighShelf
                ) {
                    return Err(invalid("bands may only be peaking or shelf filters"));
                }
                if !(b.freq_hz.is_finite() && (MIN_HZ..=MAX_HZ).contains(&b.freq_hz)) {
                    return Err(invalid("a band's frequency is outside 20 Hz to 20 kHz"));
                }
                if !(b.q.is_finite() && (0.1..=10.0).contains(&b.q)) {
                    return Err(invalid("a band's Q is outside 0.1 to 10"));
                }
                if !(b.gain_db.is_finite() && (-MAX_CUT_DB..=MAX_BOOST_DB).contains(&b.gain_db)) {
                    return Err(invalid("a band's gain is out of range"));
                }
            }
            // The combined response has to respect the same limits.
            check_curve(&sample_bands(bands))?;
        }
        Ok(())
    }

    /// The layer to store in a profile: the curve if present, else the
    /// bands' combined response sampled on 1/3 octaves.
    pub fn to_layer(&self) -> GameEqLayer {
        let curve = match (&self.curve, &self.bands) {
            (Some(c), _) => c.clone(),
            (None, Some(b)) => sample_bands(b),
            (None, None) => Vec::new(),
        };
        GameEqLayer {
            base: Some(curve.clone()),
            curve,
            source: LayerSource::Imported,
            exe_version: self.game.version.clone(),
            note: self.note.clone(),
        }
    }

    /// Build an export from a profile's layer. The fitted bands are included
    /// for other EQ tools; the curve is what Relay reads back.
    pub fn export(
        exe: &str,
        name: &str,
        layer: &GameEqLayer,
        goal: Option<Goal>,
        note: &str,
    ) -> Self {
        let bands = crate::fit::fit_curve(&layer.curve, super::GAME_BUDGET).bands;
        let round = |x: f32| (x * 100.0).round() / 100.0;
        Self {
            format: FORMAT.into(),
            schema: SCHEMA,
            game: GameIdentity {
                exe: exe.to_owned(),
                name: name.chars().take(MAX_NAME).collect(),
                version: layer.exe_version.clone(),
            },
            curve: Some(layer.curve.iter().map(|&(h, d)| (round(h), round(d))).collect()),
            bands: if bands.is_empty() {
                None
            } else {
                Some(
                    bands
                        .iter()
                        .map(|b| FileBand {
                            kind: b.kind,
                            freq_hz: round(b.freq_hz),
                            gain_db: round(b.gain_db),
                            q: round(b.q),
                        })
                        .collect(),
                )
            },
            goal,
            note: note.chars().take(MAX_NOTE).collect(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// A band set's response on the 1/3-octave grid (20 Hz – 16 kHz).
fn sample_bands(bands: &[FileBand]) -> Vec<(f32, f32)> {
    let params: Vec<BandParams> = bands
        .iter()
        .map(|b| BandParams {
            kind: b.kind,
            freq_hz: b.freq_hz,
            gain_db: b.gain_db,
            q: b.q,
            enabled: true,
        })
        .collect();
    let mut out = Vec::new();
    let mut hz = 20.0f64;
    while hz <= 16_000.0 {
        let db = crate::fit::response_db(&params, hz) as f32;
        out.push((hz as f32, (db * 100.0).round() / 100.0));
        hz *= 2f64.powf(1.0 / 3.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> GameEqLayer {
        GameEqLayer {
            curve: vec![(20.0, -4.0), (100.0, -3.0), (1000.0, 0.0), (3150.0, 3.0), (16000.0, 0.0)],
            source: LayerSource::Learned,
            exe_version: Some("1.2.3.4".into()),
            note: String::new(),
            base: None,
        }
    }

    #[test]
    fn export_import_round_trip() {
        let f = GameEqFile::export(
            "Game.exe",
            "Some Game",
            &layer(),
            Some(Goal::Dialogue),
            "quieter booms",
        );
        let text = f.to_json();
        let back = GameEqFile::parse(&text).unwrap();
        assert_eq!(back, f);
        let l = back.to_layer();
        assert_eq!(l.curve, layer().curve);
        assert_eq!(l.source, LayerSource::Imported);
        assert_eq!(l.exe_version.as_deref(), Some("1.2.3.4"));
        assert_eq!(l.note, "quieter booms");
        assert_eq!(back.goal, Some(Goal::Dialogue));
        assert_eq!(l.base.as_deref(), Some(&layer().curve[..]), "the import is kept as the base");
        // No hardware or personal fields exist to leak.
        assert!(!text.contains("headset") && !text.contains("correction"));
    }

    #[test]
    fn bands_only_files_become_a_curve() {
        let text = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe"},
            "bands":[{"kind":"peaking","freq_hz":3000,"gain_db":3,"q":1},
                     {"kind":"lowshelf","freq_hz":105,"gain_db":-4,"q":0.7}]}"#;
        let l = GameEqFile::parse(text).unwrap().to_layer();
        assert!(l.curve.len() > 20);
        let near = |hz: f32| {
            l.curve.iter().min_by(|a, b| (a.0 - hz).abs().total_cmp(&(b.0 - hz).abs())).unwrap().1
        };
        assert!(near(3000.0) > 2.0 && near(25.0) < -3.0);
    }

    fn rejects(text: &str) -> FileError {
        GameEqFile::parse(text).unwrap_err()
    }

    #[test]
    fn rejection_cases() {
        let ok = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe"},"curve":[[20,0],[1000,1]]}"#;
        assert!(GameEqFile::parse(ok).is_ok());

        assert_eq!(rejects(&" ".repeat(MAX_FILE_BYTES + 1)), FileError::TooLarge);
        assert!(matches!(rejects("not json"), FileError::Parse(_)));
        assert!(matches!(rejects(&ok.replace("relay-game-eq", "other")), FileError::Format(_)));
        assert_eq!(rejects(&ok.replace("\"schema\":1", "\"schema\":2")), FileError::Schema(2));
        // Unknown fields are refused, not ignored.
        assert!(matches!(
            rejects(&ok.replace("\"schema\":1", "\"schema\":1,\"extra\":1")),
            FileError::Parse(_)
        ));
        assert!(matches!(
            rejects(&ok.replace("{\"exe\":\"g.exe\"}", "{\"exe\":\"g.exe\",\"user\":\"x\"}")),
            FileError::Parse(_)
        ));
        // Identity.
        for exe in ["", "game", "..\\\\evil.exe", "C:/g.exe", ".exe"] {
            let t = ok.replace("g.exe", exe);
            assert!(matches!(rejects(&t), FileError::Invalid(_) | FileError::Parse(_)), "{exe}");
        }
        // Curve shape and ranges.
        for curve in [
            "[[20,0]]",
            "[[1000,0],[20,0]]",
            "[[10,0],[1000,0]]",
            "[[20,0],[30000,0]]",
            "[[20,0],[1000,9]]",
            "[[20,-9.5],[1000,0]]",
        ] {
            let t = ok.replace("[[20,0],[1000,1]]", curve);
            assert!(matches!(rejects(&t), FileError::Invalid(_)), "{curve}");
        }
        // Neither curve nor bands.
        assert!(matches!(
            rejects(r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe"}}"#),
            FileError::Invalid(_)
        ));
        // Bad bands.
        for band in [
            r#"{"kind":"lowpass","freq_hz":100,"gain_db":0,"q":1}"#,
            r#"{"kind":"peaking","freq_hz":5,"gain_db":0,"q":1}"#,
            r#"{"kind":"peaking","freq_hz":100,"gain_db":0,"q":50}"#,
            r#"{"kind":"peaking","freq_hz":100,"gain_db":20,"q":1}"#,
        ] {
            let t = format!(
                r#"{{"format":"relay-game-eq","schema":1,"game":{{"exe":"g.exe"}},"bands":[{band}]}}"#
            );
            assert!(matches!(rejects(&t), FileError::Invalid(_)), "{band}");
        }
        // Stacked boosts that sum past the limit.
        let stacked = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe"},"bands":[
            {"kind":"peaking","freq_hz":3000,"gain_db":5,"q":1},{"kind":"peaking","freq_hz":3000,"gain_db":5,"q":1}]}"#;
        assert!(matches!(rejects(stacked), FileError::Invalid(_)));
        // Long note.
        let long = ok.replace(
            "\"schema\":1",
            &format!("\"schema\":1,\"note\":\"{}\"", "x".repeat(MAX_NOTE + 1)),
        );
        assert!(matches!(rejects(&long), FileError::Invalid(_)));
    }

    #[test]
    fn an_import_applies_as_is_and_fine_tuning_blends_from_the_original() {
        let text = GameEqFile::export("g.exe", "", &layer(), None, "").to_json();
        let imported = GameEqFile::parse(&text).unwrap().to_layer();
        assert!(imported.is_imported());
        assert_eq!(imported.curve, layer().curve, "applied exactly as imported");
        // A learned curve 4 dB above the import everywhere: the offer moves
        // halfway (IMPORT_BLEND), from the original import.
        let learned: Vec<(f32, f32)> = layer().curve.iter().map(|&(hz, _)| (hz, 0.0)).collect();
        let learned: Vec<(f32, f32)> = learned
            .iter()
            .map(|&(hz, _)| (hz, crate::fit::interp_db(&layer().curve, hz as f64) as f32 + 4.0))
            .collect();
        let offer = imported.offer(&learned);
        for (&(hz, o), &(_, l)) in offer.iter().zip(learned.iter()) {
            assert!((o - (l - 4.0 * (1.0 - IMPORT_BLEND))).abs() < 0.11, "{hz} Hz: {o} vs {l}");
        }
        // Taking it makes a tuned layer that still blends from the import.
        let tuned =
            GameEqLayer { curve: offer.clone(), source: LayerSource::Tuned, ..imported.clone() };
        assert_eq!(tuned.offer(&learned), offer, "no drift on repeated offers");
        // A learned layer just takes the candidate.
        let own = GameEqLayer::learned(layer().curve, None);
        assert_eq!(own.offer(&learned), learned);
    }

    #[test]
    fn export_carries_whatever_layer_is_applied() {
        for src in [LayerSource::Learned, LayerSource::Imported, LayerSource::Tuned] {
            let l = GameEqLayer { source: src, ..layer() };
            let f = GameEqFile::export("g.exe", "G", &l, None, "");
            assert_eq!(f.curve.as_deref(), Some(&l.curve[..]));
            assert!(GameEqFile::parse(&f.to_json()).is_ok());
        }
    }
}
