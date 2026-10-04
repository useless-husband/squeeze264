//! In-loop deblocking filter (clause 8.7) for progressive frames with
//! 4x4 transforms and 4:2:0 chroma. Runs over the whole reconstructed
//! picture after all macroblocks are coded, exactly as a decoder does,
//! because the filtered picture is the reference for the next one.

use crate::frame::Frame;
use crate::state::PicState;
use crate::tables::{chroma_qp, ALPHA, BETA, TC0};

#[derive(Clone, Copy, Debug, Default)]
pub struct DeblockParams {
    /// slice_alpha_c0_offset_div2 and slice_beta_offset_div2, each -6..=6.
    pub alpha_offset_div2: i8,
    pub beta_offset_div2: i8,
    pub chroma_qp_offset: i8,
}

/// Boundary strength between 4x4 luma blocks `p` and `q` (indices into the
/// per-4x4 arrays), where `mb_edge` says the edge is a macroblock edge.
#[inline]
pub fn boundary_strength(s: &PicState, p: usize, q: usize, mb_p: usize, mb_q: usize, mb_edge: bool) -> u8 {
    if s.is_intra(mb_p) || s.is_intra(mb_q) {
        return if mb_edge { 4 } else { 3 };
    }
    if s.nnz_y[p] != 0 || s.nnz_y[q] != 0 {
        return 2;
    }
    let (a, b) = (s.motion.mv[p], s.motion.mv[q]);
    if (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4 {
        return 1;
    }
    0
}

/// Filters one line of luma samples across an edge. `q0` is the index of
/// the first sample on the q side and `s` the distance between samples
/// across the edge (1 for vertical edges, the stride for horizontal ones).
#[inline(always)]
fn luma_line(d: &mut [u8], q0i: usize, s: usize, bs: u8, alpha: i32, beta: i32, tc0: i32) {
    let p0 = d[q0i - s] as i32;
    let p1 = d[q0i - 2 * s] as i32;
    let q0 = d[q0i] as i32;
    let q1 = d[q0i + s] as i32;
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let p2 = d[q0i - 3 * s] as i32;
    let q2 = d[q0i + 2 * s] as i32;
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        d[q0i - s] = (p0 + delta).clamp(0, 255) as u8;
        d[q0i] = (q0 - delta).clamp(0, 255) as u8;
        if ap < beta {
            d[q0i - 2 * s] = (p1 + ((p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1).clamp(-tc0, tc0)) as u8;
        }
        if aq < beta {
            d[q0i + s] = (q1 + ((q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1).clamp(-tc0, tc0)) as u8;
        }
    } else {
        let small_gap = (p0 - q0).abs() < ((alpha >> 2) + 2);
        if ap < beta && small_gap {
            let p3 = d[q0i - 4 * s] as i32;
            d[q0i - s] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u8;
            d[q0i - 2 * s] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u8;
            d[q0i - 3 * s] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u8;
        } else {
            d[q0i - s] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        }
        if aq < beta && small_gap {
            let q3 = d[q0i + 3 * s] as i32;
            d[q0i] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u8;
            d[q0i + s] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u8;
            d[q0i + 2 * s] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u8;
        } else {
            d[q0i] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
        }
    }
}

/// Filters one line of chroma samples across an edge.
#[inline(always)]
fn chroma_line(d: &mut [u8], q0i: usize, s: usize, bs: u8, alpha: i32, beta: i32, tc0: i32) {
    let p0 = d[q0i - s] as i32;
    let p1 = d[q0i - 2 * s] as i32;
    let q0 = d[q0i] as i32;
    let q1 = d[q0i + s] as i32;
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if bs < 4 {
        let tc = tc0 + 1;
        let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        d[q0i - s] = (p0 + delta).clamp(0, 255) as u8;
        d[q0i] = (q0 - delta).clamp(0, 255) as u8;
    } else {
        d[q0i - s] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        d[q0i] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
    }
}

/// alpha, beta and the tC0 row for an averaged QP.
#[inline]
fn thresholds(qp_av: i32, p: &DeblockParams) -> (i32, i32, [u8; 3]) {
    let ia = (qp_av + 2 * p.alpha_offset_div2 as i32).clamp(0, 51) as usize;
    let ib = (qp_av + 2 * p.beta_offset_div2 as i32).clamp(0, 51) as usize;
    (ALPHA[ia] as i32, BETA[ib] as i32, TC0[ia])
}

/// Deblocks the whole picture in place.
pub fn deblock_frame(frame: &mut Frame, s: &PicState, params: &DeblockParams) {
    let w4 = s.mb_w * 4;
    let [luma, cb, cr] = &mut frame.planes;
    for mb_y in 0..s.mb_h {
        for mb_x in 0..s.mb_w {
            let mb = mb_y * s.mb_w + mb_x;
            let qp_q = s.qp[mb] as i32;
            let qpc_q = chroma_qp(s.qp[mb], params.chroma_qp_offset) as i32;
            // dir 0: vertical edges (filter horizontally), dir 1: horizontal edges.
            for dir in 0..2 {
                for e in 0..4usize {
                    let mb_edge = e == 0;
                    if mb_edge && ((dir == 0 && mb_x == 0) || (dir == 1 && mb_y == 0)) {
                        continue;
                    }
                    let mb_p = if !mb_edge {
                        mb
                    } else if dir == 0 {
                        mb - 1
                    } else {
                        mb - s.mb_w
                    };
                    let qp_p = s.qp[mb_p] as i32;
                    let (alpha, beta, tc_row) = thresholds((qp_p + qp_q + 1) >> 1, params);
                    let qpc_p = chroma_qp(s.qp[mb_p], params.chroma_qp_offset) as i32;
                    let (c_alpha, c_beta, c_tc_row) = thresholds((qpc_p + qpc_q + 1) >> 1, params);
                    for k in 0..4usize {
                        // 4x4 blocks on the q and p sides of this edge segment.
                        let (qx, qy) = if dir == 0 {
                            (mb_x * 4 + e, mb_y * 4 + k)
                        } else {
                            (mb_x * 4 + k, mb_y * 4 + e)
                        };
                        let q = qy * w4 + qx;
                        let p = if dir == 0 { q - 1 } else { q - w4 };
                        let bs = boundary_strength(s, p, q, mb_p, mb, mb_edge);
                        if bs == 0 {
                            continue;
                        }
                        let tc0 = if bs < 4 { tc_row[bs as usize - 1] as i32 } else { 0 };
                        let (step, along) = if dir == 0 { (1, luma.stride) } else { (luma.stride, 1) };
                        let base = luma.idx((qx * 4) as i32, (qy * 4) as i32);
                        for i in 0..4 {
                            luma_line(&mut luma.data, base + i * along, step, bs, alpha, beta, tc0);
                        }
                        // Chroma edges exist only where luma edges 0 and 2 are.
                        if e & 1 == 0 {
                            let c_tc0 = if bs < 4 { c_tc_row[bs as usize - 1] as i32 } else { 0 };
                            for plane in [&mut *cb, &mut *cr] {
                                let (step, along) = if dir == 0 { (1, plane.stride) } else { (plane.stride, 1) };
                                let base = plane.idx((qx * 2) as i32, (qy * 2) as i32);
                                for i in 0..2 {
                                    chroma_line(&mut plane.data, base + i * along, step, bs, c_alpha, c_beta, c_tc0);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::*;

    fn line(v: [u8; 8]) -> Vec<u8> {
        v.to_vec()
    }

    #[test]
    fn strong_filter_on_a_small_step() {
        // p3..p0 = 60, q0..q3 = 68, alpha(QP 36)=50, beta=11 -> all conditions hold.
        let mut d = line([60, 60, 60, 60, 68, 68, 68, 68]);
        luma_line(&mut d, 4, 1, 4, 50, 11, 0);
        // p0' = (60+120+120+136+68+4)>>3 = 63, p1' = (60+60+60+68+2)>>2 = 62,
        // p2' = (120+180+60+60+68+4)>>3 = 61; mirrored on the q side.
        assert_eq!(d, vec![60, 61, 62, 63, 65, 66, 67, 68]);
    }

    #[test]
    fn strong_filter_falls_back_when_gap_is_large() {
        // |p0-q0| = 14 >= (alpha>>2)+2 = 14: only p0 and q0 move, 3-tap.
        let mut d = line([60, 60, 60, 60, 74, 74, 74, 74]);
        luma_line(&mut d, 4, 1, 4, 50, 11, 0);
        assert_eq!(d, vec![60, 60, 60, 64, 71, 74, 74, 74]);
    }

    #[test]
    fn real_edges_are_left_alone() {
        let orig = line([20, 20, 20, 20, 200, 200, 200, 200]);
        for bs in 1..=4 {
            let mut d = orig.clone();
            luma_line(&mut d, 4, 1, bs, 50, 11, 3);
            assert_eq!(d, orig, "bS {bs}");
            let mut d = orig.clone();
            chroma_line(&mut d, 4, 1, bs, 50, 11, 3);
            assert_eq!(d, orig);
        }
        // Texture on one side (|p1-p0| >= beta) also disables the filter.
        let orig = line([60, 60, 80, 60, 64, 64, 64, 64]);
        let mut d = orig.clone();
        luma_line(&mut d, 4, 1, 4, 50, 11, 0);
        assert_eq!(d, orig);
    }

    #[test]
    fn normal_filter_clips_to_tc() {
        // bS 2, tc0 = 1: ap, aq < beta so tc = 3;
        // delta = ((8<<2) + 0 + 4)>>3 = 4 -> clipped to 3.
        let mut d = line([60, 60, 60, 60, 68, 68, 68, 68]);
        luma_line(&mut d, 4, 1, 2, 50, 11, 1);
        // p1' = 60 + clip(-1,1,(60 + 64 - 120)>>1 = 2) = 61; q1' = 68 + clip((68+64-136)>>1 = -2) = 67.
        assert_eq!(d, vec![60, 60, 61, 63, 65, 67, 68, 68]);
        // Chroma: tc = tc0 + 1 = 2, only p0/q0 change.
        let mut d = line([60, 60, 60, 60, 68, 68, 68, 68]);
        chroma_line(&mut d, 4, 1, 2, 50, 11, 1);
        assert_eq!(d, vec![60, 60, 60, 62, 66, 68, 68, 68]);
        // Chroma bS 4.
        let mut d = line([60, 60, 60, 60, 68, 68, 68, 68]);
        chroma_line(&mut d, 4, 1, 4, 50, 11, 0);
        assert_eq!(d, vec![60, 60, 60, 62, 66, 68, 68, 68]);
    }

    #[test]
    fn low_qp_disables_filtering() {
        let p = DeblockParams::default();
        let (alpha, beta, _) = thresholds(15, &p);
        assert_eq!((alpha, beta), (0, 0));
        let orig = line([60, 60, 60, 60, 61, 61, 61, 61]);
        let mut d = orig.clone();
        luma_line(&mut d, 4, 1, 4, alpha, beta, 0);
        assert_eq!(d, orig);
        // Offsets shift the table index and clamp at both ends.
        let p = DeblockParams {
            alpha_offset_div2: 6,
            beta_offset_div2: -6,
            chroma_qp_offset: 0,
        };
        let (alpha, beta, _) = thresholds(45, &p);
        assert_eq!(alpha, 255);
        assert_eq!(beta, BETA[33] as i32);
    }

    fn state_2x1() -> PicState {
        let mut s = PicState::new(2, 1);
        s.kind.fill(MB_INTER);
        s.qp.fill(30);
        s.motion.refi.fill(0);
        s
    }

    #[test]
    fn boundary_strength_rules() {
        let mut s = state_2x1();
        // Blocks 3 and 4 in the top row straddle the macroblock edge.
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 0);
        s.motion.mv[4] = [3, 0];
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 0);
        s.motion.mv[4] = [4, 0];
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 1);
        s.motion.mv[4] = [0, -4];
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 1);
        s.nnz_y[3] = 1;
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 2);
        s.kind[1] = MB_I16X16;
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 4);
        assert_eq!(boundary_strength(&s, 4, 5, 1, 1, false), 3);
        s.kind[1] = MB_SKIP;
        s.nnz_y[3] = 0;
        s.motion.mv[4] = [0, 0];
        assert_eq!(boundary_strength(&s, 3, 4, 0, 1, true), 0);
    }

    #[test]
    fn picture_edges_and_flat_pictures_are_untouched() {
        let mut s = state_2x1();
        s.kind.fill(MB_I16X16);
        let mut f = Frame::new(32, 16, 0);
        for p in &mut f.planes {
            p.data.fill(100);
        }
        let before = f.clone();
        deblock_frame(&mut f, &s, &DeblockParams::default());
        for i in 0..3 {
            assert_eq!(f.planes[i].data, before.planes[i].data);
        }
    }

    #[test]
    fn intra_macroblock_edge_is_smoothed_in_luma_and_chroma() {
        let mut s = state_2x1();
        s.kind.fill(MB_I16X16);
        let mut f = Frame::new(32, 16, 0);
        for p in &mut f.planes {
            let half = p.w / 2;
            for y in 0..p.h {
                for x in 0..p.w {
                    p.set(x as i32, y as i32, if x < half { 60 } else { 68 });
                }
            }
        }
        deblock_frame(&mut f, &s, &DeblockParams::default());
        // QP 30: alpha 25, beta 8 -> strong filter with small gap (8 < 8 is false!)
        // (alpha>>2)+2 = 8, so the 3-tap fallback applies: p0' = (120+60+68+2)>>2 = 62.
        for y in 0..16 {
            assert_eq!(f.planes[0].row(y)[12..20], [60, 60, 60, 62, 66, 68, 68, 68]);
        }
        // Chroma QP for 30 is 29: alpha 22, beta 7; bS 4 chroma filter.
        for y in 0..8 {
            assert_eq!(f.planes[1].row(y)[6..10], [60, 62, 66, 68]);
            assert_eq!(f.planes[2].row(y)[6..10], [60, 62, 66, 68]);
        }
        // The left picture edge and columns away from the edge are unchanged.
        assert_eq!(f.planes[0].at(0, 0), 60);
        assert_eq!(f.planes[0].at(31, 15), 68);
    }

    #[test]
    fn mixed_qp_uses_the_average() {
        let mut s = state_2x1();
        s.qp[0] = 20;
        s.qp[1] = 41;
        let (a, _, _) = thresholds((20 + 41 + 1) >> 1, &DeblockParams::default());
        assert_eq!(a, ALPHA[31] as i32);
    }
}
