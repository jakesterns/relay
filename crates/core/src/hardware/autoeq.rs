//! Importer for the AutoEQ results CSV format (oratory1990 / crinacle
//! measurements live in that repo). Input is a local file's contents or
//! pasted text — the core never fetches anything over the network.
//!
//! What is imported is the **correction to apply**, not the headset's
//! measured response. Those are different curves and confusing them inverts
//! the sound: `raw` is what the headset does, and flattening it would aim at
//! a flat response rather than at the Harman-style target the measurement was
//! scored against. AutoEQ already publishes the answer in `equalization`.
//!
//! Column preference, best first:
//! - `equalization` — AutoEQ's recommended correction. Exactly what we want.
//! - `error` — deviation from the target, so the correction is its negation.
//! - `raw` / `db` / `gain` — a bare curve the user supplied, taken as-is.
//!
//! Accepted shapes:
//! - the full results CSV (`frequency,raw,smoothed,error,…,equalization,…`);
//! - a bare two-column `hz,db` list with or without a header.

use anyhow::{bail, Result};

/// Hard cap on imported points; the full AutoEQ grid is ~700.
const MAX_POINTS: usize = 4096;

/// Parse measured-curve text into ascending `(hz, db)` points.
pub fn parse_curve(text: &str) -> Result<Vec<(f32, f32)>> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else { bail!("empty curve") };

    // Header? Find the frequency column and the best available correction
    // column. `negate` is set for `error`, whose sign is the deviation from
    // target rather than the fix for it.
    let (fi, ri, negate, first_is_data) = {
        let cols: Vec<&str> = first.split(',').map(str::trim).collect();
        if cols.iter().any(|c| c.parse::<f32>().is_err()) {
            let find = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
            let fi = find("frequency").or_else(|| find("freq")).or_else(|| find("hz"));
            let (ri, negate) = match find("equalization") {
                Some(i) => (Some(i), false),
                None => match find("error") {
                    Some(i) => (Some(i), true),
                    None => (find("raw").or_else(|| find("db")).or_else(|| find("gain")), false),
                },
            };
            match (fi, ri) {
                (Some(f), Some(r)) => (f, r, negate, false),
                _ => bail!("header has no frequency and correction columns: {first:?}"),
            }
        } else {
            (0, 1, false, true)
        }
    };

    let mut points = Vec::new();
    let data = first_is_data.then_some(first).into_iter().chain(lines);
    for (n, line) in data.enumerate() {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        let cell = |i: usize| -> Result<f32> {
            let raw =
                cols.get(i).ok_or_else(|| anyhow::anyhow!("line {}: missing column {i}", n + 1))?;
            raw.parse::<f32>().map_err(|_| anyhow::anyhow!("line {}: bad number {raw:?}", n + 1))
        };
        let hz = cell(fi)?;
        let db = if negate { -cell(ri)? } else { cell(ri)? };
        if !(1.0..=100_000.0).contains(&hz) {
            bail!("line {}: frequency {hz} Hz out of range", n + 1);
        }
        if !(-60.0..=60.0).contains(&db) {
            bail!("line {}: level {db} dB out of range", n + 1);
        }
        if let Some(&(prev, _)) = points.last() {
            if hz <= prev {
                bail!("line {}: frequencies must ascend ({prev} → {hz})", n + 1);
            }
        }
        points.push((hz, db));
        if points.len() > MAX_POINTS {
            bail!("more than {MAX_POINTS} points");
        }
    }
    if points.len() < 2 {
        bail!("need at least two points, got {}", points.len());
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real oratory1990 result for the Sennheiser HD 560S (see plan DoR).
    const HD560S: &str = include_str!("../../tests/fixtures/autoeq-hd560s.csv");

    #[test]
    fn imports_the_correction_column_from_the_real_autoeq_results_csv() {
        let curve = parse_curve(HD560S).unwrap();
        assert_eq!(curve.len(), 695);
        // First data row is
        //   frequency,raw,  smoothed,error,error_smoothed,equalization,…
        //   20.00,   -4.11, -4.12,   -6.86,-6.87,          6.00,…
        // so the imported value must be the +6.00 correction, not the -4.11
        // the headset measured. Getting this backwards would EQ towards a
        // flat response instead of the measurement's target.
        assert_eq!(curve[0], (20.0, 6.00));
        assert!(curve.last().unwrap().0 > 19_000.0);
        assert!(curve.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn error_column_is_negated_because_it_is_the_deviation_not_the_fix() {
        // No `equalization` column, so `error` is used and its sign flipped.
        let csv = "frequency,raw,error\n20,-4.11,-6.86\n1000,0,0.5\n20000,-2,1.0";
        let curve = parse_curve(csv).unwrap();
        assert_eq!(curve[0], (20.0, 6.86));
        assert_eq!(curve[1], (1000.0, -0.5));
    }

    #[test]
    fn equalization_wins_over_error_and_raw() {
        let csv = "frequency,raw,error,equalization\n20,1,2,3\n1000,1,2,4";
        assert_eq!(parse_curve(csv).unwrap()[0], (20.0, 3.0));
    }

    #[test]
    fn imports_bare_two_column_text_with_or_without_header() {
        let with = "hz,db\n20,-2.5\n1000,0\n20000,-8";
        let without = "20,-2.5\n1000,0\n20000,-8";
        assert_eq!(parse_curve(with).unwrap(), parse_curve(without).unwrap());
        assert_eq!(parse_curve(without).unwrap()[1], (1000.0, 0.0));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_curve("").is_err());
        assert!(parse_curve("just some prose").is_err());
        assert!(parse_curve("frequency,raw\n20,-2").is_err(), "one point is not a curve");
        assert!(parse_curve("20,-2\n10,-3").is_err(), "descending frequencies");
        assert!(parse_curve("20,-2\n20,-3").is_err(), "duplicate frequency");
        assert!(parse_curve("0.1,-2\n30,900").is_err(), "out of range");
    }
}
