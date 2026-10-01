//! The uninstall proof, against the real endpoint export taken as this
//! milestone's baseline (a RODECaster Duo endpoint that ships a vendor FX
//! chain — exactly the store risk #2 worries about).
//!
//! Nothing here touches the live registry: install/uninstall run on the
//! parsed image, and "byte-for-byte" is proven two ways — deep equality of
//! every key/value/byte, and string equality of the re-serialized .reg
//! export (the same diff a VM run will do with two real `reg export`s).

use relay_apo::fxstore::{
    diff, plan_install, plan_uninstall, vet_fx_diff, Backup, FxStore, PlanError,
};
use relay_apo::ids;
use relay_apo::regfile::{
    decode_bytes, multi_sz_bytes, parse, parse_multi_sz, serialize, sz_bytes, sz_from_bytes,
    RegKind, RegValue,
};

const ENDPOINT: &str = "{f8ae226b-a4e3-45ab-97fc-3977dad232d1}";
const DLL: &str = r"C:\Program Files\Relay\relay_apo.dll";

fn fixture_text() -> String {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/fx-baseline-rodecaster.reg"
    ))
    .expect("baseline fixture present");
    decode_bytes(&bytes).expect("fixture decodes")
}

fn fixture_store() -> FxStore {
    let map = parse(&fixture_text()).expect("fixture parses");
    FxStore::from_reg_map(&map, ENDPOINT).expect("endpoint subtree present")
}

/// The parser/serializer must reproduce the 7k-line export exactly —
/// this is what makes a string diff of two exports a valid proof.
#[test]
fn fixture_round_trips_byte_for_byte() {
    let text = fixture_text();
    let map = parse(&text).expect("parses");
    assert_eq!(serialize(&map), text, "serialize(parse(fixture)) == fixture");
}

/// The real RODECaster store with its MFX slot freed (the vendor SFX and
/// every sub-key kept): install, then uninstall, restores it byte-for-byte.
fn fixture_store_free_mfx() -> FxStore {
    let mut s = fixture_store();
    s.keys.get_mut("").unwrap().remove(ids::PKEY_FX_MODE_EFFECT_CLSID);
    s
}

#[test]
fn install_then_uninstall_restores_byte_for_byte() {
    let original = fixture_store_free_mfx();
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    assert_ne!(plan.new_store, original);
    let restored = plan_uninstall(&plan.backup);
    assert_eq!(restored, original);
    assert!(diff(&original, &restored).is_empty());
    assert_eq!(
        serialize(&restored.to_reg_map(ENDPOINT)),
        serialize(&original.to_reg_map(ENDPOINT)),
    );
    // Only the MFX slot changed (the fixture already lists DEFAULT for MFX);
    // the vendor SFX is untouched.
    assert_eq!(
        diff(&original, &plan.new_store),
        vec![(String::new(), ids::PKEY_FX_MODE_EFFECT_CLSID.to_owned())]
    );
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        sz_from_bytes(&root.get(SFX).unwrap().data).as_deref(),
        Some("{C9453E73-8C5C-4463-9984-AF8BAB2F5447}")
    );
}

const MS_GFX: &str = "{13AB3EBD-137E-4903-9D89-60BE8277FD17}";

