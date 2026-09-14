//! The catalogue's network path, against the real source.
//!
//! `cargo test -p relay-core --test catalog_fetch -- --ignored --nocapture`
//!
//! Ignored by default: it is the only test that reaches the internet, and a
//! test suite that fails when GitHub is slow is a test suite people learn to
//! ignore. Run it when the index is regenerated or the fetch path changes.

#![cfg(windows)]

use relay_audio::fit;
use relay_core::audio_bridge::CORRECTION_BUDGET;
use relay_core::hardware::catalog;

fn cache() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("relay-catalog-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
#[ignore = "reaches the network; run by hand"]
fn a_searched_model_downloads_fits_and_caches() {
    let index = catalog::index_path().expect("the index must be staged beside the test binary");
    let hits = catalog::search(&index, "hd 560s", 10).expect("search");
    let entry = hits
        .iter()
        .find(|e| e.source == "oratory1990")
        .expect("oratory1990 measured the HD 560S")
        .clone();
    println!("picked: {} — {}", entry.name, entry.credit());
    println!("url:    {}", entry.curve_url());

    let dir = cache();
    let _ = std::fs::remove_file(entry.cache_path(&dir));

    // First call: over the network.
    let (points, fetched) = catalog::curve(&entry, &dir).expect("fetch the curve");
    assert!(fetched, "a cold cache must actually download");
    assert!(points.len() > 100, "a real measurement has hundreds of points, got {}", points.len());
    assert!(points.windows(2).all(|w| w[0].0 < w[1].0), "ascending frequencies");
    println!(
        "curve:  {} points, {:.0}..{:.0} Hz",
        points.len(),
        points[0].0,
        points[points.len() - 1].0
    );

    // Second call: from disk, no network.
    let (again, refetched) = catalog::curve(&entry, &dir).expect("cached read");
    assert!(!refetched, "the second call must not hit the network");
    assert_eq!(points, again, "the cached curve is the one that was downloaded");
    assert!(entry.cache_path(&dir).exists());

    // And it is usable: the fitter turns it into bands the chain can run.
    let f = fit::fit_curve(&points, CORRECTION_BUDGET);
    println!(
        "fit:    {} bands, max {:.2} dB, rms {:.2} dB",
        f.bands.len(),
        f.max_error_db,
        f.rms_error_db
    );
    assert!(!f.bands.is_empty());
    assert!(f.max_error_db < 4.0, "the real curve should fit within a few dB");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "reaches the network; run by hand"]
fn a_model_whose_name_has_brackets_also_downloads() {
    // A quarter of the catalogue has parentheses in the name, and they end up
    // in the URL. This is the case that broke the index build.
    let index = catalog::index_path().expect("index");
    let hits = catalog::search(&index, "anc off", 40).expect("search");
    let entry = hits.first().expect("at least one ANC variant").clone();
    println!("picked: {} — {}", entry.name, entry.curve_url());
    assert!(entry.name.contains('('), "expected a bracketed name, got {}", entry.name);

    let dir = cache();
    let (points, _) = catalog::curve(&entry, &dir).expect("fetch");
    assert!(points.len() > 100);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "reaches the network; run by hand"]
fn a_bad_path_reports_a_useful_error_rather_than_hanging() {
    let entry = catalog::CatalogEntry {
        name: "Nonexistent Headphone".into(),
        source: "oratory1990".into(),
        rig: String::new(),
        path: "oratory1990/over-ear/Definitely%20Not%20A%20Real%20Model".into(),
    };
    let err = catalog::curve(&entry, &cache()).expect_err("404 must be an error");
    let text = format!("{err:#}");
    assert!(text.contains("404"), "the message should name the status: {text}");
}
