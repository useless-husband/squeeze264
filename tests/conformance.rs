//! Bit-exact conformance: every stream this encoder produces is decoded by
//! an independent decoder and the output must equal the encoder's own
//! reconstruction byte for byte, with no decoder warnings.
//!
//! Needs `ffmpeg` on PATH (or $FFMPEG); tests skip with a message otherwise.

use squeeze264::encoder::{Config, Encoder};
use squeeze264::mp4::Mp4Writer;
use squeeze264::ratecontrol::RcMode;
use squeeze264::synth::{synth_frame, Pattern};
use squeeze264::verify::{check_decode, find_ffmpeg, has_videotoolbox, Decoder};
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
    for (qp, pattern) in [
        (20u8, Pattern::Moving),
        (28, Pattern::Moving),
        (36, Pattern::Moving),
        (45, Pattern::Moving),
        (26, Pattern::Still),
        (30, Pattern::Noise),
        (24, Pattern::Extremes),
    ] {
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

/// Picture sizes that are not multiples of 16 are padded internally and
/// cropped by the SPS; the decoder must output exactly the visible area.
#[test]
fn cropped_sizes() {
    for (i, (w, h)) in [(50, 38), (130, 74), (2, 2), (18, 16), (16, 18), (62, 46), (354, 290)]
        .into_iter()
        .enumerate()
    {
        let mut cfg = base(w, h);
        cfg.rc = RcMode::ConstQp { qp: 27, i_offset: 2 };
        let (clip, _) = encode(cfg, 6, Pattern::Moving, 300 + i as u64);
        assert_eq!(clip.recon.len(), 6 * w * h * 3 / 2);
        assert_bit_exact(&format!("crop-{w}x{h}"), &clip, w, h, Decoder::FfmpegSoftware);
    }
}

/// Every encoder option on its own, on content with real motion.
#[test]
fn option_matrix() {
    type Tweak = Box<dyn Fn(&mut Config)>;
    let variants: Vec<(&str, Tweak)> = vec![
        ("subpel0", Box::new(|c| c.subpel = 0)),
        ("subpel1", Box::new(|c| c.subpel = 1)),
        ("no-partitions", Box::new(|c| c.partitions = false)),
        ("no-sub8x8", Box::new(|c| c.sub8x8 = false)),
        ("no-intra-in-p", Box::new(|c| c.intra_in_p = false)),
        ("no-deblock", Box::new(|c| c.deblock = false)),
        (
            "deblock-strong",
            Box::new(|c| (c.alpha_offset_div2, c.beta_offset_div2) = (6, 6)),
        ),
        (
            "deblock-weak",
            Box::new(|c| (c.alpha_offset_div2, c.beta_offset_div2) = (-6, -6)),
        ),
        ("chroma-qp+6", Box::new(|c| c.chroma_qp_offset = 6)),
        ("chroma-qp-12", Box::new(|c| c.chroma_qp_offset = -12)),
        ("no-decimate", Box::new(|c| c.decimate = false)),
        ("range4", Box::new(|c| c.me_range = 4)),
        ("range48", Box::new(|c| c.me_range = 48)),
        ("keyint5", Box::new(|c| c.keyint = 5)),
        ("fps-ntsc", Box::new(|c| (c.fps_num, c.fps_den) = (30000, 1001))),
    ];
    for (name, tweak) in variants {
        for qp in [23u8, 34] {
            let mut cfg = base(112, 96);
            cfg.rc = RcMode::ConstQp { qp, i_offset: 3 };
            tweak(&mut cfg);
            let (clip, _) = encode(cfg, 9, Pattern::Moving, 400);
            assert_bit_exact(&format!("opt-{name}-qp{qp}"), &clip, 112, 96, Decoder::FfmpegSoftware);
        }
    }
}

/// Target-bitrate mode changes QP from frame to frame; streams must stay
/// decodable and the average rate must land near the target.
#[test]
fn bitrate_mode() {
    for kbps in [150.0, 600.0] {
        let mut cfg = base(176, 144);
        cfg.rc = RcMode::Abr { bps: kbps * 1000.0 };
        cfg.keyint = 30;
        let frames = 90;
        let (clip, _) = encode(cfg, frames, Pattern::Moving, 500);
        assert_bit_exact(&format!("abr-{kbps}"), &clip, 176, 144, Decoder::FfmpegSoftware);
        let achieved = clip.stream.len() as f64 * 8.0 * 30.0 / frames as f64 / 1000.0;
        eprintln!("ABR target {kbps} kbit/s, achieved {achieved:.1} kbit/s");
        assert!(
            (achieved / kbps - 1.0).abs() < 0.2,
            "target {kbps}, achieved {achieved:.1}"
        );
    }
}

/// frame_num is 8 bits wide; a long run without IDR pictures must wrap cleanly.
#[test]
fn frame_num_wraps_without_idr() {
    let mut cfg = base(32, 32);
    cfg.keyint = 10_000;
    let (clip, _) = encode(cfg, 530, Pattern::Moving, 600);
    assert_bit_exact("frame-num-wrap", &clip, 32, 32, Decoder::FfmpegSoftware);
}

/// From level 3.1 up at most 16 motion vectors per two macroblocks are
/// allowed, so sub-8x8 partitions must not be used at 720p.
#[test]
fn large_pictures_respect_the_motion_vector_count_limit() {
    let cfg = base(1280, 720);
    let mut enc = Encoder::new(cfg).unwrap();
    assert_eq!(enc.params.level_idc, 31);
    let mut subs = [0u32; 4];
    for i in 0..3 {
        let src = synth_frame(1280, 720, i, 3, Pattern::Moving, 700);
        let out = enc.encode(&src);
        for k in 0..4 {
            subs[k] += out.stats.subs[k];
        }
    }
    assert!(subs[0] > 0, "8x8 partitions should be in use");
    assert_eq!(&subs[1..], &[0, 0, 0]);
}

/// The second, independent decoder: Apple's VideoToolbox (hardware where
/// available), driven through ffmpeg's hwaccel with the surface format
/// forced so that a software fallback cannot go unnoticed.
#[test]
fn videotoolbox_matches() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SKIP videotoolbox: ffmpeg not found");
        return;
    };
    if !has_videotoolbox(&ffmpeg) {
        eprintln!("SKIP videotoolbox: this ffmpeg build has no VideoToolbox hwaccel (not macOS?)");
        return;
    }
    for (name, w, h, qp, pattern) in [
        ("vt-cif", 352, 288, 26u8, Pattern::Moving),
        ("vt-small", 176, 144, 35, Pattern::Moving),
        ("vt-crop", 322, 242, 22, Pattern::Extremes),
        ("vt-noise", 192, 128, 30, Pattern::Noise),
    ] {
        let mut cfg = base(w, h);
        cfg.rc = RcMode::ConstQp { qp, i_offset: 3 };
        let (clip, _) = encode(cfg, 10, pattern, 800);
        assert_bit_exact(name, &clip, w, h, Decoder::VideoToolbox);
    }
    // Random decisions through the hardware decoder as well.
    for seed in 0..6u64 {
        let mut cfg = base(320, 240);
        cfg.fuzz = Some((seed, 0, 51));
        cfg.keyint = 4;
        let (clip, _) = encode(cfg, 8, Pattern::Moving, 900 + seed);
        assert_bit_exact(&format!("vt-fuzz{seed}"), &clip, 320, 240, Decoder::VideoToolbox);
    }
}

