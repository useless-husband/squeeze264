//! Motion vector prediction (clause 8.4.1.3) and the P_Skip vector
//! (clause 8.4.1.1), over a per-4x4-block motion field.

use crate::inter::Mv;
use crate::tables::BLK_IDX;

/// Motion data of the picture being coded, one entry per 4x4 luma block.
/// `refi` is 0 for inter blocks and -1 for intra blocks. Entries of blocks
/// not coded yet are stale; availability is decided from positions.
pub struct MotionField {
    pub w4: usize,
    pub h4: usize,
    pub mv: Vec<Mv>,
    pub refi: Vec<i8>,
}

/// A neighbouring partition: None when not available.
type Neighbour = Option<(i8, Mv)>;

impl MotionField {
    pub fn new(mb_w: usize, mb_h: usize) -> Self {
        MotionField {
            w4: mb_w * 4,
            h4: mb_h * 4,
            mv: vec![[0, 0]; mb_w * mb_h * 16],
            refi: vec![-1; mb_w * mb_h * 16],
        }
    }

    /// Fills a rectangle of 4x4 blocks inside macroblock (mb_x, mb_y).
    #[inline]
    pub fn fill(&mut self, mb_x: usize, mb_y: usize, bx: usize, by: usize, pw: usize, ph: usize, refi: i8, mv: Mv) {
        for y in 0..ph {
            let o = (mb_y * 4 + by + y) * self.w4 + mb_x * 4 + bx;
            self.mv[o..o + pw].fill(mv);
            self.refi[o..o + pw].fill(refi);
        }
    }

    #[inline]
    fn get(&self, x4: i32, y4: i32) -> Neighbour {
        if x4 < 0 || y4 < 0 || x4 >= self.w4 as i32 {
            return None;
        }
        let i = y4 as usize * self.w4 + x4 as usize;
        Some((self.refi[i], self.mv[i]))
    }

    /// Neighbours A (left), B (above) and C (above-right, replaced by D,
    /// above-left, when C is not available) of the partition whose top-left
    /// 4x4 block is (bx, by) inside macroblock (mb_x, mb_y), `pw` blocks wide.
    fn neighbours(&self, mb_x: usize, mb_y: usize, bx: usize, by: usize, pw: usize) -> [Neighbour; 3] {
        let x4 = (mb_x * 4 + bx) as i32;
        let y4 = (mb_y * 4 + by) as i32;
        let a = self.get(x4 - 1, y4);
        let b = self.get(x4, y4 - 1);
        let cx = bx + pw;
        let c_available = if by == 0 {
            // Row above the macroblock: already decoded if inside the picture.
            mb_y > 0 && (mb_x * 4 + cx) < self.w4
        } else if cx >= 4 {
            // Belongs to the macroblock on the right: not decoded yet.
            false
        } else {
            // Inside this macroblock: available only if earlier in decoding order.
            BLK_IDX[(by - 1) * 4 + cx] < BLK_IDX[by * 4 + bx]
        };
        let c = if c_available {
            self.get(x4 + pw as i32, y4 - 1)
        } else {
            self.get(x4 - 1, y4 - 1)
        };
        [a, b, c]
    }

    /// Predicted vector for a partition of `pw` x `ph` 4x4 blocks at (bx, by)
    /// in macroblock (mb_x, mb_y), reference index 0.
    pub fn predict(&self, mb_x: usize, mb_y: usize, bx: usize, by: usize, pw: usize, ph: usize) -> Mv {
        let [a, b, c] = self.neighbours(mb_x, mb_y, bx, by, pw);
        let matches = |n: Neighbour| matches!(n, Some((0, _)));
        // Directional rules for 16x8 and 8x16 partitions.
        if pw == 4 && ph == 2 {
            if by == 0 {
                if let Some((0, mv)) = b {
                    return mv;
                }
            } else if let Some((0, mv)) = a {
                return mv;
            }
        } else if pw == 2 && ph == 4 {
            if bx == 0 {
                if let Some((0, mv)) = a {
                    return mv;
                }
            } else if let Some((0, mv)) = c {
                return mv;
            }
        }
        // Median rule. When only A exists it stands in for B and C.
        let (a, b, c) = if b.is_none() && c.is_none() && a.is_some() {
            (a, a, a)
        } else {
            (a, b, c)
        };
        let n = [a, b, c];
        let count = n.iter().filter(|&&x| matches(x)).count();
        if count == 1 {
            for x in n {
                if let Some((0, mv)) = x {
                    return mv;
                }
            }
        }
        // Unavailable and intra neighbours contribute a zero vector.
        let v = |x: Neighbour| x.map_or([0, 0], |(_, mv)| mv);
        let (va, vb, vc) = (v(a), v(b), v(c));
        [median(va[0], vb[0], vc[0]), median(va[1], vb[1], vc[1])]
    }

