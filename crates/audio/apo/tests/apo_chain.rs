//! S44b: Relay's APO hosting the APO it displaced from the MFX slot, driven
//! in-process with a fake child COM object (a -6 dB gain that refuses
//! 44.1 kHz). Proves Initialize/negotiation/lock/unlock forwarding, the
//! Relay-then-child order, bypass = child alone, the merged effects list,
//! and that a child which fails Initialize leaves Relay running alone.

#![cfg(windows)]
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use relay_apo::com::RelayApo;
use relay_audio::params::{BandParams, ChainParams};
use relay_audio::shm::{section_name, SharedParams};
use windows::core::{implement, Interface, Ref, BOOL, GUID};
use windows::Win32::Foundation::{E_FAIL, HANDLE};
use windows::Win32::Media::Audio::Apo::*;
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CoTaskMemAlloc;

#[implement(IAudioMediaType)]
struct TestMediaType {
    format: UNCOMPRESSEDAUDIOFORMAT,
    wfx: Box<WAVEFORMATEX>,
}

fn f32_stereo(rate: f32) -> IAudioMediaType {
    TestMediaType {
        format: UNCOMPRESSEDAUDIOFORMAT {
            guidFormatType: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            dwSamplesPerFrame: 2,
            dwBytesPerSampleContainer: 4,
            dwValidBitsPerSample: 32,
            fFramesPerSecond: rate,
            dwChannelMask: 0x3,
        },
        wfx: Box::new(WAVEFORMATEX::default()),
    }
    .into()
}

impl IAudioMediaType_Impl for TestMediaType_Impl {
    fn IsCompressedFormat(&self) -> windows::core::Result<BOOL> {
        Ok(false.into())
    }
    fn IsEqual(&self, _other: Ref<IAudioMediaType>) -> windows::core::Result<u32> {
        Ok(0)
    }
    fn GetAudioFormat(&self) -> *mut WAVEFORMATEX {
        &*self.wfx as *const WAVEFORMATEX as *mut WAVEFORMATEX
    }
    fn GetUncompressedAudioFormat(
        &self,
        out: *mut UNCOMPRESSEDAUDIOFORMAT,
    ) -> windows::core::Result<()> {
        // SAFETY: caller-provided out pointer.
        unsafe { out.write(self.format) };
        Ok(())
    }
}

/// What the fake child saw.
#[derive(Default)]
struct Seen {
    init: AtomicUsize,
    init_size: AtomicUsize,
    locked: AtomicUsize,
    unlocked: AtomicUsize,
    processed: AtomicUsize,
    lock_frames: AtomicU32,
}

const CHILD_EFFECT: GUID = GUID::from_u128(0x0badc0de_0000_4000_8000_000000000042);

/// A child APO: output = 0.5 * input; refuses 44.1 kHz; latency 1000 hns.
#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects,
    IAudioSystemEffects2
)]
struct FakeChild {
    seen: Arc<Seen>,
    fail_init: bool,
}