/// The MP4 file must decode to the same frames as the raw stream.
#[test]
fn mp4_container_decodes_identically() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SKIP mp4: ffmpeg not found");
        return;
    };
    let (w, h, frames) = (176, 144, 20);
    let mut cfg = Config::new(w, h, 30000, 1001);
    cfg.keyint = 8;
    let mut enc = Encoder::new(cfg).unwrap();
    let (sps, pps) = enc.headers();
    let path = tmp_path("container.mp4");
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut mp4 = Mp4Writer::new(file, &sps, &pps, w, h, 30000, 1001).unwrap();
    let mut recon = Vec::new();
    for i in 0..frames {
        let out = enc.encode(&synth_frame(w, h, i, frames, Pattern::Moving, 42));
        mp4.write_sample(&out.nal, out.stats.idr).unwrap();
        enc.recon().write_i420(w, h, &mut recon);
    }
    mp4.finish().unwrap();
    let mut decoders = vec![Decoder::FfmpegSoftware];
    if has_videotoolbox(&ffmpeg) {
        decoders.push(Decoder::VideoToolbox);
    }
    for d in decoders {
        let r = check_decode(&ffmpeg, d, &path, w, h, &recon[..]).unwrap();
        assert!(r.bit_exact(frames), "{}: {r:?}", d.name());
    }
    // ffprobe-style sanity through ffmpeg: frame rate and frame count survive.
    let probe = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(&path)
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&probe.stderr).into_owned();
    assert!(info.contains("Constrained Baseline"), "{info}");
    assert!(info.contains("176x144"), "{info}");
    assert!(info.contains("29.97 fps"), "{info}");
    std::fs::remove_file(&path).ok();
}

