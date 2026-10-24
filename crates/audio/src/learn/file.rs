//! The game-layer EQ as stored in a profile, and the versioned file format
//! users import and export. Spec: `docs/eq-file-format.md`.
//!
//! A file carries the game's identity and the game layer only — never the
//! headphone correction (that belongs to the listener's hardware, not the
//! game) and never anything about the person or PC that made it.

use serde::{Deserialize, Serialize};

use super::derive::Goal;
use super::dynamics::{guard_dynamics, DynamicsTuning};
use crate::params::{
    BandParams, DynBandParams, DynMode, DynamicsParams, FilterKind, MAX_DYN_BANDS,
};

pub const FORMAT: &str = "relay-game-eq";
/// The newest schema this Relay writes and reads. 2 (S49) adds `dynamics`;
/// a file without it is still written as 1, so older Relays read it.
pub const SCHEMA: u32 = 2;
/// The oldest schema still read.
pub const MIN_SCHEMA: u32 = 1;
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
    /// S49: the dynamic stage as learned or imported (before the player's
    /// tuning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamics: Option<DynamicsParams>,
    /// The player's adjustments to it; kept across relearns and new offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamics_tuning: Option<DynamicsTuning>,
}

impl GameEqLayer {
    /// A learned layer.
    pub fn learned(curve: Vec<(f32, f32)>, exe_version: Option<String>) -> Self {
        Self {
            curve,
            source: LayerSource::Learned,
            exe_version,
            note: String::new(),
            base: None,
            dynamics: None,
            dynamics_tuning: None,
        }
    }

    /// The same layer carrying `dynamics` (learned or imported).
    pub fn with_dynamics(mut self, dynamics: Option<DynamicsParams>) -> Self {
        self.dynamics = dynamics.map(|d| guard_dynamics(&d));
        self
    }

    /// The player's tuning, or the default (on, as learned).
    pub fn tuning(&self) -> DynamicsTuning {
        self.dynamics_tuning.unwrap_or_default().clamped()
    }

    /// What the dynamic stage runs with: the stored stage, tuned and
    /// guarded. `None` when there is none, it is off, or it is inert.
    pub fn effective_dynamics(&self) -> Option<DynamicsParams> {
        // A Gunshots strength or any part level works even where nothing
        // dynamic was learned.
        let empty = DynamicsParams { bands: Vec::new(), duck_lift: false };
        let t = self.tuning();
        let player_set =
            t.gun.is_some() || t.gun_db.is_some() || t.boom_db.is_some() || t.steps_db.is_some();
        let base = match &self.dynamics {
            Some(d) => d,
            None if player_set => &empty,
            None => return None,
        };
        super::dynamics::effective(base, &self.tuning())
    }

    /// Imported or tuned: the learned result blends into this one.
    pub fn is_imported(&self) -> bool {
        matches!(self.source, LayerSource::Imported | LayerSource::Tuned)
    }