/// S44b. The baseline RODECaster endpoint already carries an MFX (MS_GFX,
/// Microsoft's inbox "WM audio GFX APO", WMALFXGFXDSP.dll) and no composite
/// MFX list: Relay takes the slot, records the original as its child (in the
/// store for the APO, in the backup for the record), and uninstall puts the
/// original back byte-for-byte.
#[test]
fn microsoft_gfx_mfx_is_chained_and_restored() {
    let original = fixture_store();
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_MODE_EFFECT_CLSID).unwrap().data).as_deref(),
        Some(ids::APO_CLSID)
    );
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_RELAY_CHILD_MFX).unwrap().data).as_deref(),
        Some(MS_GFX)
    );
    assert_eq!(plan.backup.chained_clsid.as_deref(), Some(MS_GFX));
    let mut changed = diff(&original, &plan.new_store);
    changed.sort();
    let mut want = vec![
        (String::new(), ids::PKEY_FX_MODE_EFFECT_CLSID.to_owned()),
        (String::new(), ids::PKEY_RELAY_CHILD_MFX.to_owned()),
    ];
    want.sort();
    assert_eq!(changed, want, "only the slot and Relay's own record change");
    assert!(vet_fx_diff(&plan).is_ok());
    // The vendor SFX is untouched.
    assert_eq!(
        sz_from_bytes(&root.get(SFX).unwrap().data).as_deref(),
        Some("{C9453E73-8C5C-4463-9984-AF8BAB2F5447}")
    );

    // Uninstall: the original CLSID is back, every byte of the export equal.
    let json = serde_json::to_string(&plan.backup).unwrap();
    let back: Backup = serde_json::from_str(&json).unwrap();
    let restored = plan_uninstall(&back);
    assert_eq!(restored, original);
    assert_eq!(
        serialize(&restored.to_reg_map(ENDPOINT)),
        serialize(&original.to_reg_map(ENDPOINT))
    );
    assert_eq!(
        sz_from_bytes(
            &restored.keys.get("").unwrap().get(ids::PKEY_FX_MODE_EFFECT_CLSID).unwrap().data
        )
        .as_deref(),
        Some(MS_GFX)
    );
    // And reinstalling over the chained store keeps the same child.
    let again = plan_install(&plan.new_store, ENDPOINT, DLL).unwrap();
    assert_eq!(again.new_store, plan.new_store);
}

/// Shape of a Realtek onboard endpoint: vendor SFX and MFX in the legacy
/// slots, DEFAULT modes, no composite lists. The CLSIDs here are
/// placeholders; the shape is what matters.
fn realtek_like_store() -> FxStore {
    let mut store = spdif_like_store(true, false);
    let root = store.keys.get_mut("").unwrap();
    root.insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), sz(REALTEK_MFX));
    root.insert(
        ids::PKEY_FX_ENDPOINT_EFFECT_CLSID.into(),
        sz("{33333333-4444-5555-6666-777777777777}"),
    );
    store
}
const REALTEK_MFX: &str = "{22222222-3333-4444-5555-666666666666}";

#[test]
fn realtek_mfx_is_chained_and_restored() {
    let original = realtek_like_store();
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    assert_eq!(plan.backup.chained_clsid.as_deref(), Some(REALTEK_MFX));
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_RELAY_CHILD_MFX).unwrap().data).as_deref(),
        Some(REALTEK_MFX)
    );
    // Vendor SFX and EFX untouched.
    assert_eq!(sz_from_bytes(&root.get(SFX).unwrap().data).as_deref(), Some(VENDOR_SFX));
    assert_eq!(
        root.get(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID),
        original.keys.get("").unwrap().get(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID)
    );
    let restored = plan_uninstall(&plan.backup);
    assert_eq!(
        serialize(&restored.to_reg_map(ENDPOINT)),
        serialize(&original.to_reg_map(ENDPOINT))
    );
}

/// Refusal stays for the cases chaining cannot handle, worded as such.
#[test]
fn unchainable_slots_still_refuse() {
    // A child with no COM registration.
    let err = relay_apo::fxstore::plan_install_with(&fixture_store(), ENDPOINT, DLL, &|_| false)
        .unwrap_err();
    assert_eq!(err, PlanError::SlotTaken(MS_GFX.to_owned()));
    assert!(err.to_string().starts_with("slot taken"));
    assert!(err.to_string().contains("cannot chain"));
    // Not a CLSID.
    let mut s = spdif_like_store(false, false);
    s.keys.get_mut("").unwrap().insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), sz("RtkApo"));
    assert!(matches!(plan_install(&s, ENDPOINT, DLL), Err(PlanError::SlotTaken(_))));
    // A list in the single-effect slot.
    let mut s = spdif_like_store(false, false);
    s.keys
        .get_mut("")
        .unwrap()
        .insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), msz(&[REALTEK_MFX, MS_GFX]));
    assert!(matches!(plan_install(&s, ENDPOINT, DLL), Err(PlanError::SlotTaken(_))));
}

fn sz(s: &str) -> RegValue {
    RegValue { kind: RegKind::Sz, data: sz_bytes(s) }
}
fn msz(list: &[&str]) -> RegValue {
    let v: Vec<String> = list.iter().map(|s| s.to_string()).collect();
    RegValue { kind: RegKind::MultiSz, data: multi_sz_bytes(&v) }
}

