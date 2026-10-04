//! Inter prediction sample generation (clause 8.4.2.2): quarter-sample luma
//! interpolation with the 6-tap filter and eighth-sample bilinear chroma.
//!
//! A reference picture keeps four luma planes: the integer samples and the
//! three half-sample planes (horizontal, vertical, centre). Every quarter
//! position is by definition the rounded average of two of those samples,
//! so motion compensation is one or two plane fetches per sample.

use crate::frame::{Frame, Plane};

/// Luma border around reference pictures. Motion vectors may reach
/// `MV_BORDER` samples outside the picture; the remainder covers the filter
/// taps. Border samples equal the clamped fetch the specification defines.
pub const PAD: usize = 32;
pub const MV_BORDER: i32 = 24;

pub type Mv = [i16; 2];

pub struct RefPic {
    /// 0: integer (G), 1: horizontal half (b), 2: vertical half (h), 3: centre (j).
    pub luma: [Plane; 4],
    pub cb: Plane,
    pub cr: Plane,
}

#[inline(always)]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

/// For each (yFrac, xFrac): two (plane, dx, dy) sources to average.
/// Derived from the sample naming of Figure 8-4.
const QPEL: [[[(usize, i32, i32); 2]; 4]; 4] = [
    [
        [(0, 0, 0), (0, 0, 0)], // G
        [(0, 0, 0), (1, 0, 0)], // a = (G + b)
        [(1, 0, 0), (1, 0, 0)], // b
        [(1, 0, 0), (0, 1, 0)], // c = (H + b)
    ],
    [
        [(0, 0, 0), (2, 0, 0)], // d = (G + h)
        [(1, 0, 0), (2, 0, 0)], // e = (b + h)
        [(1, 0, 0), (3, 0, 0)], // f = (b + j)
        [(1, 0, 0), (2, 1, 0)], // g = (b + m)
    ],
    [
        [(2, 0, 0), (2, 0, 0)], // h
        [(2, 0, 0), (3, 0, 0)], // i = (h + j)
        [(3, 0, 0), (3, 0, 0)], // j
        [(3, 0, 0), (2, 1, 0)], // k = (j + m)
    ],
    [
        [(2, 0, 0), (0, 0, 1)], // n = (M + h)
        [(2, 0, 0), (1, 0, 1)], // p = (h + s)
        [(3, 0, 0), (1, 0, 1)], // q = (j + s)
        [(2, 1, 0), (1, 0, 1)], // r = (m + s)
    ],
];

impl RefPic {
    pub fn new(w: usize, h: usize) -> Self {
        RefPic {
            luma: std::array::from_fn(|_| Plane::new(w, h, PAD)),
            cb: Plane::new(w / 2, h / 2, PAD / 2),
            cr: Plane::new(w / 2, h / 2, PAD / 2),
        }
    }

    /// Takes a reconstructed picture (same geometry, any border contents),
    /// extends its borders and builds the half-sample planes.
    pub fn load(&mut self, recon: &Frame) {
        self.luma[0].data.copy_from_slice(&recon.planes[0].data);
        self.cb.data.copy_from_slice(&recon.planes[1].data);
        self.cr.data.copy_from_slice(&recon.planes[2].data);
        self.luma[0].extend_borders();
        self.cb.extend_borders();
        self.cr.extend_borders();
        self.build_half_planes();
    }

