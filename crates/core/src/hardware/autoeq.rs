//! Importer for the AutoEQ results CSV format (oratory1990 / crinacle
//! measurements live in that repo). Input is a local file's contents or
//! pasted text — the core never fetches anything over the network.
//!
//! Accepted shapes:
//! - the full results CSV (`frequency,raw,smoothed,error,…`): the `frequency`
//!   and `raw` columns are used, everything else ignored;
//! - a bare two-column `hz,db` list with or without a header.

use anyhow::{bail, Result};

/// Hard cap on imported points; the full AutoEQ grid is ~700.
const MAX_POINTS: usize = 4096;

/// Parse measured-curve text into ascending `(hz, db)` points.
pub fn parse_curve(text: &str) -> Result<Vec<(f32, f32)>> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else { bail!("empty curve") };

    // Header? Find the `frequency` and `raw` columns; default to 0 and 1.
    let (fi, ri, first_is_data) = {
        let cols: Vec<&str> = first.split(',').map(str::trim).collect();
        if cols.iter().any(|c| c.parse::<f32>().is_err()) {
            let find = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
            let fi = find("frequency").or_else(|| find("freq")).or_else(|| find("hz"));
            let ri = find("raw").or_else(|| find("db")).or_else(|| find("gain"));
            match (fi, ri) {
                (Some(f), Some(r)) => (f, r, false),
                _ => bail!("header has no frequency/raw columns: {first:?}"),
            }
        } else {
            (0, 1, true)
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
        let db = cell(ri)?;
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
    fn imports_the_real_autoeq_results_csv() {
        let curve = parse_curve(HD560S).unwrap();
        assert_eq!(curve.len(), 695);
        // First data row: 20.00,-4.11,…  — raw column, not any other.
        assert_eq!(curve[0], (20.0, -4.11));
        assert!(curve.last().unwrap().0 > 19_000.0);
        assert!(curve.windows(2).all(|w| w[0].0 < w[1].0));
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
