//! COM aggregation for the APO (S44d).
//!
//! audiodg creates every APO *aggregated*: `CreateInstance(pUnkOuter, IID_IUnknown)`
//! with its own wrapper as the outer object. Refusing (`CLASS_E_NOAGGREGATION`)
//! leaves the endpoint with no audio at all: the owner's RODECaster went silent
//! the first time the DLL actually loaded (2026-10-14, diag log).
//!
//! The rules aggregation imposes, and how they are kept:
//!
//! - The object handed back is a separate **inner, non-delegating IUnknown**
//!   ([`Inner`]) with its own count. Its `QueryInterface` answers `IUnknown`
//!   with itself and every other interface from the APO.
//! - Every interface pointer the APO hands out must **delegate**
//!   `QueryInterface` / `AddRef` / `Release` to the outer object. The
//!   `windows` crate's generated vtables cannot do that, so each of the APO's
//!   interface slots is pointed at a copy of its vtable whose first three
//!   entries are thunks: they look the pointer up in [`OUTERS`] and call the
//!   outer object's method. Every other entry, `APOProcess` included, is the
//!   generated one untouched, and the real-time path never reaches a thunk.
//! - A QI through the inner object returns a pointer whose reference is the
//!   **outer's**: the APO's own count is put back and the outer is AddRef'd.
//!   That is what lets the outer release itself after caching the pointer,
//!   as aggregating outers do, without destroying itself.
//! - The APO lives exactly as long as the inner object. On the last inner
//!   Release the slots are put back to the generated vtables and the APO is
//!   dropped.
//!
//! An APO created *without* an outer (the in-process tests, a host that does
//! not aggregate) is untouched: no slot is patched and no thunk ever runs.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::{IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT};
use windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER};

type Raw = *mut core::ffi::c_void;

/// Interface pointer -> its outer object, for every patched slot.
fn outers() -> &'static Mutex<HashMap<usize, usize>> {
    static M: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// Generated vtable -> patched copy, and back. At most one entry per
/// interface the APO implements; the copies live for the process.
fn patched_of() -> &'static Mutex<HashMap<usize, usize>> {
    static M: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    M.get_or_init(Default::default)
}
fn generated_of() -> &'static Mutex<HashMap<usize, usize>> {
    static M: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// # Safety
/// `p` is a live COM interface pointer.
unsafe fn vtbl(p: Raw) -> *const IUnknown_Vtbl {
    // SAFETY: a COM pointer points at its vtable pointer.
    unsafe { *(p as *const *const IUnknown_Vtbl) }
}

/// The generated vtable behind a (possibly patched) interface pointer.
unsafe fn generated(p: Raw) -> *const IUnknown_Vtbl {
    // SAFETY: caller's live pointer.
    let current = unsafe { vtbl(p) } as usize;
    generated_of().lock().unwrap().get(&current).copied().unwrap_or(current) as *const _
}

fn outer_for(p: Raw) -> Option<Raw> {
    outers().lock().unwrap().get(&(p as usize)).map(|&o| o as Raw)
}

unsafe extern "system" fn qi_thunk(this: Raw, iid: *const GUID, out: *mut Raw) -> HRESULT {
    // SAFETY: COM contract; the outer outlives every pointer it was given.
    unsafe {
        match outer_for(this) {
            Some(o) => ((*vtbl(o)).QueryInterface)(o, iid, out as *mut _),
            None => ((*generated(this)).QueryInterface)(this, iid, out as *mut _),
        }
    }
}

unsafe extern "system" fn add_ref_thunk(this: Raw) -> u32 {
    // SAFETY: as above.
    unsafe {
        match outer_for(this) {
            Some(o) => ((*vtbl(o)).AddRef)(o),
            None => ((*generated(this)).AddRef)(this),
        }
    }
}

unsafe extern "system" fn release_thunk(this: Raw) -> u32 {
    // SAFETY: as above.
    unsafe {
        match outer_for(this) {
            Some(o) => ((*vtbl(o)).Release)(o),
            None => ((*generated(this)).Release)(this),
        }
    }
}

