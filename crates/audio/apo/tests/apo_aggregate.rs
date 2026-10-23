//! S44d: the APO created aggregated, the way audiodg creates every APO. A
//! hand-made outer object plays audiodg's wrapper: it counts its own
//! references and forwards QueryInterface for the APO's interfaces to the
//! inner IUnknown, as an aggregating outer does. Proves the inner object is
//! what comes back, that interface pointers delegate AddRef/Release/QI to the
//! outer, that the outer can release itself after caching a pointer (the
//! aggregation idiom) without dying, and that the APO works and is freed.

#![cfg(windows)]
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU32, Ordering};

use relay_apo::com::create_instance;
use windows::core::{IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_NOINTERFACE};
use windows::Win32::Media::Audio::Apo::{
    IAudioProcessingObject, IAudioProcessingObjectRT, IAudioSystemEffects2,
};

type Raw = *mut core::ffi::c_void;

#[repr(C)]
struct Outer {
    vtbl: *const IUnknown_Vtbl,
    refs: AtomicU32,
    inner: Raw,
}

unsafe extern "system" fn outer_qi(this: Raw, iid: *const GUID, out: *mut Raw) -> HRESULT {
    unsafe {
        let o = &*(this as *const Outer);
        if *iid == IUnknown::IID {
            o.refs.fetch_add(1, Ordering::AcqRel);
            *out = this;
            return HRESULT(0);
        }
        if o.inner.is_null() {
            *out = std::ptr::null_mut();
            return E_NOINTERFACE;
        }
        let vt = *(o.inner as *const *const IUnknown_Vtbl);
        ((*vt).QueryInterface)(o.inner, iid, out as *mut _)
    }
}
unsafe extern "system" fn outer_add_ref(this: Raw) -> u32 {
    unsafe { (*(this as *const Outer)).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}
unsafe extern "system" fn outer_release(this: Raw) -> u32 {
    unsafe { (*(this as *const Outer)).refs.fetch_sub(1, Ordering::AcqRel) - 1 }
}
static OUTER_VTBL: IUnknown_Vtbl =
    IUnknown_Vtbl { QueryInterface: outer_qi, AddRef: outer_add_ref, Release: outer_release };

fn refs(o: &Outer) -> u32 {
    o.refs.load(Ordering::Acquire)
}

unsafe fn qi(p: Raw, iid: &GUID) -> Result<Raw, HRESULT> {
    unsafe {
        let mut out: Raw = std::ptr::null_mut();
        let vt = *(p as *const *const IUnknown_Vtbl);
        let hr = ((*vt).QueryInterface)(p, iid, &mut out as *mut Raw as *mut _);
        if hr.is_ok() {
            Ok(out)
        } else {
            Err(hr)
        }
    }
}
unsafe fn release(p: Raw) -> u32 {
    unsafe { ((**(p as *const *const IUnknown_Vtbl)).Release)(p) }
}
unsafe fn add_ref(p: Raw) -> u32 {
    unsafe { ((**(p as *const *const IUnknown_Vtbl)).AddRef)(p) }
}

#[test]
fn aggregated_apo_delegates_to_the_outer_and_works() {
    let mut outer =
        Box::new(Outer { vtbl: &OUTER_VTBL, refs: AtomicU32::new(1), inner: std::ptr::null_mut() });
    let outer_raw = &mut *outer as *mut Outer as Raw;
    unsafe {
        // Only IID_IUnknown may be asked for with an outer.
        let mut obj: Raw = std::ptr::null_mut();
        let hr = create_instance(Some(outer_raw), &IAudioProcessingObject::IID, &mut obj);
        assert_eq!(hr, CLASS_E_NOAGGREGATION);
        assert!(obj.is_null());

        let hr = create_instance(Some(outer_raw), &IUnknown::IID, &mut obj);
        assert!(hr.is_ok(), "{hr:?}");
        assert!(!obj.is_null());
        outer.inner = obj;
        assert_eq!(refs(&outer), 1, "creating does not touch the outer's count");

        // The inner QI for IUnknown is the inner itself.
        let inner_again = qi(obj, &IUnknown::IID).unwrap();
        assert_eq!(inner_again, obj);
        release(inner_again);

        // An interface through the inner: the reference is the outer's.
        let apo = qi(obj, &IAudioProcessingObject::IID).unwrap();
        assert_eq!(refs(&outer), 2);
        // The aggregation idiom: the outer releases itself after caching.
        outer_release(outer_raw);
        assert_eq!(refs(&outer), 1);

        // AddRef/Release on the interface delegate to the outer.
        assert_eq!(add_ref(apo), 2);
        assert_eq!(release(apo), 1);

        // QI through the interface goes to the outer, which asks the inner.
        let rt = qi(apo, &IAudioProcessingObjectRT::IID).unwrap();
        assert_eq!(refs(&outer), 2);
        let fx2 = qi(apo, &IAudioSystemEffects2::IID).unwrap();
        assert_eq!(refs(&outer), 3);
        // QI for IUnknown through an interface answers the outer: identity.
        let id = qi(apo, &IUnknown::IID).unwrap();
        assert_eq!(id, outer_raw);
        release(id);
        release(fx2);
        release(rt);
        assert_eq!(refs(&outer), 1);

        // Marshaling: the inner answers IMarshal only through the FTM aggregated
        // on the outer; the windows object's own IAgileObject is not exposed.
        const IID_IMARSHAL: GUID = GUID::from_u128(0x00000003_0000_0000_c000_000000000046);
        const IID_IAGILE: GUID = GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);
        // IMarshal: the free-threaded marshaler, aggregated on the outer, so
        // its reference is the outer's (what audiodg needs after Lock).
        let m = qi(obj, &IID_IMARSHAL).expect("IMarshal answered");
        assert_eq!(refs(&outer), 2);
        release(m);
        assert_eq!(refs(&outer), 1);
        assert_eq!(qi(obj, &IID_IAGILE).unwrap_err(), E_NOINTERFACE);
        assert_eq!(refs(&outer), 1, "a refused QI leaves the outer alone");

        // The APO itself works through the delegating pointer.
        let typed = IAudioProcessingObject::from_raw_borrowed(&apo).unwrap();
        assert!(typed.GetLatency().is_ok());

        // Last inner release frees the APO; the outer is untouched.
        assert_eq!(release(obj), 0);
        outer.inner = std::ptr::null_mut();
        assert_eq!(refs(&outer), 1);
    }
}

#[test]
fn without_an_outer_nothing_changes() {
    unsafe {
        let mut obj: Raw = std::ptr::null_mut();
        let hr = create_instance(None, &IAudioProcessingObject::IID, &mut obj);
        assert!(hr.is_ok());
        let apo = IAudioProcessingObject::from_raw(obj);
        assert!(apo.GetLatency().is_ok());
        let rt: IAudioProcessingObjectRT = apo.cast().unwrap();
        drop(rt);
        drop(apo);
    }
}
