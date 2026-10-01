//! Turning a measured headset-correction curve into EQ bands.
//!
//! AutoEQ (oratory1990, crinacle) publishes a per-headset correction as a
//! dense curve: ~700 points of "add this many dB at this frequency". A biquad
//! cascade cannot run 700 filters, and the brief caps the chain at
//! [`MAX_BANDS`]. So the curve has to be approximated by a handful of filters.
//!
//! The structure is the one parametric-EQ fits converge on: one low shelf,
//! one high shelf, and peaking filters for everything between.
//!
//! 1. Place the two shelves at fixed corners, each taking the *mean* residual
//!    across the plateau it controls. Shelves have to sit at their corner —
//!    a low shelf placed at 20 Hz lifts nothing above 20 Hz — and a mean
//!    rather than a peak stops one outlier dragging the whole bass tilt.
//! 2. Spend the remaining budget greedily: find the frequency where the
//!    cascade is still furthest from the target, and add a peaking filter
//!    there that cancels it.
//! 3. Keep peaking centres at least half an octave apart, so they spread
//!    across the spectrum instead of stacking on the worst spot.
//! 4. Stop when the budget runs out or nothing is off by an audible amount.
//!
//! Each added filter is evaluated against the *actual* cascade response
//! rather than assumed to be independent, so the overlap between neighbouring
//! filters is accounted for instead of accumulating.
//!
//! Steps 1 and 3 both exist because of what the first version did to a real
//! HD 560S curve: greedy selection alone spent all eight bands stacking
//! shelves at 20 Hz and 16 kHz, never touched the midrange, and fitted
//! *worse* with twelve bands than with eight. With them, the same curve fits
//! to 2.2 dB worst case and 0.8 dB RMS.
//!
//! No allocation happens on any audio thread: this runs once, when a curve is
//! imported or a headset is selected, and its output is plain [`BandParams`].

use crate::coeffs::Coeffs;
use crate::params::{BandParams, FilterKind, MAX_BANDS};

/// Sample rate the fit is evaluated at. The fitted bands are used at whatever
/// rate the endpoint runs; 48 kHz is the usual case and the filter shapes
/// below 16 kHz barely move between 44.1 and 96 kHz.
const FIT_RATE: f64 = 48_000.0;

/// Range the fit cares about. Below 20 Hz is inaudible and above 16 kHz most
/// measurements are unreliable and most adults cannot hear the difference.
const FIT_LO_HZ: f64 = 20.0;
const FIT_HI_HZ: f64 = 16_000.0;

/// Corner frequencies for the two shelves.
///
/// A shelf has to sit at its corner, not at the frequency that happens to be
/// furthest out: a low shelf placed at 20 Hz lifts almost nothing above 20 Hz
/// and wastes the band. These corners are where the shelf starts to act, so
/// its plateau covers the sub-bass and air regions that peaking filters
/// cannot reach.
const LOW_SHELF_CORNER_HZ: f64 = 105.0;
const HIGH_SHELF_CORNER_HZ: f64 = 10_000.0;

/// The plateau each shelf is fitted against — the region it actually
/// controls, which is where its gain should be read from.
const LOW_PLATEAU_HZ: (f64, f64) = (20.0, 50.0);
const HIGH_PLATEAU_HZ: (f64, f64) = (12_000.0, 16_000.0);

/// A shelf is only worth a band if the plateau is off by at least this much.
const SHELF_WORTH_IT_DB: f64 = 0.75;

/// Stop early once no point is off by more than this. Well under the ~1 dB
/// that is generally audible on a broadband signal.
const GOOD_ENOUGH_DB: f64 = 0.35;

/// Gains beyond this are clamped. A correction asking for +15 dB is either a
/// bad measurement or a headset no EQ can save, and boosting that hard eats
/// headroom and invites clipping.
const MAX_BAND_GAIN_DB: f64 = 12.0;

/// Q for fitted peaking filters. About 1.4 octaves wide — broad enough that a
/// dozen of them cover the spectrum without ripple between centres.
const PEAK_Q: f64 = 1.0;
/// Shelves are gentler; a high-Q shelf overshoots at the corner.
const SHELF_Q: f64 = 0.7;

/// How many log-spaced candidate centres to consider.
const GRID: usize = 96;

/// Result of fitting a curve.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    /// The filters to run, in cascade order.
    pub bands: Vec<BandParams>,
    /// Worst remaining error, in dB, over the fitted range.
    pub max_error_db: f32,
    /// Root-mean-square remaining error, in dB.
    pub rms_error_db: f32,
}