impl IAudioProcessingObject_Impl for FakeChild_Impl {
    fn Reset(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn GetLatency(&self) -> windows::core::Result<i64> {
        Ok(1000)
    }
    fn GetRegistrationProperties(&self) -> windows::core::Result<*mut APO_REG_PROPERTIES> {
        Err(E_FAIL.into())
    }
    fn Initialize(&self, size: u32, _data: *const u8) -> windows::core::Result<()> {
        self.seen.init.fetch_add(1, Ordering::SeqCst);
        self.seen.init_size.store(size as usize, Ordering::SeqCst);
        if self.fail_init {
            Err(E_FAIL.into())
        } else {
            Ok(())
        }
    }
    fn IsInputFormatSupported(
        &self,
        _o: Ref<IAudioMediaType>,
        r: Ref<IAudioMediaType>,
    ) -> windows::core::Result<IAudioMediaType> {
        check(r)
    }
    fn IsOutputFormatSupported(
        &self,
        _o: Ref<IAudioMediaType>,
        r: Ref<IAudioMediaType>,
    ) -> windows::core::Result<IAudioMediaType> {
        check(r)
    }
    fn GetInputChannelCount(&self) -> windows::core::Result<u32> {
        Ok(2)
    }
}

fn check(r: Ref<IAudioMediaType>) -> windows::core::Result<IAudioMediaType> {
    let mt = r.ok()?;
    let mut f = UNCOMPRESSEDAUDIOFORMAT::default();
    // SAFETY: out-pointer to a local.
    unsafe { mt.GetUncompressedAudioFormat(&mut f)? };
    if f.fFramesPerSecond == 44_100.0 {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    Ok(mt.clone())
}

impl IAudioProcessingObjectConfiguration_Impl for FakeChild_Impl {
    fn LockForProcess(
        &self,
        num_in: u32,
        in_desc: *const *const APO_CONNECTION_DESCRIPTOR,
        _num_out: u32,
        _out_desc: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> windows::core::Result<()> {
        assert_eq!(num_in, 1);
        // SAFETY: one valid descriptor.
        let d = unsafe { &**in_desc };
        assert_ne!(d.pBuffer, 0, "child input is Relay's mid buffer");
        self.seen.lock_frames.store(d.u32MaxFrameCount, Ordering::SeqCst);
        self.seen.locked.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn UnlockForProcess(&self) -> windows::core::Result<()> {
        self.seen.unlocked.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl IAudioProcessingObjectRT_Impl for FakeChild_Impl {
    fn APOProcess(
        &self,
        _ni: u32,
        inp: *const *const APO_CONNECTION_PROPERTY,
        _no: u32,
        out: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // SAFETY: one valid connection each way, f32 stereo.
        unsafe {
            let i = &**inp;
            let o = &mut **out;
            let n = i.u32ValidFrameCount as usize * 2;
            let src = std::slice::from_raw_parts(i.pBuffer as *const f32, n);
            let dst = std::slice::from_raw_parts_mut(o.pBuffer as *mut f32, n);
            for (d, s) in dst.iter_mut().zip(src) {
                *d = 0.5 * s;
            }
            o.u32ValidFrameCount = i.u32ValidFrameCount;
            o.u32BufferFlags = i.u32BufferFlags;
        }
        self.seen.processed.fetch_add(1, Ordering::SeqCst);
    }
    fn CalcInputFrames(&self, n: u32) -> u32 {
        n
    }
    fn CalcOutputFrames(&self, n: u32) -> u32 {
        n
    }
}

impl IAudioSystemEffects_Impl for FakeChild_Impl {}

impl IAudioSystemEffects2_Impl for FakeChild_Impl {
    fn GetEffectsList(
        &self,
        ids: *mut *mut GUID,
        count: *mut u32,
        _event: HANDLE,
    ) -> windows::core::Result<()> {
        // SAFETY: caller out-pointers; CoTaskMem per the contract.
        unsafe {
            let p = CoTaskMemAlloc(std::mem::size_of::<GUID>()) as *mut GUID;
            p.write(CHILD_EFFECT);
            *ids = p;
            *count = 1;
        }
        Ok(())
    }
}

fn descriptor(mt: &IAudioMediaType, max_frames: u32) -> APO_CONNECTION_DESCRIPTOR {
    APO_CONNECTION_DESCRIPTOR {
        Type: APO_CONNECTION_BUFFER_TYPE_EXTERNAL,
        pBuffer: 0,
        u32MaxFrameCount: max_frames,
        pFormat: ManuallyDrop::new(Some(mt.clone())),
        u32Signature: 0,
    }
}

const FRAMES: usize = 256;

fn process(rt: &IAudioProcessingObjectRT, input: &[f32], output: &mut [f32]) {
    let in_c = APO_CONNECTION_PROPERTY {
        pBuffer: input.as_ptr() as usize,
        u32ValidFrameCount: (input.len() / 2) as u32,
        u32BufferFlags: Default::default(),
        u32Signature: 0,
    };
    let mut out_c = APO_CONNECTION_PROPERTY {
        pBuffer: output.as_mut_ptr() as usize,
        u32ValidFrameCount: 0,
        u32BufferFlags: Default::default(),
        u32Signature: 0,
    };
    let in_cp: *const APO_CONNECTION_PROPERTY = &in_c;
    let mut out_cp: *mut APO_CONNECTION_PROPERTY = &mut out_c;
    // SAFETY: valid connection arrays of length 1 with live buffers.
    unsafe { rt.APOProcess(1, &in_cp, 1, &mut out_cp) };
    assert_eq!(out_c.u32ValidFrameCount as usize, input.len() / 2);
}

fn sine() -> Vec<f32> {
    let mut v = vec![0.0f32; FRAMES * 2];
    for i in 0..FRAMES {
        let s = (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 48_000.0).sin() * 0.25;
        v[2 * i] = s;
        v[2 * i + 1] = s;
    }
    v
}

/// One test: the env vars and the shared section are process-wide.
#[test]
fn chained_child_runs_after_relay_and_alone_in_bypass() {
    const ENDPOINT: &str = "{feedface-0000-4000-8000-0000000000c1}";
    std::env::set_var("RELAY_APO_LOCAL_SECTION", "1");
    std::env::set_var("RELAY_APO_ENDPOINT_OVERRIDE", ENDPOINT);
    std::env::set_var("RELAY_INSTANCE", format!("apochain{}", std::process::id()));

    let seen = Arc::new(Seen::default());
    let child: IAudioProcessingObject = FakeChild { seen: seen.clone(), fail_init: false }.into();
    let apo: IAudioProcessingObject = RelayApo::with_child(child).into();
    let cfg: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
    let rt: IAudioProcessingObjectRT = apo.cast().unwrap();
    let fx2: IAudioSystemEffects2 = apo.cast().unwrap();

    // Initialize is forwarded with the same payload.
    // SAFETY: empty payload accepted.
    unsafe { apo.Initialize(&[]) }.expect("initialize");
    assert_eq!(seen.init.load(Ordering::SeqCst), 1);
    assert_eq!(seen.init_size.load(Ordering::SeqCst), 0);

    // Negotiation must satisfy both: Relay alone would take 44.1 k f32
    // stereo; the child refuses it, so the pair does.
    let f48 = f32_stereo(48_000.0);
    let f44 = f32_stereo(44_100.0);
    // SAFETY: valid interface pointers.
    unsafe {
        apo.IsInputFormatSupported(None, &f48).expect("both accept 48 k");
        apo.IsOutputFormatSupported(&f48, &f48).expect("both accept 48 k out");
        apo.IsInputFormatSupported(None, &f44).expect_err("child refuses 44.1 k");
        apo.IsOutputFormatSupported(None, &f44).expect_err("child refuses 44.1 k out");
    }

    // Effects list is the child's.
    let mut ids: *mut GUID = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: valid out-pointers.
    unsafe { fx2.GetEffectsList(&mut ids, &mut count, HANDLE::default()) }.unwrap();
    assert_eq!(count, 1);
    // SAFETY: one GUID written by the child.
    assert_eq!(unsafe { *ids }, CHILD_EFFECT);

    // Params: +6 dB at 1 kHz.
    let name = section_name(ENDPOINT, &std::env::var("RELAY_INSTANCE").unwrap())
        .replacen("Global\\", "Local\\", 1);
    let core_side = SharedParams::open(&name).expect("APO created the section");
    let params = ChainParams {
        bands: vec![BandParams::peaking(1_000.0, 6.0, 1.0)],
        limiter: None,
        hrtf: false,
    };
    core_side.block().write_params(&params);
    core_side.block().set_bypass(false);

    let in_d = descriptor(&f48, FRAMES as u32);
    let out_d = descriptor(&f48, FRAMES as u32);
    let (in_p, out_p): (*const _, *const _) = (&in_d, &out_d);
    // SAFETY: valid descriptor arrays of length 1.
    unsafe { cfg.LockForProcess(&[in_p], &[out_p]) }.expect("lock");
    assert_eq!(seen.locked.load(Ordering::SeqCst), 1);
    assert_eq!(seen.lock_frames.load(Ordering::SeqCst), FRAMES as u32);
    // SAFETY: plain COM call.
    assert_eq!(unsafe { apo.GetLatency() }.unwrap(), 1000, "EQ 0 + child 1000 hns");

    let input = sine();
    let mut output = vec![0.0f32; FRAMES * 2];
    process(&rt, &input, &mut output);

    // Relay first, then the child: 0.5 * EQ(input).
    let mut eq = vec![0.0f32; FRAMES * 2];
    let mut chain = relay_audio::dsp::Chain::new(params);
    chain.prepare(48_000, FRAMES).unwrap();
    chain.process(&input, &mut eq);
    let want: Vec<f32> = eq.iter().map(|s| 0.5 * s).collect();
    assert_eq!(output, want, "Relay's EQ, then the child");

    // Bypass = the child alone.
    core_side.block().set_bypass(true);
    process(&rt, &input, &mut output);
    let half: Vec<f32> = input.iter().map(|s| 0.5 * s).collect();
    assert_eq!(output, half, "bypass runs only the child");
    assert_eq!(seen.processed.load(Ordering::SeqCst), 2);

    // SAFETY: plain COM call.
    unsafe { cfg.UnlockForProcess() }.expect("unlock");
    assert_eq!(seen.unlocked.load(Ordering::SeqCst), 1);
    // SAFETY: plain COM call.
    assert_eq!(unsafe { apo.GetLatency() }.unwrap(), 0);

    // A child that fails Initialize is dropped: Relay runs alone (no
    // section needed for this half - bypass by absence is a straight copy).
    std::env::set_var("RELAY_APO_ENDPOINT_OVERRIDE", "{feedface-0000-4000-8000-0000000000c2}");
    let seen2 = Arc::new(Seen::default());
    let bad: IAudioProcessingObject = FakeChild { seen: seen2.clone(), fail_init: true }.into();
    let apo2: IAudioProcessingObject = RelayApo::with_child(bad).into();
    let cfg2: IAudioProcessingObjectConfiguration = apo2.cast().unwrap();
    let rt2: IAudioProcessingObjectRT = apo2.cast().unwrap();
    // SAFETY: as above.
    unsafe {
        apo2.Initialize(&[]).expect("Relay initializes without its child");
        apo2.IsInputFormatSupported(None, &f44).expect("no child: Relay's own rules");
        cfg2.LockForProcess(&[in_p], &[out_p]).expect("lock alone");
    }
    assert_eq!(seen2.init.load(Ordering::SeqCst), 1);
    assert_eq!(seen2.locked.load(Ordering::SeqCst), 0, "a failed child is never locked");
    let mut out2 = vec![0.0f32; FRAMES * 2];
    process(&rt2, &input, &mut out2);
    assert_eq!(out2, input, "Relay alone, no params: pass-through");
    assert_eq!(seen2.processed.load(Ordering::SeqCst), 0);
    // SAFETY: plain COM call.
    unsafe { cfg2.UnlockForProcess() }.unwrap();
}

#[test]
fn child_clsid_parses_from_the_recorded_value() {
    let g = relay_apo::com::parse_clsid("{13AB3EBD-137E-4903-9D89-60BE8277FD17}").unwrap();
    assert_eq!(g, GUID::from_u128(0x13AB3EBD_137E_4903_9D89_60BE8277FD17));
    assert!(relay_apo::com::parse_clsid("13AB3EBD-137E-4903-9D89-60BE8277FD17").is_none());
    assert_eq!(relay_apo::com::PKEY_RELAY_CHILD_MFX.pid, 1);
    assert_eq!(
        format!("{{{:?}}}", relay_apo::com::PKEY_RELAY_CHILD_MFX.fmtid).to_ascii_lowercase(),
        relay_apo::ids::RELAY_FX_FMTID
    );
}