/// Point `iface`'s slot at the delegating copy of its vtable (`slots`
/// entries long) and record its outer.
///
/// # Safety
/// `iface` is a live interface pointer of an object this module owns, whose
/// vtable has `slots` pointer-sized entries.
unsafe fn patch(iface: Raw, outer: Raw, slots: usize) {
    // SAFETY: per the contract; the copy is leaked, so it outlives the slot.
    unsafe {
        let slot = iface as *mut *const usize;
        let gen = *slot as usize;
        // Two IIDs can share one slot (an interface and the one it extends):
        // patch it once.
        if generated_of().lock().unwrap().contains_key(&gen) {
            outers().lock().unwrap().insert(iface as usize, outer as usize);
            return;
        }
        let mut patched = patched_of().lock().unwrap();
        let copy = *patched.entry(gen).or_insert_with(|| {
            let mut v: Vec<usize> = std::slice::from_raw_parts(gen as *const usize, slots).to_vec();
            v[0] = qi_thunk as *const () as usize;
            v[1] = add_ref_thunk as *const () as usize;
            v[2] = release_thunk as *const () as usize;
            let p = Box::leak(v.into_boxed_slice()).as_ptr() as usize;
            generated_of().lock().unwrap().insert(p, gen);
            p
        });
        drop(patched);
        outers().lock().unwrap().insert(iface as usize, outer as usize);
        *slot = copy as *const usize;
    }
}

/// Undo [`patch`].
unsafe fn unpatch(iface: Raw) {
    // SAFETY: `iface` was patched by this module and is still alive.
    unsafe {
        let gen = generated(iface);
        outers().lock().unwrap().remove(&(iface as usize));
        *(iface as *mut *const IUnknown_Vtbl) = gen;
    }
}

/// The inner, non-delegating IUnknown handed to the outer object.
#[repr(C)]
struct Inner {
    vtbl: *const IUnknown_Vtbl,
    refs: AtomicU32,
    /// The APO's own identity: holds it alive for exactly our lifetime.
    apo: Option<IUnknown>,
    outer: Raw,
    /// Its patched interface pointers.
    ifaces: Vec<Raw>,
}

static INNER_VTBL: IUnknown_Vtbl =
    IUnknown_Vtbl { QueryInterface: inner_qi, AddRef: inner_add_ref, Release: inner_release };

unsafe extern "system" fn inner_qi(
    this: Raw,
    iid: *const GUID,
    out: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: `this` is an Inner we allocated; caller's pointers checked.
    unsafe {
        *out = std::ptr::null_mut();
        let inner = &*(this as *const Inner);
        if *iid == IUnknown::IID {
            inner.refs.fetch_add(1, Ordering::AcqRel);
            *out = this;
            return HRESULT(0);
        }
        let Some(apo) = inner.apo.as_ref() else { return E_NOINTERFACE };
        let hr = apo.query(iid, out);
        crate::diag!("inner QI {:?} -> {:#010x}", *iid, hr.0 as u32);
        if hr.is_err() {
            return hr;
        }
        let p = *out;
        if !inner.ifaces.contains(&p) {
            // Not one of the slots we patched (an interface added later):
            // hand it out as the APO gave it rather than half-delegated.
            return hr;
        }
        // The reference belongs to the outer: put the APO's back.
        ((*generated(p)).Release)(p);
        ((*vtbl(inner.outer)).AddRef)(inner.outer);
        hr
    }
}

unsafe extern "system" fn inner_add_ref(this: Raw) -> u32 {
    // SAFETY: an Inner we allocated.
    unsafe { (*(this as *const Inner)).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn inner_release(this: Raw) -> u32 {
    // SAFETY: an Inner we allocated with Box::into_raw; freed once, at zero.
    unsafe {
        let left = (*(this as *const Inner)).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        crate::diag!("inner Release -> {left}");
        if left == 0 {
            let mut inner = Box::from_raw(this as *mut Inner);
            for &p in &inner.ifaces {
                unpatch(p);
            }
            inner.apo = None;
        }
        left
    }
}

/// The APO's interfaces and their vtable lengths, for the patching.
pub struct Iface {
    pub iid: GUID,
    pub slots: usize,
}

/// Wrap `apo` (its identity IUnknown) for `outer` and return the inner
/// IUnknown, with one reference the caller owns.
///
/// # Safety
/// `outer` is the live outer object; `ifaces` lists interfaces `apo`
/// implements with their exact vtable lengths.
pub unsafe fn create(apo: IUnknown, outer: Raw, ifaces: &[Iface]) -> Raw {
    let mut patched = Vec::new();
    for i in ifaces {
        let mut p: Raw = std::ptr::null_mut();
        // SAFETY: valid out pointer; a successful QI returns a live pointer
        // with one extra reference on the APO, given straight back.
        unsafe {
            if apo.query(&i.iid, &mut p).is_ok() && !p.is_null() {
                patch(p, outer, i.slots);
                ((*generated(p)).Release)(p);
                if !patched.contains(&p) {
                    patched.push(p);
                }
            }
        }
    }
    let inner = Box::new(Inner {
        vtbl: &INNER_VTBL,
        refs: AtomicU32::new(1),
        apo: Some(apo),
        outer,
        ifaces: patched,
    });
    Box::into_raw(inner) as Raw
}