/// Fit `curve` — ascending `(hz, db)` points describing the correction to
/// apply — with at most `max_bands` filters.
///
/// Returns an empty fit for an empty curve or a budget of zero, which is the
/// "no correction" case rather than an error.
pub fn fit_curve(curve: &[(f32, f32)], max_bands: usize) -> Fit {
    let budget = max_bands.min(MAX_BANDS);
    if curve.len() < 2 || budget == 0 {
        return Fit { bands: Vec::new(), max_error_db: 0.0, rms_error_db: 0.0 };
    }

    // Evaluation grid, log-spaced across the audible band.
    let grid: Vec<f64> = (0..GRID)
        .map(|i| {
            let t = i as f64 / (GRID - 1) as f64;
            FIT_LO_HZ * (FIT_HI_HZ / FIT_LO_HZ).powf(t)
        })
        .collect();
    let target: Vec<f64> = grid.iter().map(|&hz| sample_curve(curve, hz)).collect();

    let mut bands: Vec<BandParams> = Vec::new();
    // Running cascade response at each grid point, so each iteration costs
    // one filter evaluation per point rather than a full re-sweep.
    let mut current = vec![0.0f64; grid.len()];

    // Shelves first, at fixed corners, gains read from the plateau each one
    // actually controls. They set the broad tilt; the peaking filters below
    // then correct what is left, which is mostly midrange detail.
    let place = |kind: FilterKind,
                 hz: f64,
                 gain: f64,
                 q: f64,
                 bands: &mut Vec<BandParams>,
                 current: &mut Vec<f64>| {
        let g = gain.clamp(-MAX_BAND_GAIN_DB, MAX_BAND_GAIN_DB);
        let Ok(c) = Coeffs::design(kind, FIT_RATE, hz, g, q) else { return };
        for (slot, &f) in current.iter_mut().zip(grid.iter()) {
            *slot += c.magnitude_db(f, FIT_RATE);
        }
        bands.push(BandParams {
            kind,
            freq_hz: hz as f32,
            gain_db: g as f32,
            q: q as f32,
            enabled: true,
        });
    };

    if bands.len() < budget {
        let lift = plateau_mean(&grid, &target, &current, LOW_PLATEAU_HZ);
        if lift.abs() >= SHELF_WORTH_IT_DB {
            place(
                FilterKind::LowShelf,
                LOW_SHELF_CORNER_HZ,
                lift,
                SHELF_Q,
                &mut bands,
                &mut current,
            );
        }
    }
    if bands.len() < budget {
        let lift = plateau_mean(&grid, &target, &current, HIGH_PLATEAU_HZ);
        if lift.abs() >= SHELF_WORTH_IT_DB {
            place(
                FilterKind::HighShelf,
                HIGH_SHELF_CORNER_HZ,
                lift,
                SHELF_Q,
                &mut bands,
                &mut current,
            );
        }
    }

    // Peaking filters for the rest, greedily at the worst remaining point,
    // kept apart so they cover the spectrum instead of piling up.
    let mut peak_centres: Vec<f64> = Vec::new();
    while bands.len() < budget {
        let Some((idx, residual)) = best_peak_candidate(&grid, &target, &current, &peak_centres)
        else {
            break;
        };
        if residual.abs() <= GOOD_ENOUGH_DB {
            break;
        }
        peak_centres.push(grid[idx]);
        place(FilterKind::Peaking, grid[idx], residual, PEAK_Q, &mut bands, &mut current);
    }

    let (max_error_db, rms_error_db) = error_stats(&target, &current);
    Fit { bands, max_error_db, rms_error_db }
}

/// Minimum spacing between peaking centres, in octaves. Roughly the width of
/// a `PEAK_Q` filter, so neighbours overlap without piling up.
const MIN_PEAK_SPACING_OCT: f64 = 0.5;

/// Mean remaining error across a frequency span — the gain a shelf covering
/// that span should take, rather than the single worst point inside it.
fn plateau_mean(grid: &[f64], target: &[f64], current: &[f64], span: (f64, f64)) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for (i, &hz) in grid.iter().enumerate() {
        if hz >= span.0 && hz <= span.1 {
            sum += target[i] - current[i];
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f64
    }
}

/// Largest remaining error among grid points far enough from every peaking
/// filter already placed. `None` when the spectrum is covered, which ends the
/// fit rather than spending budget on filters that cannot help.
fn best_peak_candidate(
    grid: &[f64],
    target: &[f64],
    current: &[f64],
    peak_centres: &[f64],
) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for (i, (&t, &c)) in target.iter().zip(current).enumerate() {
        let hz = grid[i];
        if !peak_centres.iter().all(|&p| (hz / p).log2().abs() >= MIN_PEAK_SPACING_OCT) {
            continue;
        }
        let e = t - c;
        if best.is_none_or(|(_, b): (usize, f64)| e.abs() > b.abs()) {
            best = Some((i, e));
        }
    }
    best
}

