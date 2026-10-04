//! Bit-exact conformance: every stream this encoder produces is decoded by
//! an independent decoder and the output must equal the encoder's own
//! reconstruction byte for byte, with no decoder warnings.
//!
//! Needs `ffmpeg` on PATH (or $FFMPEG); tests skip with a message otherwise.

use squeeze264::encoder::{Config, Encoder};
use squeeze264::ratecontrol::RcMode;
use squeeze264::synth::{synth_frame, Pattern};
use squeeze264::verify::{check_decode, find_ffmpeg, Decoder};
use std::path::PathBuf;

struct Clip {
    stream: Vec<u8>,
    recon: Vec<u8>,
    frames: usize,
}

fn encode(cfg: Config, frames: usize, pattern: Pattern, seed: u64) -> (Clip, Encoder) {
    let (w, h) = (cfg.width, cfg.height);
    let mut enc = Encoder::new(cfg).expect("config");
    let mut stream = Vec::new();
    let mut recon = Vec::new();
    let (sps, pps) = enc.headers();
    for i in 0..frames {
        let src = synth_frame(w, h, i, frames, pattern, seed);
        let out = enc.encode(&src);
        if out.stats.idr {
            sps.write_annexb(&mut stream);
            pps.write_annexb(&mut stream);
        }
        out.nal.write_annexb(&mut stream);
        enc.recon().write_i420(w, h, &mut recon);
    }
    (Clip { stream, recon, frames }, enc)
}

fn tmp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("squeeze264-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Decodes with the given decoder and panics with a diagnostic on any difference.
fn assert_bit_exact(name: &str, clip: &Clip, w: usize, h: usize, decoder: Decoder) {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SKIP {name}: ffmpeg not found (set $FFMPEG or install it)");
        return;
    };
    let path = tmp_path(&format!("{name}.h264"));
    std::fs::write(&path, &clip.stream).unwrap();
    let report = check_decode(&ffmpeg, decoder, &path, w, h, &clip.recon[..]).unwrap();
    if !report.bit_exact(clip.frames) {
        let keep = tmp_path(&format!("{name}.failed.h264"));
        std::fs::copy(&path, &keep).ok();
        panic!(
            "{name}: NOT bit-exact with {}: decoded {}/{} frames, {} mismatching (first diff {:?}), exit ok {}, stderr: {:?}; stream kept at {}",
            decoder.name(),
            report.decoded_frames,
            clip.frames,
            report.mismatches.len(),
            report.first_diff,
            report.exit_ok,
            report.stderr,
            keep.display()
        );
    }
    std::fs::remove_file(&path).ok();
}

fn base(w: usize, h: usize) -> Config {
    Config::new(w, h, 30, 1)
}

#[test]
fn intra_only_all_qps() {
    for qp in [0u8, 1, 10, 17, 24, 30, 37, 44, 51] {
        let mut cfg = base(64, 48);
        cfg.rc = RcMode::ConstQp { qp, i_offset: 0 };
        cfg.keyint = 1;
        let (clip, _) = encode(cfg, 3, Pattern::Moving, 100 + qp as u64);
        assert_bit_exact(&format!("intra-qp{qp}"), &clip, 64, 48, Decoder::FfmpegSoftware);
    }
}

#[test]
fn harness_detects_a_single_wrong_sample() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SKIP harness check: ffmpeg not found");
        return;
    };
    let (mut clip, _) = encode(base(64, 48), 4, Pattern::Moving, 1);
    let path = tmp_path("harness.h264");
    std::fs::write(&path, &clip.stream).unwrap();
    let ok = check_decode(&ffmpeg, Decoder::FfmpegSoftware, &path, 64, 48, &clip.recon[..]).unwrap();
    assert!(ok.bit_exact(4), "{ok:?}");
    // Flip one chroma sample of frame 2 in the expected data.
    let pos = 2 * (64 * 48 * 3 / 2) + 64 * 48 + 37;
    clip.recon[pos] ^= 1;
    let bad = check_decode(&ffmpeg, Decoder::FfmpegSoftware, &path, 64, 48, &clip.recon[..]).unwrap();
    assert_eq!(bad.mismatches, vec![2]);
    assert_eq!(bad.first_diff.map(|d| (d.0, d.1)), Some((2, 1)));
    // A truncated stream must not pass either.
    std::fs::write(&path, &clip.stream[..clip.stream.len() / 2]).unwrap();
    clip.recon[pos] ^= 1;
    let cut = check_decode(&ffmpeg, Decoder::FfmpegSoftware, &path, 64, 48, &clip.recon[..]).unwrap();
    assert!(!cut.bit_exact(4));
    std::fs::remove_file(&path).ok();
}

#[test]
fn p_frames_default_settings() {
    for (qp, pattern) in [(20u8, Pattern::Moving), (28, Pattern::Moving), (36, Pattern::Moving), (45, Pattern::Moving), (26, Pattern::Still), (30, Pattern::Noise), (24, Pattern::Extremes)] {
        let mut cfg = base(96, 80);
        cfg.rc = RcMode::ConstQp { qp, i_offset: 3 };
        let (clip, _) = encode(cfg, 12, pattern, 200 + qp as u64);
        assert_bit_exact(&format!("p-qp{qp}-{pattern:?}"), &clip, 96, 80, Decoder::FfmpegSoftware);
    }
}

/// Random encoder decisions on random content: every macroblock type,
/// partition shape, prediction mode, vector (including far outside the
/// picture), coded_block_pattern and QP change must still decode exactly.
#[test]
fn fuzz_random_decisions() {
    let mut coverage = squeeze264::cavlc::Coverage::default();
    let sizes = [(64, 48), (16, 16), (32, 16), (16, 48), (80, 64), (128, 96), (176, 144)];
    for seed in 0..28u64 {
        let (w, h) = sizes[seed as usize % sizes.len()];
        let pattern = [Pattern::Noise, Pattern::Moving, Pattern::Extremes, Pattern::Still][seed as usize % 4];
        let (lo, hi) = [(0, 51), (0, 12), (20, 40), (40, 51), (0, 51)][seed as usize % 5];
        let mut cfg = base(w, h);
        cfg.fuzz = Some((seed, lo, hi));
        cfg.keyint = [1000, 3, 7][seed as usize % 3];
        cfg.deblock = seed % 7 != 3;
        cfg.alpha_offset_div2 = (seed % 13) as i8 - 6;
        cfg.beta_offset_div2 = (seed % 11) as i8 - 5;
        cfg.chroma_qp_offset = [0, -12, 12, 3, -5][seed as usize % 5];
        let (clip, enc) = encode(cfg, 10, pattern, 1000 + seed);
        assert_bit_exact(&format!("fuzz-seed{seed}"), &clip, w, h, Decoder::FfmpegSoftware);
        coverage.merge(&enc.coverage);
    }
    let (used, total) = coverage.summary();
    eprintln!("fuzz corpus exercised {used}/{total} coeff_token table entries");
    assert_eq!(used, total, "fuzz corpus should reach every coeff_token entry");
}