/// Shape of the dev PC's Realtek USB S/PDIF endpoint (live, 2026-09-30):
/// SFX/MFX processing modes = DEFAULT, association = nil GUID, no effect
/// CLSIDs — plus a synthetic vendor SFX, which must survive untouched.
fn spdif_like_store(vendor_sfx: bool, disable_sysfx: bool) -> FxStore {
    let mut store = FxStore::empty();
    let root = store.keys.get_mut("").unwrap();
    root.insert("{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},0".into(), sz(NIL));
    root.insert("{d3993a3f-99c2-4402-b5ec-a92a0367664b},5".into(), msz(&[ids::MODE_DEFAULT]));
    root.insert(ids::PKEY_MFX_MODES.into(), msz(&[ids::MODE_DEFAULT]));
    if vendor_sfx {
        root.insert(SFX.into(), sz(VENDOR_SFX));
    }
    if disable_sysfx {
        root.insert(
            ids::PKEY_DISABLE_SYSFX.into(),
            RegValue { kind: RegKind::Dword, data: vec![1, 0, 0, 0] },
        );
    }
    store
}

const NIL: &str = "{00000000-0000-0000-0000-000000000000}";
const SFX: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5";
const VENDOR_SFX: &str = "{11111111-2222-3333-4444-555555555555}";

#[test]
fn free_mfx_slot_takes_legacy_mfx_and_keeps_vendor_sfx() {
    let original = spdif_like_store(true, false);
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let changed = diff(&original, &plan.new_store);
    // Modes already list DEFAULT; the only change is the MFX slot itself.
    assert_eq!(changed, vec![(String::new(), ids::PKEY_FX_MODE_EFFECT_CLSID.to_owned())]);
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_MODE_EFFECT_CLSID).unwrap().data).as_deref(),
        Some(ids::APO_CLSID)
    );
    assert_eq!(sz_from_bytes(&root.get(SFX).unwrap().data).as_deref(), Some(VENDOR_SFX));
    // No EFX value is written any more (the S42c live finding).
    assert!(root.get(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID).is_none());
    assert!(root.get(ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID).is_none());
    assert!(vet_fx_diff(&plan).is_ok());
    assert_eq!(plan_uninstall(&plan.backup), original);
    assert_eq!(
        serialize(&plan_uninstall(&plan.backup).to_reg_map(ENDPOINT)),
        serialize(&original.to_reg_map(ENDPOINT))
    );
}

#[test]
fn nil_guid_mfx_slot_counts_as_free() {
    let mut original = spdif_like_store(false, false);
    original.keys.get_mut("").unwrap().insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), sz(NIL));
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_MODE_EFFECT_CLSID).unwrap().data).as_deref(),
        Some(ids::APO_CLSID)
    );
    assert_eq!(plan_uninstall(&plan.backup), original);
}

#[test]
fn composite_mfx_chain_is_joined_not_replaced() {
    let vendor_mfx = "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}";
    let mut original = spdif_like_store(true, false);
    {
        let root = original.keys.get_mut("").unwrap();
        root.insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), sz(vendor_mfx));
        root.insert(ids::PKEY_COMPOSITEFX_MODE_EFFECT_CLSID.into(), msz(&[vendor_mfx]));
    }
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let root = plan.new_store.keys.get("").unwrap();
    assert_eq!(
        parse_multi_sz(&root.get(ids::PKEY_COMPOSITEFX_MODE_EFFECT_CLSID).unwrap().data),
        vec![vendor_mfx.to_owned(), ids::APO_CLSID.to_owned()]
    );
    // The legacy slot still names the vendor.
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_MODE_EFFECT_CLSID).unwrap().data).as_deref(),
        Some(vendor_mfx)
    );
    assert_eq!(
        diff(&original, &plan.new_store),
        vec![(String::new(), ids::PKEY_COMPOSITEFX_MODE_EFFECT_CLSID.to_owned())]
    );
    assert_eq!(plan_uninstall(&plan.backup), original);
}

