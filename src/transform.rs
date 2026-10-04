//! 4x4 integer transform, Hadamard transforms for DC coefficients, and
//! quantisation (clauses 8.5.9 to 8.5.12 for the inverse; the forward
//! direction is the encoder's own choice and follows the textbook design).
//!
//! Blocks are 16 values in raster order (index = y*4 + x).

use crate::tables::{DEQUANT_V, POS_CLASS, QUANT_MF};

/// Largest coefficient magnitude this encoder emits. CAVLC in Baseline can
/// carry a level_prefix of at most 15, which limits |level| to a little over
/// 2063; only reachable at very low QP with extreme residuals.
pub const MAX_LEVEL: i32 = 2047;

/// Forward core transform W = Cf * X * Cf^T, in place.
pub fn fdct4x4(b: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, bb, c, d) = (b[r * 4], b[r * 4 + 1], b[r * 4 + 2], b[r * 4 + 3]);
        let (s03, d03, s12, d12) = (a + d, a - d, bb + c, bb - c);
        b[r * 4] = s03 + s12;
        b[r * 4 + 1] = 2 * d03 + d12;
        b[r * 4 + 2] = s03 - s12;
        b[r * 4 + 3] = d03 - 2 * d12;
    }
    for c in 0..4 {
        let (a, bb, cc, d) = (b[c], b[4 + c], b[8 + c], b[12 + c]);
        let (s03, d03, s12, d12) = (a + d, a - d, bb + cc, bb - cc);
        b[c] = s03 + s12;
        b[4 + c] = 2 * d03 + d12;
        b[8 + c] = s03 - s12;
        b[12 + c] = d03 - 2 * d12;
    }
}

/// Inverse transform of scaled coefficients `d` (clause 8.5.12.2): rows
/// first, then columns, then (x + 32) >> 6. Returns the residual block.
pub fn idct4x4(d: &[i32; 16]) -> [i32; 16] {
    let mut f = [0i32; 16];
    for r in 0..4 {
        let (d0, d1, d2, d3) = (d[r * 4], d[r * 4 + 1], d[r * 4 + 2], d[r * 4 + 3]);
        let e0 = d0 + d2;
        let e1 = d0 - d2;
        let e2 = (d1 >> 1) - d3;
        let e3 = d1 + (d3 >> 1);
        f[r * 4] = e0 + e3;
        f[r * 4 + 1] = e1 + e2;
        f[r * 4 + 2] = e1 - e2;
        f[r * 4 + 3] = e0 - e3;
    }
    let mut out = [0i32; 16];
    for c in 0..4 {
        let (f0, f1, f2, f3) = (f[c], f[4 + c], f[8 + c], f[12 + c]);
        let g0 = f0 + f2;
        let g1 = f0 - f2;
        let g2 = (f1 >> 1) - f3;
        let g3 = f1 + (f3 >> 1);
        out[c] = (g0 + g3 + 32) >> 6;
        out[4 + c] = (g1 + g2 + 32) >> 6;
        out[8 + c] = (g1 - g2 + 32) >> 6;
        out[12 + c] = (g0 - g3 + 32) >> 6;
    }
    out
}

/// Adds a residual block to the prediction already stored at `dst` and clips.
#[inline]
pub fn add_residual(dst: &mut [u8], stride: usize, res: &[i32; 16]) {
    for y in 0..4 {
        let row = &mut dst[y * stride..y * stride + 4];
        for x in 0..4 {
            row[x] = (row[x] as i32 + res[y * 4 + x]).clamp(0, 255) as u8;
        }
    }
}

/// 4x4 Hadamard butterfly shared by forward and inverse luma DC transforms.
fn hadamard4x4(b: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, bb, c, d) = (b[r * 4], b[r * 4 + 1], b[r * 4 + 2], b[r * 4 + 3]);
        b[r * 4] = a + bb + c + d;
        b[r * 4 + 1] = a + bb - c - d;
        b[r * 4 + 2] = a - bb - c + d;
        b[r * 4 + 3] = a - bb + c - d;
    }
    for c in 0..4 {
        let (a, bb, cc, d) = (b[c], b[4 + c], b[8 + c], b[12 + c]);
        b[c] = a + bb + cc + d;
        b[4 + c] = a + bb - cc - d;
        b[8 + c] = a - bb - cc + d;
        b[12 + c] = a - bb + cc - d;
    }
}

