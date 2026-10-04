//! Block distortion measures used for encoder decisions (not normative).

/// Sum of absolute differences over a w x h block.
#[inline]
pub fn sad(a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize) -> u32 {
    let mut total = 0u32;
    for y in 0..h {
        let (ra, rb) = (&a[y * a_stride..y * a_stride + w], &b[y * b_stride..y * b_stride + w]);
        let mut s = 0u32;
        for x in 0..w {
            s += (ra[x] as i32 - rb[x] as i32).unsigned_abs();
        }
        total += s;
    }
    total
}

/// Sum of absolute values of the 4x4 Hadamard transform of a difference block.
#[inline]
pub fn satd4x4(a: &[u8], a_stride: usize, b: &[u8], b_stride: usize) -> u32 {
    let mut d = [0i32; 16];
    for y in 0..4 {
        for x in 0..4 {
            d[y * 4 + x] = a[y * a_stride + x] as i32 - b[y * b_stride + x] as i32;
        }
    }
    for r in 0..4 {
        let (s0, s1) = (d[r * 4] + d[r * 4 + 1], d[r * 4] - d[r * 4 + 1]);
        let (s2, s3) = (d[r * 4 + 2] + d[r * 4 + 3], d[r * 4 + 2] - d[r * 4 + 3]);
        d[r * 4] = s0 + s2;
        d[r * 4 + 1] = s1 + s3;
        d[r * 4 + 2] = s0 - s2;
        d[r * 4 + 3] = s1 - s3;
    }
    let mut sum = 0u32;
    for c in 0..4 {
        let (s0, s1) = (d[c] + d[4 + c], d[c] - d[4 + c]);
        let (s2, s3) = (d[8 + c] + d[12 + c], d[8 + c] - d[12 + c]);
        sum +=
            (s0 + s2).unsigned_abs() + (s1 + s3).unsigned_abs() + (s0 - s2).unsigned_abs() + (s1 - s3).unsigned_abs();
    }
    sum.div_ceil(2)
}

/// SATD over a w x h block (multiples of 4), as a sum of 4x4 transforms.
#[inline]
pub fn satd(a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize) -> u32 {
    let mut sum = 0;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            sum += satd4x4(&a[y * a_stride + x..], a_stride, &b[y * b_stride + x..], b_stride);
        }
    }
    sum
}

/// Lagrange multiplier for SATD-domain decisions, scaled by 16:
/// lambda = 2^((QP - 12) / 6), the usual square root of the RD lambda.
pub fn lambda16(qp: u8) -> u32 {
    (16.0 * 2f64.powf((qp as f64 - 12.0) / 6.0)).round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_blocks_cost_nothing() {
        let a: Vec<u8> = (0..64).map(|i| (i * 3) as u8).collect();
        assert_eq!(sad(&a, 8, &a, 8, 8, 8), 0);
        assert_eq!(satd(&a, 8, &a, 8, 8, 8), 0);
    }

    #[test]
    fn known_values() {
        let a = [10u8; 16];
        let b = [13u8; 16];
        assert_eq!(sad(&a, 4, &b, 4, 4, 4), 48);
        // A flat difference of 3 has a single Hadamard coefficient 48 -> 24.
        assert_eq!(satd4x4(&a, 4, &b, 4), 24);
        // A single differing sample spreads to all 16 coefficients: 16*5/2.
        let mut c = a;
        c[5] = 15;
        assert_eq!(satd4x4(&a, 4, &c, 4), 40);
    }

    #[test]
    fn lambda_doubles_every_six_qp() {
        assert_eq!(lambda16(12), 16);
        assert_eq!(lambda16(24), 64);
        assert_eq!(lambda16(30), 128);
        assert!(lambda16(0) >= 1);
    }
}