#[test]
fn disabled_enhancements_are_reenabled_and_restored() {
    let original = spdif_like_store(false, true);
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let root = plan.new_store.keys.get("").unwrap();
    assert!(root.get(ids::PKEY_DISABLE_SYSFX).is_none());
    let mut changed = diff(&original, &plan.new_store);
    changed.sort();
    let mut want = vec![
        (String::new(), ids::PKEY_FX_MODE_EFFECT_CLSID.to_owned()),
        (String::new(), ids::PKEY_DISABLE_SYSFX.to_owned()),
    ];
    want.sort();
    assert_eq!(changed, want);
    let restored = plan_uninstall(&plan.backup);
    assert_eq!(restored, original);
    assert_eq!(
        restored.keys.get("").unwrap().get(ids::PKEY_DISABLE_SYSFX).unwrap().data,
        vec![1, 0, 0, 0]
    );
}

#[test]
fn install_is_idempotent() {
    let original = spdif_like_store(true, false);
    let once = plan_install(&original, ENDPOINT, DLL).unwrap().new_store;
    let twice = plan_install(&once, ENDPOINT, DLL).unwrap().new_store;
    assert_eq!(once, twice, "second install adds nothing");
}

#[test]
fn install_on_empty_store() {
    let empty = FxStore::empty();
    let plan = plan_install(&empty, ENDPOINT, DLL).unwrap();
    let root = plan.new_store.keys.get("").expect("root");
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_MODE_EFFECT_CLSID).expect("MFX").data).as_deref(),
        Some(ids::APO_CLSID)
    );
    let modes = root.get(ids::PKEY_MFX_MODES).expect("MFX modes");
    assert_eq!(parse_multi_sz(&modes.data), vec![ids::MODE_DEFAULT.to_owned()]);
    assert_eq!(plan_uninstall(&plan.backup), empty);
}

#[test]
fn vetting_refuses_any_other_value_name() {
    let mut plan = plan_install(&spdif_like_store(false, false), ENDPOINT, DLL).unwrap();
    assert!(vet_fx_diff(&plan).is_ok());
    // A plan that would touch the vendor SFX slot is refused...
    plan.new_store.keys.get_mut("").unwrap().insert(SFX.into(), sz(ids::APO_CLSID));
    assert!(vet_fx_diff(&plan).is_err());
    // ...as is one that writes into a sub-key.
    let mut plan = plan_install(&spdif_like_store(false, false), ENDPOINT, DLL).unwrap();
    let mut sub = relay_apo::regfile::ValueMap::new();
    sub.insert(ids::PKEY_FX_MODE_EFFECT_CLSID.into(), sz(ids::APO_CLSID));
    plan.new_store.keys.insert("Sub".into(), sub);
    assert!(vet_fx_diff(&plan).is_err());
    // The old EFX names are no longer writable.
    let mut plan = plan_install(&spdif_like_store(false, false), ENDPOINT, DLL).unwrap();
    plan.new_store
        .keys
        .get_mut("")
        .unwrap()
        .insert(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID.into(), sz(ids::APO_CLSID));
    assert!(vet_fx_diff(&plan).is_err());
}

#[test]
fn backup_json_round_trips_losslessly() {
    let original = spdif_like_store(true, true);
    let plan = plan_install(&original, ENDPOINT, DLL).unwrap();
    let json = serde_json::to_string(&plan.backup).expect("serializes");
    let back: Backup = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, plan.backup);
    assert_eq!(back.store, original);
}

#[test]
fn com_keys_are_scoped_to_our_clsid() {
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL).unwrap();
    let ae = ids::audio_engine_key(ids::APO_CLSID);
    for path in plan.com_keys.keys() {
        assert!(
            path.starts_with(&ids::clsid_key(ids::APO_CLSID)) || path == ae,
            "machine-wide key outside our two CLSID keys: {path}"
        );
    }
    assert!(plan.com_keys.contains_key(&ae), "the audio-engine registration is planned");
    assert_eq!(plan.backup.com_keys, plan.com_keys.keys().map(str::to_owned).collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------
// S42b: the audio-engine registration, against the dev PC's real export of
// HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects (read-only
// `reg export`, 2026-09-30; 22 APOs, Relay's absent — the live finding).

fn ae_fixture_text() -> String {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/audioengine-apos-baseline.reg"
    ))
    .expect("audio-engine fixture present");
    decode_bytes(&bytes).expect("fixture decodes")
}

