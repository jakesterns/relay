//! Synthetic frame sequences only. Nothing here touches a screen or a GPU.

use super::analyse::*;
use super::converge::*;
use super::derive::*;

const W: usize = 480;
const H: usize = 270;

/// Deterministic pseudo-random, so frames vary without a rand dependency.
struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn frame(mut f: impl FnMut(usize, usize) -> (u8, u8, u8)) -> Vec<u8> {
    let mut v = Vec::with_capacity(W * H * 3);
    for y in 0..H {
        for x in 0..W {
            let (r, g, b) = f(x, y);
            v.extend_from_slice(&[r, g, b]);
        }
    }
    v
}

fn run(an: &mut Analyser, data: &[u8]) -> FrameReport {
    an.analyse(&Frame { width: W, height: H, data, order: Order::Rgb })
}

/// Dark scene: 40 % of the frame is shadow with faint texture at codes 2..10
/// (detail present but crushed), the rest a mid-dark, coloured world that
/// moves with `t`.
fn dark_crushed(t: usize) -> Vec<u8> {
    let mut rng = Lcg(t as u32 + 7);
    frame(|x, y| {
        let n = rng.next();
        if y < H * 2 / 5 {
            let v = 2 + (n * 8.0) as u8;
            (v, v, v)
        } else {
            let base = 50 + ((x + t * 13) % 90) as u8;
            (base / 2, base, base / 3)
        }
    })
}

/// Bright daylight scene, saturated, moving.
fn bright(t: usize) -> Vec<u8> {
    frame(|x, y| {
        let v = 200 + ((x * 3 + y + t * 17) % 56) as u8;
        (v, (v as u16 * 4 / 5) as u8, 60)
    })
}

/// Washed out: mid-grey with a faint tint, moving.
fn washed(t: usize) -> Vec<u8> {
    frame(|x, y| {
        let v = 90 + ((x + y * 2 + t * 11) % 60) as u8;
        (v + 10, v + 5, v)
    })
}

/// A cutscene: 12 % black bars top and bottom around a moving picture.
fn letterboxed(t: usize) -> Vec<u8> {
    let bar = H * 12 / 100;
    frame(|x, y| {
        if y < bar || y >= H - bar {
            (0, 0, 0)
        } else {
            let v = 60 + ((x + t * 23) % 120) as u8;
            (v, v / 2, v / 3)
        }
    })
}

fn menu() -> Vec<u8> {
    frame(|x, y| if (x / 40 + y / 30) % 2 == 0 { (30, 30, 40) } else { (200, 190, 170) })
}

fn loading() -> Vec<u8> {
    frame(|_, _| (5, 5, 5))
}

// --- analysis -------------------------------------------------------------

#[test]
fn dark_scene_reports_crushed_shadows_with_detail() {
    let mut an = Analyser::new();
    run(&mut an, &dark_crushed(0));
    let r = run(&mut an, &dark_crushed(1));
    assert_eq!(r.class, FrameClass::Gameplay);
    assert!((0.35..=0.45).contains(&r.stats.crush_frac), "{:?}", r.stats);
    assert!(r.stats.crushed_detail >= CRUSH_DETAIL_MIN, "{:?}", r.stats);
    assert!(r.stats.mean_luma < 0.25);
}

#[test]
fn flat_black_is_not_hidden_detail() {
    let mut an = Analyser::new();
    let f = |t: usize| {
        frame(move |x, y| {
            if y < H / 2 {
                (0, 0, 0)
            } else {
                let v = 80 + ((x + t * 9) % 100) as u8;
                (v, v, v)
            }
        })
    };
    run(&mut an, &f(0));
    let r = run(&mut an, &f(1));
    assert!(r.stats.crush_frac > 0.45);
    assert!(r.stats.crushed_detail < CRUSH_DETAIL_MIN, "{:?}", r.stats);
}

