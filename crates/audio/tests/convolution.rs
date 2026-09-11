//! Partitioned-convolution correctness against a naive time-domain
//! reference, latency, and the bundled "Relay Arena" IR set.

use relay_audio::dsp::hrtf::{Hrtf, PARTITION};

/// Plain O(n·m) convolution.
fn naive_conv(x: &[f32], h: &[f32]) -> Vec<f64> {
    let mut y = vec![0.0f64; x.len() + h.len() - 1];
    for (i, &xi) in x.iter().enumerate() {
        for (j, &hj) in h.iter().enumerate() {
            y[i + j] += xi as f64 * hj as f64;
        }
    }
    y
}

fn xorshift(state: &mut u32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state as f32 / u32::MAX as f32) * 2.0 - 1.0
}

#[test]
fn partitioned_matches_naive_reference() {
    let mut rng = 0xDEAD_BEEFu32;
    // IR length deliberately not a multiple of the partition size.
    let ir_len = 300;
    let n = 2000;
    let mk =
        |rng: &mut u32, len: usize| (0..len).map(|_| xorshift(rng) * 0.5).collect::<Vec<f32>>();
    let ir_ll = mk(&mut rng, ir_len);
    let ir_lr = mk(&mut rng, ir_len);
    let ir_rl = mk(&mut rng, ir_len - 37); // different lengths per ear too
    let ir_rr = mk(&mut rng, ir_len);
    let left_in = mk(&mut rng, n);
    let right_in = mk(&mut rng, n);

    let mut hrtf =
        Hrtf::prepare_with_ir(512, [ir_ll.clone(), ir_lr.clone()], [ir_rl.clone(), ir_rr.clone()])
            .unwrap();

    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    // Drive with awkward, non-partition-aligned block sizes.
    let mut pos = 0;
    for block in [160usize, 7, 333, 128, 500].iter().cycle() {
        if pos >= n {
            break;
        }
        let end = (pos + block).min(n);
        hrtf.process(
            &left_in[pos..end],
            &right_in[pos..end],
            &mut out_l[pos..end],
            &mut out_r[pos..end],
        );
        pos = end;
    }

    // Expected: engine output is the true convolution delayed by one partition.
    let exp_l: Vec<f64> = {
        let a = naive_conv(&left_in, &ir_ll);
        let b = naive_conv(&right_in, &ir_rl);
        (0..n).map(|i| a.get(i).unwrap_or(&0.0) + b.get(i).unwrap_or(&0.0)).collect()
    };
    let exp_r: Vec<f64> = {
        let a = naive_conv(&left_in, &ir_lr);
        let b = naive_conv(&right_in, &ir_rr);
        (0..n).map(|i| a.get(i).unwrap_or(&0.0) + b.get(i).unwrap_or(&0.0)).collect()
    };
    for i in 0..n {
        let (el, er) =
            if i < PARTITION { (0.0, 0.0) } else { (exp_l[i - PARTITION], exp_r[i - PARTITION]) };
        assert!(
            (out_l[i] as f64 - el).abs() < 1e-3,
            "left ear sample {i}: engine {} vs reference {el}",
            out_l[i]
        );
        assert!(
            (out_r[i] as f64 - er).abs() < 1e-3,
            "right ear sample {i}: engine {} vs reference {er}",
            out_r[i]
        );
    }
}

#[test]
fn delta_ir_is_a_pure_one_partition_delay() {
    let delta = [vec![1.0f32], vec![0.0f32]];
    let mut hrtf = Hrtf::prepare_with_ir(
        256,
        [delta[0].clone(), delta[1].clone()],
        [delta[1].clone(), delta[0].clone()],
    )
    .unwrap();
    let n = 4 * PARTITION;
    let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).sin()).collect();
    let zeros = vec![0.0f32; n];
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    hrtf.process(&input, &zeros, &mut out_l, &mut out_r);
    assert_eq!(hrtf.latency_frames(), PARTITION);
    for i in 0..n {
        let expected = if i < PARTITION { 0.0 } else { input[i - PARTITION] };
        assert!((out_l[i] - expected).abs() < 1e-5, "sample {i}: {} vs {expected}", out_l[i]);
        assert!(out_r[i].abs() < 1e-6, "right ear should be silent");
    }
}

#[test]
fn arena_set_loads_at_all_bundled_rates_and_rejects_others() {
    for rate in [44_100u32, 48_000, 96_000] {
        Hrtf::prepare_arena(rate, 512).unwrap_or_else(|e| panic!("rate {rate}: {e}"));
    }
    assert!(matches!(
        Hrtf::prepare_arena(22_050, 512),
        Err(relay_audio::PrepareError::HrtfUnsupportedRate(22_050))
    ));
}

#[test]
fn arena_left_source_lands_in_the_left_ear() {
    // Noise on the left channel only: after HRTF the left ear must carry
    // clearly more energy than the right (the ±30° set is strongly lateral).
    let mut hrtf = Hrtf::prepare_arena(48_000, 512).unwrap();
    let mut rng = 12345u32;
    let n = 48_000 / 2;
    let left_in: Vec<f32> = (0..n).map(|_| xorshift(&mut rng) * 0.5).collect();
    let zeros = vec![0.0f32; n];
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    hrtf.process(&left_in, &zeros, &mut out_l, &mut out_r);
    let e = |b: &[f32]| b.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
    let ratio_db = 10.0 * (e(&out_l) / e(&out_r)).log10();
    assert!(
        ratio_db > 3.0,
        "left source should favour the left ear; interaural energy ratio {ratio_db:.1} dB"
    );
}
