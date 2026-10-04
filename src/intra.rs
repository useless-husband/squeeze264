//! Intra prediction (clause 8.3): nine 4x4 luma modes, four 16x16 luma modes
//! and four chroma modes. Predictors work on small neighbour structs so they
//! can be tested without a picture around them.

pub const I4_V: u8 = 0;
pub const I4_H: u8 = 1;
pub const I4_DC: u8 = 2;
pub const I4_DDL: u8 = 3;
pub const I4_DDR: u8 = 4;
pub const I4_VR: u8 = 5;
pub const I4_HD: u8 = 6;
pub const I4_VL: u8 = 7;
pub const I4_HU: u8 = 8;

pub const I16_V: u8 = 0;
pub const I16_H: u8 = 1;
pub const I16_DC: u8 = 2;
pub const I16_PLANE: u8 = 3;

pub const CHROMA_DC: u8 = 0;
pub const CHROMA_H: u8 = 1;
pub const CHROMA_V: u8 = 2;
pub const CHROMA_PLANE: u8 = 3;

/// Neighbouring samples of a 4x4 block. `top[4..8]` is the above-right
/// block; the caller substitutes `top[3]` when it is not available.
#[derive(Clone, Copy, Default, Debug)]
pub struct Edge4 {
    pub top: [u8; 8],
    pub left: [u8; 4],
    pub tl: u8,
    pub has_top: bool,
    pub has_left: bool,
}

/// Whether a 4x4 mode may be used with the given neighbours. The above-left
/// sample exists whenever both the row above and the column to the left do
/// (one slice per picture).
pub fn i4_mode_allowed(mode: u8, has_top: bool, has_left: bool) -> bool {
    match mode {
        I4_V | I4_DDL | I4_VL => has_top,
        I4_H | I4_HU => has_left,
        I4_DC => true,
        _ => has_top && has_left,
    }
}

