//! Macroblock coding: prediction, residual transform and quantisation,
//! reconstruction (exactly as a decoder will do it), and the
//! macroblock_layer() syntax of clause 7.3.5 with CAVLC residuals.
//!
//! The mode decisions live in analysis.rs; this file turns a decision into
//! bits and reconstructed samples.

use crate::bitstream::BitWriter;
use crate::cavlc::{write_block, Coverage};
use crate::encoder::Config;
use crate::frame::{Frame, Plane};
use crate::inter::{Mv, RefPic, MV_BORDER};
use crate::intra::{self, Edge16, Edge4, Edge8};
use crate::rng::Rng;
use crate::state::{PicState, MB_I16X16, MB_I4X4, MB_INTER, MB_PCM, MB_SKIP};
use crate::tables::{cbp_code, chroma_qp, BLK_IDX, BLK_X, BLK_Y, ZIGZAG};
use crate::transform::*;

pub const PART_16X16: u8 = 0;
pub const PART_16X8: u8 = 1;
pub const PART_8X16: u8 = 2;
pub const PART_8X8: u8 = 3;

pub const SUB_8X8: u8 = 0;
pub const SUB_8X4: u8 = 1;
pub const SUB_4X8: u8 = 2;
pub const SUB_4X4: u8 = 3;

/// A motion partition inside a macroblock, in units of 4x4 blocks.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Part {
    pub bx: usize,
    pub by: usize,
    pub pw: usize,
    pub ph: usize,
}

/// Sub-partitions of the 8x8 quadrant at (qx, qy) (in 4x4 units), in decoding order.
pub fn sub_partitions(sub: u8, qx: usize, qy: usize, out: &mut [Part; 16], n: &mut usize) {
    let mut push = |bx, by, pw, ph| {
        out[*n] = Part { bx, by, pw, ph };
        *n += 1;
    };
    match sub {
        SUB_8X8 => push(qx, qy, 2, 2),
        SUB_8X4 => {
            push(qx, qy, 2, 1);
            push(qx, qy + 1, 2, 1);
        }
        SUB_4X8 => {
            push(qx, qy, 1, 2);
            push(qx + 1, qy, 1, 2);
        }
        _ => {
            push(qx, qy, 1, 1);
            push(qx + 1, qy, 1, 1);
            push(qx, qy + 1, 1, 1);
            push(qx + 1, qy + 1, 1, 1);
        }
    }
}

/// All motion partitions of a macroblock in decoding order.
pub fn partitions(part: u8, sub: &[u8; 4], out: &mut [Part; 16]) -> usize {
    let mut n = 0;
    match part {
        PART_16X16 => {
            out[0] = Part {
                bx: 0,
                by: 0,
                pw: 4,
                ph: 4,
            };
            n = 1;
        }
        PART_16X8 => {
            out[0] = Part {
                bx: 0,
                by: 0,
                pw: 4,
                ph: 2,
            };
            out[1] = Part {
                bx: 0,
                by: 2,
                pw: 4,
                ph: 2,
            };
            n = 2;
        }
        PART_8X16 => {
            out[0] = Part {
                bx: 0,
                by: 0,
                pw: 2,
                ph: 4,
            };
            out[1] = Part {
                bx: 2,
                by: 0,
                pw: 2,
                ph: 4,
            };
            n = 2;
        }
        _ => {
            for q in 0..4 {
                sub_partitions(sub[q], (q % 2) * 2, (q / 2) * 2, out, &mut n);
            }
        }
    }
    n
}

#[derive(Clone, Debug)]
pub enum MbMode {
    /// Intra 4x4 with one prediction mode per block (indexed by luma4x4BlkIdx).
    I4 {
        modes: [u8; 16],
    },
    I16 {
        mode: u8,
    },
    /// Motion vectors are stored per 4x4 block in raster order.
    Inter {
        part: u8,
        sub: [u8; 4],
        mvs: [Mv; 16],
    },
    Skip {
        mv: Mv,
    },
    /// Raw samples (I_PCM): the fallback when coding would exceed the
    /// 3200-bit macroblock limit of Annex A.
    Pcm,
}

/// Annex A.3.1: macroblock_layer() must not exceed this many bits.
pub const MAX_MB_BITS: u32 = 3200;

/// Quantised coefficients of one macroblock, in scan order.
#[derive(Clone, Default)]
pub struct Residual {
    /// Per luma4x4BlkIdx; for Intra16x16 entry 0 of each block is unused.
    pub luma: [[i32; 16]; 16],
    pub luma_dc: [i32; 16],
    pub chroma_dc: [[i32; 4]; 2],
    /// Per plane and block; entry 0 of each block is unused.
    pub chroma_ac: [[[i32; 16]; 4]; 2],
    /// coded_block_pattern: bits 0..3 luma 8x8 blocks, bits 4..5 chroma (0, 1 or 2).
    pub cbp: u8,
}

