//! Frame-level encoder: frame types, slice assembly, deblocking, reference
//! update and statistics.

use crate::bitstream::{BitWriter, Nal, NalType};
use crate::cavlc::Coverage;
use crate::deblock::{deblock_frame, DeblockParams};
use crate::frame::{plane_sse, psnr, Frame};
use crate::headers::{write_pps, write_slice_header, write_sps, SliceHeader, StreamParams};
use crate::inter::{Mv, RefPic, MV_BORDER, PAD};
use crate::mb::{MbCoder, MbMode, MbRecord};
use crate::ratecontrol::{RateControl, RcMode};
use crate::rng::Rng;
use crate::state::PicState;
use crate::tables::pick_level;

#[derive(Clone, Debug)]
pub struct Config {
    /// Visible picture size in luma samples (even numbers).
    pub width: usize,
    pub height: usize,
    pub fps_num: u32,
    pub fps_den: u32,
    pub rc: RcMode,
    /// Distance between IDR pictures.
    pub keyint: usize,
    /// Integer motion search range in samples around the predictor.
    pub me_range: i32,
    /// 0: integer vectors, 1: half sample, 2: quarter sample.
    pub subpel: u8,
    /// Try 16x8, 8x16 and 8x8 partitions.
    pub partitions: bool,
    /// Try 8x4, 4x8 and 4x4 sub-partitions (where the level allows them).
    pub sub8x8: bool,
    /// Allow intra macroblocks in P pictures.
    pub intra_in_p: bool,
    pub deblock: bool,
    pub alpha_offset_div2: i8,
    pub beta_offset_div2: i8,
    pub chroma_qp_offset: i8,
    /// Drop isolated small coefficients in inter blocks.
    pub decimate: bool,
    /// Fuzzing: replace all decisions with random legal ones (seed, QP range).
    pub fuzz: Option<(u64, u8, u8)>,
}