    fn build_half_planes(&mut self) {
        let [full, hp, vp, cp] = &mut self.luma;
        let stride = full.stride;
        let rows = full.h + 2 * PAD;
        let f = &full.data;
        // Vertical intermediates (unrounded) for one row, reused for j.
        let mut mid = vec![0i32; stride];
        for r in 0..rows {
            let o = r * stride;
            // b: horizontal half samples.
            for x in 2..stride - 3 {
                let i = o + x;
                let v = tap6(
                    f[i - 2] as i32,
                    f[i - 1] as i32,
                    f[i] as i32,
                    f[i + 1] as i32,
                    f[i + 2] as i32,
                    f[i + 3] as i32,
                );
                hp.data[i] = ((v + 16) >> 5).clamp(0, 255) as u8;
            }
            if r < 2 || r + 3 >= rows {
                continue;
            }
            // h: vertical half samples, keeping the intermediate values.
            for x in 0..stride {
                let i = o + x;
                let v = tap6(
                    f[i - 2 * stride] as i32,
                    f[i - stride] as i32,
                    f[i] as i32,
                    f[i + stride] as i32,
                    f[i + 2 * stride] as i32,
                    f[i + 3 * stride] as i32,
                );
                mid[x] = v;
                vp.data[i] = ((v + 16) >> 5).clamp(0, 255) as u8;
            }
            // j: 6-tap across the vertical intermediates.
            for x in 2..stride - 3 {
                let v = tap6(mid[x - 2], mid[x - 1], mid[x], mid[x + 1], mid[x + 2], mid[x + 3]);
                cp.data[o + x] = ((v + 512) >> 10).clamp(0, 255) as u8;
            }
        }
    }

    /// Predicts a w x h luma block whose top-left sample is at (x, y) in the
    /// current picture, displaced by `mv` (quarter-sample units).
    #[inline]
    pub fn mc_luma(&self, x: i32, y: i32, mv: Mv, w: usize, h: usize, dst: &mut [u8], dstride: usize) {
        let (fx, fy) = ((mv[0] & 3) as usize, (mv[1] & 3) as usize);
        let (ix, iy) = (x + (mv[0] >> 2) as i32, y + (mv[1] >> 2) as i32);
        let [(pa, ax, ay), (pb, bx, by)] = QPEL[fy][fx];
        let (a, b) = (&self.luma[pa], &self.luma[pb]);
        let mut oa = a.idx(ix + ax, iy + ay);
        let mut ob = b.idx(ix + bx, iy + by);
        let stride = a.stride;
        if pa == pb && oa == ob {
            for r in 0..h {
                dst[r * dstride..r * dstride + w].copy_from_slice(&a.data[oa..oa + w]);
                oa += stride;
            }
        } else {
            for r in 0..h {
                let (ra, rb) = (&a.data[oa..oa + w], &b.data[ob..ob + w]);
                let d = &mut dst[r * dstride..r * dstride + w];
                for i in 0..w {
                    d[i] = ((ra[i] as u16 + rb[i] as u16 + 1) >> 1) as u8;
                }
                oa += stride;
                ob += stride;
            }
        }
    }