/// Test hook: forces parts of the residual to zero so that every
/// coded_block_pattern can be produced regardless of content.
#[derive(Clone, Copy, Default, Debug)]
pub struct Force {
    /// Bit i set: zero the i-th luma 8x8 block.
    pub zero_luma8: u8,
    pub zero_chroma_ac: bool,
    pub zero_chroma: bool,
}

/// What was coded for one macroblock; kept for statistics and the report.
#[derive(Clone, Debug)]
pub struct MbRecord {
    pub mode: MbMode,
    pub chroma_mode: u8,
    pub qp: u8,
    pub cbp: u8,
    pub bits: u32,
}

/// A fully coded macroblock ready for the syntax writer.
pub struct Coded {
    pub mode: MbMode,
    pub chroma_mode: u8,
    pub res: Residual,
    pub qp: u8,
}

/// Coefficient thresholding scores: an isolated +/-1 after a run of zeros
/// costs more bits than the distortion it removes.
const DECIMATE_SCORE: [u32; 16] = [3, 2, 2, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Score of one block in scan order (starting at `first`): 9 or more means
/// "keep", small values mean the block is cheap to drop.
fn decimate_score(scan: &[i32], first: usize) -> u32 {
    let mut score = 0;
    let mut run = 0;
    for &l in &scan[first..] {
        if l == 0 {
            run += 1;
        } else {
            if l.abs() > 1 {
                return 9;
            }
            score += DECIMATE_SCORE[run];
            run = 0;
        }
    }
    score
}

pub struct MbCoder<'a> {
    pub cfg: &'a Config,
    pub src: &'a Frame,
    pub recon: &'a mut Frame,
    pub refp: &'a RefPic,
    pub st: &'a mut PicState,
    pub bw: &'a mut BitWriter,
    pub cov: &'a mut Coverage,
    pub rng: Option<&'a mut Rng>,
    pub records: &'a mut Vec<MbRecord>,
    /// Motion of the co-located macroblock in the previous picture.
    pub prev_mvs: &'a [Mv],
    pub is_p: bool,
    pub sub8x8_ok: bool,
    /// Vertical vector limit of the level, in quarter samples.
    pub max_vmv_q: i32,
    /// QP_Y of the previous macroblock in decoding order (QP_Y,PRED).
    pub prev_qp: u8,
    pub skip_run: u32,
    pub mb_x: usize,
    pub mb_y: usize,
    pub cur_y: [u8; 256],
    pub cur_c: [[u8; 64]; 2],
    pub mv_min: [i32; 2],
    pub mv_max: [i32; 2],
}

fn copy_block(p: &Plane, x: usize, y: usize, w: usize, h: usize, out: &mut [u8]) {
    for r in 0..h {
        let s = p.idx(x as i32, (y + r) as i32);
        out[r * w..r * w + w].copy_from_slice(&p.data[s..s + w]);
    }
}

fn put_block(p: &mut Plane, x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
    for r in 0..h {
        let s = p.idx(x as i32, (y + r) as i32);
        p.data[s..s + w].copy_from_slice(&data[r * w..r * w + w]);
    }
}