#[test]
fn bright_scene_is_bright_and_saturated() {
    let mut an = Analyser::new();
    run(&mut an, &bright(0));
    let r = run(&mut an, &bright(1));
    assert_eq!(r.class, FrameClass::Gameplay);
    assert!(r.stats.mean_luma > 0.55, "{:?}", r.stats);
    assert!(r.stats.crush_frac < 0.01);
    assert!(r.stats.sat_mean > 0.4);
    // Orange-yellow: the hue mass sits in the 30–60° bin.
    let peak = (0..HUE_BINS).max_by(|a, b| r.stats.hue_hist[*a].total_cmp(&r.stats.hue_hist[*b]));
    assert_eq!(peak, Some(1));
    assert!((r.stats.hue_hist.iter().sum::<f32>() - 1.0).abs() < 1e-3);
}

#[test]
fn washed_out_scene_has_low_saturation() {
    let mut an = Analyser::new();
    run(&mut an, &washed(0));
    let r = run(&mut an, &washed(1));
    assert_eq!(r.class, FrameClass::Gameplay);
    assert!(r.stats.sat_mean < 0.15, "{:?}", r.stats);
}

#[test]
fn menus_and_static_frames_are_excluded() {
    let mut an = Analyser::new();
    assert_eq!(run(&mut an, &menu()).class, FrameClass::Warmup);
    assert_eq!(run(&mut an, &menu()).class, FrameClass::Static);
}

#[test]
fn loading_screens_are_excluded() {
    let mut an = Analyser::new();
    assert_eq!(run(&mut an, &loading()).class, FrameClass::Loading);
}

#[test]
fn letterboxed_cutscenes_are_excluded_and_bars_do_not_count_as_shadow() {
    let mut an = Analyser::new();
    run(&mut an, &letterboxed(0));
    let r = run(&mut an, &letterboxed(1));
    assert_eq!(r.class, FrameClass::Cutscene);
    assert!(r.stats.letterbox >= LETTERBOX_BAR_FRAC);
    assert!(r.stats.crush_frac < 0.05, "bars leaked into stats: {:?}", r.stats);
}

#[test]
fn whiteouts_are_outliers() {
    let mut an = Analyser::new();
    let white = |t: usize| {
        frame(move |x, _| if (x + t) % 50 == 0 { (90, 90, 90) } else { (255, 255, 255) })
    };
    run(&mut an, &white(0));
    assert_eq!(run(&mut an, &white(1)).class, FrameClass::Outlier);
}

#[test]
fn bgr_and_rgb_agree_on_luma() {
    let rgb = bright(3);
    let bgr: Vec<u8> = rgb.chunks(3).flat_map(|p| [p[2], p[1], p[0]]).collect();
    let a = Analyser::new().analyse(&Frame { width: W, height: H, data: &rgb, order: Order::Rgb });
    let b = Analyser::new().analyse(&Frame { width: W, height: H, data: &bgr, order: Order::Bgr });
    assert_eq!(a.stats, b.stats);
}

#[test]
fn short_buffers_are_outliers_not_panics() {
    let r =
        Analyser::new().analyse(&Frame { width: W, height: H, data: &[0; 10], order: Order::Rgb });
    assert_eq!(r.class, FrameClass::Outlier);
}

#[test]
fn classification_thresholds_are_the_named_constants() {
    let base = FrameStats { luma_std: 0.2, motion: 0.1, ..Default::default() };
    assert_eq!(classify(&base, false), FrameClass::Gameplay);
    let s = FrameStats { luma_std: LOADING_MAX_STDDEV - 1e-4, ..base.clone() };
    assert_eq!(classify(&s, false), FrameClass::Loading);
    let s = FrameStats { letterbox: LETTERBOX_BAR_FRAC, ..base.clone() };
    assert_eq!(classify(&s, false), FrameClass::Cutscene);
    let s = FrameStats { clip_frac: OUTLIER_CLIP_FRAC + 0.01, ..base.clone() };
    assert_eq!(classify(&s, false), FrameClass::Outlier);
    let s = FrameStats { motion: MOTION_STATIC - 1e-4, ..base.clone() };
    assert_eq!(classify(&s, false), FrameClass::Static);
    assert_eq!(classify(&base, true), FrameClass::Warmup);
}