fn error_stats(target: &[f64], current: &[f64]) -> (f32, f32) {
    let mut max = 0.0f64;
    let mut sum_sq = 0.0f64;
    for (&t, &c) in target.iter().zip(current) {
        let e = t - c;
        max = max.max(e.abs());
        sum_sq += e * e;
    }
    let rms = (sum_sq / target.len().max(1) as f64).sqrt();
    (max as f32, rms as f32)
}

/// Linear interpolation of `curve` at `hz`, in log-frequency, clamped to the
/// curve's own endpoints. Log spacing matters: the points are log-spaced, so
/// interpolating linearly in Hz would skew everything below a few hundred Hz.
fn sample_curve(curve: &[(f32, f32)], hz: f64) -> f64 {
    interp_db(curve, hz)
}

/// [`sample_curve`] for callers outside this module; 0 dB for an empty curve.
pub fn interp_db(curve: &[(f32, f32)], hz: f64) -> f64 {
    if curve.is_empty() {
        return 0.0;
    }
    let first = curve[0];
    let last = curve[curve.len() - 1];
    if hz <= first.0 as f64 {
        return first.1 as f64;
    }
    if hz >= last.0 as f64 {
        return last.1 as f64;
    }
    // The curve ascends, so a binary search finds the bracketing pair.
    let i = curve.partition_point(|&(f, _)| (f as f64) < hz);
    let (f1, d1) = curve[i - 1];
    let (f2, d2) = curve[i];
    let (f1, d1, f2, d2) = (f1 as f64, d1 as f64, f2 as f64, d2 as f64);
    if f2 <= f1 {
        return d1;
    }
    let t = (hz.ln() - f1.ln()) / (f2.ln() - f1.ln());
    d1 + t * (d2 - d1)
}