impl MbCoder<'_> {
    #[inline]
    pub fn mb_index(&self) -> usize {
        self.mb_y * self.st.mb_w + self.mb_x
    }

    /// Loads the source samples and vector limits for macroblock (mb_x, mb_y).
    pub fn begin_mb(&mut self, mb_x: usize, mb_y: usize) {
        self.mb_x = mb_x;
        self.mb_y = mb_y;
        copy_block(&self.src.planes[0], mb_x * 16, mb_y * 16, 16, 16, &mut self.cur_y);
        copy_block(&self.src.planes[1], mb_x * 8, mb_y * 8, 8, 8, &mut self.cur_c[0]);
        copy_block(&self.src.planes[2], mb_x * 8, mb_y * 8, 8, 8, &mut self.cur_c[1]);
        let (w, h) = (self.st.mb_w as i32 * 16, self.st.mb_h as i32 * 16);
        let (x0, y0) = (mb_x as i32 * 16, mb_y as i32 * 16);
        // The 16x16 block may reach MV_BORDER samples outside the picture;
        // partitions inside the macroblock then stay inside that area too.
        self.mv_min = [(-MV_BORDER - x0) * 4, ((-MV_BORDER - y0) * 4).max(-self.max_vmv_q)];
        self.mv_max = [
            (w + MV_BORDER - 16 - x0) * 4,
            ((h + MV_BORDER - 16 - y0) * 4).min(self.max_vmv_q - 1),
        ];
    }

    #[inline]
    pub fn mv_in_range(&self, mv: Mv) -> bool {
        let (x, y) = (mv[0] as i32, mv[1] as i32);
        x >= self.mv_min[0] && x <= self.mv_max[0] && y >= self.mv_min[1] && y <= self.mv_max[1]
    }

    // ----- neighbour samples for intra prediction (unfiltered reconstruction) -----

    pub fn edge16(&self) -> Edge16 {
        let p = &self.recon.planes[0];
        let (x0, y0) = (self.mb_x as i32 * 16, self.mb_y as i32 * 16);
        let (has_top, has_left) = (self.mb_y > 0, self.mb_x > 0);
        let mut e = Edge16 {
            top: [0; 16],
            left: [0; 16],
            tl: 0,
            has_top,
            has_left,
        };
        if has_top {
            let s = p.idx(x0, y0 - 1);
            e.top.copy_from_slice(&p.data[s..s + 16]);
        }
        if has_left {
            for (i, v) in e.left.iter_mut().enumerate() {
                *v = p.at(x0 - 1, y0 + i as i32);
            }
        }
        if has_top && has_left {
            e.tl = p.at(x0 - 1, y0 - 1);
        }
        e
    }

    pub fn edge8(&self, plane: usize) -> Edge8 {
        let p = &self.recon.planes[1 + plane];
        let (x0, y0) = (self.mb_x as i32 * 8, self.mb_y as i32 * 8);
        let (has_top, has_left) = (self.mb_y > 0, self.mb_x > 0);
        let mut e = Edge8 {
            top: [0; 8],
            left: [0; 8],
            tl: 0,
            has_top,
            has_left,
        };
        if has_top {
            let s = p.idx(x0, y0 - 1);
            e.top.copy_from_slice(&p.data[s..s + 8]);
        }
        if has_left {
            for (i, v) in e.left.iter_mut().enumerate() {
                *v = p.at(x0 - 1, y0 + i as i32);
            }
        }
        if has_top && has_left {
            e.tl = p.at(x0 - 1, y0 - 1);
        }
        e
    }

    /// Whether the block above-right of 4x4 block (bx, by) is already decoded.
    fn i4_top_right_available(&self, bx: usize, by: usize) -> bool {
        if by == 0 {
            self.mb_y > 0 && (bx < 3 || self.mb_x + 1 < self.st.mb_w)
        } else if bx == 3 {
            false
        } else {
            BLK_IDX[(by - 1) * 4 + bx + 1] < BLK_IDX[by * 4 + bx]
        }
    }

    pub fn edge4(&self, bx: usize, by: usize) -> Edge4 {
        let p = &self.recon.planes[0];
        let px = (self.mb_x * 16 + bx * 4) as i32;
        let py = (self.mb_y * 16 + by * 4) as i32;
        let (has_top, has_left) = (py > 0, px > 0);
        let mut e = Edge4 {
            has_top,
            has_left,
            ..Default::default()
        };
        if has_top {
            let s = p.idx(px, py - 1);
            e.top[..4].copy_from_slice(&p.data[s..s + 4]);
            if self.i4_top_right_available(bx, by) {
                e.top[4..].copy_from_slice(&p.data[s + 4..s + 8]);
            } else {
                let last = e.top[3];
                e.top[4..].fill(last);
            }
        }
        if has_left {
            for (i, v) in e.left.iter_mut().enumerate() {
                *v = p.at(px - 1, py + i as i32);
            }
        }
        if has_top && has_left {
            e.tl = p.at(px - 1, py - 1);
        }
        e
    }

    /// Predicted Intra4x4PredMode for block (bx, by) (clause 8.3.1.1).
    pub fn predicted_i4_mode(&self, bx: usize, by: usize) -> u8 {
        let x4 = self.mb_x * 4 + bx;
        let y4 = self.mb_y * 4 + by;
        if x4 == 0 || y4 == 0 {
            return intra::I4_DC;
        }
        let w4 = self.st.mb_w * 4;
        self.st.i4mode[y4 * w4 + x4 - 1].min(self.st.i4mode[(y4 - 1) * w4 + x4])
    }

    // ----- luma -----

    /// Transforms and quantises the luma residual against `pred`, applies
    /// thresholding and forced zeroing, then reconstructs into the picture.
    pub fn luma_residual(
        &mut self,
        pred: &[u8; 256],
        qp: u8,
        i16: bool,
        intra: bool,
        force: Force,
        res: &mut Residual,
    ) {
        let mut lv = [[0i32; 16]; 16];
        let mut dc = [0i32; 16];
        let mut nz = [0u32; 16];
        for blk in 0..16 {
            let (bx, by) = (BLK_X[blk], BLK_Y[blk]);
            let mut w = [0i32; 16];
            for y in 0..4 {
                let o = (by * 4 + y) * 16 + bx * 4;
                for x in 0..4 {
                    w[y * 4 + x] = self.cur_y[o + x] as i32 - pred[o + x] as i32;
                }
            }
            fdct4x4(&mut w);
            if i16 {
                dc[by * 4 + bx] = w[0];
                w[0] = 0;
            }
            nz[blk] = quant4x4(&mut w, qp, intra, i16);
            lv[blk] = w;
        }
        let mut cbp = 0u8;
        if i16 {
            luma_dc_forward(&mut dc);
            quant_dc(&mut dc, qp, true);
            if nz.iter().any(|&n| n > 0) && force.zero_luma8 == 0 {
                cbp = 15;
            } else {
                lv = [[0; 16]; 16];
            }
        } else {
            let mut scores = [0u32; 4];
            for q in 0..4 {
                if force.zero_luma8 >> q & 1 == 1 {
                    continue;
                }
                let mut any = false;
                for blk in q * 4..q * 4 + 4 {
                    if nz[blk] > 0 {
                        any = true;
                        let scan: [i32; 16] = std::array::from_fn(|i| lv[blk][ZIGZAG[i]]);
                        scores[q] += decimate_score(&scan, 0);
                    }
                }
                if any {
                    cbp |= 1 << q;
                }
            }
            if self.cfg.decimate && !intra {
                for q in 0..4 {
                    if scores[q] < 4 {
                        cbp &= !(1 << q);
                    }
                }
                let total: u32 = (0..4).filter(|q| cbp >> q & 1 == 1).map(|q| scores[q]).sum();
                if total < 6 {
                    cbp = 0;
                }
            }
            for q in 0..4 {
                if cbp >> q & 1 == 0 {
                    for blk in q * 4..q * 4 + 4 {
                        lv[blk] = [0; 16];
                    }
                }
            }
        }
        for blk in 0..16 {
            res.luma[blk] = std::array::from_fn(|i| lv[blk][ZIGZAG[i]]);
        }
        res.luma_dc = std::array::from_fn(|i| dc[ZIGZAG[i]]);
        res.cbp = (res.cbp & 0x30) | cbp;

        // Reconstruction, following the decoder's arithmetic.
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        put_block(&mut self.recon.planes[0], x0, y0, 16, 16, pred);
        if i16 {
            dequant_luma_dc(&mut dc, qp);
        }
        let plane = &mut self.recon.planes[0];
        for blk in 0..16 {
            let (bx, by) = (BLK_X[blk], BLK_Y[blk]);
            let mut d = lv[blk];
            dequant4x4(&mut d, qp, i16);
            if i16 {
                d[0] = dc[by * 4 + bx];
            }
            if d.iter().any(|&v| v != 0) {
                let r = idct4x4(&d);
                let o = plane.idx((x0 + bx * 4) as i32, (y0 + by * 4) as i32);
                add_residual(&mut plane.data[o..], plane.stride, &r);
            }
        }
    }

    pub fn encode_i16(&mut self, mode: u8, qp: u8, force: Force, res: &mut Residual) {
        let mut pred = [0u8; 256];
        intra::pred16x16(mode, &self.edge16(), &mut pred);
        self.luma_residual(&pred, qp, true, true, force, res);
    }

    /// Codes the sixteen 4x4 blocks in decoding order. With `modes` None the
    /// best mode per block is chosen by SATD + lambda * mode bits.
    /// Returns the modes used and the accumulated cost.
    pub fn encode_i4(
        &mut self,
        modes: Option<&[u8; 16]>,
        qp: u8,
        force: Force,
        lambda16: u32,
        res: &mut Residual,
    ) -> ([u8; 16], u32) {
        let mut used = [0u8; 16];
        let mut total_cost = 0u32;
        let mut cbp = 0u8;
        let w4 = self.st.mb_w * 4;
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        for blk in 0..16 {
            let (bx, by) = (BLK_X[blk], BLK_Y[blk]);
            let e = self.edge4(bx, by);
            let mut src = [0u8; 16];
            for y in 0..4 {
                let o = (by * 4 + y) * 16 + bx * 4;
                src[y * 4..y * 4 + 4].copy_from_slice(&self.cur_y[o..o + 4]);
            }
            let mut pred = [0u8; 16];
            let mode = match modes {
                Some(m) => {
                    intra::pred4x4(m[blk], &e, &mut pred);
                    m[blk]
                }
                None => {
                    let predicted = self.predicted_i4_mode(bx, by);
                    let (mut best, mut best_cost) = (intra::I4_DC, u32::MAX);
                    let mut trial = [0u8; 16];
                    for m in 0..9u8 {
                        if !intra::i4_mode_allowed(m, e.has_top, e.has_left) {
                            continue;
                        }
                        intra::pred4x4(m, &e, &mut trial);
                        let bits = if m == predicted { 1 } else { 4 };
                        let cost = crate::cost::satd4x4(&src, 4, &trial, 4) + ((lambda16 * bits + 8) >> 4);
                        if cost < best_cost {
                            best_cost = cost;
                            best = m;
                            pred = trial;
                        }
                    }
                    total_cost += best_cost;
                    best
                }
            };
            used[blk] = mode;
            self.st.i4mode[(self.mb_y * 4 + by) * w4 + self.mb_x * 4 + bx] = mode;

            let mut w: [i32; 16] = std::array::from_fn(|i| src[i] as i32 - pred[i] as i32);
            fdct4x4(&mut w);
            let mut nz = quant4x4(&mut w, qp, true, false);
            if force.zero_luma8 >> (blk / 4) & 1 == 1 {
                w = [0; 16];
                nz = 0;
            }
            res.luma[blk] = std::array::from_fn(|i| w[ZIGZAG[i]]);
            let plane = &mut self.recon.planes[0];
            put_block(plane, x0 + bx * 4, y0 + by * 4, 4, 4, &pred);
            if nz > 0 {
                cbp |= 1 << (blk / 4);
                dequant4x4(&mut w, qp, false);
                let r = idct4x4(&w);
                let o = plane.idx((x0 + bx * 4) as i32, (y0 + by * 4) as i32);
                add_residual(&mut plane.data[o..], plane.stride, &r);
            }
        }
        res.cbp = (res.cbp & 0x30) | cbp;
        (used, total_cost)
    }

    pub fn inter_luma_pred(&self, part: u8, sub: &[u8; 4], mvs: &[Mv; 16], pred: &mut [u8; 256]) {
        let mut parts = [Part::default(); 16];
        let n = partitions(part, sub, &mut parts);
        let (x0, y0) = (self.mb_x as i32 * 16, self.mb_y as i32 * 16);
        for p in &parts[..n] {
            self.refp.mc_luma(
                x0 + 4 * p.bx as i32,
                y0 + 4 * p.by as i32,
                mvs[p.by * 4 + p.bx],
                p.pw * 4,
                p.ph * 4,
                &mut pred[p.by * 64 + p.bx * 4..],
                16,
            );
        }
    }

    pub fn inter_chroma_pred(&self, part: u8, sub: &[u8; 4], mvs: &[Mv; 16], pred: &mut [[u8; 64]; 2]) {
        let mut parts = [Part::default(); 16];
        let n = partitions(part, sub, &mut parts);
        let (x0, y0) = (self.mb_x as i32 * 8, self.mb_y as i32 * 8);
        for p in &parts[..n] {
            for (plane, out) in pred.iter_mut().enumerate() {
                self.refp.mc_chroma(
                    plane,
                    x0 + 2 * p.bx as i32,
                    y0 + 2 * p.by as i32,
                    mvs[p.by * 4 + p.bx],
                    p.pw * 2,
                    p.ph * 2,
                    &mut out[p.by * 16 + p.bx * 2..],
                    8,
                );
            }
        }
    }

    /// Luma and chroma of an inter macroblock.
    pub fn encode_inter(&mut self, part: u8, sub: &[u8; 4], mvs: &[Mv; 16], qp: u8, force: Force) -> Residual {
        let mut res = Residual::default();
        let mut pred = [0u8; 256];
        self.inter_luma_pred(part, sub, mvs, &mut pred);
        self.luma_residual(&pred, qp, false, false, force, &mut res);
        let mut cpred = [[0u8; 64]; 2];
        self.inter_chroma_pred(part, sub, mvs, &mut cpred);
        self.encode_chroma(&cpred, qp, false, force, &mut res);
        res
    }

    // ----- chroma -----

    pub fn intra_chroma_pred(&self, mode: u8, pred: &mut [[u8; 64]; 2]) {
        for (plane, out) in pred.iter_mut().enumerate() {
            intra::pred_chroma(mode, &self.edge8(plane), out);
        }
    }

    pub fn encode_chroma(&mut self, pred: &[[u8; 64]; 2], qp: u8, intra: bool, force: Force, res: &mut Residual) {
        let qpc = chroma_qp(qp, self.cfg.chroma_qp_offset);
        let mut ac = [[[0i32; 16]; 4]; 2];
        let mut dc = [[0i32; 4]; 2];
        let (mut any_ac, mut any_dc) = (false, false);
        let mut ac_score = 0u32;
        for plane in 0..2 {
            for blk in 0..4 {
                let (bx, by) = (blk % 2, blk / 2);
                let mut w = [0i32; 16];
                for y in 0..4 {
                    let o = (by * 4 + y) * 8 + bx * 4;
                    for x in 0..4 {
                        w[y * 4 + x] = self.cur_c[plane][o + x] as i32 - pred[plane][o + x] as i32;
                    }
                }
                fdct4x4(&mut w);
                dc[plane][blk] = w[0];
                w[0] = 0;
                if quant4x4(&mut w, qpc, intra, true) > 0 {
                    any_ac = true;
                    let scan: [i32; 16] = std::array::from_fn(|i| w[ZIGZAG[i]]);
                    ac_score += decimate_score(&scan, 1);
                }
                ac[plane][blk] = w;
            }
            hadamard2x2(&mut dc[plane]);
            any_dc |= quant_dc(&mut dc[plane], qpc, intra) > 0;
        }
        if self.cfg.decimate && !intra && ac_score < 7 {
            any_ac = false;
        }
        if force.zero_chroma_ac || force.zero_chroma {
            any_ac = false;
        }
        if force.zero_chroma {
            any_dc = false;
        }
        let cbp_c = if any_ac {
            2
        } else if any_dc {
            1
        } else {
            0
        };
        if cbp_c < 2 {
            ac = [[[0; 16]; 4]; 2];
        }
        if cbp_c == 0 {
            dc = [[0; 4]; 2];
        }
        res.cbp = (res.cbp & 0x0f) | (cbp_c << 4);
        res.chroma_dc = dc;
        for plane in 0..2 {
            for blk in 0..4 {
                res.chroma_ac[plane][blk] = std::array::from_fn(|i| ac[plane][blk][ZIGZAG[i]]);
            }
        }

        let (x0, y0) = (self.mb_x * 8, self.mb_y * 8);
        for plane in 0..2 {
            let p = &mut self.recon.planes[1 + plane];
            put_block(p, x0, y0, 8, 8, &pred[plane]);
            if cbp_c == 0 {
                continue;
            }
            let mut dcq = dc[plane];
            dequant_chroma_dc(&mut dcq, qpc);
            for blk in 0..4 {
                let (bx, by) = (blk % 2, blk / 2);
                let mut d = ac[plane][blk];
                dequant4x4(&mut d, qpc, true);
                d[0] = dcq[blk];
                if d.iter().any(|&v| v != 0) {
                    let r = idct4x4(&d);
                    let o = p.idx((x0 + bx * 4) as i32, (y0 + by * 4) as i32);
                    add_residual(&mut p.data[o..], p.stride, &r);
                }
            }
        }
    }

    // ----- syntax -----

    #[inline]
    fn nc(nnz: &[u8], stride: usize, x: usize, y: usize) -> i32 {
        let a = if x > 0 {
            Some(nnz[y * stride + x - 1] as i32)
        } else {
            None
        };
        let b = if y > 0 {
            Some(nnz[(y - 1) * stride + x] as i32)
        } else {
            None
        };
        match (a, b) {
            (Some(a), Some(b)) => (a + b + 1) >> 1,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => 0,
        }
    }

    /// Codes the macroblock as I_PCM: the source samples verbatim.
    fn write_pcm(&mut self) {
        let mb = self.mb_index();
        let (mb_x, mb_y) = (self.mb_x, self.mb_y);
        let w4 = self.st.mb_w * 4;
        let w2 = self.st.mb_w * 2;
        let start_bits = self.bw.bit_len();
        // Neighbours see sixteen coefficients in every block of an I_PCM macroblock.
        for y in 0..4 {
            let o = (mb_y * 4 + y) * w4 + mb_x * 4;
            self.st.nnz_y[o..o + 4].fill(16);
            self.st.i4mode[o..o + 4].fill(intra::I4_DC);
        }
        for plane in 0..2 {
            for y in 0..2 {
                let o = (mb_y * 2 + y) * w2 + mb_x * 2;
                self.st.nnz_c[plane][o..o + 2].fill(16);
            }
        }
        self.st.motion.fill(mb_x, mb_y, 0, 0, 4, 4, -1, [0, 0]);
        if self.is_p {
            self.bw.ue(self.skip_run);
            self.skip_run = 0;
        }
        self.bw.ue(if self.is_p { 30 } else { 25 }); // mb_type I_PCM
        self.bw.align_zero();
        self.bw.put_bytes(&self.cur_y);
        self.bw.put_bytes(&self.cur_c[0]);
        self.bw.put_bytes(&self.cur_c[1]);
        put_block(&mut self.recon.planes[0], mb_x * 16, mb_y * 16, 16, 16, &self.cur_y);
        put_block(&mut self.recon.planes[1], mb_x * 8, mb_y * 8, 8, 8, &self.cur_c[0]);
        put_block(&mut self.recon.planes[2], mb_x * 8, mb_y * 8, 8, 8, &self.cur_c[1]);
        self.st.kind[mb] = MB_PCM;
        // The deblocking filter treats I_PCM as QP 0; QP prediction for the
        // next macroblock is unaffected (prev_qp stays).
        self.st.qp[mb] = 0;
        self.records.push(MbRecord {
            mode: MbMode::Pcm,
            chroma_mode: 0,
            qp: 0,
            cbp: 0,
            bits: (self.bw.bit_len() - start_bits) as u32,
        });
    }

    /// Writes the macroblock (or extends the skip run) and updates all
    /// neighbour context. Falls back to I_PCM if the coded form would be
    /// larger than the level limits allow.
    pub fn write_mb(&mut self, coded: &Coded) {
        if let MbMode::Pcm = coded.mode {
            self.write_pcm();
            return;
        }
        let mark = self.bw.mark();
        let saved = (self.skip_run, self.prev_qp, self.records.len());
        let layer_bits = self.write_mb_coded(coded);
        if layer_bits > MAX_MB_BITS {
            self.bw.rewind(mark);
            self.skip_run = saved.0;
            self.prev_qp = saved.1;
            self.records.truncate(saved.2);
            self.write_pcm();
        }
    }

    /// Returns the size of macroblock_layer() in bits (0 for a skip).
    fn write_mb_coded(&mut self, coded: &Coded) -> u32 {
        let mb = self.mb_index();
        let (mb_x, mb_y) = (self.mb_x, self.mb_y);
        let w4 = self.st.mb_w * 4;
        let w2 = self.st.mb_w * 2;
        let start_bits = self.bw.bit_len();
        let res = &coded.res;

        // Reset this macroblock's coefficient counts; coded blocks fill them in.
        for y in 0..4 {
            let o = (mb_y * 4 + y) * w4 + mb_x * 4;
            self.st.nnz_y[o..o + 4].fill(0);
        }
        for plane in 0..2 {
            for y in 0..2 {
                let o = (mb_y * 2 + y) * w2 + mb_x * 2;
                self.st.nnz_c[plane][o..o + 2].fill(0);
            }
        }
        // Intra pred mode context: DC for everything that is not Intra4x4.
        if !matches!(coded.mode, MbMode::I4 { .. }) {
            for y in 0..4 {
                let o = (mb_y * 4 + y) * w4 + mb_x * 4;
                self.st.i4mode[o..o + 4].fill(intra::I4_DC);
            }
        }
        // Motion context.
        match &coded.mode {
            MbMode::Inter { mvs, .. } => {
                for by in 0..4 {
                    for bx in 0..4 {
                        self.st.motion.fill(mb_x, mb_y, bx, by, 1, 1, 0, mvs[by * 4 + bx]);
                    }
                }
            }
            MbMode::Skip { mv } => self.st.motion.fill(mb_x, mb_y, 0, 0, 4, 4, 0, *mv),
            _ => self.st.motion.fill(mb_x, mb_y, 0, 0, 4, 4, -1, [0, 0]),
        }

        if let MbMode::Skip { .. } = coded.mode {
            debug_assert!(self.is_p && res.cbp == 0);
            self.skip_run += 1;
            self.st.kind[mb] = MB_SKIP;
            self.st.qp[mb] = self.prev_qp;
            self.records.push(MbRecord {
                mode: coded.mode.clone(),
                chroma_mode: 0,
                qp: self.prev_qp,
                cbp: 0,
                bits: 0,
            });
            return 0;
        }

        if self.is_p {
            self.bw.ue(self.skip_run);
            self.skip_run = 0;
        }
        let layer_start = self.bw.bit_len();
        let cbp = res.cbp;
        let (cbp_luma, cbp_chroma) = (cbp & 15, cbp >> 4);
        let intra_offset = if self.is_p { 5 } else { 0 };
        let mut is_i16 = false;
        match &coded.mode {
            MbMode::I4 { modes } => {
                self.bw.ue(intra_offset); // I_NxN
                for blk in 0..16 {
                    let predicted = self.predicted_i4_mode(BLK_X[blk], BLK_Y[blk]);
                    let m = modes[blk];
                    if m == predicted {
                        self.bw.put1(true); // prev_intra4x4_pred_mode_flag
                    } else {
                        self.bw.put1(false);
                        self.bw.put(3, if m < predicted { m } else { m - 1 } as u32);
                        // rem_intra4x4_pred_mode
                    }
                }
                self.bw.ue(coded.chroma_mode as u32);
                self.st.kind[mb] = MB_I4X4;
            }
            MbMode::I16 { mode } => {
                is_i16 = true;
                debug_assert!(cbp_luma == 0 || cbp_luma == 15);
                // Table 7-11: mb_type folds prediction mode and both CBP parts.
                let mb_type = 1 + *mode as u32 + 4 * cbp_chroma as u32 + if cbp_luma != 0 { 12 } else { 0 };
                self.bw.ue(intra_offset + mb_type);
                self.bw.ue(coded.chroma_mode as u32);
                self.st.kind[mb] = MB_I16X16;
            }
            MbMode::Inter { part, sub, mvs } => {
                self.bw.ue(*part as u32);
                if *part == PART_8X8 {
                    for &s in sub {
                        self.bw.ue(s as u32);
                    }
                }
                // ref_idx_l0 is absent: only one reference picture is active.
                let mut parts = [Part::default(); 16];
                let n = partitions(*part, sub, &mut parts);
                for p in &parts[..n] {
                    let pmv = self.st.motion.predict(mb_x, mb_y, p.bx, p.by, p.pw, p.ph);
                    let mv = mvs[p.by * 4 + p.bx];
                    self.bw.se(mv[0] as i32 - pmv[0] as i32);
                    self.bw.se(mv[1] as i32 - pmv[1] as i32);
                }
                self.st.kind[mb] = MB_INTER;
            }
            MbMode::Skip { .. } | MbMode::Pcm => unreachable!(),
        }
        if !is_i16 {
            self.bw.ue(cbp_code(cbp, self.st.kind[mb] == MB_I4X4));
        }
        if cbp > 0 || is_i16 {
            let delta = coded.qp as i32 - self.prev_qp as i32;
            debug_assert!((-26..=25).contains(&delta));
            self.bw.se(delta); // mb_qp_delta
            self.prev_qp = coded.qp;

            if is_i16 {
                let nc = Self::nc(&self.st.nnz_y, w4, mb_x * 4, mb_y * 4);
                write_block(self.bw, &res.luma_dc, nc, self.cov);
            }
            for blk in 0..16 {
                if cbp_luma >> (blk / 4) & 1 == 0 {
                    continue;
                }
                let (x, y) = (mb_x * 4 + BLK_X[blk], mb_y * 4 + BLK_Y[blk]);
                let nc = Self::nc(&self.st.nnz_y, w4, x, y);
                let coeffs = if is_i16 {
                    &res.luma[blk][1..]
                } else {
                    &res.luma[blk][..]
                };
                self.st.nnz_y[y * w4 + x] = write_block(self.bw, coeffs, nc, self.cov);
            }
            if cbp_chroma > 0 {
                for plane in 0..2 {
                    write_block(self.bw, &res.chroma_dc[plane], -1, self.cov);
                }
            }
            if cbp_chroma == 2 {
                for plane in 0..2 {
                    for blk in 0..4 {
                        let (x, y) = (mb_x * 2 + blk % 2, mb_y * 2 + blk / 2);
                        let nc = Self::nc(&self.st.nnz_c[plane], w2, x, y);
                        self.st.nnz_c[plane][y * w2 + x] =
                            write_block(self.bw, &res.chroma_ac[plane][blk][1..], nc, self.cov);
                    }
                }
            }
        }
        // Without a transmitted delta the decoder keeps the previous QP.
        self.st.qp[mb] = self.prev_qp;
        self.records.push(MbRecord {
            mode: coded.mode.clone(),
            chroma_mode: coded.chroma_mode,
            qp: self.prev_qp,
            cbp,
            bits: (self.bw.bit_len() - start_bits) as u32,
        });
        (self.bw.bit_len() - layer_start) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_lists() {
        let mut out = [Part::default(); 16];
        assert_eq!(partitions(PART_16X16, &[0; 4], &mut out), 1);
        assert_eq!(partitions(PART_16X8, &[0; 4], &mut out), 2);
        assert_eq!(
            out[1],
            Part {
                bx: 0,
                by: 2,
                pw: 4,
                ph: 2
            }
        );
        assert_eq!(partitions(PART_8X16, &[0; 4], &mut out), 2);
        assert_eq!(
            out[1],
            Part {
                bx: 2,
                by: 0,
                pw: 2,
                ph: 4
            }
        );
        let n = partitions(PART_8X8, &[SUB_8X8, SUB_8X4, SUB_4X8, SUB_4X4], &mut out);
        assert_eq!(n, 1 + 2 + 2 + 4);
        assert_eq!(
            out[0],
            Part {
                bx: 0,
                by: 0,
                pw: 2,
                ph: 2
            }
        );
        assert_eq!(
            out[2],
            Part {
                bx: 2,
                by: 1,
                pw: 2,
                ph: 1
            }
        );
        assert_eq!(
            out[4],
            Part {
                bx: 1,
                by: 2,
                pw: 1,
                ph: 2
            }
        );
        assert_eq!(
            out[8],
            Part {
                bx: 3,
                by: 3,
                pw: 1,
                ph: 1
            }
        );
        // Every list tiles the macroblock exactly once.
        for part in 0..4u8 {
            for s in 0..4u8 {
                let n = partitions(part, &[s; 4], &mut out);
                let mut seen = [0u8; 16];
                for p in &out[..n] {
                    for y in p.by..p.by + p.ph {
                        for x in p.bx..p.bx + p.pw {
                            seen[y * 4 + x] += 1;
                        }
                    }
                }
                assert!(seen.iter().all(|&c| c == 1));
            }
        }
    }

    #[test]
    fn decimation_scores() {
        let mut scan = [0i32; 16];
        assert_eq!(decimate_score(&scan, 0), 0);
        scan[0] = 1;
        assert_eq!(decimate_score(&scan, 0), 3);
        scan[5] = -1; // after a run of four zeros
        assert_eq!(decimate_score(&scan, 0), 4);
        scan[6] = 2;
        assert_eq!(decimate_score(&scan, 0), 9);
        // AC-only scoring ignores position 0.
        let mut scan = [0i32; 16];
        scan[0] = 50;
        scan[9] = 1;
        assert_eq!(decimate_score(&scan, 1), 0);
    }
}
