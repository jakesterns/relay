//! Proof of the real-time contract: after `prepare()`, `process()` never
//! touches the allocator — counted by a wrapping global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static DEALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: delegates 1:1 to the system allocator; only counts around it.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: Counting = Counting;

use relay_audio::dsp::biquad::BandParams;
use relay_audio::dsp::limiter::LimiterParams;
use relay_audio::dsp::{Chain, ChainParams};

#[test]
fn process_is_allocation_free_after_prepare() {
    // The heaviest chain we ship: 16 bands, band-split limiter, HRTF.
    let params = ChainParams {
        bands: (0..16)
            .map(|i| {
                BandParams::peaking(
                    60.0 * (i + 1) as f32 * 1.5,
                    if i % 2 == 0 { 4.0 } else { -3.0 },
                    1.2,
                )
            })
            .collect(),
        limiter: Some(LimiterParams::new(120.0, -10.0)),
        hrtf: true,
    };
    let mut chain = Chain::new(params);
    chain.prepare(48_000, 480).unwrap();

    let input: Vec<f32> = (0..960).map(|i| ((i as f32) * 0.13).sin() * 0.5).collect();
    let mut output = vec![0.0f32; 960];

    // Warm up (first partitions, lazy anything).
    for _ in 0..16 {
        chain.process(&input, &mut output);
    }

    let a0 = ALLOCS.load(Ordering::SeqCst);
    let d0 = DEALLOCS.load(Ordering::SeqCst);
    for _ in 0..2000 {
        chain.process(&input, &mut output);
    }
    // Also the bypass path, which must be a bare copy.
    chain.set_bypass(true);
    for _ in 0..100 {
        chain.process(&input, &mut output);
    }
    let a1 = ALLOCS.load(Ordering::SeqCst);
    let d1 = DEALLOCS.load(Ordering::SeqCst);

    assert_eq!(a1 - a0, 0, "process() allocated {} times", a1 - a0);
    assert_eq!(d1 - d0, 0, "process() deallocated {} times", d1 - d0);
}