/// Real footage from the Xiph collection, when `make data` has been run.
#[test]
fn real_clips() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data");
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SKIP real clips: ffmpeg not found");
        return;
    };
    let mut ran = 0;
    for (name, frames) in [
        ("foreman_cif.y4m", 60),
        ("akiyo_cif.y4m", 40),
        ("mobile_cif.y4m", 40),
        ("shields_720p_100f.y4m", 12),
    ] {
        let path = dir.join(name);
        let Ok(file) = std::fs::File::open(&path) else {
            eprintln!("SKIP {name}: not in data/ (run `make data` to download the test clips)");
            continue;
        };
        let hdr = squeeze264::y4m::Y4mReader::new(std::io::BufReader::new(file))
            .unwrap()
            .header;
        for qp in [22u8, 37] {
            let mut cfg = Config::new(hdr.width, hdr.height, hdr.fps_num, hdr.fps_den);
            cfg.rc = RcMode::ConstQp { qp, i_offset: 3 };
            let mut enc = Encoder::new(cfg).unwrap();
            let (sps, pps) = enc.headers();
            let mut src = enc.new_input_frame();
            let (mut stream, mut recon) = (Vec::new(), Vec::new());
            let mut reader =
                squeeze264::y4m::Y4mReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
            let mut n = 0;
            while n < frames && reader.read_frame(&mut src).unwrap() {
                let out = enc.encode(&src);
                if out.stats.idr {
                    sps.write_annexb(&mut stream);
                    pps.write_annexb(&mut stream);
                }
                out.nal.write_annexb(&mut stream);
                enc.recon().write_i420(hdr.width, hdr.height, &mut recon);
                n += 1;
            }
            let clip = Clip {
                stream,
                recon,
                frames: n,
            };
            let tag = format!("{}-qp{qp}", name.trim_end_matches(".y4m"));
            assert_bit_exact(&tag, &clip, hdr.width, hdr.height, Decoder::FfmpegSoftware);
            if has_videotoolbox(&ffmpeg) {
                assert_bit_exact(
                    &format!("{tag}-vt"),
                    &clip,
                    hdr.width,
                    hdr.height,
                    Decoder::VideoToolbox,
                );
            }
        }
        ran += 1;
    }
    eprintln!("real clips checked: {ran}");
}

/// Annex A limits macroblock_layer() to 3200 bits. Noise at QP 0 would
/// exceed that, so the encoder must fall back to I_PCM, in I and P slices.
#[test]
fn oversized_macroblocks_fall_back_to_pcm() {
    let mut cfg = base(176, 144);
    cfg.rc = RcMode::ConstQp { qp: 0, i_offset: 0 };
    cfg.keyint = 3;
    let mut enc = Encoder::new(cfg).unwrap();
    let (sps, pps) = enc.headers();
    let (mut stream, mut recon) = (Vec::new(), Vec::new());
    let mut pcm = [0u32; 2];
    for i in 0..6 {
        let src = synth_frame(176, 144, i, 6, Pattern::Noise, 77);
        let out = enc.encode(&src);
        if out.stats.idr {
            sps.write_annexb(&mut stream);
            pps.write_annexb(&mut stream);
        }
        out.nal.write_annexb(&mut stream);
        enc.recon().write_i420(176, 144, &mut recon);
        pcm[!out.stats.idr as usize] += out.stats.n_pcm;
        for r in &out.records {
            assert!(
                r.bits <= squeeze264::mb::MAX_MB_BITS + 16,
                "macroblock of {} bits",
                r.bits
            );
        }
        // I_PCM is lossless.
        if out.stats.n_pcm as usize == out.records.len() {
            assert_eq!(out.stats.sse, [0, 0, 0]);
        }
    }
    assert!(
        pcm[0] > 0 && pcm[1] > 0,
        "expected I_PCM in both slice types, got {pcm:?}"
    );
    let clip = Clip {
        stream,
        recon,
        frames: 6,
    };
    assert_bit_exact("pcm-fallback", &clip, 176, 144, Decoder::FfmpegSoftware);
    // (QCIF because the VideoToolbox decoder refuses to open very small pictures.)
    if find_ffmpeg().is_some_and(|f| has_videotoolbox(&f)) {
        assert_bit_exact("pcm-fallback-vt", &clip, 176, 144, Decoder::VideoToolbox);
    }
}
