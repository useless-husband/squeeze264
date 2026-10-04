//! Deterministic synthetic test sequences. They stand in for real footage
//! in unit tests, CI and the offline demo: textured backgrounds with
//! sub-pixel panning, independently moving objects, sensor-like noise and
//! optional scene cuts, so that every coding tool has something to do.

use crate::frame::Frame;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Panning texture with moving rectangles and a scene cut halfway.
    Moving,
    /// Independent uniform noise in every frame (worst case for prediction).
    Noise,
    /// Static picture with a little noise (best case for skip).
    Still,
    /// Saturated black/white blocks that stress clipping paths.
    Extremes,
}

fn hash(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E3779B1) ^ (y as u32).wrapping_mul(0x85EBCA77) ^ seed.wrapping_mul(0xC2B2AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A2D39);
    h ^ (h >> 15)
}

/// Smooth value noise in [0, 1) at fixed-point position (x, y) / 256.
fn value_noise(x: i32, y: i32, seed: u32) -> f32 {
    let (xi, yi) = (x >> 8, y >> 8);
    let (fx, fy) = ((x & 255) as f32 / 256.0, (y & 255) as f32 / 256.0);
    let v = |dx: i32, dy: i32| (hash(xi + dx, yi + dy, seed) & 0xffff) as f32 / 65536.0;
    let top = v(0, 0) * (1.0 - fx) + v(1, 0) * fx;
    let bottom = v(0, 1) * (1.0 - fx) + v(1, 1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Generates frame `index` of a sequence. The frame is allocated with
/// macroblock-aligned size and padded from the visible area.
pub fn synth_frame(width: usize, height: usize, index: usize, total: usize, pattern: Pattern, seed: u64) -> Frame {
    let (aw, ah) = (width.div_ceil(16) * 16, height.div_ceil(16) * 16);
    let mut f = Frame::new(aw, ah, 0);
    let mut rng = Rng::new(seed ^ (index as u64).wrapping_mul(0x9E3779B97F4A7C15));
    let t = index as i32;
    // Scene cut: a different texture seed and motion in the second half.
    let scene = if pattern == Pattern::Moving && total >= 4 && index >= total / 2 { 1u32 } else { 0 };
    let s = seed as u32 ^ (scene * 7919);
    for (pi, plane) in f.planes.iter_mut().enumerate() {
        let (pw, ph) = if pi == 0 { (width, height) } else { (width.div_ceil(2), height.div_ceil(2)) };
        let scale = if pi == 0 { 1 } else { 2 };
        for y in 0..ph {
            for x in 0..pw {
                let (lx, ly) = ((x * scale) as i32, (y * scale) as i32);
                let v: i32 = match pattern {
                    Pattern::Noise => rng.range(0, 255),
                    Pattern::Still => {
                        let base = 60.0 + 120.0 * value_noise(lx * 20, ly * 20, s + pi as u32);
                        base as i32 + rng.range(-1, 1)
                    }
                    Pattern::Extremes => {
                        let cell = hash((lx + t * 3) / 12, (ly + t) / 12, s + pi as u32);
                        match cell % 5 {
                            0 => 0,
                            1 => 255,
                            2 => rng.range(0, 255),
                            3 => ((lx * 255) / width.max(1) as i32).clamp(0, 255),
                            _ => 128,
                        }
                    }
                    Pattern::Moving => {
                        // Background pans by a fractional amount each frame.
                        let (vx, vy) = if scene == 0 { (150, 60) } else { (-90, 200) };
                        let bx = lx * 256 + t * vx;
                        let by = ly * 256 + t * vy;
                        let mut val = 40.0
                            + 110.0 * value_noise(bx / 14, by / 14, s + pi as u32)
                            + 50.0 * value_noise(bx / 3, by / 3, s + 17 + pi as u32);
                        // Three rectangles with their own textures and velocities.
                        for k in 0..3i32 {
                            let ow = (width as i32 / 5).max(8);
                            let oh = (height as i32 / 5).max(8);
                            let span_x = (width as i32 + ow).max(1);
                            let span_y = (height as i32 + oh).max(1);
                            let ox = ((k * 97 + t * (5 + 3 * k) * 64 / 16) * 4).rem_euclid(span_x * 256) - ow * 256;
                            let oy = ((k * 53 + t * (2 * k - 1) * 64 / 16) * 4 + k * 40 * 256).rem_euclid(span_y * 256) - oh * 256;
                            let (rx, ry) = (lx * 256 - ox, ly * 256 - oy);
                            if rx >= 0 && ry >= 0 && rx < ow * 256 && ry < oh * 256 {
                                val = 30.0 + 60.0 * k as f32 + 90.0 * value_noise(rx / 5, ry / 5, s + 100 + k as u32 + pi as u32);
                            }
                        }
                        val as i32 + rng.range(-2, 2)
                    }
                };
                plane.set(x as i32, y as i32, v.clamp(0, 255) as u8);
            }
        }
    }
    f.pad_from_visible(width, height);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_distinct() {
        let a = synth_frame(48, 32, 3, 10, Pattern::Moving, 7);
        let b = synth_frame(48, 32, 3, 10, Pattern::Moving, 7);
        let c = synth_frame(48, 32, 4, 10, Pattern::Moving, 7);
        assert_eq!(a.planes[0].data, b.planes[0].data);
        assert_ne!(a.planes[0].data, c.planes[0].data);
    }

    #[test]
    fn odd_sized_visible_area_is_padded() {
        let f = synth_frame(50, 38, 0, 1, Pattern::Still, 1);
        assert_eq!((f.planes[0].w, f.planes[0].h), (64, 48));
        assert_eq!(f.planes[0].at(63, 10), f.planes[0].at(49, 10));
        assert_eq!(f.planes[1].at(5, 23), f.planes[1].at(5, 18));
    }

    #[test]
    fn extremes_pattern_reaches_both_ends() {
        let f = synth_frame(64, 64, 0, 1, Pattern::Extremes, 5);
        assert!(f.planes[0].data.contains(&0));
        assert!(f.planes[0].data.contains(&255));
    }
}