#[test]
fn analysis_constants_are_sane() {
    assert_eq!(LUMA_CRUSH, 0.05);
    assert_eq!(LUMA_CLIP, 0.98);
    const { assert!(DARK_REGION > LUMA_CRUSH) };
    assert_eq!(MOTION_GRID, (64, 36));
}

// --- derivation -----------------------------------------------------------

fn summary(crush: f32, detail: f32, sat: f32, p90: f32, clip: f32) -> AggregateSummary {
    AggregateSummary {
        mean_luma: 0.3,
        crush_frac: crush,
        clip_frac: clip,
        crushed_detail: detail,
        sat_mean: sat,
        sat_p90: p90,
    }
}

#[test]
fn a_well_balanced_game_is_left_alone() {
    let l = derive_look(&summary(CRUSH_OK, 0.02, SAT_TARGET, 0.6, CLIP_OK));
    assert!(l.is_neutral(), "{l:?}");
    assert_eq!(l.highlight, 0.0);
}

#[test]
fn crushed_shadows_with_detail_ask_for_shadow_recovery() {
    let l = derive_look(&summary(CRUSH_OK + CRUSH_SPAN / 2.0, 0.02, 0.4, 0.6, 0.0));
    assert_eq!(l.shadow, 0.5);
    let full = derive_look(&summary(0.9, 0.02, 0.4, 0.6, 0.0));
    assert_eq!(full.shadow, 1.0, "clamped");
}

#[test]
fn flat_black_never_asks_for_lift() {
    let l = derive_look(&summary(0.5, CRUSH_DETAIL_MIN / 2.0, 0.4, 0.6, 0.0));
    assert_eq!(l.shadow, 0.0);
}

#[test]
fn washed_out_asks_for_saturation_unless_it_would_clip() {
    let l = derive_look(&summary(0.0, 0.0, SAT_TARGET - SAT_SPAN, 0.5, 0.0));
    assert_eq!(l.saturation, 1.0);
    let l = derive_look(&summary(0.0, 0.0, SAT_TARGET - SAT_SPAN, SAT_P90_CEILING, 0.0));
    assert_eq!(l.saturation, 0.0);
}

#[test]
fn clipping_raises_highlight_caution() {
    let l = derive_look(&summary(0.0, 0.0, 0.4, 0.6, CLIP_OK + CLIP_SPAN));
    assert_eq!(l.highlight, 1.0);
}

#[test]
fn look_is_quantised_to_the_step() {
    let l = derive_look(&summary(CRUSH_OK + 0.033, 0.02, 0.4, 0.6, 0.0));
    let steps = l.shadow / LOOK_STEP;
    assert!((steps - steps.round()).abs() < 1e-4, "{l:?}");
}

fn caps(kind: PanelKind) -> PanelCaps {
    PanelCaps { kind, black_equalizer_max: None }
}

const FULL: LookTargets = LookTargets { shadow: 1.0, saturation: 1.0, highlight: 0.0 };

#[test]
fn oled_never_raises_black_and_never_uses_ddc() {
    let a = realize(&FULL, &PanelCaps { kind: PanelKind::Oled, black_equalizer_max: Some(10) });
    assert_eq!(a.shadow_lift, 0, "a lifted floor greys OLED black");
    assert_eq!(a.black_equalizer, None);
    assert_eq!(a.gamma, 1.0 + OLED_MAX_GAMMA_BOOST);
    assert_eq!(a.vibrance, 50 + OLED_MAX_VIBRANCE_BOOST);
    // Gamma keeps code 0 at 0: the ramp's first entry is still black.
    let ramp = crate::gamma::build_ramp(&crate::gamma::RampParams {
        gamma: a.gamma,
        contrast: 0,
        shadow_lift: a.shadow_lift,
    });
    assert_eq!(ramp.r[0], 0);
    assert!(a.notes.iter().any(|n| n.contains("OLED")));
}

