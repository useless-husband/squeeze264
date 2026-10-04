//! Mode decision: which macroblock type, prediction modes, partitions and
//! motion vectors to use. Everything here is the encoder's free choice; any
//! outcome is a valid stream. The fuzzing mode replaces the decisions with
//! random (but legal) ones to exercise the syntax and reconstruction paths.

use crate::cost::{lambda16, satd};
use crate::inter::Mv;
use crate::intra;
use crate::mb::*;
use crate::me::Me;

/// Rough header cost in bits of each macroblock type, used to compare modes.
const BITS_I16_HEADER: u32 = 7;
const BITS_I4_HEADER: u32 = 3;
const BITS_INTRA_IN_P: u32 = 5;

/// An inter candidate under evaluation.
#[derive(Clone)]
struct InterChoice {
    part: u8,
    sub: [u8; 4],
    mvs: [Mv; 16],
    cost: u32,
}

impl MbCoder<'_> {
    #[inline]
    fn bit_cost(lambda16: u32, bits: u32) -> u32 {
        (lambda16 * bits + 8) >> 4
    }

    /// Best Intra16x16 mode by SATD.
    fn best_i16(&self, lam: u32) -> (u8, u32) {
        let e = self.edge16();
        let mut pred = [0u8; 256];
        let (mut best, mut best_cost) = (intra::I16_DC, u32::MAX);
        for mode in 0..4u8 {
            if !intra::i16_mode_allowed(mode, e.has_top, e.has_left) {
                continue;
            }
            intra::pred16x16(mode, &e, &mut pred);
            let cost = satd(&self.cur_y, 16, &pred, 16, 16, 16) + Self::bit_cost(lam, BITS_I16_HEADER);
            if cost < best_cost {
                best_cost = cost;
                best = mode;
            }
        }
        (best, best_cost)
    }

    /// Best intra chroma mode by SATD over both planes.
    fn best_chroma_mode(&self, lam: u32) -> u8 {
        let edges = [self.edge8(0), self.edge8(1)];
        let (mut best, mut best_cost) = (intra::CHROMA_DC, u32::MAX);
        let mut pred = [0u8; 64];
        for mode in 0..4u8 {
            if !intra::chroma_mode_allowed(mode, edges[0].has_top, edges[0].has_left) {
                continue;
            }
            // intra_chroma_pred_mode is ue(v): 1, 3, 3, 5 bits.
            let mut cost = Self::bit_cost(lam, [1, 3, 3, 5][mode as usize]);
            for plane in 0..2 {
                intra::pred_chroma(mode, &edges[plane], &mut pred);
                cost += satd(&self.cur_c[plane], 8, &pred, 8, 8, 8);
            }
            if cost < best_cost {
                best_cost = cost;
                best = mode;
            }
        }
        best
    }

    /// Codes the chroma of an intra macroblock with the best chroma mode.
    fn finish_intra(&mut self, mode: MbMode, mut res: Residual, qp: u8, lam: u32) -> Coded {
        let chroma_mode = self.best_chroma_mode(lam);
        let mut cpred = [[0u8; 64]; 2];
        self.intra_chroma_pred(chroma_mode, &mut cpred);
        self.encode_chroma(&cpred, qp, true, Force::default(), &mut res);
        Coded {
            mode,
            chroma_mode,
            res,
            qp,
        }
    }

    /// Picks between Intra16x16 and Intra4x4 and codes the luma.
    /// `limit` lets the caller stop early when intra cannot win.
    fn analyse_intra(&mut self, qp: u8, lam: u32, extra_bits: u32, limit: u32) -> Option<(MbMode, Residual, u32)> {
        let extra = Self::bit_cost(lam, extra_bits);
        let (i16_mode, i16_cost) = self.best_i16(lam);
        let i16_cost = i16_cost + extra;
        // Intra4x4 is only worth trying when 16x16 is within reach of the limit.
        if i16_cost > limit.saturating_add(limit / 4) {
            return None;
        }
        let mut res4 = Residual::default();
        let (modes, cost4) = self.encode_i4(None, qp, Force::default(), lam, &mut res4);
        let i4_cost = cost4 + Self::bit_cost(lam, BITS_I4_HEADER) + extra;
        if i4_cost < i16_cost {
            if i4_cost >= limit {
                return None;
            }
            Some((MbMode::I4 { modes }, res4, i4_cost))
        } else {
            if i16_cost >= limit {
                return None;
            }
            let mut res = Residual::default();
            self.encode_i16(i16_mode, qp, Force::default(), &mut res);
            Some((MbMode::I16 { mode: i16_mode }, res, i16_cost))
        }
    }

    /// Mode decision for a macroblock of an I slice.
    pub fn analyse_i(&mut self, qp: u8) -> Coded {
        let lam = lambda16(qp);
        let (mode, res, _) = self.analyse_intra(qp, lam, 0, u32::MAX).expect("intra always possible");
        self.finish_intra(mode, res, qp, lam)
    }

    /// Mode decision for a macroblock of a P slice.
    pub fn analyse_p(&mut self, qp: u8) -> Coded {
        let lam = lambda16(qp);
        let (mb_x, mb_y) = (self.mb_x, self.mb_y);
        let skip_mv = self.st.motion.predict_skip(mb_x, mb_y);
        let skip_ok = self.mv_in_range(skip_mv);

        // Early skip: if the skip prediction leaves no coefficients, take it.
        if skip_ok {
            let res = self.encode_inter(PART_16X16, &[0; 4], &[skip_mv; 16], qp, Force::default());
            if res.cbp == 0 {
                return Coded {
                    mode: MbMode::Skip { mv: skip_mv },
                    chroma_mode: 0,
                    res,
                    qp,
                };
            }
        }

        let cur = self.cur_y;
        let me = Me {
            cur: &cur,
            refp: self.refp,
            x0: mb_x as i32 * 16,
            y0: mb_y as i32 * 16,
            mv_min: self.mv_min,
            mv_max: self.mv_max,
            lambda16: lam,
            range: self.cfg.me_range,
            subpel: self.cfg.subpel,
        };

        // 16x16: candidates from the spatial neighbours and the previous picture.
        let pmv16 = self.st.motion.predict(mb_x, mb_y, 0, 0, 4, 4);
        let w4 = self.st.mb_w * 4;
        let mut cands: Vec<Mv> = vec![[0, 0], skip_mv, self.prev_mvs[self.mb_index()]];
        if mb_x > 0 {
            cands.push(self.st.motion.mv[mb_y * 4 * w4 + mb_x * 4 - 1]);
        }
        if mb_y > 0 {
            cands.push(self.st.motion.mv[(mb_y * 4 - 1) * w4 + mb_x * 4]);
            if mb_x + 1 < self.st.mb_w {
                cands.push(self.st.motion.mv[(mb_y * 4 - 1) * w4 + mb_x * 4 + 4]);
            }
        }
        let (mv16, c16) = me.search(0, 0, 4, 4, pmv16, &cands);
        let mut best = InterChoice {
            part: PART_16X16,
            sub: [0; 4],
            mvs: [mv16; 16],
            cost: c16 + Self::bit_cost(lam, 1),
        };

        // Smaller partitions are only tried when 16x16 leaves a real residual.
        if self.cfg.partitions && c16 > 256 + Self::bit_cost(lam, 16) {
            // 8x8 (with optional sub-partitions), in decoding order so that
            // each prediction sees the vectors chosen before it.
            let mut c8 = InterChoice {
                part: PART_8X8,
                sub: [0; 4],
                mvs: [[0, 0]; 16],
                cost: Self::bit_cost(lam, 5),
            };
            let mut mv8 = [[0i16; 2]; 4];
            for q in 0..4 {
                let (qx, qy) = ((q % 2) * 2, (q / 2) * 2);
                let pmv = self.st.motion.predict(mb_x, mb_y, qx, qy, 2, 2);
                let (mv, cost) = me.search(qx, qy, 2, 2, pmv, &[mv16]);
                mv8[q] = mv;
                let mut q_cost = cost + Self::bit_cost(lam, 1);
                let mut q_sub = SUB_8X8;
                let mut q_mvs = [(Part { bx: qx, by: qy, pw: 2, ph: 2 }, mv); 4];
                let mut q_n = 1;
                if self.sub8x8_ok && self.cfg.sub8x8 && cost > 64 + Self::bit_cost(lam, 12) {
                    for sub in [SUB_8X4, SUB_4X8, SUB_4X4] {
                        let mut parts = [Part::default(); 16];
                        let mut n = 0;
                        sub_partitions(sub, qx, qy, &mut parts, &mut n);
                        let mut total = Self::bit_cost(lam, if sub == SUB_4X4 { 5 } else { 3 });
                        let mut trial = [(Part::default(), [0i16; 2]); 4];
                        for (i, p) in parts[..n].iter().enumerate() {
                            let pmv = self.st.motion.predict(mb_x, mb_y, p.bx, p.by, p.pw, p.ph);
                            let (smv, scost) = me.search(p.bx, p.by, p.pw, p.ph, pmv, &[mv]);
                            self.st.motion.fill(mb_x, mb_y, p.bx, p.by, p.pw, p.ph, 0, smv);
                            trial[i] = (*p, smv);
                            total += scost;
                        }
                        if total < q_cost {
                            q_cost = total;
                            q_sub = sub;
                            q_mvs = trial;
                            q_n = n;
                        }
                    }
                }
                c8.sub[q] = q_sub;
                c8.cost += q_cost;
                for (p, pmv) in &q_mvs[..q_n] {
                    self.st.motion.fill(mb_x, mb_y, p.bx, p.by, p.pw, p.ph, 0, *pmv);
                    for y in p.by..p.by + p.ph {
                        for x in p.bx..p.bx + p.pw {
                            c8.mvs[y * 4 + x] = *pmv;
                        }
                    }
                }
            }
            if c8.cost < best.cost {
                best = c8;
            }

            // 16x8 and 8x16, seeded with the 8x8 results.
            for part in [PART_16X8, PART_8X16] {
                let mut c = InterChoice {
                    part,
                    sub: [0; 4],
                    mvs: [[0, 0]; 16],
                    cost: Self::bit_cost(lam, 3),
                };
                for i in 0..2 {
                    let (p, seeds) = if part == PART_16X8 {
                        (Part { bx: 0, by: i * 2, pw: 4, ph: 2 }, [mv8[i * 2], mv8[i * 2 + 1], mv16])
                    } else {
                        (Part { bx: i * 2, by: 0, pw: 2, ph: 4 }, [mv8[i], mv8[i + 2], mv16])
                    };
                    let pmv = self.st.motion.predict(mb_x, mb_y, p.bx, p.by, p.pw, p.ph);
                    let (mv, cost) = me.search(p.bx, p.by, p.pw, p.ph, pmv, &seeds);
                    self.st.motion.fill(mb_x, mb_y, p.bx, p.by, p.pw, p.ph, 0, mv);
                    c.cost += cost;
                    for y in p.by..p.by + p.ph {
                        for x in p.bx..p.bx + p.pw {
                            c.mvs[y * 4 + x] = mv;
                        }
                    }
                }
                if c.cost < best.cost {
                    best = c;
                }
            }
        }

        // Intra in a P slice, for occlusions and scene changes.
        if self.cfg.intra_in_p {
            if let Some((mode, res, _)) = self.analyse_intra(qp, lam, BITS_INTRA_IN_P, best.cost) {
                return self.finish_intra(mode, res, qp, lam);
            }
        }

        let res = self.encode_inter(best.part, &best.sub, &best.mvs, qp, Force::default());
        if best.part == PART_16X16 && skip_ok && best.mvs[0] == skip_mv && res.cbp == 0 {
            return Coded {
                mode: MbMode::Skip { mv: skip_mv },
                chroma_mode: 0,
                res,
                qp,
            };
        }
        Coded {
            mode: MbMode::Inter {
                part: best.part,
                sub: best.sub,
                mvs: best.mvs,
            },
            chroma_mode: 0,
            res,
            qp,
        }
    }

    /// Fuzzing: a random legal macroblock. Exercises every macroblock type,
    /// partition shape, prediction mode, coded_block_pattern and QP change
    /// regardless of what the content would favour.
    pub fn random_mb(&mut self, qp_lo: u8, qp_hi: u8) -> Coded {
        let (mb_x, mb_y) = (self.mb_x, self.mb_y);
        let is_p = self.is_p;
        let sub_ok = self.sub8x8_ok;
        let (mv_min, mv_max) = (self.mv_min, self.mv_max);
        let prev_qp = self.prev_qp as i32;
        let skip_mv = self.st.motion.predict_skip(mb_x, mb_y);
        let skip_ok = is_p && self.mv_in_range(skip_mv);
        let rng = self.rng.take().expect("fuzz mode needs an rng");

        // QP: mostly unchanged, sometimes a small step, rarely a large jump;
        // mb_qp_delta must stay within -26..=25.
        let mut qp = prev_qp.clamp(qp_lo as i32, qp_hi as i32);
        if rng.chance(1, 3) {
            let step = if rng.chance(1, 8) { rng.range(-26, 25) } else { rng.range(-5, 5) };
            qp = (prev_qp + step).clamp(qp_lo as i32, qp_hi as i32);
        }
        if !(-26..=25).contains(&(qp - prev_qp)) {
            qp = prev_qp;
        }
        let qp = qp as u8;
        let lam = lambda16(qp);

        let mut force = Force::default();
        if rng.chance(1, 3) {
            force.zero_luma8 = rng.below(16) as u8;
        }
        if rng.chance(1, 5) {
            force.zero_chroma_ac = true;
        }
        if rng.chance(1, 8) {
            force.zero_chroma = true;
        }

        let roll = rng.below(100);
        let coded = if is_p && roll < 12 && skip_ok {
            let all_zero = Force {
                zero_luma8: 15,
                zero_chroma_ac: true,
                zero_chroma: true,
            };
            let res = self.encode_inter(PART_16X16, &[0; 4], &[skip_mv; 16], qp, all_zero);
            Coded {
                mode: MbMode::Skip { mv: skip_mv },
                chroma_mode: 0,
                res,
                qp,
            }
        } else if is_p && roll < 65 {
            let part = rng.below(4) as u8;
            let sub: [u8; 4] = std::array::from_fn(|_| if sub_ok { rng.below(4) as u8 } else { 0 });
            let mut parts = [Part::default(); 16];
            let n = partitions(part, &sub, &mut parts);
            let mut mvs = [[0i16; 2]; 16];
            for p in &parts[..n] {
                let pmv = self.st.motion.predict(mb_x, mb_y, p.bx, p.by, p.pw, p.ph);
                let mut mv = match rng.below(8) {
                    0 => [0, 0],
                    1 => pmv,
                    2 | 3 => [
                        rng.range(mv_min[0], mv_max[0]) as i16,
                        rng.range(mv_min[1], mv_max[1]) as i16,
                    ],
                    _ => [
                        (pmv[0] as i32 + rng.range(-9, 9)) as i16,
                        (pmv[1] as i32 + rng.range(-9, 9)) as i16,
                    ],
                };
                mv[0] = (mv[0] as i32).clamp(mv_min[0], mv_max[0]) as i16;
                mv[1] = (mv[1] as i32).clamp(mv_min[1], mv_max[1]) as i16;
                self.st.motion.fill(mb_x, mb_y, p.bx, p.by, p.pw, p.ph, 0, mv);
                for y in p.by..p.by + p.ph {
                    for x in p.bx..p.bx + p.pw {
                        mvs[y * 4 + x] = mv;
                    }
                }
            }
            let res = self.encode_inter(part, &sub, &mvs, qp, force);
            Coded {
                mode: MbMode::Inter { part, sub, mvs },
                chroma_mode: 0,
                res,
                qp,
            }
        } else {
            let (has_top, has_left) = (mb_y > 0, mb_x > 0);
            let mut res = Residual::default();
            let mode = if rng.chance(1, 2) {
                let allowed: Vec<u8> = (0..4).filter(|&m| intra::i16_mode_allowed(m, has_top, has_left)).collect();
                let mode = allowed[rng.below(allowed.len() as u32) as usize];
                self.encode_i16(mode, qp, force, &mut res);
                MbMode::I16 { mode }
            } else {
                let modes: [u8; 16] = std::array::from_fn(|blk| {
                    let top = mb_y > 0 || crate::tables::BLK_Y[blk] > 0;
                    let left = mb_x > 0 || crate::tables::BLK_X[blk] > 0;
                    loop {
                        let m = rng.below(9) as u8;
                        if intra::i4_mode_allowed(m, top, left) {
                            return m;
                        }
                    }
                });
                self.encode_i4(Some(&modes), qp, force, lam, &mut res);
                MbMode::I4 { modes }
            };
            let allowed: Vec<u8> = (0..4).filter(|&m| intra::chroma_mode_allowed(m, has_top, has_left)).collect();
            let chroma_mode = allowed[rng.below(allowed.len() as u32) as usize];
            let mut cpred = [[0u8; 64]; 2];
            self.intra_chroma_pred(chroma_mode, &mut cpred);
            self.encode_chroma(&cpred, qp, true, force, &mut res);
            Coded {
                mode,
                chroma_mode,
                res,
                qp,
            }
        };
        self.rng = Some(rng);
        coded
    }
}