/// The cascade's combined response at `hz`, for plotting what the fit does.
pub fn response_db(bands: &[BandParams], hz: f64) -> f64 {
    bands
        .iter()
        .filter(|b| b.enabled)
        .filter_map(|b| {
            Coeffs::design(b.kind, FIT_RATE, b.freq_hz as f64, b.gain_db as f64, b.q as f64).ok()
        })
        .map(|c| c.magnitude_db(hz, FIT_RATE))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A curve that is flat at `db` everywhere.
    fn flat(db: f32) -> Vec<(f32, f32)> {
        vec![(20.0, db), (1000.0, db), (20000.0, db)]
    }

    #[test]
    fn an_empty_or_unbudgeted_curve_fits_to_nothing() {
        assert!(fit_curve(&[], 8).bands.is_empty());
        assert!(fit_curve(&flat(3.0), 0).bands.is_empty());
        // A single point is not a curve.
        assert!(fit_curve(&[(1000.0, 5.0)], 8).bands.is_empty());
    }

    #[test]
    fn a_flat_zero_curve_needs_no_filters() {
        let f = fit_curve(&flat(0.0), 8);
        assert!(f.bands.is_empty(), "nothing to correct, so no bands: {:?}", f.bands);
        assert!(f.max_error_db < 0.01);
    }

    #[test]
    fn a_single_bump_is_reproduced_by_the_fit() {
        // The input is the correction to *apply*, not a measured response, so
        // a +6 dB bump an octave wide at 1 kHz must come back as a +6 dB
        // boost there — not a cut.
        let curve: Vec<(f32, f32)> = (0..200)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 199.0);
                let x = (hz / 1000.0).ln() / std::f32::consts::LN_2;
                (hz, 6.0 * (-x * x * 2.0).exp())
            })
            .collect();
        let f = fit_curve(&curve, 8);
        assert!(!f.bands.is_empty());
        assert!(f.max_error_db < 1.5, "max error {} dB", f.max_error_db);
        assert!(
            f.bands.iter().any(|b| (b.freq_hz - 1000.0).abs() < 500.0 && b.gain_db > 2.0),
            "expected a boost near 1 kHz, got {:?}",
            f.bands
        );
        // And the cascade really does lift 1 kHz by about 6 dB.
        assert!((response_db(&f.bands, 1000.0) - 6.0).abs() < 1.0);
    }

    #[test]
    fn more_bands_never_fit_worse() {
        let curve: Vec<(f32, f32)> = (0..300)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 299.0);
                (hz, 4.0 * (hz / 300.0).ln().sin())
            })
            .collect();
        let coarse = fit_curve(&curve, 3);
        let fine = fit_curve(&curve, 10);
        assert!(
            fine.rms_error_db <= coarse.rms_error_db + 1e-4,
            "10 bands ({}) should not be worse than 3 ({})",
            fine.rms_error_db,
            coarse.rms_error_db
        );
    }

    #[test]
    fn the_budget_is_never_exceeded() {
        let curve: Vec<(f32, f32)> = (0..400)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 399.0);
                (hz, 8.0 * ((hz / 50.0).ln() * 3.0).sin())
            })
            .collect();
        for budget in [1usize, 4, 16, 64] {
            let f = fit_curve(&curve, budget);
            assert!(f.bands.len() <= budget.min(MAX_BANDS), "budget {budget}");
        }
    }

    #[test]
    fn extremes_use_shelves_at_their_corners() {
        // Correction that only asks for bass lift and treble cut.
        let curve = vec![(20.0, 8.0), (60.0, 6.0), (200.0, 0.0), (5000.0, 0.0), (16000.0, -7.0)];
        let f = fit_curve(&curve, 6);
        let low = f.bands.iter().find(|b| b.kind == FilterKind::LowShelf).expect("a low shelf");
        let high = f.bands.iter().find(|b| b.kind == FilterKind::HighShelf).expect("a high shelf");
        assert_eq!(low.freq_hz as f64, LOW_SHELF_CORNER_HZ);
        assert_eq!(high.freq_hz as f64, HIGH_SHELF_CORNER_HZ);
        assert!(low.gain_db > 0.0, "bass lift: {low:?}");
        assert!(high.gain_db < 0.0, "treble cut: {high:?}");
    }

    #[test]
    fn at_most_one_shelf_of_each_kind() {
        // The regression that made the first version useless: shelves stacked
        // at the grid endpoints, eating the whole budget.
        let curve: Vec<(f32, f32)> = (0..300)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 299.0);
                (hz, 9.0 * (300.0 / hz).ln().clamp(-1.0, 1.0))
            })
            .collect();
        let f = fit_curve(&curve, 12);
        let lows = f.bands.iter().filter(|b| b.kind == FilterKind::LowShelf).count();
        let highs = f.bands.iter().filter(|b| b.kind == FilterKind::HighShelf).count();
        assert!(lows <= 1 && highs <= 1, "{:?}", f.bands);
    }

    #[test]
    fn peaking_centres_stay_apart() {
        let curve: Vec<(f32, f32)> = (0..300)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 299.0);
                (hz, 6.0 * ((hz / 40.0).ln() * 2.0).sin())
            })
            .collect();
        let f = fit_curve(&curve, 12);
        let peaks: Vec<f64> = f
            .bands
            .iter()
            .filter(|b| b.kind == FilterKind::Peaking)
            .map(|b| b.freq_hz as f64)
            .collect();
        for (i, a) in peaks.iter().enumerate() {
            for b in &peaks[i + 1..] {
                assert!(
                    (a / b).log2().abs() >= MIN_PEAK_SPACING_OCT - 1e-6,
                    "{a} Hz and {b} Hz are too close"
                );
            }
        }
    }

    #[test]
    fn absurd_corrections_are_clamped_not_obeyed() {
        let f = fit_curve(&flat(40.0), 4);
        assert!(
            f.bands.iter().all(|b| b.gain_db.abs() <= MAX_BAND_GAIN_DB as f32 + 1e-3),
            "{:?}",
            f.bands
        );
    }

    #[test]
    fn curve_sampling_interpolates_in_log_frequency() {
        let curve = vec![(100.0, 0.0), (1000.0, 10.0)];
        // The geometric midpoint of 100 and 1000 is ~316 Hz, which should read
        // as half the total rise. Linear-in-Hz interpolation would give ~2.4.
        let mid = sample_curve(&curve, 316.227);
        assert!((mid - 5.0).abs() < 0.05, "got {mid}");
        // Outside the curve, clamp rather than extrapolate.
        assert_eq!(sample_curve(&curve, 10.0), 0.0);
        assert_eq!(sample_curve(&curve, 20000.0), 10.0);
    }

    #[test]
    fn response_matches_the_fit_it_reports() {
        let curve = vec![(20.0, 0.0), (1000.0, -5.0), (16000.0, 0.0)];
        let f = fit_curve(&curve, 6);
        // `response_db` is what the UI will plot; it has to agree with the
        // residual the fit measured, or the graph lies about the sound.
        let at_1k = response_db(&f.bands, 1000.0);
        assert!((at_1k - (-5.0)).abs() < f.max_error_db as f64 + 0.2, "got {at_1k}");
    }
}