    /// What taking a learned `candidate` would make of this layer: the
    /// candidate itself for a learned layer; for an imported one, the
    /// original import moved [`IMPORT_BLEND`] of the way towards it.
    /// Always guarded ([`super::derive::guard_curve`]).
    pub fn offer(&self, candidate: &[(f32, f32)]) -> Vec<(f32, f32)> {
        if !self.is_imported() {
            return super::derive::guard_curve(candidate);
        }
        let base = self.base.as_deref().unwrap_or(&self.curve);
        super::derive::guard_curve(&blend(base, candidate, IMPORT_BLEND))
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

/// One dynamic band in a file (S49): [`DynBandParams`], strict.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDynBand {
    pub mode: DynMode,
    pub freq_hz: f32,
    pub q: f32,
    pub threshold_db: f32,
    pub ratio: f32,
    pub range_db: f32,
    #[serde(default = "no_floor")]
    pub floor_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    #[serde(default = "on")]
    pub enabled: bool,
}

fn no_floor() -> f32 {
    -100.0
}
fn on() -> bool {
    true
}

impl From<&DynBandParams> for FileDynBand {
    fn from(b: &DynBandParams) -> Self {
        let r = |x: f32| (x * 100.0).round() / 100.0;
        Self {
            mode: b.mode,
            freq_hz: r(b.freq_hz),
            q: r(b.q),
            threshold_db: r(b.threshold_db),
            ratio: r(b.ratio),
            range_db: r(b.range_db),
            floor_db: r(b.floor_db),
            attack_ms: r(b.attack_ms),
            release_ms: r(b.release_ms),
            enabled: b.enabled,
        }
    }
}

impl From<&FileDynBand> for DynBandParams {
    fn from(b: &FileDynBand) -> Self {
        Self {
            mode: b.mode,
            freq_hz: b.freq_hz,
            q: b.q,
            threshold_db: b.threshold_db,
            ratio: b.ratio,
            range_db: b.range_db,
            floor_db: b.floor_db,
            attack_ms: b.attack_ms,
            release_ms: b.release_ms,
            enabled: b.enabled,
        }
    }
}

/// The dynamic stage in a file (S49): the stage as learned or imported, and
/// the creator's tuning on top, so whoever imports it hears what they heard
/// and can still re-tune from the same base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDynamics {
    pub bands: Vec<FileDynBand>,
    #[serde(default)]
    pub duck_lift: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuning: Option<DynamicsTuning>,
}

