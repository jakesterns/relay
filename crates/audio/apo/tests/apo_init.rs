//! S42c: what audiodg sees before any audio flows — the QI table, the
//! registration properties against the registry values the installer
//! writes, and Initialize with each engine payload size. The v1 case is the
//! regression: an APO without IAudioSystemEffects2 is initialised with
//! APOInitSystemEffects, and we used to drop its endpoint store, so the
//! params section was never created.

#![cfg(windows)]
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use relay_apo::com::{answers, expected_iids, init_kind, InitKind, RelayApo, CLSID_RELAY_APO};
use relay_audio::shm::{section_name, SharedParams};
use windows::core::{implement, IUnknown, Interface, BOOL};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Apo::{
    APOInitBaseStruct, APOInitSystemEffects, APOInitSystemEffects2, APOInitSystemEffects3,
    IAudioProcessingObject,
};
use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;
use windows::Win32::System::Com::CoTaskMemAlloc;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, IPropertyStore_Impl};

/// Endpoint property store with just PKEY_AudioEndpoint_GUID.
#[implement(IPropertyStore)]
struct Store(String);

impl IPropertyStore_Impl for Store_Impl {
    fn GetCount(&self) -> windows::core::Result<u32> {
        Ok(1)
    }
    fn GetAt(&self, _i: u32, k: *mut PROPERTYKEY) -> windows::core::Result<()> {
        // SAFETY: caller-provided out-pointer.
        unsafe { *k = PKEY_AudioEndpoint_GUID };
        Ok(())
    }
    fn GetValue(&self, key: *const PROPERTYKEY) -> windows::core::Result<PROPVARIANT> {
        // SAFETY: valid key pointer from the caller.
        if unsafe { *key } != PKEY_AudioEndpoint_GUID {
            return Ok(PROPVARIANT::default());
        }
        let w: Vec<u16> = self.0.encode_utf16().chain(Some(0)).collect();
        // SAFETY: CoTaskMem string, owned by the PROPVARIANT (PropVariantClear
        // frees it); vt/pwszVal written through the raw union.
        unsafe {
            let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            let mut v = PROPVARIANT::default();
            let inner = &mut v.Anonymous.Anonymous;
            inner.vt = VT_LPWSTR;
            inner.Anonymous.pwszVal = windows::core::PWSTR(p);
            Ok(v)
        }
    }
    fn SetValue(&self, _: *const PROPERTYKEY, _: *const PROPVARIANT) -> windows::core::Result<()> {
        Ok(())
    }
    fn Commit(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

fn base<T>() -> APOInitBaseStruct {
    APOInitBaseStruct { cbSize: std::mem::size_of::<T>() as u32, clsid: CLSID_RELAY_APO }
}

fn bytes<T>(v: &T) -> &[u8] {
    // SAFETY: plain view of a live struct for the duration of the borrow.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

#[test]
fn qi_table_answers_every_engine_interface() {
    let apo: IAudioProcessingObject = RelayApo::default().into();
    let unk: IUnknown = apo.cast().unwrap();
    for (name, id) in expected_iids() {
        assert!(answers(&unk, &id), "{name} not answered");
    }
    // Not implemented, so must say no (the engine then sends v2, not v3).
    let fx3 = windows::Win32::Media::Audio::Apo::IAudioSystemEffects3::IID;
    assert!(!answers(&unk, &fx3));
}

#[test]
fn registration_properties_match_registry_values() {
    let apo: IAudioProcessingObject = RelayApo::default().into();
    // SAFETY: CoTaskMem struct; read then leaked (test process).
    unsafe {
        let p = &*apo.GetRegistrationProperties().unwrap();
        assert_eq!(p.clsid, CLSID_RELAY_APO);
        assert_eq!(p.Flags.0 as u32, relay_apo::ids::APO_REG_FLAGS);
        assert_eq!(p.u32NumAPOInterfaces, 1);
        assert_eq!(
            format!("{{{:?}}}", p.iidAPOInterfaceList[0]),
            relay_apo::ids::IID_IAUDIO_PROCESSING_OBJECT
        );
        assert_eq!((p.u32MinInputConnections, p.u32MaxInputConnections), (1, 1));
        assert_eq!((p.u32MinOutputConnections, p.u32MaxOutputConnections), (1, 1));
    }
}

#[test]
fn init_kind_by_size() {
    use std::mem::size_of;
    assert_eq!(init_kind(0), InitKind::None);
    assert_eq!(init_kind(size_of::<APOInitSystemEffects>()), InitKind::V1);
    assert_eq!(init_kind(size_of::<APOInitSystemEffects2>()), InitKind::V2);
    assert_eq!(init_kind(size_of::<APOInitSystemEffects3>()), InitKind::V3);
}

/// One test for everything touching process-wide env vars and sections.
#[test]
fn initialize_every_payload_creates_the_section() {
    std::env::set_var("RELAY_APO_LOCAL_SECTION", "1");
    std::env::remove_var("RELAY_APO_ENDPOINT_OVERRIDE");
    let inst = format!("apoinit{}", std::process::id());
    std::env::set_var("RELAY_INSTANCE", &inst);
    let local = |ep: &str| section_name(ep, &inst).replacen("Global\\", "Local\\", 1);

    // v1: the struct the engine sends an APO without IAudioSystemEffects2.
    let ep1 = "{feedface-0000-4000-8000-0000000000a1}";
    let store: IPropertyStore = Store(ep1.into()).into();
    let v1 = APOInitSystemEffects {
        APOInit: base::<APOInitSystemEffects>(),
        pAPOEndpointProperties: ManuallyDrop::new(Some(store.clone())),
        ..Default::default()
    };
    let a1: IAudioProcessingObject = RelayApo::default().into();
    // SAFETY: payload is a live, correctly sized struct.
    unsafe { a1.Initialize(bytes(&v1)) }.expect("v1 initialize");
    let _held1 = SharedParams::open(&local(ep1)).expect("v1 created the section");

    // v2: normal mode.
    let ep2 = "{feedface-0000-4000-8000-0000000000a2}";
    let store2: IPropertyStore = Store(ep2.into()).into();
    let v2 = APOInitSystemEffects2 {
        APOInit: base::<APOInitSystemEffects2>(),
        pAPOEndpointProperties: ManuallyDrop::new(Some(store2.clone())),
        ..Default::default()
    };
    let a2: IAudioProcessingObject = RelayApo::default().into();
    // SAFETY: as above.
    unsafe { a2.Initialize(bytes(&v2)) }.expect("v2 initialize");
    let _held2 = SharedParams::open(&local(ep2)).expect("v2 created the section");

    // v2 discovery-only: accepted, no section.
    let ep3 = "{feedface-0000-4000-8000-0000000000a3}";
    let store3: IPropertyStore = Store(ep3.into()).into();
    let v2d = APOInitSystemEffects2 {
        APOInit: base::<APOInitSystemEffects2>(),
        pAPOEndpointProperties: ManuallyDrop::new(Some(store3.clone())),
        InitializeForDiscoveryOnly: BOOL(1),
        ..Default::default()
    };
    let a3: IAudioProcessingObject = RelayApo::default().into();
    // SAFETY: as above.
    unsafe { a3.Initialize(bytes(&v2d)) }.expect("discovery initialize");
    assert!(SharedParams::open(&local(ep3)).is_err(), "discovery must not create a section");

    // v3 (in case a future build implements IAudioSystemEffects3).
    let ep4 = "{feedface-0000-4000-8000-0000000000a4}";
    let store4: IPropertyStore = Store(ep4.into()).into();
    let v3 = APOInitSystemEffects3 {
        APOInit: base::<APOInitSystemEffects3>(),
        pAPOEndpointProperties: ManuallyDrop::new(Some(store4.clone())),
        ..Default::default()
    };
    let a4: IAudioProcessingObject = RelayApo::default().into();
    // SAFETY: as above.
    unsafe { a4.Initialize(bytes(&v3)) }.expect("v3 initialize");
    let _held4 = SharedParams::open(&local(ep4)).expect("v3 created the section");

    // Second Initialize is refused.
    // SAFETY: as above.
    assert!(unsafe { a1.Initialize(bytes(&v1)) }.is_err());
}