impl Config {
    pub fn new(width: usize, height: usize, fps_num: u32, fps_den: u32) -> Self {
        Config {
            width,
            height,
            fps_num,
            fps_den,
            rc: RcMode::ConstQp { qp: 28, i_offset: 3 },
            keyint: 250,
            me_range: 16,
            subpel: 2,
            partitions: true,
            sub8x8: true,
            intra_in_p: true,
            deblock: true,
            alpha_offset_div2: 0,
            beta_offset_div2: 0,
            chroma_qp_offset: 0,
            decimate: true,
            fuzz: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    pub index: usize,
    pub idr: bool,
    /// Slice QP.
    pub qp: u8,
    /// Mean QP_Y over macroblocks (differs from `qp` only when it varies).
    pub avg_qp: f64,
    /// Size of the slice NAL unit in bytes.
    pub bytes: usize,
    /// Y, Cb, Cr over the visible area.
    pub sse: [u64; 3],
    pub psnr: [f64; 3],
    pub n_i4: u32,
    pub n_i16: u32,
    pub n_inter: u32,
    pub n_skip: u32,
    /// I_PCM macroblocks (raw samples; only when coding would be larger).
    pub n_pcm: u32,
    /// Inter macroblocks by partition type: 16x16, 16x8, 8x16, 8x8.
    pub parts: [u32; 4],
    /// 8x8 quadrants by sub-partition type: 8x8, 8x4, 4x8, 4x4.
    pub subs: [u32; 4],
}

pub struct EncodedFrame {
    pub nal: Nal,
    pub stats: FrameStats,
    /// One record per macroblock in raster order.
    pub records: Vec<MbRecord>,
}

pub struct Encoder {
    pub cfg: Config,
    pub params: StreamParams,
    pub mb_w: usize,
    pub mb_h: usize,
    recon: Frame,
    refpic: RefPic,
    state: PicState,
    prev_mvs: Vec<Mv>,
    rc: RateControl,
    rng: Option<Rng>,
    sub8x8_ok: bool,
    max_vmv_q: i32,
    frame_index: usize,
    frame_num: u32,
    idr_pic_id: u32,
    since_idr: usize,
    /// Which coeff_token entries have been written so far.
    pub coverage: Coverage,
}

impl Encoder {
    pub fn new(cfg: Config) -> Result<Self, String> {
        if cfg.width < 2 || cfg.height < 2 || !cfg.width.is_multiple_of(2) || !cfg.height.is_multiple_of(2) {
            return Err(format!(
                "picture size {}x{} is not supported: 4:2:0 needs even width and height",
                cfg.width, cfg.height
            ));
        }
        if cfg.fps_num == 0 || cfg.fps_den == 0 {
            return Err("frame rate must be positive".into());
        }
        if !(-12..=12).contains(&cfg.chroma_qp_offset) {
            return Err("chroma QP offset must be within -12..=12".into());
        }
        if !(-6..=6).contains(&cfg.alpha_offset_div2) || !(-6..=6).contains(&cfg.beta_offset_div2) {
            return Err("deblocking offsets must be within -6..=6".into());
        }
        if let RcMode::ConstQp { qp, .. } = cfg.rc {
            if qp > 51 {
                return Err("QP must be within 0..=51".into());
            }
        }
        if cfg.keyint == 0 {
            return Err("keyint must be at least 1".into());
        }
        let mb_w = cfg.width.div_ceil(16);
        let mb_h = cfg.height.div_ceil(16);
        let fps = cfg.fps_num as f64 / cfg.fps_den as f64;
        let kbps = match cfg.rc {
            RcMode::Abr { bps } => Some((bps / 1000.0).ceil() as u32),
            _ => None,
        };
        if cfg.fps_num > 1 << 30 {
            return Err("frame rate numerator is too large".into());
        }
        let level = pick_level(mb_w as u32, mb_h as u32, fps, kbps).ok_or_else(|| {
            format!(
                "{}x{} at {:.2} fps{} exceeds the limits of H.264 level 5.1",
                cfg.width,
                cfg.height,
                fps,
                kbps.map(|k| format!(" and {k} kbit/s")).unwrap_or_default()
            )
        })?;
        let max_vmv_q = level.max_vmv * 4;
        let init_qp = match cfg.rc {
            RcMode::ConstQp { qp, .. } => qp,
            RcMode::Abr { .. } => 26,
        };
        let params = StreamParams {
            width: cfg.width,
            height: cfg.height,
            mb_w,
            mb_h,
            fps_num: cfg.fps_num,
            fps_den: cfg.fps_den,
            level_idc: level.idc,
            init_qp,
            chroma_qp_offset: cfg.chroma_qp_offset,
            max_mv_x: ((mb_w as i32 * 16 + MV_BORDER) * 4).min(8191) as u32,
            max_mv_y: (((mb_h as i32 * 16 + MV_BORDER) * 4).min(max_vmv_q)) as u32,
        };
        let rc = RateControl::new(cfg.rc, fps, cfg.keyint, cfg.width, cfg.height);
        Ok(Encoder {
            rng: cfg.fuzz.map(|(seed, _, _)| Rng::new(seed)),
            // Levels 3.1 and up allow at most 16 vectors per two macroblocks,
            // which 8x8 partitions always satisfy but smaller ones may not.
            sub8x8_ok: level.max_mvs_per_2mb != 16,
            max_vmv_q,
            recon: Frame::new(mb_w * 16, mb_h * 16, PAD),
            refpic: RefPic::new(mb_w * 16, mb_h * 16),
            state: PicState::new(mb_w, mb_h),
            prev_mvs: vec![[0, 0]; mb_w * mb_h],
            rc,
            cfg,
            params,
            mb_w,
            mb_h,
            frame_index: 0,
            frame_num: 0,
            idr_pic_id: 0,
            since_idr: 0,
            coverage: Coverage::default(),
        })
    }

    /// Sequence and picture parameter sets.
    pub fn headers(&self) -> (Nal, Nal) {
        (write_sps(&self.params), write_pps(&self.params))
    }

    /// A frame buffer of the right geometry for `encode`.
    pub fn new_input_frame(&self) -> Frame {
        Frame::new(self.mb_w * 16, self.mb_h * 16, 0)
    }

    /// The reconstruction of the most recently encoded picture, i.e. what a
    /// conforming decoder outputs for it.
    pub fn recon(&self) -> &Frame {
        &self.recon
    }

    /// Encodes one picture. `src` must be macroblock aligned with its
    /// padding already replicated (see `Frame::pad_from_visible`).
    pub fn encode(&mut self, src: &Frame) -> EncodedFrame {
        assert!(
            src.planes[0].w == self.mb_w * 16 && src.planes[0].h == self.mb_h * 16 && src.planes[0].pad == 0,
            "input frame must come from Encoder::new_input_frame()"
        );
        let idr = self.since_idr == 0 || self.since_idr >= self.cfg.keyint;
        if idr {
            self.frame_num = 0;
            self.since_idr = 0;
        }
        let (fuzz_lo, fuzz_hi) = self.cfg.fuzz.map_or((0, 51), |(_, lo, hi)| (lo, hi));
        let qp = match self.rng.as_mut() {
            Some(rng) => rng.range(fuzz_lo as i32, fuzz_hi as i32) as u8,
            None => self.rc.frame_qp(idr),
        };

        let mut bw = BitWriter::with_capacity(self.mb_w * self.mb_h * 32);
        write_slice_header(
            &mut bw,
            &SliceHeader {
                idr,
                frame_num: self.frame_num,
                idr_pic_id: self.idr_pic_id,
                qp,
                init_qp: self.params.init_qp,
                deblock: self.cfg.deblock,
                alpha_offset_div2: self.cfg.alpha_offset_div2,
                beta_offset_div2: self.cfg.beta_offset_div2,
            },
        );

        let mut records = Vec::with_capacity(self.mb_w * self.mb_h);
        {
            let fuzzing = self.rng.is_some();
            let mut c = MbCoder {
                cfg: &self.cfg,
                src,
                recon: &mut self.recon,
                refp: &self.refpic,
                st: &mut self.state,
                bw: &mut bw,
                cov: &mut self.coverage,
                rng: self.rng.as_mut(),
                records: &mut records,
                prev_mvs: &self.prev_mvs,
                is_p: !idr,
                sub8x8_ok: self.sub8x8_ok,
                max_vmv_q: self.max_vmv_q,
                prev_qp: qp,
                skip_run: 0,
                mb_x: 0,
                mb_y: 0,
                cur_y: [0; 256],
                cur_c: [[0; 64]; 2],
                mv_min: [0; 2],
                mv_max: [0; 2],
            };
            for mb_y in 0..self.mb_h {
                for mb_x in 0..self.mb_w {
                    c.begin_mb(mb_x, mb_y);
                    let coded = if fuzzing {
                        c.random_mb(fuzz_lo, fuzz_hi)
                    } else if c.is_p {
                        c.analyse_p(qp)
                    } else {
                        c.analyse_i(qp)
                    };
                    c.write_mb(&coded);
                }
            }
            // Skipped macroblocks at the end of the slice still need their run.
            if c.is_p && c.skip_run > 0 {
                let run = c.skip_run;
                c.bw.ue(run);
            }
        }
        bw.rbsp_trailing();
        let nal = if idr {
            Nal::new(NalType::IdrSlice, 3, &bw.into_bytes())
        } else {
            Nal::new(NalType::Slice, 2, &bw.into_bytes())
        };

        if self.cfg.deblock {
            deblock_frame(
                &mut self.recon,
                &self.state,
                &DeblockParams {
                    alpha_offset_div2: self.cfg.alpha_offset_div2,
                    beta_offset_div2: self.cfg.beta_offset_div2,
                    chroma_qp_offset: self.cfg.chroma_qp_offset,
                },
            );
        }

        let mut stats = FrameStats {
            index: self.frame_index,
            idr,
            qp,
            bytes: nal.bytes.len(),
            ..Default::default()
        };
        let (vw, vh) = (self.cfg.width, self.cfg.height);
        for i in 0..3 {
            let (pw, ph) = if i == 0 { (vw, vh) } else { (vw / 2, vh / 2) };
            stats.sse[i] = plane_sse(&src.planes[i], &self.recon.planes[i], pw, ph);
            stats.psnr[i] = psnr(stats.sse[i], (pw * ph) as u64);
        }
        let mut qp_sum = 0u64;
        for r in &records {
            qp_sum += r.qp as u64;
            match &r.mode {
                MbMode::I4 { .. } => stats.n_i4 += 1,
                MbMode::I16 { .. } => stats.n_i16 += 1,
                MbMode::Skip { .. } => stats.n_skip += 1,
                MbMode::Pcm => stats.n_pcm += 1,
                MbMode::Inter { part, sub, .. } => {
                    stats.n_inter += 1;
                    stats.parts[*part as usize] += 1;
                    if *part == crate::mb::PART_8X8 {
                        for &s in sub {
                            stats.subs[s as usize] += 1;
                        }
                    }
                }
            }
        }
        stats.avg_qp = qp_sum as f64 / records.len() as f64;

        // Motion of this picture seeds the search in the next one.
        let w4 = self.mb_w * 4;
        for mb_y in 0..self.mb_h {
            for mb_x in 0..self.mb_w {
                self.prev_mvs[mb_y * self.mb_w + mb_x] = self.state.motion.mv[mb_y * 4 * w4 + mb_x * 4];
            }
        }
        self.refpic.load(&self.recon);
        self.rc.update(idr, qp, nal.bytes.len() * 8);

        if idr {
            self.idr_pic_id = (self.idr_pic_id + 1) & 0xffff;
        }
        self.frame_num = (self.frame_num + 1) & ((1 << crate::headers::LOG2_MAX_FRAME_NUM) - 1);
        self.since_idr += 1;
        self.frame_index += 1;
        EncodedFrame { nal, stats, records }
    }
}
