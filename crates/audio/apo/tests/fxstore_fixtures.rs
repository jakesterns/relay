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
    for path in plan.com_keys.keys() {
        assert!(
            path.starts_with(&ids::clsid_key(ids::APO_CLSID)),
            "COM key outside our CLSID: {path}"
        );
    }
    assert_eq!(plan.backup.com_keys, plan.com_keys.keys().map(str::to_owned).collect::<Vec<_>>());
}