impl FileDynamics {
    fn params(&self) -> DynamicsParams {
        DynamicsParams {
            bands: self.bands.iter().map(Into::into).collect(),
            duck_lift: self.duck_lift,
        }
    }
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
    /// S49 (schema 2): the dynamic stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamics: Option<FileDynamics>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FileError {
    #[error("the file is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge,
    #[error("not a Relay game EQ file: {0}")]
    Parse(String),
    #[error("not a Relay game EQ file (format is {0:?})")]
    Format(String),
    #[error("made by a newer Relay (schema {0}); this one reads up to schema {SCHEMA}")]
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
        if !(MIN_SCHEMA..=SCHEMA).contains(&self.schema) {
            return Err(FileError::Schema(self.schema));
        }
        if self.dynamics.is_some() && self.schema < 2 {
            return Err(invalid("dynamics need schema 2"));
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
        if self.curve.is_none() && self.bands.is_none() && self.dynamics.is_none() {
            return Err(invalid("the file has no curve, bands or dynamics"));
        }
        if let Some(d) = &self.dynamics {
            // No bands is fine when the file carries the player's levels.
            if (d.bands.is_empty() && d.tuning.is_none()) || d.bands.len() > MAX_DYN_BANDS {
                return Err(invalid(format!("dynamics needs 1 to {MAX_DYN_BANDS} bands")));
            }
            if let Err(e) = d.params().check() {
                return Err(invalid(e));
            }
            if let Some(t) = d.tuning {
                if !(t.tame.is_finite() && t.lift.is_finite()) {
                    return Err(invalid("dynamics tuning is not numeric"));
                }
            }
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
    /// The file's curve as written (before the safety guard).
    fn raw_curve(&self) -> Vec<(f32, f32)> {
        match (&self.curve, &self.bands) {
            (Some(c), _) => c.clone(),
            (None, Some(b)) => sample_bands(b),
            (None, None) => Vec::new(),
        }
    }

    /// True when the import had to be changed to meet the safety guard
    /// (clamp-and-warn: the caller tells the user).
    pub fn adjusted(&self) -> bool {
        let curve = self.raw_curve();
        (!curve.is_empty() && !super::derive::is_guarded(&curve))
            || self.dynamics.as_ref().is_some_and(|d| {
                !super::dynamics::is_guarded(&d.params())
                    || d.tuning.is_some_and(|t| t.clamped() != t)
            })
    }

    /// The layer to store: the file's curve passed through
    /// [`super::derive::guard_curve`], so no file can boost the sub-bass, cut
    /// the speech band, step sharply or make the game louder.
    pub fn to_layer(&self) -> GameEqLayer {
        let raw = self.raw_curve();
        let curve = if raw.is_empty() { raw } else { super::derive::guard_curve(&raw) };
        GameEqLayer {
            base: Some(curve.clone()),
            curve,
            source: LayerSource::Imported,
            exe_version: self.game.version.clone(),
            note: self.note.clone(),
            dynamics: self.dynamics.as_ref().map(|d| guard_dynamics(&d.params())),
            dynamics_tuning: self.dynamics.as_ref().and_then(|d| d.tuning).map(|t| t.clamped()),
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
        let bands = if layer.curve.len() >= 2 {
            super::fit_game_layer(&layer.curve, super::GAME_BUDGET)
        } else {
            Vec::new()
        };
        let round = |x: f32| (x * 100.0).round() / 100.0;
        // The learned stage and the player's levels travel together; levels
        // alone (nothing learned) travel too.
        let learned = layer.dynamics.as_ref().filter(|d| !d.bands.is_empty());
        let tuning = layer.dynamics_tuning.map(|t| t.clamped());
        let dynamics = (learned.is_some() || tuning.is_some()).then(|| FileDynamics {
            bands: learned.map(|d| d.bands.iter().map(Into::into).collect()).unwrap_or_default(),
            duck_lift: learned.is_some_and(|d| d.duck_lift),
            tuning,
        });
        Self {
            format: FORMAT.into(),
            schema: if dynamics.is_some() { SCHEMA } else { 1 },
            game: GameIdentity {
                exe: exe.to_owned(),
                name: name.chars().take(MAX_NAME).collect(),
                version: layer.exe_version.clone(),
            },
            curve: (layer.curve.len() >= 2)
                .then(|| layer.curve.iter().map(|&(h, d)| (round(h), round(d))).collect()),
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
            dynamics,
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
            curve: crate::learn::derive::guard_curve(&[
                (20.0, -4.0),
                (100.0, -3.0),
                (1000.0, 0.0),
                (3150.0, 3.0),
                (16000.0, 0.0),
            ]),
            source: LayerSource::Learned,
            exe_version: Some("1.2.3.4".into()),
            note: String::new(),
            base: None,
            dynamics: None,
            dynamics_tuning: None,
        }
    }

    #[test]
    fn part_levels_travel_in_the_file_even_with_nothing_learned() {
        let layer = GameEqLayer {
            curve: vec![(20.0, -3.0), (2000.0, 2.4), (16000.0, -1.0)],
            source: LayerSource::Learned,
            exe_version: None,
            note: String::new(),
            base: None,
            dynamics: None,
            dynamics_tuning: Some(DynamicsTuning {
                gun_db: Some(-24.0),
                steps_db: Some(4.0),
                ..Default::default()
            }),
        };
        assert!(layer.effective_dynamics().is_some(), "levels alone run");
        let f = GameEqFile::export("cod.exe", "MW4", &layer, None, "");
        let text = serde_json::to_string(&f).unwrap();
        let back = GameEqFile::parse(&text).unwrap().to_layer();
        assert_eq!(back.tuning().gun_db, Some(-24.0));
        assert_eq!(back.tuning().steps_db, Some(4.0));
        assert_eq!(back.effective_dynamics(), layer.effective_dynamics());
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
        assert_eq!(rejects(&ok.replace("\"schema\":1", "\"schema\":3")), FileError::Schema(3));
        assert_eq!(rejects(&ok.replace("\"schema\":1", "\"schema\":0")), FileError::Schema(0));
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
        // Halfway from the import, then through the same safety guard.
        let want =
            crate::learn::derive::guard_curve(&blend(&layer().curve, &learned, IMPORT_BLEND));
        assert_eq!(offer, want);
        assert!(crate::learn::derive::is_guarded(&offer));
        let at = |c: &[(f32, f32)], hz: f32| c.iter().find(|p| p.0 == hz).unwrap().1;
        assert!(
            at(&offer, 3150.0) > at(&layer().curve, 3150.0) - 0.05,
            "moved towards the learned curve"
        );
        // Taking it makes a tuned layer that still blends from the import.
        let tuned =
            GameEqLayer { curve: offer.clone(), source: LayerSource::Tuned, ..imported.clone() };
        assert_eq!(tuned.offer(&learned), offer, "no drift on repeated offers");
        // A learned layer takes the candidate (guarded).
        let own = GameEqLayer::learned(layer().curve, None);
        assert_eq!(own.offer(&learned), crate::learn::derive::guard_curve(&learned));
    }

    #[test]
    fn a_hostile_import_is_clamped_and_flagged() {
        let text = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"h.exe"},
            "curve":[[20,6],[80,6],[1000,-9],[16000,6]]}"#;
        let f = GameEqFile::parse(text).unwrap();
        assert!(f.adjusted());
        let l = f.to_layer();
        assert!(crate::learn::derive::is_guarded(&l.curve));
        assert!(l.curve.iter().filter(|p| p.0 <= 80.0).all(|p| p.1 <= 0.0));
        assert!(l.curve.iter().filter(|p| (300.0..=4000.0).contains(&p.0)).all(|p| p.1 >= -2.05));
        let again = GameEqFile::export("h.exe", "", &l, None, "").to_json();
        assert!(!GameEqFile::parse(&again).unwrap().adjusted());
        // Non-finite numbers are still refused outright.
        assert!(GameEqFile::parse(&text.replace("[1000,-9]", "[1000,1e999]")).is_err());
    }

    fn stage() -> DynamicsParams {
        DynamicsParams {
            bands: vec![
                DynBandParams {
                    mode: DynMode::Tame,
                    freq_hz: 1000.0,
                    q: 0.6,
                    threshold_db: -44.0,
                    ratio: 6.0,
                    range_db: 9.0,
                    floor_db: -100.0,
                    attack_ms: 1.0,
                    release_ms: 120.0,
                    enabled: true,
                },
                DynBandParams {
                    mode: DynMode::Lift,
                    freq_hz: 4000.0,
                    q: 1.0,
                    threshold_db: -50.0,
                    ratio: 2.5,
                    range_db: 6.0,
                    floor_db: -58.0,
                    attack_ms: 3.0,
                    release_ms: 80.0,
                    enabled: true,
                },
            ],
            duck_lift: true,
        }
    }

    #[test]
    fn dynamics_and_tuning_round_trip_as_schema_2() {
        let tuning = DynamicsTuning {
            enabled: true,
            tame: 1.5,
            lift: 0.5,
            gun: None,
            gun_db: None,
            boom_db: None,
            steps_db: None,
        };
        let mut l = layer().with_dynamics(Some(stage()));
        l.dynamics_tuning = Some(tuning);
        let f = GameEqFile::export("g.exe", "G", &l, Some(Goal::Awareness), "louder steps");
        assert_eq!(f.schema, 2);
        let text = f.to_json();
        let back = GameEqFile::parse(&text).unwrap();
        assert_eq!(back, f);
        assert!(!back.adjusted());
        let imported = back.to_layer();
        assert_eq!(imported.dynamics.as_ref(), Some(&stage()));
        assert_eq!(imported.dynamics_tuning, Some(tuning));
        // The importer hears what the creator heard.
        assert_eq!(imported.effective_dynamics(), l.effective_dynamics());
        assert!(l.effective_dynamics().unwrap().bands[0].range_db > 9.0, "tame turned up");
    }

    #[test]
    fn a_layer_without_dynamics_still_exports_schema_1_for_older_relays() {
        let f = GameEqFile::export("g.exe", "", &layer(), None, "");
        assert_eq!(f.schema, 1);
        assert!(!f.to_json().contains("dynamics"));
        // A schema 1 file as an older Relay wrote it still imports.
        let v1 = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe","name":"G"},
            "curve":[[20,-2],[100,-2],[1000,0],[3150,2],[16000,0]],"goal":"awareness"}"#;
        let l = GameEqFile::parse(v1).unwrap().to_layer();
        assert!(l.dynamics.is_none() && l.effective_dynamics().is_none());
        // But dynamics under schema 1 are refused (an older reader would
        // reject the field; a file must say what it needs).
        let lying = v1.replace("\"goal\":\"awareness\"", "\"dynamics\":{\"bands\":[]}");
        assert!(matches!(GameEqFile::parse(&lying), Err(FileError::Invalid(_))));
    }