#[test]
fn ips_uses_a_small_lift_without_a_verified_black_equalizer() {
    let a = realize(&FULL, &caps(PanelKind::Ips));
    assert_eq!(a.shadow_lift, LCD_MAX_SHADOW_LIFT);
    assert_eq!(a.black_equalizer, None);
    assert_eq!(a.gamma, 1.0 + LCD_MAX_GAMMA_BOOST);
    assert_eq!(a.vibrance, 50 + LCD_MAX_VIBRANCE_BOOST);
}

#[test]
fn va_with_a_verified_black_equalizer_uses_it_instead_of_lift() {
    let a = realize(&FULL, &PanelCaps { kind: PanelKind::Va, black_equalizer_max: Some(20) });
    assert_eq!(a.black_equalizer, Some((20.0 * BLACK_EQ_MAX_FRACTION) as u16));
    assert_eq!(a.shadow_lift, 0);
    assert!(a.ddc_writes() <= LEARNED_DDC_WRITES_MAX);
}

#[test]
fn unknown_panels_get_gamma_only() {
    let a = realize(&FULL, &PanelCaps { kind: PanelKind::Unknown, black_equalizer_max: Some(9) });
    assert_eq!(a.shadow_lift, 0);
    assert_eq!(a.black_equalizer, None);
    assert!(a.gamma > 1.0);
}

#[test]
fn highlight_caution_damps_brightening() {
    let calm = realize(&FULL, &caps(PanelKind::Oled));
    let hot = realize(&LookTargets { highlight: 1.0, ..FULL }, &caps(PanelKind::Oled));
    assert!(hot.gamma < calm.gamma);
    assert!((hot.gamma - (1.0 + OLED_MAX_GAMMA_BOOST * (1.0 - HIGHLIGHT_DAMPING))).abs() <= 0.006);
}

#[test]
fn neutral_look_realises_to_neutral_everywhere() {
    for kind in [PanelKind::Oled, PanelKind::Ips, PanelKind::Va, PanelKind::Tn, PanelKind::Unknown]
    {
        let a =
            realize(&LookTargets::default(), &PanelCaps { kind, black_equalizer_max: Some(10) });
        assert_eq!((a.gamma, a.shadow_lift, a.vibrance, a.black_equalizer), (1.0, 0, 50, None));
    }
}

#[test]
fn nothing_is_extreme_even_for_out_of_range_input() {
    let wild = LookTargets { shadow: 9.0, saturation: 9.0, highlight: -3.0 };
    for kind in [PanelKind::Oled, PanelKind::Ips, PanelKind::Unknown] {
        let a = realize(&wild, &PanelCaps { kind, black_equalizer_max: Some(100) });
        assert!(a.gamma <= 1.0 + OLED_MAX_GAMMA_BOOST + 1e-6);
        assert!(a.shadow_lift <= LCD_MAX_SHADOW_LIFT);
        assert!(a.vibrance <= 50 + LCD_MAX_VIBRANCE_BOOST);
        assert!(a.black_equalizer.unwrap_or(0) <= 50);
    }
    // The constants themselves stay gentle.
    const { assert!(OLED_MAX_GAMMA_BOOST <= 0.2 && LCD_MAX_GAMMA_BOOST <= 0.2) };
    const { assert!(LCD_MAX_SHADOW_LIFT <= 25) };
    assert_eq!(LEARNED_DDC_WRITES_MAX, 1);
}

#[test]
fn panel_labels_parse() {
    assert_eq!(PanelKind::from_label("WOLED"), PanelKind::Oled);
    assert_eq!(PanelKind::from_label("QD-OLED"), PanelKind::Oled);
    assert_eq!(PanelKind::from_label("Nano IPS"), PanelKind::Ips);
    assert_eq!(PanelKind::from_label("VA"), PanelKind::Va);
    assert_eq!(PanelKind::from_label(""), PanelKind::Unknown);
}

#[test]
fn apl_buckets_follow_the_edges() {
    assert_eq!(apl_bucket(0.0), 0);
    assert_eq!(apl_bucket(APL_BUCKET_EDGES[0]), 1);
    assert_eq!(apl_bucket(0.99), APL_BUCKETS - 1);
}

