//! Encoder behaviour that can be checked without an external decoder.

use squeeze264::encoder::{Config, Encoder};
use squeeze264::mb::MbMode;
use squeeze264::ratecontrol::RcMode;
use squeeze264::synth::{synth_frame, Pattern};

fn run(cfg: Config, frames: usize, pattern: Pattern, seed: u64) -> (Vec<u8>, Vec<squeeze264::encoder::FrameStats>) {
    let (w, h) = (cfg.width, cfg.height);
    let mut enc = Encoder::new(cfg).unwrap();
    let mut bytes = Vec::new();
    let mut stats = Vec::new();
    for i in 0..frames {
        let out = enc.encode(&synth_frame(w, h, i, frames, pattern, seed));
        out.nal.write_annexb(&mut bytes);
        stats.push(out.stats);
    }
    (bytes, stats)
}

#[test]
fn encoding_is_deterministic() {
    let a = run(Config::new(96, 64, 30, 1), 8, Pattern::Moving, 5);
    let b = run(Config::new(96, 64, 30, 1), 8, Pattern::Moving, 5);
    assert_eq!(a.0, b.0);
    let mut cfg = Config::new(96, 64, 30, 1);
    cfg.fuzz = Some((9, 0, 51));
    let c = run(cfg.clone(), 8, Pattern::Moving, 5);
    let d = run(cfg, 8, Pattern::Moving, 5);
    assert_eq!(c.0, d.0, "fuzz mode must be reproducible from its seed");
}

#[test]
fn higher_qp_means_fewer_bits_and_lower_psnr() {
    let mut last: Option<(usize, f64)> = None;
    for qp in [16u8, 22, 28, 34, 40, 46] {
        let mut cfg = Config::new(176, 144, 30, 1);
        cfg.rc = RcMode::ConstQp { qp, i_offset: 3 };
        let (bytes, stats) = run(cfg, 10, Pattern::Moving, 6);
        let psnr = stats.iter().map(|s| s.psnr[0]).sum::<f64>() / stats.len() as f64;
        if let Some((prev_bytes, prev_psnr)) = last {
            assert!(
                bytes.len() < prev_bytes,
                "qp {qp}: {} bytes after {prev_bytes}",
                bytes.len()
            );
            assert!(psnr < prev_psnr, "qp {qp}: {psnr:.2} dB after {prev_psnr:.2}");
        }
        assert!(psnr > 20.0 && psnr < 60.0, "implausible PSNR {psnr}");
        last = Some((bytes.len(), psnr));
    }
}

#[test]
fn static_content_is_mostly_skipped() {
    let w = 176;
    let h = 144;
    let mut enc = Encoder::new(Config::new(w, h, 30, 1)).unwrap();
    let frame = synth_frame(w, h, 0, 1, Pattern::Still, 3);
    let first = enc.encode(&frame);
    assert!(first.stats.idr);
    assert_eq!(first.stats.n_skip, 0);
    // The same picture again: nearly everything should be P_Skip and tiny.
    let second = enc.encode(&frame);
    assert!(!second.stats.idr);
    let total = (w / 16) * (h / 16);
    assert!(
        second.stats.n_skip as usize > total * 9 / 10,
        "only {} of {total} skipped",
        second.stats.n_skip
    );
    assert!(second.stats.bytes * 20 < first.stats.bytes);
}

#[test]
fn panning_content_is_found_by_motion_search() {
    // The Moving pattern pans by a fractional amount per frame; with motion
    // search the P frames must be far smaller than with intra coding only.
    let mut inter = Config::new(176, 144, 30, 1);
    inter.keyint = 100;
    let mut intra = inter.clone();
    intra.keyint = 1;
    let (a, sa) = run(inter, 6, Pattern::Moving, 8);
    let (b, _) = run(intra, 6, Pattern::Moving, 8);
    assert!(
        a.len() * 2 < b.len(),
        "inter {} bytes vs intra {} bytes",
        a.len(),
        b.len()
    );
    // Frame 0 is the keyframe and frame 3 is the pattern's scene cut.
    for s in sa.iter().filter(|s| s.index != 0 && s.index != 3) {
        assert!(s.n_inter + s.n_skip > s.n_i4 + s.n_i16, "{s:?}");
    }
}

#[test]
fn keyframe_interval_and_scene_cut() {
    let mut cfg = Config::new(96, 64, 30, 1);
    cfg.keyint = 4;
    let (_, stats) = run(cfg, 10, Pattern::Moving, 2);
    let idr: Vec<bool> = stats.iter().map(|s| s.idr).collect();
    assert_eq!(idr, [true, false, false, false, true, false, false, false, true, false]);
    // A scene cut inside a GOP shows up as intra macroblocks in a P frame.
    let mut cfg = Config::new(176, 144, 30, 1);
    cfg.keyint = 100;
    let (_, stats) = run(cfg, 8, Pattern::Moving, 2);
    let cut = &stats[4];
    assert!(!cut.idr);
    assert!(
        cut.n_i4 + cut.n_i16 > (cut.n_inter + cut.n_skip) / 2,
        "scene cut frame: {cut:?}"
    );
}