    #[test]
    fn a_dynamics_only_file_imports_with_no_curve() {
        let l = GameEqLayer::learned(Vec::new(), None).with_dynamics(Some(stage()));
        let f = GameEqFile::export("g.exe", "", &l, None, "");
        assert!(f.curve.is_none() && f.bands.is_none());
        let back = GameEqFile::parse(&f.to_json()).unwrap().to_layer();
        assert!(back.curve.is_empty());
        assert_eq!(back.dynamics, Some(stage()));
    }

    #[test]
    fn hostile_or_broken_dynamics_are_clamped_or_refused() {
        let base = r#"{"format":"relay-game-eq","schema":2,"game":{"exe":"g.exe"},
            "dynamics":{"bands":[BAND],"duck_lift":false TUNING}}"#;
        let band = r#"{"mode":"lift","freq_hz":3000,"q":1,"threshold_db":-20,"ratio":20,
            "range_db":24,"floor_db":-70,"attack_ms":3,"release_ms":80}"#;
        let text = base.replace("BAND", band).replace("TUNING", "");
        let f = GameEqFile::parse(&text).unwrap();
        assert!(f.adjusted(), "past the caps: clamp and say so");
        let d = f.to_layer().dynamics.unwrap();
        assert!(crate::learn::dynamics::is_guarded(&d));
        let b = d.bands[0];
        assert!(b.range_db <= crate::learn::dynamics::MAX_LIFT_DB);
        assert!(b.ratio <= crate::learn::dynamics::MAX_SAFE_RATIO);
        assert!(b.threshold_db <= crate::learn::dynamics::MAX_LIFT_THRESHOLD_DB);
        // Out-of-range tuning is clamped and flagged.
        let t = base.replace("BAND", band).replace("TUNING", r#","tuning":{"tame":9,"lift":1}"#);
        let f = GameEqFile::parse(&t).unwrap();
        assert!(f.adjusted());
        assert_eq!(
            f.to_layer().dynamics_tuning.unwrap().tame,
            crate::learn::dynamics::MAX_STRENGTH
        );
        // Refused outright: non-numeric, impossible, unknown fields, too many.
        for bad in [
            band.replace("\"q\":1", "\"q\":0"),
            band.replace("\"ratio\":20", "\"ratio\":0.5"),
            band.replace("\"floor_db\":-70", "\"floor_db\":0"),
            band.replace("\"mode\":\"lift\"", "\"mode\":\"boost\""),
            band.replace("\"q\":1", "\"q\":1,\"extra\":1"),
            band.replace("\"range_db\":24", "\"range_db\":1e999"),
        ] {
            let t = base.replace("BAND", &bad).replace("TUNING", "");
            assert!(GameEqFile::parse(&t).is_err(), "{bad}");
        }
        let sneaky =
            base.replace("BAND", band).replace("TUNING", r#","tuning":{"tame":1,"who":"me"}"#);
        assert!(GameEqFile::parse(&sneaky).is_err(), "unknown tuning field");
        let five = [band; 5].join(",");
        assert!(GameEqFile::parse(&base.replace("BAND", &five).replace("TUNING", "")).is_err());
        assert!(GameEqFile::parse(&base.replace("BAND", "").replace("TUNING", "")).is_err());
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
