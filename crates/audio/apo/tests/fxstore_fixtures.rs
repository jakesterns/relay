//! The uninstall proof, against the real endpoint export taken as this
//! milestone's baseline (a RODECaster Duo endpoint that ships a vendor FX
//! chain — exactly the store risk #2 worries about).
//!
//! Nothing here touches the live registry: install/uninstall run on the
//! parsed image, and "byte-for-byte" is proven two ways — deep equality of
//! every key/value/byte, and string equality of the re-serialized .reg
//! export (the same diff a VM run will do with two real `reg export`s).

use relay_apo::fxstore::{diff, plan_install, plan_uninstall, Backup, FxStore};
use relay_apo::ids;
use relay_apo::regfile::{decode_bytes, parse, parse_multi_sz, serialize, sz_from_bytes};

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

#[test]
fn install_then_uninstall_restores_byte_for_byte() {
    let original = fixture_store();
    let plan = plan_install(&original, ENDPOINT, DLL);

    // The install visibly changed the store…
    assert_ne!(plan.new_store, original);

    // …and the uninstall puts back the exact original: every key, every
    // value name, kind and data byte…
    let restored = plan_uninstall(&plan.backup);
    assert_eq!(restored, original);
    assert!(diff(&original, &restored).is_empty());

    // …including at the "two registry exports diff clean" level.
    assert_eq!(
        serialize(&restored.to_reg_map(ENDPOINT)),
        serialize(&original.to_reg_map(ENDPOINT)),
    );
}

#[test]
fn install_touches_exactly_the_three_pkeys() {
    let original = fixture_store();
    let plan = plan_install(&original, ENDPOINT, DLL);
    let mut changed = diff(&original, &plan.new_store);
    changed.sort();
    // The fixture's legacy EFX slot is empty, so all three are written; all
    // on the FxProperties key itself, none in any vendor sub-key.
    let mut expected = vec![
        (String::new(), ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID.to_owned()),
        (String::new(), ids::PKEY_FX_ENDPOINT_EFFECT_CLSID.to_owned()),
        (String::new(), ids::PKEY_EFX_MODES.to_owned()),
    ];
    expected.sort();
    assert_eq!(changed, expected);
}

#[test]
fn install_preserves_vendor_chain_and_modes() {
    let original = fixture_store();
    let plan = plan_install(&original, ENDPOINT, DLL);
    let root = plan.new_store.keys.get("").expect("root key");

    // Composite EFX now lists (exactly) our CLSID — nothing was evicted,
    // the fixture had no composite EFX chain.
    let efx = root.get(ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID).expect("composite EFX");
    assert_eq!(parse_multi_sz(&efx.data), vec![ids::APO_CLSID.to_owned()]);

    // The vendor's SFX/MFX slots are untouched.
    let sfx = root.get("{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5").expect("vendor SFX");
    assert_eq!(sz_from_bytes(&sfx.data).as_deref(), Some("{C9453E73-8C5C-4463-9984-AF8BAB2F5447}"));

    // Modes: DEFAULT appended to the EFX modes list.
    let modes = root.get(ids::PKEY_EFX_MODES).expect("EFX modes");
    assert!(parse_multi_sz(&modes.data).contains(&ids::MODE_DEFAULT.to_owned()));
}

#[test]
fn install_is_idempotent() {
    let original = fixture_store();
    let once = plan_install(&original, ENDPOINT, DLL).new_store;
    let twice = plan_install(&once, ENDPOINT, DLL).new_store;
    assert_eq!(once, twice, "second install adds nothing");
}

#[test]
fn install_on_empty_store() {
    let empty = FxStore::empty();
    let plan = plan_install(&empty, ENDPOINT, DLL);
    let root = plan.new_store.keys.get("").expect("root");
    let efx = root.get(ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID).expect("composite EFX");
    assert_eq!(parse_multi_sz(&efx.data), vec![ids::APO_CLSID.to_owned()]);
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID).expect("legacy EFX").data)
            .as_deref(),
        Some(ids::APO_CLSID)
    );
    // And back to nothing.
    assert_eq!(plan_uninstall(&plan.backup), empty);
}

#[test]
fn occupied_legacy_efx_slot_is_left_alone() {
    let mut store = FxStore::empty();
    let vendor = "{11111111-2222-3333-4444-555555555555}";
    {
        let root = store.keys.get_mut("").unwrap();
        root.insert(
            ids::PKEY_FX_ENDPOINT_EFFECT_CLSID.to_owned(),
            relay_apo::regfile::RegValue {
                kind: relay_apo::regfile::RegKind::Sz,
                data: relay_apo::regfile::sz_bytes(vendor),
            },
        );
    }
    let plan = plan_install(&store, ENDPOINT, DLL);
    let root = plan.new_store.keys.get("").unwrap();
    // Vendor EFX kept; we ride the composite chain only.
    assert_eq!(
        sz_from_bytes(&root.get(ids::PKEY_FX_ENDPOINT_EFFECT_CLSID).unwrap().data).as_deref(),
        Some(vendor)
    );
    let efx = root.get(ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID).unwrap();
    assert_eq!(parse_multi_sz(&efx.data), vec![ids::APO_CLSID.to_owned()]);
}

#[test]
fn backup_json_round_trips_losslessly() {
    let original = fixture_store();
    let plan = plan_install(&original, ENDPOINT, DLL);
    let json = serde_json::to_string(&plan.backup).expect("serializes");
    let back: Backup = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, plan.backup);
    // The store inside survives with every byte intact.
    assert_eq!(back.store, original);
}

#[test]
fn com_keys_are_scoped_to_our_clsid() {
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL);
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
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL);
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
    let plan = plan_install(&FxStore::empty(), ENDPOINT, DLL);
    let installed = apply_machine_keys(&base, &plan);
    assert_eq!(installed.len(), base.len() + 3, "class, InprocServer32, audio-engine key");
    let restored = remove_machine_keys(&installed, &plan.backup.com_keys);
    assert_eq!(serialize(&restored), before_text);
    // Other APOs are untouched throughout.
    for (path, values) in base.iter() {
        assert_eq!(installed.get(path), Some(values), "{path}");
    }
}