    /// Predicts a w x h chroma block (w, h in chroma samples) at chroma
    /// position (x, y); `mv` is the luma vector, i.e. eighth-sample units.
    #[inline]
    pub fn mc_chroma(&self, plane: usize, x: i32, y: i32, mv: Mv, w: usize, h: usize, dst: &mut [u8], dstride: usize) {
        let p = if plane == 0 { &self.cb } else { &self.cr };
        let (fx, fy) = ((mv[0] & 7) as u32, (mv[1] & 7) as u32);
        let (ix, iy) = (x + (mv[0] >> 3) as i32, y + (mv[1] >> 3) as i32);
        let stride = p.stride;
        let mut o = p.idx(ix, iy);
        let (ca, cb, cc, cd) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
        for r in 0..h {
            let (r0, r1) = (&p.data[o..o + w + 1], &p.data[o + stride..o + stride + w + 1]);
            let d = &mut dst[r * dstride..r * dstride + w];
            for i in 0..w {
                let v = ca * r0[i] as u32 + cb * r0[i + 1] as u32 + cc * r1[i] as u32 + cd * r1[i + 1] as u32;
                d[i] = ((v + 32) >> 6) as u8;
            }
            o += stride;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::rng::Rng;

    /// Luma sample at integer position (xi, yi) plus fraction (xf, yf),
    /// written sample-by-sample from clause 8.4.2.2.1 with clamped fetches.
    /// Deliberately computes j from the horizontal intermediates (the
    /// production code uses the vertical ones).
    pub fn luma_sample_spec(p: &Plane, xi: i32, yi: i32, xf: i32, yf: i32) -> u8 {
        let px = |x: i32, y: i32| p.at_clamped(x, y) as i32;
        let clip = |v: i32| v.clamp(0, 255);
        let b1 = |x: i32, y: i32| {
            tap6(
                px(x - 2, y),
                px(x - 1, y),
                px(x, y),
                px(x + 1, y),
                px(x + 2, y),
                px(x + 3, y),
            )
        };
        let h1 = |x: i32, y: i32| {
            tap6(
                px(x, y - 2),
                px(x, y - 1),
                px(x, y),
                px(x, y + 1),
                px(x, y + 2),
                px(x, y + 3),
            )
        };
        let g = px(xi, yi);
        let b = clip((b1(xi, yi) + 16) >> 5);
        let h = clip((h1(xi, yi) + 16) >> 5);
        let m = clip((h1(xi + 1, yi) + 16) >> 5);
        let s = clip((b1(xi, yi + 1) + 16) >> 5);
        let j1 = tap6(
            b1(xi, yi - 2),
            b1(xi, yi - 1),
            b1(xi, yi),
            b1(xi, yi + 1),
            b1(xi, yi + 2),
            b1(xi, yi + 3),
        );
        let j = clip((j1 + 512) >> 10);
        let avg = |a: i32, b: i32| (a + b + 1) >> 1;
        let v = match (xf, yf) {
            (0, 0) => g,
            (1, 0) => avg(g, b),
            (2, 0) => b,
            (3, 0) => avg(px(xi + 1, yi), b),
            (0, 1) => avg(g, h),
            (1, 1) => avg(b, h),
            (2, 1) => avg(b, j),
            (3, 1) => avg(b, m),
            (0, 2) => h,
            (1, 2) => avg(h, j),
            (2, 2) => j,
            (3, 2) => avg(j, m),
            (0, 3) => avg(px(xi, yi + 1), h),
            (1, 3) => avg(h, s),
            (2, 3) => avg(j, s),
            _ => avg(m, s),
        };
        v as u8
    }

    pub fn chroma_sample_spec(p: &Plane, x: i32, y: i32, mv: Mv) -> u8 {
        let (xi, yi) = (x + (mv[0] >> 3) as i32, y + (mv[1] >> 3) as i32);
        let (xf, yf) = ((mv[0] & 7) as i32, (mv[1] & 7) as i32);
        let px = |x: i32, y: i32| p.at_clamped(x, y) as i32;
        (((8 - xf) * (8 - yf) * px(xi, yi)
            + xf * (8 - yf) * px(xi + 1, yi)
            + (8 - xf) * yf * px(xi, yi + 1)
            + xf * yf * px(xi + 1, yi + 1)
            + 32)
            >> 6) as u8
    }

    pub fn random_frame(w: usize, h: usize, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let mut f = Frame::new(w, h, PAD);
        for p in &mut f.planes {
            for y in 0..p.h {
                for x in 0..p.w {
                    // Mix smooth structure and noise so clipping paths trigger.
                    let v = if rng.chance(1, 4) {
                        rng.range(0, 255)
                    } else {
                        ((x * 9 + y * 5) % 256) as i32
                    };
                    p.set(x as i32, y as i32, v as u8);
                }
            }
        }
        f
    }

    #[test]
    fn luma_interpolation_matches_spec_for_all_sixteen_positions() {
        let (w, h) = (48, 32);
        let f = random_frame(w, h, 21);
        let mut r = RefPic::new(w, h);
        r.load(&f);
        let mut rng = Rng::new(22);
        let mut dst = [0u8; 256];
        for case in 0..600 {
            let (bw, bh) = [(16, 16), (16, 8), (8, 16), (8, 8), (8, 4), (4, 8), (4, 4)][case % 7];
            let x = rng.range(0, (w - bw) as i32);
            let y = rng.range(0, (h - bh) as i32);
            // Vectors chosen so the block reaches up to MV_BORDER outside.
            let mvx = rng.range((-MV_BORDER - x) * 4, (w as i32 + MV_BORDER - bw as i32 - x) * 4);
            let mvy = rng.range((-MV_BORDER - y) * 4, (h as i32 + MV_BORDER - bh as i32 - y) * 4);
            let mv = [mvx as i16, mvy as i16];
            r.mc_luma(x, y, mv, bw, bh, &mut dst, 16);
            for yy in 0..bh {
                for xx in 0..bw {
                    let e = luma_sample_spec(
                        &f.planes[0],
                        x + xx as i32 + (mvx >> 2),
                        y + yy as i32 + (mvy >> 2),
                        mvx & 3,
                        mvy & 3,
                    );
                    assert_eq!(dst[yy * 16 + xx], e, "case {case} mv {mv:?} at ({xx},{yy})");
                }
            }
        }
    }

    #[test]
    fn every_fraction_is_exercised_at_picture_corners() {
        let (w, h) = (16, 16);
        let f = random_frame(w, h, 23);
        let mut r = RefPic::new(w, h);
        r.load(&f);
        let mut dst = [0u8; 256];
        for fy in 0..4 {
            for fx in 0..4 {
                for (bx, by) in [
                    (-MV_BORDER, -MV_BORDER),
                    (MV_BORDER, MV_BORDER),
                    (-MV_BORDER, MV_BORDER),
                    (0, 0),
                ] {
                    let mv = [(bx * 4 + fx) as i16, (by * 4 + fy) as i16];
                    if bx == MV_BORDER && fx != 0 || by == MV_BORDER && fy != 0 {
                        continue;
                    }
                    r.mc_luma(0, 0, mv, 16, 16, &mut dst, 16);
                    for yy in 0..16 {
                        for xx in 0..16 {
                            let e = luma_sample_spec(&f.planes[0], bx + xx, by + yy, fx, fy);
                            assert_eq!(dst[(yy * 16 + xx) as usize], e, "frac ({fx},{fy}) base ({bx},{by})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn chroma_interpolation_matches_spec() {
        let (w, h) = (48, 32);
        let f = random_frame(w, h, 24);
        let mut r = RefPic::new(w, h);
        r.load(&f);
        let mut rng = Rng::new(25);
        let mut dst = [0u8; 64];
        for case in 0..600 {
            let (bw, bh) = [(8, 8), (8, 4), (4, 8), (4, 4), (4, 2), (2, 4), (2, 2)][case % 7];
            let plane = case % 2;
            let x = rng.range(0, (w / 2 - bw) as i32);
            let y = rng.range(0, (h / 2 - bh) as i32);
            let mvx = rng.range(
                (-MV_BORDER - 2 * x) * 4,
                (w as i32 + MV_BORDER - 2 * bw as i32 - 2 * x) * 4,
            );
            let mvy = rng.range(
                (-MV_BORDER - 2 * y) * 4,
                (h as i32 + MV_BORDER - 2 * bh as i32 - 2 * y) * 4,
            );
            let mv = [mvx as i16, mvy as i16];
            r.mc_chroma(plane, x, y, mv, bw, bh, &mut dst, 8);
            for yy in 0..bh {
                for xx in 0..bw {
                    let e = chroma_sample_spec(&f.planes[1 + plane], x + xx as i32, y + yy as i32, mv);
                    assert_eq!(dst[yy * 8 + xx], e, "case {case} mv {mv:?}");
                }
            }
        }
    }

    #[test]
    fn six_tap_filter_on_known_samples() {
        // A step edge 0,0,0,255,255,255: b between the two middle samples is
        // (0 - 0 + 0 + 20*255 - 5*255 + 255 + 16) >> 5 = 128.
        assert_eq!((tap6(0, 0, 0, 255, 255, 255) + 16) >> 5, 128);
        // Overshoot must clip: 0,0,255,255,0,0 -> 318 -> 255.
        assert_eq!(((tap6(0, 0, 255, 255, 0, 0) + 16) >> 5).clamp(0, 255), 255);
        // And undershoot: 255,255,0,0,255,255 -> negative -> 0.
        assert_eq!(((tap6(255, 255, 0, 0, 255, 255) + 16) >> 5).clamp(0, 255), 0);
        // Flat areas are preserved.
        assert_eq!((tap6(9, 9, 9, 9, 9, 9) + 16) >> 5, 9);
    }
}
