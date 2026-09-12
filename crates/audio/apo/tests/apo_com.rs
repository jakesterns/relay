//! In-process exercise of the real APO COM object — everything audiodg would
//! do short of loading the DLL: Initialize, format negotiation, LockForProcess,
//! APOProcess with live parameter updates over the shared-memory section, and
//! UnlockForProcess. This is the closest an unsigned build gets to the engine
//! on a machine with no test-signing VM; the audiodg-hosted pass is the VM
//! runbook in the M3b plan.

#![cfg(windows)]
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use relay_apo::com::{RelayApo, CLSID_RELAY_APO};
use relay_audio::params::{BandParams, ChainParams};
use relay_audio::shm::{section_name, SharedParams};
use windows::core::{implement, Interface, Ref, BOOL};
use windows::Win32::Media::Audio::Apo::{
    IAudioMediaType, IAudioMediaType_Impl, IAudioProcessingObject,
    IAudioProcessingObjectConfiguration, IAudioProcessingObjectRT,
    APO_CONNECTION_BUFFER_TYPE_EXTERNAL, APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY,
    UNCOMPRESSEDAUDIOFORMAT,
};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;

const ENDPOINT: &str = "{feedface-0000-4000-8000-000000000001}";

/// Minimal media type standing in for the engine's: uncompressed, fixed
/// format fields, enough for the APO's negotiation and lock paths.
#[implement(IAudioMediaType)]
struct TestMediaType {
    format: UNCOMPRESSEDAUDIOFORMAT,
    wfx: Box<WAVEFORMATEX>,
}

impl TestMediaType {
    fn stereo_f32(rate: f32) -> IAudioMediaType {
        let format = UNCOMPRESSEDAUDIOFORMAT {
            guidFormatType: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            dwSamplesPerFrame: 2,
            dwBytesPerSampleContainer: 4,
            dwValidBitsPerSample: 32,
            fFramesPerSecond: rate,
            dwChannelMask: 0x3,
        };
        Self { format, wfx: Box::new(WAVEFORMATEX::default()) }.into()
    }

    fn stereo_i16(rate: f32) -> IAudioMediaType {
        let format = UNCOMPRESSEDAUDIOFORMAT {
            guidFormatType: windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71),
            dwSamplesPerFrame: 2,
            dwBytesPerSampleContainer: 2,
            dwValidBitsPerSample: 16,
            fFramesPerSecond: rate,
            dwChannelMask: 0x3,
        };
        Self { format, wfx: Box::new(WAVEFORMATEX::default()) }.into()
    }
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
        // SAFETY: caller-provided out pointer per the COM contract.
        unsafe { out.write(self.format) };
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

/// Everything in one test: the env vars, the section and the COM object are
/// process-wide state.
#[test]
fn apo_end_to_end_in_process() {
    std::env::set_var("RELAY_APO_LOCAL_SECTION", "1");
    std::env::set_var("RELAY_APO_ENDPOINT_OVERRIDE", ENDPOINT);
    std::env::set_var("RELAY_INSTANCE", format!("apocom{}", std::process::id()));

    let apo_obj: IAudioProcessingObject = RelayApo::default().into();
    let cfg: IAudioProcessingObjectConfiguration = apo_obj.cast().expect("configuration iface");
    let rt: IAudioProcessingObjectRT = apo_obj.cast().expect("RT iface");

    // Registration properties carry our CLSID and one connection each way.
    // SAFETY: contract returns a CoTaskMem struct; we only read and leak it
    // (test process).
    unsafe {
        let props = apo_obj.GetRegistrationProperties().expect("reg props");
        assert_eq!((*props).clsid, CLSID_RELAY_APO);
        assert_eq!((*props).u32MaxInputConnections, 1);
    }

    // Initialize with no engine payload: endpoint comes from the override,
    // and the APO creates the (Local\) section.
    // SAFETY: an empty Initialize payload is accepted by this APO.
    unsafe { apo_obj.Initialize(&[]) }.expect("initialize");

    // Format negotiation: f32 stereo accepted, 16-bit refused, rate
    // mismatch against the opposite side refused (never resample).
    let f32_48k = TestMediaType::stereo_f32(48_000.0);
    let f32_44k = TestMediaType::stereo_f32(44_100.0);
    let i16_48k = TestMediaType::stereo_i16(48_000.0);
    // SAFETY: valid interface pointers throughout.
    unsafe {
        apo_obj.IsInputFormatSupported(None, &f32_48k).expect("f32 accepted");
        apo_obj.IsInputFormatSupported(&f32_48k, &i16_48k).expect_err("i16 refused");
        apo_obj.IsInputFormatSupported(&f32_48k, &f32_44k).expect_err("rate change refused");
    }

    // The core side opens the section the APO just created and configures a
    // +6 dB peaking band before the stream locks.
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
    core_side.notify();

    const FRAMES: usize = 256;
    let in_d = descriptor(&f32_48k, FRAMES as u32);
    let out_d = descriptor(&f32_48k, FRAMES as u32);
    let in_p: *const APO_CONNECTION_DESCRIPTOR = &in_d;
    let out_p: *const APO_CONNECTION_DESCRIPTOR = &out_d;
    // SAFETY: valid descriptor arrays of length 1.
    unsafe { cfg.LockForProcess(&[in_p], &[out_p]) }.expect("lock");

    // 1 kHz sine at 48 k, interleaved stereo.
    let mut input = vec![0.0f32; FRAMES * 2];
    for i in 0..FRAMES {
        let s = (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 48_000.0).sin() * 0.25;
        input[2 * i] = s;
        input[2 * i + 1] = s;
    }
    let mut output = vec![0.0f32; FRAMES * 2];

    let process = |input: &[f32], output: &mut [f32]| {
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
    };

    process(&input, &mut output);

    // Reference: the same params through the DSP chain directly.
    let mut reference = vec![0.0f32; FRAMES * 2];
    let mut chain = relay_audio::dsp::Chain::new(params);
    chain.prepare(48_000, FRAMES).expect("prepare reference");
    chain.process(&input, &mut reference);
    assert_eq!(output, reference, "APO output equals the direct DSP chain");
    assert_ne!(output, input, "the band audibly did something");

    // GetLatency: EQ-only chain has no look-ahead — 0 hns, comfortably
    // inside the DoD's 1 ms bound at 48 k.
    // SAFETY: plain COM call.
    let latency = unsafe { apo_obj.GetLatency() }.expect("latency");
    assert_eq!(latency, 0);

    // Bypass is honoured on the very next block, no rebuild involved.
    core_side.block().set_bypass(true);
    process(&input, &mut output);
    assert_eq!(output, input, "bypass is a straight copy");
    core_side.block().set_bypass(false);

    // Live parameter update: write new params + event; the control thread
    // rebuilds off the RT path and swaps.
    let params2 = ChainParams {
        bands: vec![BandParams::peaking(1_000.0, -6.0, 1.0)],
        limiter: None,
        hrtf: false,
    };
    core_side.block().write_params(&params2);
    core_side.notify();
    let mut reference2 = vec![0.0f32; FRAMES * 2];
    let mut chain2 = relay_audio::dsp::Chain::new(params2);
    chain2.prepare(48_000, FRAMES).expect("prepare reference2");
    chain2.process(&input, &mut reference2);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        process(&input, &mut output);
        if output == reference2 {
            break; // rebuilt chain took over
        }
        assert!(std::time::Instant::now() < deadline, "chain rebuild never landed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // SAFETY: plain COM call; drops the chain and joins the control thread.
    unsafe { cfg.UnlockForProcess() }.expect("unlock");
}