#[test]
fn hdr_colour_space_is_detected() {
    assert!(super::is_hdr_color_space(12));
    assert!(!super::is_hdr_color_space(0)); // RGB_FULL_G22_NONE_P709
}

// --- state machine --------------------------------------------------------

fn gameplay(mean_luma: f32, crush: f32) -> FrameReport {
    FrameReport {
        class: FrameClass::Gameplay,
        stats: FrameStats {
            mean_luma,
            luma_std: 0.2,
            crush_frac: crush,
            crushed_detail: 0.02,
            sat_mean: 0.4,
            sat_p90: 0.6,
            motion: 0.1,
            ..Default::default()
        },
    }
}

/// Feed `n` frames cycling through three scenes with one crush level.
fn feed(l: &mut Learner, n: usize, crush: f32) {
    let apl = [0.05, 0.3, 0.7];
    for i in 0..n {
        l.observe(&gameplay(apl[i % 3], crush));
    }
}

#[test]
fn not_ready_without_enough_frames() {
    let mut l = Learner::new("b1");
    feed(&mut l, MIN_GAMEPLAY_FRAMES as usize - 1, 0.2);
    assert_eq!(l.phase(), Phase::Learning);
    assert!(l.readiness().progress < 1.0);
}

#[test]
fn not_ready_with_one_kind_of_scene() {
    let mut l = Learner::new("b1");
    for _ in 0..MIN_GAMEPLAY_FRAMES * 3 {
        l.observe(&gameplay(0.3, 0.2));
    }
    assert_eq!(l.scenes(), 1);
    assert_eq!(l.phase(), Phase::Learning);
}

#[test]
fn converges_on_stable_varied_evidence() {
    let mut l = Learner::new("b1");
    feed(&mut l, (MIN_GAMEPLAY_FRAMES + CHECKPOINT_FRAMES as u64 * 3) as usize, 0.14);
    assert_eq!(l.phase(), Phase::Converged);
    assert_eq!(l.converged.unwrap().shadow, 0.5);
    assert!((l.readiness().progress - 1.0).abs() < 1e-6);
}

#[test]
fn excluded_frames_do_not_count() {
    let mut l = Learner::new("b1");
    for class in
        [FrameClass::Static, FrameClass::Loading, FrameClass::Cutscene, FrameClass::Outlier]
    {
        for _ in 0..1000 {
            l.observe(&FrameReport { class, ..gameplay(0.3, 0.5) });
        }
    }
    assert_eq!(l.agg.frames, 0);
    assert_eq!(l.excluded.static_frames, 1000);
    assert_eq!(l.excluded.cutscene, 1000);
}

#[test]
fn nan_stats_are_dropped() {
    let mut l = Learner::new("b1");
    let mut r = gameplay(0.3, 0.2);
    r.stats.sat_mean = f32::NAN;
    l.observe(&r);
    assert_eq!(l.agg.frames, 0);
}

#[test]
fn applied_look_is_frozen_until_a_meaningful_change() {
    let mut l = Learner::new("b1");
    feed(&mut l, 1200, 0.14);
    assert!(l.apply());
    let first = l.applied.unwrap();
    // Small drift: converged moves, applied does not.
    feed(&mut l, ROLLING_WINDOW_FRAMES as usize * 2, 0.16);
    assert!(l.converged.unwrap().distance(&first) > 0.0);
    assert!(l.converged.unwrap().distance(&first) < MEANINGFUL_CHANGE);
    assert_eq!(l.applied, Some(first));
    // A real change (game update, new art direction) carries through.
    feed(&mut l, ROLLING_WINDOW_FRAMES as usize * 3, 0.04);
    assert!(
        l.applied.unwrap().shadow <= first.shadow - MEANINGFUL_CHANGE + 1e-4,
        "{:?}",
        l.applied
    );
}

#[test]
fn apply_needs_a_converged_result() {
    let mut l = Learner::new("b1");
    assert!(!l.apply());
    assert!(!l.use_learned);
}

