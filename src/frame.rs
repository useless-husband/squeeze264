//! Picture storage: 8-bit planes with an optional border of replicated samples.

/// One 8-bit sample plane. `pad` extra samples surround the picture on every
/// side so that motion vectors may point outside it.
#[derive(Clone)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub pad: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl Plane {
    pub fn new(w: usize, h: usize, pad: usize) -> Self {
        let stride = w + 2 * pad;
        Plane {
            w,
            h,
            pad,
            stride,
            data: vec![0; stride * (h + 2 * pad)],
        }
    }

    /// Index of sample (x, y); coordinates may reach into the border.
    #[inline(always)]
    pub fn idx(&self, x: i32, y: i32) -> usize {
        debug_assert!(x >= -(self.pad as i32) && x < (self.w + self.pad) as i32);
        debug_assert!(y >= -(self.pad as i32) && y < (self.h + self.pad) as i32);
        ((y + self.pad as i32) as usize) * self.stride + (x + self.pad as i32) as usize
    }

    #[inline(always)]
    pub fn at(&self, x: i32, y: i32) -> u8 {
        self.data[self.idx(x, y)]
    }

    #[inline(always)]
    pub fn set(&mut self, x: i32, y: i32, v: u8) {
        let i = self.idx(x, y);
        self.data[i] = v;
    }

    /// The picture row `y` without borders.
    #[inline]
    pub fn row(&self, y: usize) -> &[u8] {
        let s = self.idx(0, y as i32);
        &self.data[s..s + self.w]
    }

    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
        let s = self.idx(0, y as i32);
        let w = self.w;
        &mut self.data[s..s + w]
    }

    /// Sample with coordinates clamped to the picture, as the reference
    /// picture sample fetch of clause 8.4.2.2 prescribes.
    #[inline]
    pub fn at_clamped(&self, x: i32, y: i32) -> u8 {
        self.at(x.clamp(0, self.w as i32 - 1), y.clamp(0, self.h as i32 - 1))
    }

    /// Fills the border by replicating the nearest picture sample.
    pub fn extend_borders(&mut self) {
        let (w, h, pad, stride) = (self.w, self.h, self.pad, self.stride);
        if pad == 0 {
            return;
        }
        for y in 0..h {
            let s = (y + pad) * stride;
            let left = self.data[s + pad];
            let right = self.data[s + pad + w - 1];
            self.data[s..s + pad].fill(left);
            self.data[s + pad + w..s + stride].fill(right);
        }
        let top = pad * stride;
        let bottom = (pad + h - 1) * stride;
        for y in 0..pad {
            self.data.copy_within(top..top + stride, y * stride);
            self.data.copy_within(bottom..bottom + stride, (pad + h + y) * stride);
        }
    }
}

/// A 4:2:0 picture: luma plus two half-resolution chroma planes.
#[derive(Clone)]
pub struct Frame {
    pub planes: [Plane; 3],
}

impl Frame {
    /// `w` and `h` are luma dimensions (even); `pad` is the luma border,
    /// chroma gets half of it.
    pub fn new(w: usize, h: usize, pad: usize) -> Self {
        Frame {
            planes: [
                Plane::new(w, h, pad),
                Plane::new(w / 2, h / 2, pad / 2),
                Plane::new(w / 2, h / 2, pad / 2),
            ],
        }
    }

    pub fn extend_borders(&mut self) {
        for p in &mut self.planes {
            p.extend_borders();
        }
    }

    /// Replicates the right column and bottom row of a `vis_w` x `vis_h`
    /// picture into the rest of the (macroblock aligned) frame.
    pub fn pad_from_visible(&mut self, vis_w: usize, vis_h: usize) {
        for (i, p) in self.planes.iter_mut().enumerate() {
            let (vw, vh) = if i == 0 {
                (vis_w, vis_h)
            } else {
                (vis_w.div_ceil(2), vis_h.div_ceil(2))
            };
            let (w, h) = (p.w, p.h);
            for y in 0..vh {
                let row = p.row_mut(y);
                let last = row[vw - 1];
                row[vw..w].fill(last);
            }
            for y in vh..h {
                let src = p.idx(0, vh as i32 - 1);
                let dst = p.idx(0, y as i32);
                p.data.copy_within(src..src + w, dst);
            }
        }
    }

    /// Appends the visible `vis_w` x `vis_h` area as planar I420 bytes.
    pub fn write_i420(&self, vis_w: usize, vis_h: usize, out: &mut Vec<u8>) {
        for (i, p) in self.planes.iter().enumerate() {
            let (vw, vh) = if i == 0 {
                (vis_w, vis_h)
            } else {
                (vis_w.div_ceil(2), vis_h.div_ceil(2))
            };
            for y in 0..vh {
                out.extend_from_slice(&p.row(y)[..vw]);
            }
        }
    }
}

/// Sum of squared differences over the visible area of one plane.
pub fn plane_sse(a: &Plane, b: &Plane, vw: usize, vh: usize) -> u64 {
    let mut sse = 0u64;
    for y in 0..vh {
        let (ra, rb) = (&a.row(y)[..vw], &b.row(y)[..vw]);
        let mut s = 0u32;
        for x in 0..vw {
            let d = ra[x] as i32 - rb[x] as i32;
            s += (d * d) as u32;
        }
        sse += s as u64;
    }
    sse
}

/// PSNR in dB for 8-bit samples; identical pictures are reported as 100 dB.
pub fn psnr(sse: u64, samples: u64) -> f64 {
    if sse == 0 {
        return 100.0;
    }
    10.0 * (255.0 * 255.0 * samples as f64 / sse as f64).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_extension_replicates_edges() {
        let mut p = Plane::new(4, 3, 2);
        for y in 0..3 {
            for x in 0..4 {
                p.set(x, y, (10 * y + x) as u8);
            }
        }
        p.extend_borders();
        for y in -2..5 {
            for x in -2..6 {
                assert_eq!(p.at(x, y), p.at_clamped(x, y), "({x},{y})");
            }
        }
    }

    #[test]
    fn padding_from_visible_area() {
        let mut f = Frame::new(16, 16, 0);
        for y in 0..10 {
            for x in 0..6 {
                f.planes[0].set(x, y, (x + y) as u8);
            }
        }
        f.pad_from_visible(6, 10);
        assert_eq!(f.planes[0].at(15, 3), 5 + 3);
        assert_eq!(f.planes[0].at(2, 15), 2 + 9);
        assert_eq!(f.planes[0].at(15, 15), 5 + 9);
        let mut out = Vec::new();
        f.write_i420(6, 10, &mut out);
        assert_eq!(out.len(), 60 + 2 * 15);
    }

    #[test]
    fn psnr_values() {
        assert_eq!(psnr(0, 100), 100.0);
        // MSE of 1 -> 48.13 dB
        assert!((psnr(100, 100) - 48.1308).abs() < 1e-3);
    }
}