    /// Motion vector of a P_Skip macroblock.
    pub fn predict_skip(&self, mb_x: usize, mb_y: usize) -> Mv {
        let x4 = (mb_x * 4) as i32;
        let y4 = (mb_y * 4) as i32;
        let a = self.get(x4 - 1, y4);
        let b = self.get(x4, y4 - 1);
        match (a, b) {
            (None, _) | (_, None) => [0, 0],
            (Some((0, [0, 0])), _) | (_, Some((0, [0, 0]))) => [0, 0],
            _ => self.predict(mb_x, mb_y, 0, 0, 4, 4),
        }
    }
}

#[inline]
pub fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> MotionField {
        MotionField::new(3, 3)
    }

    #[test]
    fn median_of_three() {
        assert_eq!(median(1, 2, 3), 2);
        assert_eq!(median(3, 1, 2), 2);
        assert_eq!(median(2, 3, 1), 2);
        assert_eq!(median(5, 5, 1), 5);
        assert_eq!(median(-4, 7, -4), -4);
    }

    #[test]
    fn first_macroblock_predicts_zero() {
        let f = field();
        assert_eq!(f.predict(0, 0, 0, 0, 4, 4), [0, 0]);
        assert_eq!(f.predict_skip(0, 0), [0, 0]);
    }

    #[test]
    fn only_left_available_copies_left() {
        let mut f = field();
        f.fill(0, 0, 0, 0, 4, 4, 0, [12, -8]);
        // Top row: B and C (and D) are outside the picture, A stands in.
        assert_eq!(f.predict(1, 0, 0, 0, 4, 4), [12, -8]);
        // Skip vector is zero because B is not available.
        assert_eq!(f.predict_skip(1, 0), [0, 0]);
    }

    #[test]
    fn median_and_single_match_rules() {
        let mut f = field();
        f.fill(0, 0, 0, 0, 4, 4, 0, [4, 4]); // D of MB (1,1)
        f.fill(1, 0, 0, 0, 4, 4, 0, [8, -2]); // B
        f.fill(2, 0, 0, 0, 4, 4, 0, [20, 6]); // C
        f.fill(0, 1, 0, 0, 4, 4, 0, [-6, 10]); // A
        assert_eq!(f.predict(1, 1, 0, 0, 4, 4), [8, 6]);
        // Make B and C intra: A is the only neighbour with the same reference.
        f.fill(1, 0, 0, 0, 4, 4, -1, [0, 0]);
        f.fill(2, 0, 0, 0, 4, 4, -1, [0, 0]);
        assert_eq!(f.predict(1, 1, 0, 0, 4, 4), [-6, 10]);
        // All three intra: median of zero vectors.
        f.fill(0, 1, 0, 0, 4, 4, -1, [0, 0]);
        assert_eq!(f.predict(1, 1, 0, 0, 4, 4), [0, 0]);
        // Two matching: median with a zero vector for the intra one.
        f.fill(0, 1, 0, 0, 4, 4, 0, [-6, 10]);
        f.fill(1, 0, 0, 0, 4, 4, 0, [8, -2]);
        assert_eq!(f.predict(1, 1, 0, 0, 4, 4), [0, 0]);
    }

    #[test]
    fn above_right_falls_back_to_above_left() {
        let mut f = field();
        f.fill(1, 0, 0, 0, 4, 4, 0, [4, 4]); // D for MB (2,1)
        f.fill(2, 0, 0, 0, 4, 4, 0, [8, 8]); // B
        f.fill(1, 1, 0, 0, 4, 4, 0, [40, 40]); // A
                                               // Last column: C is outside the picture, D replaces it.
        assert_eq!(f.predict(2, 1, 0, 0, 4, 4), [8, 8]);
    }

    #[test]
    fn directional_rules_for_16x8_and_8x16() {
        let mut f = field();
        f.fill(0, 0, 0, 0, 4, 4, 0, [4, 4]);
        f.fill(1, 0, 0, 0, 4, 4, 0, [8, -2]);
        f.fill(2, 0, 0, 0, 4, 4, 0, [20, 6]);
        f.fill(0, 1, 0, 0, 4, 4, 0, [-6, 10]);
        // 16x8: top uses B, bottom uses A.
        assert_eq!(f.predict(1, 1, 0, 0, 4, 2), [8, -2]);
        f.fill(1, 1, 0, 0, 4, 2, 0, [100, 100]);
        assert_eq!(f.predict(1, 1, 0, 2, 4, 2), [-6, 10]);
        // 8x16: left uses A, right uses C (above-right macroblock).
        assert_eq!(f.predict(1, 1, 0, 0, 2, 4), [-6, 10]);
        f.fill(1, 1, 0, 0, 2, 4, 0, [100, 100]);
        assert_eq!(f.predict(1, 1, 2, 0, 2, 4), [20, 6]);
        // If the preferred neighbour is intra the median rule applies:
        // left A = (100,100) from the first partition, B = (8,-2), C intra.
        f.fill(2, 0, 0, 0, 4, 4, -1, [0, 0]);
        assert_eq!(f.predict(1, 1, 2, 0, 2, 4), [8, 0]);
    }

    #[test]
    fn sub_partition_availability_follows_decoding_order() {
        let mut f = field();
        // Give every block of MB (1,1) a distinct vector equal to its raster index.
        for by in 0..4 {
            for bx in 0..4 {
                f.fill(1, 1, bx, by, 1, 1, 0, [(by * 4 + bx) as i16, 0]);
            }
        }
        f.fill(0, 1, 0, 0, 4, 4, 0, [50, 0]);
        f.fill(1, 0, 0, 0, 4, 4, 0, [60, 0]);
        f.fill(2, 0, 0, 0, 4, 4, 0, [70, 0]);
        f.fill(0, 0, 0, 0, 4, 4, 0, [80, 0]);
        // 4x4 block (1,1) = blkIdx 3: C would be (2,0) = blkIdx 4, later in
        // decoding order, so D = (0,0) is used: median(A=4, B=1, D=0) = 1.
        assert_eq!(f.predict(1, 1, 1, 1, 1, 1), [1, 0]);
        // 4x4 block (2,1) = blkIdx 6: C = (3,0) = blkIdx 5 is available:
        // median(A=5, B=2, C=3) = 3.
        assert_eq!(f.predict(1, 1, 2, 1, 1, 1), [3, 0]);
        // 8x8 block 3 at (2,2): C = (4,1) is in the next macroblock, so
        // D = (1,1) = 5: median(A=(1,2)=9, B=(2,1)=6, D=5) = 6.
        assert_eq!(f.predict(1, 1, 2, 2, 2, 2), [6, 0]);
        // 8x8 block 2 at (0,2): C = (2,1) = 6 available: A = left MB 50, B = (0,1)=4.
        assert_eq!(f.predict(1, 1, 0, 2, 2, 2), [6, 0]);
        // 8x8 block 1 at (2,0): C is the above-right macroblock (70):
        // median(A=(1,0)=1, B=60, C=70) = 60.
        assert_eq!(f.predict(1, 1, 2, 0, 2, 2), [60, 0]);
    }

    #[test]
    fn skip_vector_rules() {
        let mut f = field();
        f.fill(0, 1, 0, 0, 4, 4, 0, [12, 4]); // A
        f.fill(1, 0, 0, 0, 4, 4, 0, [8, 8]); // B
        f.fill(2, 0, 0, 0, 4, 4, 0, [4, 12]); // C
        assert_eq!(f.predict_skip(1, 1), [8, 8]);
        // A zero vector on the left or above forces a zero skip vector.
        f.fill(0, 1, 0, 0, 4, 4, 0, [0, 0]);
        assert_eq!(f.predict_skip(1, 1), [0, 0]);
        // An intra neighbour (refIdx -1) with zero vector does not.
        f.fill(0, 1, 0, 0, 4, 4, -1, [0, 0]);
        assert_eq!(f.predict_skip(1, 1), [4, 8]);
    }
}