/// Forward Hadamard of the sixteen luma DC values, halved (rounded).
pub fn luma_dc_forward(b: &mut [i32; 16]) {
    hadamard4x4(b);
    for v in b.iter_mut() {
        *v = (*v + 1) >> 1;
    }
}

/// Inverse Hadamard of clause 8.5.10 (no scaling here).
pub fn luma_dc_inverse(b: &mut [i32; 16]) {
    hadamard4x4(b);
}

/// 2x2 Hadamard for chroma DC; its own inverse up to scale. Order: raster.
pub fn hadamard2x2(b: &mut [i32; 4]) {
    let (a, bb, c, d) = (b[0], b[1], b[2], b[3]);
    b[0] = a + bb + c + d;
    b[1] = a - bb + c - d;
    b[2] = a + bb - c - d;
    b[3] = a - bb - c + d;
}

/// Rounding offset selector: intra blocks round at 1/3, inter at 1/6.
#[inline]
fn round_offset(qbits: u32, intra: bool) -> i64 {
    if intra {
        (1i64 << qbits) / 3
    } else {
        (1i64 << qbits) / 6
    }
}

#[inline]
fn quant_one(w: i32, mf: i32, f: i64, qbits: u32) -> i32 {
    let z = ((w.unsigned_abs() as i64 * mf as i64 + f) >> qbits) as i32;
    let z = z.min(MAX_LEVEL);
    if w < 0 {
        -z
    } else {
        z
    }
}

/// Quantises transform coefficients in place; returns the number of
/// non-zero levels. With `skip_dc` the DC position is left untouched
/// (it travels through a Hadamard path instead) and not counted.
pub fn quant4x4(w: &mut [i32; 16], qp: u8, intra: bool, skip_dc: bool) -> u32 {
    let qbits = 15 + (qp / 6) as u32;
    let f = round_offset(qbits, intra);
    let mf = &QUANT_MF[(qp % 6) as usize];
    let mut nz = 0;
    for i in skip_dc as usize..16 {
        w[i] = quant_one(w[i], mf[POS_CLASS[i]], f, qbits);
        nz += (w[i] != 0) as u32;
    }
    nz
}

/// Scales levels (clause 8.5.12.1, flat scaling matrix). With `skip_dc`
/// position 0 is passed through unchanged (already scaled DC).
pub fn dequant4x4(l: &mut [i32; 16], qp: u8, skip_dc: bool) {
    let v = &DEQUANT_V[(qp % 6) as usize];
    let shift = (qp / 6) as u32;
    for i in skip_dc as usize..16 {
        // (c * 16v << shift) >> 4 with rounding is exactly c * v << shift.
        l[i] = (l[i] * v[POS_CLASS[i]]) << shift;
    }
}

/// Quantises Hadamard-transformed DC values (luma 4x4 or chroma 2x2).
pub fn quant_dc(w: &mut [i32], qp: u8, intra: bool) -> u32 {
    let qbits = 16 + (qp / 6) as u32;
    let f = 2 * round_offset(qbits - 1, intra);
    let mf = QUANT_MF[(qp % 6) as usize][0];
    let mut nz = 0;
    for v in w.iter_mut() {
        *v = quant_one(*v, mf, f, qbits);
        nz += (*v != 0) as u32;
    }
    nz
}

/// Inverse of the Intra16x16 luma DC path: inverse Hadamard then scaling
/// (clause 8.5.10). Input: levels in raster block order; output: the DC
/// value to place at position 0 of each 4x4 block before the inverse DCT.
pub fn dequant_luma_dc(c: &mut [i32; 16], qp: u8) {
    luma_dc_inverse(c);
    let ls = 16 * DEQUANT_V[(qp % 6) as usize][0];
    let per = (qp / 6) as u32;
    for v in c.iter_mut() {
        *v = if qp >= 36 {
            (*v * ls) << (per - 6)
        } else {
            (*v * ls + (1 << (5 - per))) >> (6 - per)
        };
    }
}