/// Predicts a 4x4 block; `out` is raster order.
pub fn pred4x4(mode: u8, e: &Edge4, out: &mut [u8; 16]) {
    // t(i) is p[i, -1] and l(i) is p[-1, i]; index -1 is the corner sample.
    let t = |i: i32| -> i32 {
        if i < 0 {
            e.tl as i32
        } else {
            e.top[i as usize] as i32
        }
    };
    let l = |i: i32| -> i32 {
        if i < 0 {
            e.tl as i32
        } else {
            e.left[i as usize] as i32
        }
    };
    match mode {
        I4_V => {
            for y in 0..4 {
                out[y * 4..y * 4 + 4].copy_from_slice(&e.top[..4]);
            }
        }
        I4_H => {
            for y in 0..4 {
                out[y * 4..y * 4 + 4].fill(e.left[y]);
            }
        }
        I4_DC => {
            let st: i32 = e.top[..4].iter().map(|&v| v as i32).sum();
            let sl: i32 = e.left.iter().map(|&v| v as i32).sum();
            let dc = match (e.has_top, e.has_left) {
                (true, true) => (st + sl + 4) >> 3,
                (true, false) => (st + 2) >> 2,
                (false, true) => (sl + 2) >> 2,
                (false, false) => 128,
            };
            out.fill(dc as u8);
        }
        I4_DDL => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let v = if x == 3 && y == 3 {
                        (t(6) + 3 * t(7) + 2) >> 2
                    } else {
                        (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
        I4_DDR => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let v = if x > y {
                        (t(x - y - 2) + 2 * t(x - y - 1) + t(x - y) + 2) >> 2
                    } else if x < y {
                        (l(y - x - 2) + 2 * l(y - x - 1) + l(y - x) + 2) >> 2
                    } else {
                        (t(0) + 2 * t(-1) + l(0) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
        I4_VR => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * x - y;
                    let xs = x - (y >> 1);
                    let v = if z >= 0 && z & 1 == 0 {
                        (t(xs - 1) + t(xs) + 1) >> 1
                    } else if z >= 0 {
                        (t(xs - 2) + 2 * t(xs - 1) + t(xs) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * t(-1) + t(0) + 2) >> 2
                    } else {
                        (l(y - 1) + 2 * l(y - 2) + l(y - 3) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
        I4_HD => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * y - x;
                    let ys = y - (x >> 1);
                    let v = if z >= 0 && z & 1 == 0 {
                        (l(ys - 1) + l(ys) + 1) >> 1
                    } else if z >= 0 {
                        (l(ys - 2) + 2 * l(ys - 1) + l(ys) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * t(-1) + t(0) + 2) >> 2
                    } else {
                        (t(x - 1) + 2 * t(x - 2) + t(x - 3) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
        I4_VL => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let i = x + (y >> 1);
                    let v = if y & 1 == 0 {
                        (t(i) + t(i + 1) + 1) >> 1
                    } else {
                        (t(i) + 2 * t(i + 1) + t(i + 2) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
        _ => {
            // Horizontal-Up
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let v = if z > 5 {
                        l(3)
                    } else if z == 5 {
                        (l(2) + 3 * l(3) + 2) >> 2
                    } else if z & 1 == 0 {
                        (l(i) + l(i + 1) + 1) >> 1
                    } else {
                        (l(i) + 2 * l(i + 1) + l(i + 2) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = v as u8;
                }
            }
        }
    }
}

/// Neighbouring samples of a 16x16 luma macroblock.
#[derive(Clone, Copy, Debug)]
pub struct Edge16 {
    pub top: [u8; 16],
    pub left: [u8; 16],
    pub tl: u8,
    pub has_top: bool,
    pub has_left: bool,
}

pub fn i16_mode_allowed(mode: u8, has_top: bool, has_left: bool) -> bool {
    match mode {
        I16_V => has_top,
        I16_H => has_left,
        I16_DC => true,
        _ => has_top && has_left,
    }
}

/// Predicts a 16x16 luma block; `out` is raster order.
pub fn pred16x16(mode: u8, e: &Edge16, out: &mut [u8; 256]) {
    match mode {
        I16_V => {
            for y in 0..16 {
                out[y * 16..y * 16 + 16].copy_from_slice(&e.top);
            }
        }
        I16_H => {
            for y in 0..16 {
                out[y * 16..y * 16 + 16].fill(e.left[y]);
            }
        }
        I16_DC => {
            let st: i32 = e.top.iter().map(|&v| v as i32).sum();
            let sl: i32 = e.left.iter().map(|&v| v as i32).sum();
            let dc = match (e.has_top, e.has_left) {
                (true, true) => (st + sl + 16) >> 5,
                (true, false) => (st + 8) >> 4,
                (false, true) => (sl + 8) >> 4,
                (false, false) => 128,
            };
            out.fill(dc as u8);
        }
        _ => {
            let t = |i: i32| if i < 0 { e.tl as i32 } else { e.top[i as usize] as i32 };
            let l = |i: i32| if i < 0 { e.tl as i32 } else { e.left[i as usize] as i32 };
            let (mut h, mut v) = (0i32, 0i32);
            for k in 0..8i32 {
                h += (k + 1) * (t(8 + k) - t(6 - k));
                v += (k + 1) * (l(8 + k) - l(6 - k));
            }
            let a = 16 * (l(15) + t(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..16i32 {
                    let p = (a + b * (x - 7) + c * (y - 7) + 16) >> 5;
                    out[(y * 16 + x) as usize] = p.clamp(0, 255) as u8;
                }
            }
        }
    }
}

/// Neighbouring samples of an 8x8 chroma block.
#[derive(Clone, Copy, Debug)]
pub struct Edge8 {
    pub top: [u8; 8],
    pub left: [u8; 8],
    pub tl: u8,
    pub has_top: bool,
    pub has_left: bool,
}

pub fn chroma_mode_allowed(mode: u8, has_top: bool, has_left: bool) -> bool {
    match mode {
        CHROMA_DC => true,
        CHROMA_H => has_left,
        CHROMA_V => has_top,
        _ => has_top && has_left,
    }
}

/// Predicts an 8x8 chroma block; `out` is raster order.
pub fn pred_chroma(mode: u8, e: &Edge8, out: &mut [u8; 64]) {
    match mode {
        CHROMA_DC => {
            // Each 4x4 quadrant has its own DC with its own neighbour rule.
            for by in 0..2 {
                for bx in 0..2 {
                    let st: i32 = e.top[bx * 4..bx * 4 + 4].iter().map(|&v| v as i32).sum();
                    let sl: i32 = e.left[by * 4..by * 4 + 4].iter().map(|&v| v as i32).sum();
                    let top_only = (st + 2) >> 2;
                    let left_only = (sl + 2) >> 2;
                    let dc = if bx == by {
                        match (e.has_top, e.has_left) {
                            (true, true) => (st + sl + 4) >> 3,
                            (true, false) => top_only,
                            (false, true) => left_only,
                            (false, false) => 128,
                        }
                    } else if bx == 1 {
                        // Top-right quadrant prefers the row above.
                        if e.has_top {
                            top_only
                        } else if e.has_left {
                            left_only
                        } else {
                            128
                        }
                    } else if e.has_left {
                        // Bottom-left quadrant prefers the column to the left.
                        left_only
                    } else if e.has_top {
                        top_only
                    } else {
                        128
                    };
                    for y in 0..4 {
                        let o = (by * 4 + y) * 8 + bx * 4;
                        out[o..o + 4].fill(dc as u8);
                    }
                }
            }
        }
        CHROMA_H => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].fill(e.left[y]);
            }
        }
        CHROMA_V => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].copy_from_slice(&e.top);
            }
        }
        _ => {
            let t = |i: i32| if i < 0 { e.tl as i32 } else { e.top[i as usize] as i32 };
            let l = |i: i32| if i < 0 { e.tl as i32 } else { e.left[i as usize] as i32 };
            let (mut h, mut v) = (0i32, 0i32);
            for k in 0..4i32 {
                h += (k + 1) * (t(4 + k) - t(2 - k));
                v += (k + 1) * (l(4 + k) - l(2 - k));
            }
            let a = 16 * (l(7) + t(7));
            let b = (34 * h + 32) >> 6;
            let c = (34 * v + 32) >> 6;
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let p = (a + b * (x - 3) + c * (y - 3) + 16) >> 5;
                    out[(y * 8 + x) as usize] = p.clamp(0, 255) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn edge4(top: [u8; 8], left: [u8; 4], tl: u8) -> Edge4 {
        Edge4 {
            top,
            left,
            tl,
            has_top: true,
            has_left: true,
        }
    }

    #[test]
    fn flat_neighbours_give_flat_prediction_in_every_mode() {
        let e = edge4([77; 8], [77; 4], 77);
        for mode in 0..9 {
            let mut out = [0u8; 16];
            pred4x4(mode, &e, &mut out);
            assert!(out.iter().all(|&v| v == 77), "mode {mode}");
        }
        let e16 = Edge16 {
            top: [91; 16],
            left: [91; 16],
            tl: 91,
            has_top: true,
            has_left: true,
        };
        for mode in 0..4 {
            let mut out = [0u8; 256];
            pred16x16(mode, &e16, &mut out);
            assert!(out.iter().all(|&v| v == 91), "i16 mode {mode}");
        }
        let e8 = Edge8 {
            top: [33; 8],
            left: [33; 8],
            tl: 33,
            has_top: true,
            has_left: true,
        };
        for mode in 0..4 {
            let mut out = [0u8; 64];
            pred_chroma(mode, &e8, &mut out);
            assert!(out.iter().all(|&v| v == 33), "chroma mode {mode}");
        }
    }

    // Hand-evaluated from the formulas of clause 8.3.1.2 with
    // top = 10,20,..,80; left = 15,25,35,45; corner = 5.
    #[test]
    fn directional_4x4_modes_hand_checked() {
        let e = edge4([10, 20, 30, 40, 50, 60, 70, 80], [15, 25, 35, 45], 5);
        let mut o = [0u8; 16];

        pred4x4(I4_V, &e, &mut o);
        assert_eq!(o, [10, 20, 30, 40, 10, 20, 30, 40, 10, 20, 30, 40, 10, 20, 30, 40]);
        pred4x4(I4_H, &e, &mut o);
        assert_eq!(o, [15, 15, 15, 15, 25, 25, 25, 25, 35, 35, 35, 35, 45, 45, 45, 45]);
        pred4x4(I4_DC, &e, &mut o);
        assert_eq!(o[0], ((100 + 120 + 4) >> 3) as u8);

        // Diagonal down-left: (t[x+y] + 2t[x+y+1] + t[x+y+2] + 2) >> 2 on a
        // linear ramp gives the middle sample; the last one is (70+3*80+2)>>2.
        pred4x4(I4_DDL, &e, &mut o);
        assert_eq!(o, [20, 30, 40, 50, 30, 40, 50, 60, 40, 50, 60, 70, 50, 60, 70, 78]);

        // Diagonal down-right: main diagonal (10 + 2*5 + 15 + 2) >> 2 = 9.
        pred4x4(I4_DDR, &e, &mut o);
        assert_eq!(o[0], 9);
        assert_eq!(o[5], 9);
        assert_eq!(o[1], ((5 + 2 * 10 + 20 + 2) >> 2) as u8); // x-y = 1
        assert_eq!(o[3], ((20 + 2 * 30 + 40 + 2) >> 2) as u8); // x-y = 3
        assert_eq!(o[4], ((5 + 2 * 15 + 25 + 2) >> 2) as u8); // y-x = 1
        assert_eq!(o[12], ((25 + 2 * 35 + 45 + 2) >> 2) as u8); // y-x = 3

        // Vertical-right: zVR = 2x - y.
        pred4x4(I4_VR, &e, &mut o);
        assert_eq!(o[0], ((5 + 10 + 1) >> 1) as u8); // z=0
        assert_eq!(o[3], ((30 + 40 + 1) >> 1) as u8); // z=6
        assert_eq!(o[4], ((15 + 2 * 5 + 10 + 2) >> 2) as u8); // z=-1
        assert_eq!(o[5], ((5 + 2 * 10 + 20 + 2) >> 2) as u8); // z=1
        assert_eq!(o[8], ((25 + 2 * 15 + 5 + 2) >> 2) as u8); // z=-2: l(1),l(0),l(-1)
        assert_eq!(o[12], ((35 + 2 * 25 + 15 + 2) >> 2) as u8); // z=-3
        assert_eq!(o[9], o[0]); // (x=1,y=2) shares z=0 phase shifted by one

        // Horizontal-down: zHD = 2y - x.
        pred4x4(I4_HD, &e, &mut o);
        assert_eq!(o[0], ((5 + 15 + 1) >> 1) as u8);
        assert_eq!(o[1], ((15 + 2 * 5 + 10 + 2) >> 2) as u8); // z=-1
        assert_eq!(o[2], ((20 + 2 * 10 + 5 + 2) >> 2) as u8); // z=-2
        assert_eq!(o[3], ((30 + 2 * 20 + 10 + 2) >> 2) as u8); // z=-3
        assert_eq!(o[4], ((15 + 25 + 1) >> 1) as u8); // z=2
        assert_eq!(o[5], ((5 + 2 * 15 + 25 + 2) >> 2) as u8); // z=1

        // Vertical-left.
        pred4x4(I4_VL, &e, &mut o);
        assert_eq!(o[0], 15);
        assert_eq!(o[4], 20);
        assert_eq!(o[11], ((50 + 60 + 1) >> 1) as u8); // x=3,y=2: t(4),t(5)
        assert_eq!(o[15], ((50 + 2 * 60 + 70 + 2) >> 2) as u8);

        // Horizontal-up.
        pred4x4(I4_HU, &e, &mut o);
        assert_eq!(o[0], 20);
        assert_eq!(o[1], ((15 + 2 * 25 + 35 + 2) >> 2) as u8);
        assert_eq!(o[9], ((35 + 3 * 45 + 2) >> 2) as u8); // x=1,y=2: z=5
        assert_eq!(o[10], 45);
        assert_eq!(o[15], 45);
    }

    #[test]
    fn dc_fallbacks() {
        let mut e = edge4([10; 8], [30; 4], 0);
        let mut o = [0u8; 16];
        e.has_top = false;
        pred4x4(I4_DC, &e, &mut o);
        assert_eq!(o[0], 30);
        e.has_top = true;
        e.has_left = false;
        pred4x4(I4_DC, &e, &mut o);
        assert_eq!(o[0], 10);
        e.has_top = false;
        pred4x4(I4_DC, &e, &mut o);
        assert_eq!(o[0], 128);
    }

    #[test]
    fn plane_prediction_reproduces_a_linear_ramp() {
        // p(x, y) = 40 + 3x + 2y is in the span of the plane predictor;
        // the integer approximation must stay within one grey level.
        let f = |x: i32, y: i32| (40 + 3 * x + 2 * y) as u8;
        let e = Edge16 {
            top: std::array::from_fn(|x| f(x as i32, -1)),
            left: std::array::from_fn(|y| f(-1, y as i32)),
            tl: f(-1, -1),
            has_top: true,
            has_left: true,
        };
        let mut out = [0u8; 256];
        pred16x16(I16_PLANE, &e, &mut out);
        for y in 0..16 {
            for x in 0..16 {
                let d = out[y * 16 + x] as i32 - f(x as i32, y as i32) as i32;
                assert!(d.abs() <= 1, "({x},{y}): {d}");
            }
        }
        let e8 = Edge8 {
            top: std::array::from_fn(|x| f(x as i32, -1)),
            left: std::array::from_fn(|y| f(-1, y as i32)),
            tl: f(-1, -1),
            has_top: true,
            has_left: true,
        };
        let mut out = [0u8; 64];
        pred_chroma(CHROMA_PLANE, &e8, &mut out);
        for y in 0..8 {
            for x in 0..8 {
                let d = out[y * 8 + x] as i32 - f(x as i32, y as i32) as i32;
                assert!(d.abs() <= 1, "chroma ({x},{y}): {d}");
            }
        }
    }

    #[test]
    fn plane_prediction_clips() {
        let e = Edge16 {
            top: std::array::from_fn(|x| if x < 8 { 0 } else { 255 }),
            left: std::array::from_fn(|y| if y < 8 { 0 } else { 255 }),
            tl: 0,
            has_top: true,
            has_left: true,
        };
        let mut out = [0u8; 256];
        pred16x16(I16_PLANE, &e, &mut out);
        assert_eq!(out[0], 0);
        assert_eq!(out[255], 255);
    }

    #[test]
    fn chroma_dc_quadrant_rules() {
        let mut top = [0u8; 8];
        let mut left = [0u8; 8];
        top[..4].fill(10);
        top[4..].fill(20);
        left[..4].fill(30);
        left[4..].fill(40);
        let mut e = Edge8 {
            top,
            left,
            tl: 0,
            has_top: true,
            has_left: true,
        };
        let mut o = [0u8; 64];
        pred_chroma(CHROMA_DC, &e, &mut o);
        assert_eq!(o[0], 20); // (40+120+4)>>3
        assert_eq!(o[4], 20); // top-right: top only
        assert_eq!(o[32], 40); // bottom-left: left only
        assert_eq!(o[36], 30); // (80+160+4)>>3
        e.has_top = false;
        pred_chroma(CHROMA_DC, &e, &mut o);
        assert_eq!((o[0], o[4], o[32], o[36]), (30, 30, 40, 40));
        e.has_top = true;
        e.has_left = false;
        pred_chroma(CHROMA_DC, &e, &mut o);
        assert_eq!((o[0], o[4], o[32], o[36]), (10, 20, 10, 20));
        e.has_top = false;
        pred_chroma(CHROMA_DC, &e, &mut o);
        assert!(o.iter().all(|&v| v == 128));
    }

    #[test]
    fn mode_availability() {
        assert!(i4_mode_allowed(I4_DC, false, false));
        assert!(!i4_mode_allowed(I4_V, false, true));
        assert!(i4_mode_allowed(I4_HU, false, true));
        assert!(!i4_mode_allowed(I4_DDR, true, false));
        assert!(i4_mode_allowed(I4_VL, true, false));
        assert!(!i16_mode_allowed(I16_PLANE, true, false));
        assert!(chroma_mode_allowed(CHROMA_V, true, false));
        assert!(!chroma_mode_allowed(CHROMA_H, true, false));
    }

    #[test]
    fn random_edges_stay_within_neighbour_range() {
        // Every non-plane predictor is a convex combination of neighbours.
        let mut rng = Rng::new(11);
        for _ in 0..200 {
            let e = edge4(
                std::array::from_fn(|_| rng.range(60, 90) as u8),
                std::array::from_fn(|_| rng.range(60, 90) as u8),
                rng.range(60, 90) as u8,
            );
            for mode in 0..9 {
                let mut o = [0u8; 16];
                pred4x4(mode, &e, &mut o);
                assert!(o.iter().all(|&v| (60..=90).contains(&v)));
            }
        }
    }
}
