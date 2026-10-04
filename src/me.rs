//! Motion estimation: predictor candidates, hexagon search on integer
//! positions (SAD), then half- and quarter-sample refinement (SATD).
//! Encoder-side only; nothing here affects conformance.

use crate::bitstream::se_len;
use crate::cost::{sad, satd};
use crate::inter::{Mv, RefPic};

pub struct Me<'a> {
    /// Source macroblock, 16x16 raster.
    pub cur: &'a [u8; 256],
    pub refp: &'a RefPic,
    /// Luma position of the macroblock.
    pub x0: i32,
    pub y0: i32,
    /// Allowed vector range in quarter samples (inclusive).
    pub mv_min: [i32; 2],
    pub mv_max: [i32; 2],
    pub lambda16: u32,
    /// Integer search range around the predictor, in samples.
    pub range: i32,
    /// 0: integer only, 1: half sample, 2: quarter sample.
    pub subpel: u8,
}

const HEX: [[i32; 2]; 6] = [[-2, 0], [-1, -2], [1, -2], [2, 0], [1, 2], [-1, 2]];
const SQUARE: [[i32; 2]; 8] = [[-1, -1], [0, -1], [1, -1], [-1, 0], [1, 0], [-1, 1], [0, 1], [1, 1]];

impl Me<'_> {
    #[inline]
    pub fn mv_bits(mv: [i32; 2], pmv: Mv) -> u32 {
        se_len(mv[0] - pmv[0] as i32) + se_len(mv[1] - pmv[1] as i32)
    }

    #[inline]
    pub fn bit_cost(&self, bits: u32) -> u32 {
        (self.lambda16 * bits + 8) >> 4
    }

    #[inline]
    fn in_range(&self, mv: [i32; 2]) -> bool {
        mv[0] >= self.mv_min[0] && mv[0] <= self.mv_max[0] && mv[1] >= self.mv_min[1] && mv[1] <= self.mv_max[1]
    }

    /// SATD of the prediction at `mv` (quarter units) for the partition.
    pub fn satd_at(&self, bx: usize, by: usize, pw: usize, ph: usize, mv: [i32; 2]) -> u32 {
        let (w, h) = (pw * 4, ph * 4);
        let mut pred = [0u8; 256];
        self.refp.mc_luma(
            self.x0 + 4 * bx as i32,
            self.y0 + 4 * by as i32,
            [mv[0] as i16, mv[1] as i16],
            w,
            h,
            &mut pred,
            16,
        );
        satd(&self.cur[by * 64 + bx * 4..], 16, &pred, 16, w, h)
    }

    /// Finds a vector for the partition of `pw` x `ph` 4x4 blocks at
    /// (bx, by). Returns the vector and its cost (SATD + lambda * mvd bits).
    pub fn search(&self, bx: usize, by: usize, pw: usize, ph: usize, pmv: Mv, cands: &[Mv]) -> (Mv, u32) {
        let (w, h) = (pw * 4, ph * 4);
        let px = self.x0 + 4 * bx as i32;
        let py = self.y0 + 4 * by as i32;
        let cur = &self.cur[by * 64 + bx * 4..];
        let full = &self.refp.luma[0];

        // Integer window: around the (clamped) predictor, inside the legal range.
        let lo = [(self.mv_min[0] + 3) >> 2, (self.mv_min[1] + 3) >> 2];
        let hi = [self.mv_max[0] >> 2, self.mv_max[1] >> 2];
        let centre = [
            ((pmv[0] as i32 + 2) >> 2).clamp(lo[0], hi[0]),
            ((pmv[1] as i32 + 2) >> 2).clamp(lo[1], hi[1]),
        ];
        let win_lo = [lo[0].max(centre[0] - self.range), lo[1].max(centre[1] - self.range)];
        let win_hi = [hi[0].min(centre[0] + self.range), hi[1].min(centre[1] + self.range)];

        let int_cost = |m: [i32; 2]| -> u32 {
            let o = full.idx(px + m[0], py + m[1]);
            sad(cur, 16, &full.data[o..], full.stride, w, h) + self.bit_cost(Self::mv_bits([m[0] * 4, m[1] * 4], pmv))
        };

        let mut best = centre;
        let mut best_cost = int_cost(centre);
        for c in cands {
            let m = [
                ((c[0] as i32 + 2) >> 2).clamp(win_lo[0], win_hi[0]),
                ((c[1] as i32 + 2) >> 2).clamp(win_lo[1], win_hi[1]),
            ];
            if m != best {
                let cost = int_cost(m);
                if cost < best_cost {
                    best = m;
                    best_cost = cost;
                }
            }
        }
        // Hexagon steps until the centre is the best point.
        for _ in 0..self.range {
            let mut moved = None;
            for d in HEX {
                let m = [best[0] + d[0], best[1] + d[1]];
                if m[0] < win_lo[0] || m[0] > win_hi[0] || m[1] < win_lo[1] || m[1] > win_hi[1] {
                    continue;
                }
                let cost = int_cost(m);
                if cost < best_cost {
                    best_cost = cost;
                    moved = Some(m);
                }
            }
            match moved {
                Some(m) => best = m,
                None => break,
            }
        }
        // Final square of radius one, repeated once if it moved.
        for _ in 0..2 {
            let centre = best;
            for d in SQUARE {
                let m = [centre[0] + d[0], centre[1] + d[1]];
                if m[0] < win_lo[0] || m[0] > win_hi[0] || m[1] < win_lo[1] || m[1] > win_hi[1] {
                    continue;
                }
                let cost = int_cost(m);
                if cost < best_cost {
                    best_cost = cost;
                    best = m;
                }
            }
            if best == centre {
                break;
            }
        }

        // Sub-sample refinement, judged by SATD.
        let mut mv = [best[0] * 4, best[1] * 4];
        let mut cost = self.satd_at(bx, by, pw, ph, mv) + self.bit_cost(Self::mv_bits(mv, pmv));
        let steps: &[i32] = match self.subpel {
            0 => &[],
            1 => &[2],
            _ => &[2, 1],
        };
        for &step in steps {
            let centre = mv;
            for d in SQUARE {
                let m = [centre[0] + d[0] * step, centre[1] + d[1] * step];
                if !self.in_range(m) {
                    continue;
                }
                let c = self.satd_at(bx, by, pw, ph, m) + self.bit_cost(Self::mv_bits(m, pmv));
                if c < cost {
                    cost = c;
                    mv = m;
                }
            }
        }
        ([mv[0] as i16, mv[1] as i16], cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::inter::PAD;
    use crate::rng::Rng;

    /// Smooth random texture so that sub-sample shifts are well defined.
    fn textured(w: usize, h: usize, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let mut f = Frame::new(w, h, PAD);
        let coarse: Vec<i32> = (0..(w / 4 + 2) * (h / 4 + 2)).map(|_| rng.range(30, 220)).collect();
        let cw = w / 4 + 2;
        for y in 0..h {
            for x in 0..w {
                let (cx, cy, fx, fy) = (x / 4, y / 4, (x % 4) as i32, (y % 4) as i32);
                let v = (coarse[cy * cw + cx] * (4 - fx) * (4 - fy)
                    + coarse[cy * cw + cx + 1] * fx * (4 - fy)
                    + coarse[(cy + 1) * cw + cx] * (4 - fx) * fy
                    + coarse[(cy + 1) * cw + cx + 1] * fx * fy)
                    / 16;
                f.planes[0].set(x as i32, y as i32, v as u8);
            }
        }
        f
    }

    #[test]
    fn finds_known_displacements() {
        let (w, h) = (64, 64);
        let f = textured(w, h, 41);
        let mut refp = RefPic::new(w, h);
        refp.load(&f);
        // The "current" block is the reference displaced by a known vector,
        // including fractional ones (generated with the interpolator itself).
        for &truth in &[[0i16, 0], [8, -12], [-20, 4], [5, 3], [-7, 10], [2, -1], [13, 13]] {
            let mut cur = [0u8; 256];
            refp.mc_luma(24, 24, truth, 16, 16, &mut cur, 16);
            let me = Me {
                cur: &cur,
                refp: &refp,
                x0: 24,
                y0: 24,
                mv_min: [-48 * 4, -48 * 4],
                mv_max: [48 * 4, 48 * 4],
                lambda16: 16,
                range: 16,
                subpel: 2,
            };
            let (mv, cost) = me.search(0, 0, 4, 4, [0, 0], &[[0, 0]]);
            assert_eq!(mv, truth, "expected {truth:?}");
            assert!(cost < 40, "cost {cost} should be just the vector bits");
        }
    }

    #[test]
    fn respects_vector_limits() {
        let (w, h) = (32, 32);
        let f = textured(w, h, 42);
        let mut refp = RefPic::new(w, h);
        refp.load(&f);
        let mut rng = Rng::new(43);
        let cur: [u8; 256] = std::array::from_fn(|_| rng.range(0, 255) as u8);
        let me = Me {
            cur: &cur,
            refp: &refp,
            x0: 16,
            y0: 16,
            mv_min: [-6, -3],
            mv_max: [5, 9],
            lambda16: 16,
            range: 16,
            subpel: 2,
        };
        for (bx, by, pw, ph) in [(0, 0, 4, 4), (2, 2, 2, 2), (0, 2, 4, 2), (3, 3, 1, 1)] {
            // Predictor and candidates far outside the legal range.
            let (mv, _) = me.search(bx, by, pw, ph, [400, -400], &[[-300, 300], [77, 77]]);
            assert!((-6..=5).contains(&mv[0]) && (-3..=9).contains(&mv[1]), "{mv:?}");
        }
    }
}