/// Inverse of the chroma DC path for 4:2:0 (clause 8.5.11.1 and 8.5.11.2).
pub fn dequant_chroma_dc(c: &mut [i32; 4], qpc: u8) {
    hadamard2x2(c);
    let ls = 16 * DEQUANT_V[(qpc % 6) as usize][0];
    let per = (qpc / 6) as u32;
    for v in c.iter_mut() {
        *v = ((*v * ls) << per) >> 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// Straight matrix form of the forward transform for cross-checking.
    fn fdct_matrix(x: &[i32; 16]) -> [i32; 16] {
        const CF: [[i32; 4]; 4] = [[1, 1, 1, 1], [2, 1, -1, -2], [1, -1, -1, 1], [1, -2, 2, -1]];
        let mut t = [0i32; 16];
        let mut w = [0i32; 16];
        for i in 0..4 {
            for j in 0..4 {
                t[i * 4 + j] = (0..4).map(|k| CF[i][k] * x[k * 4 + j]).sum();
            }
        }
        for i in 0..4 {
            for j in 0..4 {
                w[i * 4 + j] = (0..4).map(|k| t[i * 4 + k] * CF[j][k]).sum();
            }
        }
        w
    }

    /// Inverse transform written directly from equations 8-338 to 8-354,
    /// kept apart from the production code on purpose.
    fn idct_spec(d: &[i32; 16]) -> [i32; 16] {
        let mut f = [[0i32; 4]; 4];
        for i in 0..4 {
            let e0 = d[i * 4] + d[i * 4 + 2];
            let e1 = d[i * 4] - d[i * 4 + 2];
            let e2 = (d[i * 4 + 1] >> 1) - d[i * 4 + 3];
            let e3 = d[i * 4 + 1] + (d[i * 4 + 3] >> 1);
            f[i] = [e0 + e3, e1 + e2, e1 - e2, e0 - e3];
        }
        let mut r = [0i32; 16];
        for j in 0..4 {
            let g0 = f[0][j] + f[2][j];
            let g1 = f[0][j] - f[2][j];
            let g2 = (f[1][j] >> 1) - f[3][j];
            let g3 = f[1][j] + (f[3][j] >> 1);
            let h = [g0 + g3, g1 + g2, g1 - g2, g0 - g3];
            for i in 0..4 {
                r[i * 4 + j] = (h[i] + 32) >> 6;
            }
        }
        r
    }

    #[test]
    fn forward_matches_matrix_definition() {
        let mut rng = Rng::new(1);
        for _ in 0..200 {
            let x: [i32; 16] = std::array::from_fn(|_| rng.range(-255, 255));
            let mut w = x;
            fdct4x4(&mut w);
            assert_eq!(w, fdct_matrix(&x));
        }
    }

    #[test]
    fn inverse_matches_spec_equations() {
        let mut rng = Rng::new(2);
        for _ in 0..500 {
            let d: [i32; 16] = std::array::from_fn(|_| rng.range(-20000, 20000));
            assert_eq!(idct4x4(&d), idct_spec(&d));
        }
    }

    #[test]
    fn dc_only_block_reconstructs_flat() {
        // A flat residual of value v has W00 = 16v and nothing else.
        for v in [-255, -17, -1, 0, 1, 5, 100, 255] {
            let mut b = [v; 16];
            fdct4x4(&mut b);
            assert_eq!(b[0], 16 * v);
            assert!(b[1..].iter().all(|&c| c == 0));
        }
    }

    #[test]
    fn quant_round_trip_error_is_bounded_by_step() {
        // After quantise -> scale -> inverse transform, every sample must be
        // within about one quantiser step of the input (Qstep doubles every 6).
        let mut rng = Rng::new(3);
        for qp in 0..52u8 {
            let qstep = 0.625 * 2f64.powf(qp as f64 / 6.0);
            let mut worst = 0f64;
            for _ in 0..200 {
                let x: [i32; 16] = std::array::from_fn(|_| rng.range(-255, 255));
                let mut w = x;
                fdct4x4(&mut w);
                quant4x4(&mut w, qp, true, false);
                dequant4x4(&mut w, qp, false);
                let r = idct4x4(&w);
                for i in 0..16 {
                    worst = worst.max((r[i] - x[i]).abs() as f64);
                }
            }
            assert!(worst <= qstep * 1.6 + 1.0, "qp {qp}: worst error {worst}, step {qstep}");
        }
    }

    #[test]
    fn low_qp_is_nearly_lossless() {
        let mut rng = Rng::new(4);
        for _ in 0..300 {
            let x: [i32; 16] = std::array::from_fn(|_| rng.range(-255, 255));
            let mut w = x;
            fdct4x4(&mut w);
            quant4x4(&mut w, 0, true, false);
            dequant4x4(&mut w, 0, false);
            let r = idct4x4(&w);
            for i in 0..16 {
                assert!((r[i] - x[i]).abs() <= 1);
            }
        }
    }

    #[test]
    fn dequant_matches_spec_formula() {
        // Clause 8.5.12.1 with LevelScale = 16 * normAdjust.
        for qp in 0..52u8 {
            for c in [-2047, -3, -1, 1, 2, 77, 2047] {
                let mut l = [c; 16];
                dequant4x4(&mut l, qp, false);
                for i in 0..16 {
                    let ls = 16 * DEQUANT_V[(qp % 6) as usize][POS_CLASS[i]];
                    let expect = if qp >= 24 {
                        (c * ls) << (qp / 6 - 4)
                    } else {
                        (c * ls + (1 << (3 - qp / 6))) >> (4 - qp / 6)
                    };
                    assert_eq!(l[i], expect, "qp {qp} c {c} i {i}");
                }
            }
        }
    }

    #[test]
    fn hadamard_properties() {
        let mut rng = Rng::new(5);
        for _ in 0..100 {
            let x: [i32; 16] = std::array::from_fn(|_| rng.range(-4080, 4080));
            let mut y = x;
            hadamard4x4(&mut y);
            hadamard4x4(&mut y);
            for i in 0..16 {
                assert_eq!(y[i], 16 * x[i]); // H*H = 4I per dimension
            }
            let c: [i32; 4] = std::array::from_fn(|_| rng.range(-4080, 4080));
            let mut d = c;
            hadamard2x2(&mut d);
            hadamard2x2(&mut d);
            for i in 0..4 {
                assert_eq!(d[i], 4 * c[i]);
            }
        }
    }

    #[test]
    fn luma_dc_path_round_trip() {
        // DC values of sixteen flat 4x4 blocks survive the Hadamard path to
        // within one step at each QP.
        let mut rng = Rng::new(6);
        for qp in 0..52u8 {
            let qstep = 0.625 * 2f64.powf(qp as f64 / 6.0);
            let flat: [i32; 16] = std::array::from_fn(|_| rng.range(-200, 200));
            let mut dc: [i32; 16] = std::array::from_fn(|i| 16 * flat[i]);
            luma_dc_forward(&mut dc);
            quant_dc(&mut dc, qp, true);
            dequant_luma_dc(&mut dc, qp);
            for i in 0..16 {
                let mut d = [0i32; 16];
                d[0] = dc[i];
                let r = idct4x4(&d);
                assert!(
                    (r[0] - flat[i]).abs() as f64 <= qstep * 1.2 + 1.0,
                    "qp {qp}: {} vs {}",
                    r[0],
                    flat[i]
                );
                assert!(r.iter().all(|&v| v == r[0]));
            }
        }
    }

    #[test]
    fn chroma_dc_path_round_trip() {
        let mut rng = Rng::new(7);
        for qp in 0..52u8 {
            let qstep = 0.625 * 2f64.powf(qp as f64 / 6.0);
            let flat: [i32; 4] = std::array::from_fn(|_| rng.range(-200, 200));
            let mut dc: [i32; 4] = std::array::from_fn(|i| 16 * flat[i]);
            hadamard2x2(&mut dc);
            quant_dc(&mut dc, qp, true);
            dequant_chroma_dc(&mut dc, qp);
            for i in 0..4 {
                let mut d = [0i32; 16];
                d[0] = dc[i];
                let r = idct4x4(&d);
                assert!((r[0] - flat[i]).abs() as f64 <= qstep * 1.2 + 1.0);
            }
        }
    }

    #[test]
    fn levels_are_clamped() {
        let mut dc = [32640i32; 16];
        quant_dc(&mut dc, 0, true);
        assert!(dc.iter().all(|&v| v == MAX_LEVEL));
        let mut w = [-70000i32; 16];
        quant4x4(&mut w, 0, false, false);
        assert!(w.iter().all(|&v| v == -MAX_LEVEL));
    }
}