/// Apply a plan's machine-wide keys to an HKLM image (what livereg's
/// `apply_install` does to those keys).
fn apply_machine_keys(
    map: &relay_apo::regfile::KeyMap,
    plan: &relay_apo::fxstore::InstallPlan,
) -> relay_apo::regfile::KeyMap {
    let mut out = map.clone();
    for (path, values) in plan.com_keys.iter() {
        out.insert(format!(r"HKEY_LOCAL_MACHINE\{path}"), values.clone());
    }
    out
}

/// Remove a backup's recorded machine-wide keys (livereg's `restore`:
/// delete_tree each, deepest first).
fn remove_machine_keys(
    map: &relay_apo::regfile::KeyMap,
    keys: &[String],
) -> relay_apo::regfile::KeyMap {
    map.iter()
        .filter(|(path, _)| {
            !keys.iter().any(|k| {
                let full = format!(r"HKEY_LOCAL_MACHINE\{k}").to_ascii_lowercase();
                let p = path.to_ascii_lowercase();
                p == full || p.starts_with(&format!(r"{full}\"))
            })
        })
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect()
}

#[test]
fn audio_engine_fixture_round_trips_and_lacks_relay() {
    let text = ae_fixture_text();
    let map = parse(&text).expect("parses");
    assert_eq!(serialize(&map), text);
    assert!(!text.to_ascii_lowercase().contains(&ids::APO_CLSID.to_ascii_lowercase()));
}

/// The key Relay adds reads exactly like a `RegisterAPO` registration:
/// same value names, same order, REG_SZ strings, REG_DWORD numbers.
#[test]
fn audio_engine_registration_matches_register_apo_shape() {
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL).unwrap();
    let base = parse(&ae_fixture_text()).unwrap();
    let after = serialize(&apply_machine_keys(&base, &plan));
    let want = format!(
        "[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\AudioEngine\\AudioProcessingObjects\\{clsid}]\r\n\
         \"FriendlyName\"=\"Relay Audio (per-game EQ)\"\r\n\
         \"Copyright\"=\"\u{a9} Relay\"\r\n\
         \"MajorVersion\"=dword:00000001\r\n\
         \"MinorVersion\"=dword:00000000\r\n\
         \"Flags\"=dword:0000000e\r\n\
         \"MinInputConnections\"=dword:00000001\r\n\
         \"MaxInputConnections\"=dword:00000001\r\n\
         \"MinOutputConnections\"=dword:00000001\r\n\
         \"MaxOutputConnections\"=dword:00000001\r\n\
         \"MaxInstances\"=dword:ffffffff\r\n\
         \"NumAPOInterfaces\"=dword:00000001\r\n\
         \"APOInterface0\"=\"{{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}}\"\r\n",
        clsid = ids::APO_CLSID
    );
    assert!(after.contains(&want), "{after}");

    // Every value name Relay writes is one a Windows-registered APO on this
    // PC also carries (names are what audiodg reads).
    let reference = base
        .get(r"HKEY_LOCAL_MACHINE\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{13AB3EBD-137E-4903-9D89-60BE8277FD17}")
        .expect("WM audio GFX APO in fixture");
    let ours = relay_apo::fxstore::audio_engine_values();
    let names = |m: &relay_apo::regfile::ValueMap| m.keys().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(names(&ours), names(reference));
    for (name, v) in ours.iter() {
        assert_eq!(v.kind, reference.get(name).unwrap().kind, "{name} type");
    }
}

/// Install then uninstall (last endpoint): the AudioProcessingObjects export
/// is byte-identical to the baseline; only Relay's key came and went.
#[test]
fn audio_engine_uninstall_restores_hklm_byte_for_byte() {
    let before_text = ae_fixture_text();
    let base = parse(&before_text).unwrap();
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL).unwrap();
    let installed = apply_machine_keys(&base, &plan);
    assert_eq!(installed.len(), base.len() + 3, "class, InprocServer32, audio-engine key");
    let restored = remove_machine_keys(&installed, &plan.backup.com_keys);
    assert_eq!(serialize(&restored), before_text);
    // Other APOs are untouched throughout.
    for (path, values) in base.iter() {
        assert_eq!(installed.get(path), Some(values), "{path}");
    }
}