#[test]
fn motion_vectors_stay_inside_the_legal_range() {
    // QCIF at 30 fps is level 1.1: vertical vectors within [-128, 127.75].
    // Fuzz mode draws vectors from the whole allowed range.
    let mut cfg = Config::new(176, 144, 30, 1);
    cfg.fuzz = Some((3, 20, 40));
    let (w, h) = (cfg.width as i32, cfg.height as i32);
    let mut enc = Encoder::new(cfg).unwrap();
    assert_eq!(enc.params.level_idc, 11);
    let mut seen_far = false;
    for i in 0..6 {
        let out = enc.encode(&synth_frame(176, 144, i, 6, Pattern::Moving, 1));
        for (mb, r) in out.records.iter().enumerate() {
            let (x0, y0) = ((mb % 11) as i32 * 16, (mb / 11) as i32 * 16);
            if let MbMode::Inter { mvs, .. } = &r.mode {
                for mv in mvs {
                    let (mx, my) = (mv[0] as i32, mv[1] as i32);
                    assert!(
                        (-512..=511).contains(&my),
                        "vertical vector {my} breaks the level limit"
                    );
                    // The referenced block stays within 24 samples of the picture.
                    assert!(x0 * 4 + mx >= -24 * 4 && x0 * 4 + mx <= (w + 24 - 16) * 4);
                    assert!(y0 * 4 + my >= -24 * 4 && y0 * 4 + my <= (h + 24 - 16) * 4);
                    seen_far |= x0 * 4 + mx < -32 || y0 * 4 + my < -32;
                }
            }
        }
    }
    assert!(seen_far, "fuzzing should produce vectors pointing outside the picture");
}

#[test]
fn rejects_bad_configurations() {
    assert!(Encoder::new(Config::new(0, 16, 30, 1)).is_err());
    assert!(Encoder::new(Config::new(17, 16, 30, 1)).is_err());
    assert!(Encoder::new(Config::new(16, 16, 0, 1)).is_err());
    let mut c = Config::new(16, 16, 30, 1);
    c.rc = RcMode::ConstQp { qp: 52, i_offset: 0 };
    assert!(Encoder::new(c).is_err());
    let mut c = Config::new(16, 16, 30, 1);
    c.chroma_qp_offset = 13;
    assert!(Encoder::new(c).is_err());
    let mut c = Config::new(16, 16, 30, 1);
    c.alpha_offset_div2 = 7;
    assert!(Encoder::new(c).is_err());
    let mut c = Config::new(16, 16, 30, 1);
    c.keyint = 0;
    assert!(Encoder::new(c).is_err());
    // Beyond level 5.1: 8K, or a bitrate no level allows.
    assert!(Encoder::new(Config::new(7680, 4320, 30, 1)).is_err());
    let mut c = Config::new(352, 288, 30, 1);
    c.rc = RcMode::Abr { bps: 500e6 };
    assert!(Encoder::new(c).is_err());
    assert!(Encoder::new(Config::new(16, 16, 30, 1)).is_ok());
}

#[test]
fn level_follows_picture_size_and_rate() {
    let level = |w, h, fps| Encoder::new(Config::new(w, h, fps, 1)).unwrap().params.level_idc;
    assert_eq!(level(176, 144, 15), 10);
    assert_eq!(level(352, 288, 30), 13);
    assert_eq!(level(720, 576, 25), 30);
    assert_eq!(level(1280, 720, 30), 31);
    assert_eq!(level(1920, 1080, 30), 40);
}

/// Decoders tolerate a wrong frame_num (they conceal the "gap"), so the
/// bit-exact oracle cannot see it; check the slice headers directly.
#[test]
fn slice_headers_carry_consecutive_frame_num_and_fresh_idr_ids() {
    use squeeze264::bitstream::{unescape, BitReader};
    let mut cfg = Config::new(16, 16, 30, 1);
    cfg.keyint = 300;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut last_idr_id = None;
    for i in 0..620usize {
        let out = enc.encode(&synth_frame(16, 16, i, 620, Pattern::Still, 1));
        let rbsp = unescape(&out.nal.bytes[1..]);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.ue(), 0, "first_mb_in_slice");
        let slice_type = r.ue();
        assert_eq!(r.ue(), 0, "pic_parameter_set_id");
        let frame_num = r.bits(8) as usize;
        let since_idr = i % 300;
        assert_eq!(frame_num, since_idr % 256, "frame {i}");
        if since_idr == 0 {
            assert_eq!(
                (out.nal.bytes[0] & 0x1f, slice_type),
                (5, 7),
                "frame {i} must be an IDR I slice"
            );
            let id = r.ue();
            assert_ne!(
                Some(id),
                last_idr_id,
                "consecutive IDR pictures need different idr_pic_id"
            );
            last_idr_id = Some(id);
        } else {
            assert_eq!(
                (out.nal.bytes[0] & 0x1f, slice_type),
                (1, 5),
                "frame {i} must be a non-IDR P slice"
            );
        }
        // Every picture is a reference picture.
        assert_ne!(out.nal.bytes[0] >> 5, 0);
    }
}