#[test]
fn new_build_relearns_but_keeps_the_applied_look() {
    let mut l = Learner::new("b1");
    feed(&mut l, 1200, 0.14);
    l.apply();
    assert!(!l.check_build("b1"));
    assert!(l.check_build("b2"));
    assert_eq!(l.agg.frames, 0);
    assert_eq!(l.phase(), Phase::Learning);
    assert!(l.applied.is_some(), "still in use until the new look converges");
}

#[test]
fn reset_clears_everything() {
    let mut l = Learner::new("b1");
    feed(&mut l, 1200, 0.14);
    l.apply();
    l.reset();
    assert_eq!(l, Learner { build: "b1".into(), ..Learner::default() });
}

#[test]
fn rolling_window_bounds_the_weight() {
    let mut l = Learner::new("b1");
    feed(&mut l, ROLLING_WINDOW_FRAMES as usize * 3, 0.1);
    assert!(l.agg.weight <= ROLLING_WINDOW_FRAMES + 1.0);
    assert_eq!(l.agg.frames, ROLLING_WINDOW_FRAMES as u64 * 3);
}

#[test]
fn learner_round_trips_through_json() {
    let mut l = Learner::new("b1");
    feed(&mut l, 700, 0.14);
    let back: Learner = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
    assert_eq!(back, l);
}

#[test]
fn convergence_constants() {
    assert_eq!(MIN_GAMEPLAY_FRAMES, 600);
    assert_eq!(MIN_SCENES, 3);
    assert_eq!(CHECKPOINT_FRAMES, 120);
    assert_eq!(CONVERGE_CHECKPOINTS, 3);
    const { assert!(CONVERGE_TOLERANCE < MEANINGFUL_CHANGE) };
    assert!(ROLLING_WINDOW_FRAMES > MIN_GAMEPLAY_FRAMES as f64);
}

/// End to end on synthetic frames: a dark game with crushed detail learns
/// shadow recovery; menus and cutscenes in between change nothing.
#[test]
fn synthetic_session_end_to_end() {
    let mut an = Analyser::new();
    let mut l = Learner::new("b1");
    let mut t = 0;
    for round in 0..14 {
        for _ in 0..60 {
            t += 1;
            l.observe(&run(&mut an, &dark_crushed(t)));
        }
        for _ in 0..30 {
            t += 1;
            l.observe(&run(&mut an, &washed(t)));
        }
        for _ in 0..30 {
            t += 1;
            l.observe(&run(&mut an, &bright(t)));
        }
        if round % 3 == 0 {
            for _ in 0..10 {
                l.observe(&run(&mut an, &menu()));
            }
            for _ in 0..10 {
                t += 1;
                l.observe(&run(&mut an, &letterboxed(t)));
            }
        }
    }
    assert!(l.excluded.static_frames > 0 && l.excluded.cutscene > 0);
    assert_eq!(l.phase(), Phase::Converged, "{:?}", l.readiness());
    let look = l.converged.unwrap();
    assert!(look.shadow > 0.0, "{look:?}");
    let oled = realize(&look, &caps(PanelKind::Oled));
    assert_eq!(oled.shadow_lift, 0);
    assert!(oled.gamma > 1.0);
}

#[test]
fn no_input_for_the_idle_limit_is_not_gameplay() {
    let g = gameplay(0.3, 0.2);
    assert_eq!(INPUT_IDLE_MAX_MS, 20_000);
    assert_eq!(with_input_idle(g.clone(), Some(INPUT_IDLE_MAX_MS)).class, FrameClass::Gameplay);
    assert_eq!(with_input_idle(g.clone(), Some(INPUT_IDLE_MAX_MS + 1)).class, FrameClass::Idle);
    assert_eq!(with_input_idle(g.clone(), None).class, FrameClass::Gameplay);
    // Only gameplay is reclassified; a cutscene stays a cutscene.
    let c = FrameReport { class: FrameClass::Cutscene, ..g };
    assert_eq!(with_input_idle(c, Some(60_000)).class, FrameClass::Cutscene);
    let mut l = Learner::new("b1");
    l.observe(&with_input_idle(gameplay(0.3, 0.2), Some(30_000)));
    assert_eq!((l.agg.frames, l.excluded.idle), (0, 1));
}
