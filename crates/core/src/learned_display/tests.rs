use super::*;
use relay_display::learn::{FrameClass, FrameStats};

fn tmp_store(name: &str) -> LearnStore {
    let dir = std::env::temp_dir().join(format!("relay-s47-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    LearnStore::load(dir.join("learned-display.json"))
}

fn converged_learner() -> Learner {
    let mut l = Learner::new("b1");
    let apl = [0.05, 0.3, 0.7];
    for i in 0..1200 {
        l.observe(&FrameReport {
            class: FrameClass::Gameplay,
            stats: FrameStats {
                mean_luma: apl[i % 3],
                luma_std: 0.2,
                crush_frac: 0.14,
                crushed_detail: 0.02,
                sat_mean: 0.2,
                sat_p90: 0.5,
                motion: 0.1,
                ..Default::default()
            },
        });
    }
    assert_eq!(l.phase(), Phase::Converged);
    l
}

fn mon() -> MonitorId {
    MonitorId("GSM5C7C-abc".into())
}

#[test]
fn store_round_trips_and_keys_by_lowercase_exe() {
    let mut s = tmp_store("roundtrip");
    s.game_mut("Game.EXE").enabled = true;
    s.game_mut("game.exe")
        .monitors
        .insert(mon().0, MonitorRecord { learner: converged_learner(), hdr_skipped: false });
    s.save().unwrap();
    let back = LearnStore::load(s.path.clone());
    assert_eq!(back.file, s.file);
    assert!(back.is_enabled("GAME.exe"));
}

#[test]
fn a_corrupt_or_future_store_starts_empty() {
    let s = tmp_store("corrupt");
    std::fs::create_dir_all(s.path.parent().unwrap()).unwrap();
    std::fs::write(&s.path, "{nope").unwrap();
    assert!(LearnStore::load(s.path.clone()).file.games.is_empty());
    std::fs::write(&s.path, r#"{"version":99,"games":{}}"#).unwrap();
    assert_eq!(LearnStore::load(s.path.clone()).file.version, STORE_VERSION);
}

#[test]
fn effective_prefers_the_applied_learned_look_over_an_import() {
    let mut g = GameRecord::default();
    let imported = LookTargets { shadow: 0.2, saturation: 0.0, highlight: 0.0 };
    g.imported = Some(ImportedLook { look: imported, note: String::new() });
    assert_eq!(g.effective(&mon()), Some(imported));
    let mut l = converged_learner();
    // Converged but not applied: the import is still what is used.
    g.monitors.insert(mon().0, MonitorRecord { learner: l.clone(), hdr_skipped: false });
    assert_eq!(g.effective(&mon()), Some(imported));
    l.apply();
    g.monitors.insert(mon().0, MonitorRecord { learner: l.clone(), hdr_skipped: false });
    assert_eq!(g.effective(&mon()), l.applied);
}

#[test]
fn overlay_is_a_correction_on_top_of_the_profile() {
    let mut d = DisplaySettings::default();
    d.gpu.vibrance = 60;
    d.gpu.gamma = 1.1;
    let adj = Adjustments { gamma: 1.1, shadow_lift: 10, vibrance: 58, ..Default::default() };
    overlay(&mut d, &adj);
    assert_eq!(d.gpu.vibrance, 68);
    assert_eq!(d.gpu.gamma, 1.21);
    assert_eq!(d.gpu.shadow_lift, 10);
    // A profile's own black-equalizer choice wins.
    d.monitor.black_equalizer = Some(3);
    overlay(&mut d, &Adjustments { black_equalizer: Some(9), ..Default::default() });
    assert_eq!(d.monitor.black_equalizer, Some(3));
    // ...and a profile that already writes any DDC code gets no learned one:
    // two DDC writes would not restore inside the 200 ms budget.
    let mut d = DisplaySettings::default();
    d.monitor.brightness = Some(40);
    overlay(&mut d, &Adjustments { black_equalizer: Some(9), ..Default::default() });
    assert_eq!(d.monitor.black_equalizer, None);
    let mut d = DisplaySettings::default();
    overlay(&mut d, &Adjustments { black_equalizer: Some(9), ..Default::default() });
    assert_eq!(d.monitor.black_equalizer, Some(9));
}

#[test]
fn neutral_overlay_changes_nothing() {
    let mut d = DisplaySettings::default();
    let before = d.clone();
    overlay(&mut d, &Adjustments::default());
    assert_eq!(d, before);
}

#[test]
fn the_learner_never_reaches_ddc_on_any_shipped_monitor() {
    // No quirks row carries a verified black-equalizer range today.
    for label in ["WOLED", "Nano IPS", "VA", ""] {
        assert_eq!(panel_caps(label).black_equalizer_max, None);
    }
}

#[test]
fn export_then_import_round_trips() {
    let mut s = tmp_store("export");
    let mut l = converged_learner();
    l.apply();
    let look = l.converged.unwrap();
    s.game_mut("game.exe")
        .monitors
        .insert(mon().0, MonitorRecord { learner: l, hdr_skipped: false });
    let json =
        export(&s, "Game.exe", Some("A Game".into()), "dark maps, tuned on an OLED").unwrap();
    // Nothing personal or monitor-specific in the file.
    assert!(!json.contains("GSM5C7C"), "{json}");
    assert!(!json.contains("b1"), "build fingerprint stays local: {json}");
    let f = GameDisplayFile::parse(&json).unwrap();
    assert_eq!(f.game.exe, "game.exe");
    assert_eq!(LookTargets::from(f.look), look);
    assert!(f.evidence.frames >= 600);

    let mut other = tmp_store("import");
    import(&mut other, "GAME.EXE", &json).unwrap();
    assert_eq!(other.game("game.exe").unwrap().imported.as_ref().unwrap().look, look);
    assert_eq!(other.game("game.exe").unwrap().effective(&mon()), Some(look));
}

#[test]
fn export_needs_a_settled_look() {
    let mut s = tmp_store("unsettled");
    s.game_mut("game.exe");
    assert!(export(&s, "game.exe", None, "").is_err());
    assert!(export(&tmp_store("none"), "x.exe", None, "").is_err());
}

fn good() -> serde_json::Value {
    serde_json::json!({
        "format": "relay-game-display",
        "version": 1,
        "game": { "exe": "game.exe" },
        "look": { "shadow": 0.3, "saturation": 0.1, "highlight": 0.0 },
        "note": "hi"
    })
}

fn rejects(v: serde_json::Value) -> String {
    GameDisplayFile::parse(&v.to_string()).unwrap_err().to_string()
}

#[test]
fn file_rejections() {
    assert!(GameDisplayFile::parse(&good().to_string()).is_ok());

    let mut v = good();
    v["format"] = "something-else".into();
    assert!(rejects(v).contains("not a Relay"));

    let mut v = good();
    v["version"] = 2.into();
    assert!(rejects(v).contains("version 2"));

    let mut v = good();
    v["look"]["shadow"] = 1.5.into();
    assert!(rejects(v).contains("0 to 1"));

    let mut v = good();
    v["look"]["saturation"] = (-0.1).into();
    assert!(rejects(v).contains("0 to 1"));

    let mut v = good();
    v["game"]["exe"] = r"C:\Users\someone\game.exe".into();
    assert!(rejects(v).contains("no folder"));

    let mut v = good();
    v["game"]["exe"] = "game.bat".into();
    assert!(rejects(v).contains("exe"));

    // Unknown fields are refused, so a serial number cannot ride along.
    let mut v = good();
    v["monitor_serial"] = "ABC123".into();
    assert!(rejects(v).contains("not a Relay"));
    let mut v = good();
    v["game"]["path"] = "C:/x".into();
    assert!(GameDisplayFile::parse(&v.to_string()).is_err());

    let mut v = good();
    v["note"] = "x".repeat(NOTE_MAX_CHARS + 1).into();
    assert!(rejects(v).contains("longer"));

    let mut v = good();
    v["note"] = "bell\u{7}".into();
    assert!(rejects(v).contains("control"));

    assert!(GameDisplayFile::parse(&" ".repeat(FILE_MAX_BYTES + 1)).is_err());
    assert!(GameDisplayFile::parse("not json").is_err());
}

#[test]
fn import_refuses_another_games_file() {
    let mut s = tmp_store("wronggame");
    let err = import(&mut s, "other.exe", &good().to_string()).unwrap_err().to_string();
    assert!(err.contains("game.exe"), "{err}");
    assert!(s.game("other.exe").is_none_or(|g| g.imported.is_none()));
}

#[test]
fn view_carries_the_exact_notices_and_progress() {
    let mut s = tmp_store("view");
    s.game_mut("game.exe").enabled = true;
    s.game_mut("game.exe")
        .monitors
        .insert(mon().0, MonitorRecord { learner: converged_learner(), hdr_skipped: true });
    let v = view(&s, "Game.exe", true, &|_| ("LG ULTRAGEAR+".into(), "WOLED".into()));
    assert_eq!(
        v.tournament,
        "Relay's visual enhancements may not be allowed in some tournaments or professional \
         environments. Check with your tournament host or rules."
    );
    assert!(v.privacy.contains("No frames are recorded or saved"));
    assert!(v.enabled && v.sampling);
    let m = &v.monitors[0];
    assert_eq!(m.panel, PanelKind::Oled);
    assert_eq!(m.phase, Phase::Converged);
    assert!(m.hdr_skipped);
    assert!(m.adjustments.is_none(), "nothing applied yet");
    // The breakdown and per-scene counts reach the view (PC2, r49).
    assert_eq!(m.readiness.scene_frames.len(), 5);
    assert_eq!(m.readiness.scene_frames.iter().sum::<u64>(), 1200);
    let json = serde_json::to_value(m).unwrap();
    for k in ["static_frames", "loading", "cutscene", "outlier", "idle", "warmup"] {
        assert!(json["excluded_by"].get(k).is_some(), "{k}");
    }
}

#[test]
fn sampler_wire_lines_decode() {
    let r = FrameReport { class: FrameClass::Static, stats: FrameStats::default() };
    let line = serde_json::json!({ "event": "look", "report": r }).to_string();
    assert_eq!(decode_line(&line), Some(SamplerLine::Frame(Box::new(r))));
    assert_eq!(decode_line(r#"{"event":"look_hdr"}"#), Some(SamplerLine::Hdr));
    assert_eq!(
        decode_line(r#"{"event":"error","where":"fatal","message":"boom"}"#),
        Some(SamplerLine::Error("boom".into()))
    );
    assert_eq!(decode_line("garbage"), None);
    assert_eq!(decode_line(r#"{"event":"stats"}"#), None);
    assert_eq!(sampler_args(42, SAMPLE_FPS), ["look", "--hmonitor", "42", "--fps", "1"]);
    assert_eq!(SAMPLE_FPS, 1);
}

#[test]
fn a_fast_exit_never_loses_the_hdr_line() {
    // The sampler wrote look_hdr and exited before the tick looked.
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(SamplerLine::Hdr).unwrap();
    drop(tx);
    let (lines, done) = drain(&rx, true);
    assert_eq!(lines, vec![SamplerLine::Hdr]);
    assert!(done);
    // Exit not yet noticed, but the reader hung up: still finished.
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(SamplerLine::Hdr).unwrap();
    drop(tx);
    let (lines, done) = drain(&rx, false);
    assert_eq!(lines.len(), 1);
    assert!(done);
    // Running and quiet: not finished.
    let (_tx, rx) = std::sync::mpsc::channel::<SamplerLine>();
    assert_eq!(drain(&rx, false), (vec![], false));
}

#[test]
fn build_fingerprint_changes_with_the_file() {
    let dir = std::env::temp_dir().join(format!("relay-s47-fp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("game.exe");
    std::fs::write(&exe, b"v1").unwrap();
    let a = build_fingerprint(&exe);
    std::fs::write(&exe, b"v2-longer").unwrap();
    let b = build_fingerprint(&exe);
    assert!(!a.is_empty() && a != b);
    assert_eq!(build_fingerprint(&dir.join("missing.exe")), "");
}

#[test]
fn a_monitor_switch_refits_the_learned_look_without_relearning() {
    let mut g = GameRecord::default();
    let mut l = converged_learner();
    l.apply();
    let look = l.applied.unwrap();
    let oled = MonitorId("GSM5C7C-oled".into());
    let ips = MonitorId("DEL4321-ips".into());
    g.monitors.insert(oled.0.clone(), MonitorRecord { learner: l, hdr_skipped: false });

    // The IPS monitor has never been seen: same look, no evidence needed.
    assert_eq!(g.effective(&ips), Some(look));
    let on_oled = realize(&g.effective(&oled).unwrap(), &panel_caps("WOLED"));
    let on_ips = realize(&g.effective(&ips).unwrap(), &panel_caps("Nano IPS"));
    assert!(look.shadow > 0.0);
    // OLED: gamma only, black floor untouched. IPS: a small lift is allowed.
    assert_eq!(on_oled.shadow_lift, 0);
    assert!(on_ips.shadow_lift > 0);
    assert_ne!(on_oled, on_ips);
    // A verified black equalizer on the second panel is used instead of lift.
    let va = PanelCaps { kind: PanelKind::Va, black_equalizer_max: Some(10) };
    let on_va = realize(&look, &va);
    assert!(on_va.black_equalizer.is_some() && on_va.shadow_lift == 0);

    // Starting to fine-tune on the new monitor starts from the same look.
    let rec = g.monitor_mut(&ips);
    assert_eq!(rec.learner.applied, Some(look));
    assert_eq!(rec.learner.agg.frames, 0, "no evidence was copied, only the look");
}

#[test]
fn an_import_applies_at_once_with_learning_off() {
    let mut s = tmp_store("import-now");
    // The game was learning before; the import turns that off.
    s.game_mut("game.exe").enabled = true;
    s.game_mut("game.exe")
        .monitors
        .insert(mon().0, MonitorRecord { learner: converged_learner(), hdr_skipped: false });
    import(&mut s, "game.exe", &good().to_string()).unwrap();
    let g = s.game("game.exe").unwrap();
    assert!(!g.enabled, "learning is off for an imported game by default");
    let imported = LookTargets { shadow: 0.3, saturation: 0.1, highlight: 0.0 };
    assert_eq!(g.effective(&mon()), Some(imported));
    assert_eq!(g.status(Some(&mon())), LookStatus::AppliedImported);
    assert_eq!(g.status(None), LookStatus::AppliedImported);
    // A monitor never seen before gets it too.
    assert_eq!(g.status(Some(&MonitorId("NEW".into()))), LookStatus::AppliedImported);
    let v = view(&s, "game.exe", false, &|_| ("x".into(), "WOLED".into()));
    assert_eq!(v.status, LookStatus::AppliedImported);
    assert!(v.monitors[0].adjustments.is_some());
}

fn feed(l: &mut Learner, crush: f32, n: usize) {
    let apl = [0.05, 0.3, 0.7];
    for i in 0..n {
        l.observe(&FrameReport {
            class: FrameClass::Gameplay,
            stats: FrameStats {
                mean_luma: apl[i % 3],
                luma_std: 0.2,
                crush_frac: crush,
                crushed_detail: 0.02,
                sat_mean: 0.33,
                sat_p90: 0.5,
                motion: 0.1,
                ..Default::default()
            },
        });
    }
}

#[test]
fn fine_tuning_an_import_follows_the_same_convergence_rules() {
    let mut s = tmp_store("finetune");
    import(&mut s, "game.exe", &good().to_string()).unwrap();
    let g = s.game_mut("game.exe");
    g.enabled = true; // "Keep learning to fine-tune for my monitor"
    let rec = g.monitor_mut(&mon());
    let imported = rec.learner.applied.unwrap();
    // Close to the import (shadow 0.3 = crush 0.10): the import stays.
    feed(&mut rec.learner, 0.10, 1200);
    assert_eq!(rec.learner.phase(), Phase::Converged);
    assert_eq!(rec.learner.applied, Some(imported));
    assert_eq!(g.status(Some(&mon())), LookStatus::AppliedImported);
    // Clearly different on this monitor: the fine-tuned look takes over.
    let rec = g.monitor_mut(&mon());
    feed(&mut rec.learner, 0.24, 12_000);
    let tuned = rec.learner.applied.unwrap();
    assert!(tuned.distance(&imported) >= relay_display::learn::converge::MEANINGFUL_CHANGE);
    assert_eq!(g.status(Some(&mon())), LookStatus::Applied);
}

#[test]
fn export_writes_the_current_game_layer() {
    let mut s = tmp_store("export-layer");
    // Imported, nothing learned: export writes the import.
    import(&mut s, "game.exe", &good().to_string()).unwrap();
    let f = GameDisplayFile::parse(&export(&s, "game.exe", None, "").unwrap()).unwrap();
    assert_eq!(f.look.shadow, 0.3);
    // A learned look applied (in use) wins over the import.
    let mut l = converged_learner();
    l.apply();
    let in_use = l.applied.unwrap();
    s.game_mut("game.exe")
        .monitors
        .insert(mon().0, MonitorRecord { learner: l, hdr_skipped: false });
    let f = GameDisplayFile::parse(&export(&s, "game.exe", None, "note").unwrap()).unwrap();
    assert_eq!(LookTargets::from(f.look), in_use);
    assert_eq!(f.note, "note");
}

#[test]
fn status_walks_off_learning_ready_applied() {
    let mut g = GameRecord::default();
    assert_eq!(g.status(Some(&mon())), LookStatus::Off);
    g.enabled = true;
    assert_eq!(g.status(Some(&mon())), LookStatus::Learning);
    let mut l = converged_learner();
    g.monitors.insert(mon().0, MonitorRecord { learner: l.clone(), hdr_skipped: false });
    assert_eq!(g.status(Some(&mon())), LookStatus::Ready);
    l.apply();
    g.monitors.insert(mon().0, MonitorRecord { learner: l, hdr_skipped: false });
    assert_eq!(g.status(Some(&mon())), LookStatus::Applied);
    g.monitors.get_mut(&mon().0).unwrap().hdr_skipped = true;
    assert_eq!(g.status(Some(&mon())), LookStatus::HdrSkipped);
}
