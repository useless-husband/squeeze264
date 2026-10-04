//! Rate control: constant QP, or a simple single-pass average-bitrate
//! controller that picks one QP per frame from a running complexity
//! estimate (bits x quantiser scale) and a bit-budget error term.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RcMode {
    /// P frames use `qp`, I frames `qp - i_offset`.
    ConstQp { qp: u8, i_offset: u8 },
    /// Target average bitrate in bits per second.
    Abr { bps: f64 },
}

pub struct RateControl {
    mode: RcMode,
    fps: f64,
    keyint: usize,
    pixels: f64,
    // ABR state
    total_bits: f64,
    total_target: f64,
    cplx_p: Option<f64>,
    /// Ratio of I-frame to P-frame cost at equal quantiser, learnt online.
    i_ratio: f64,
    last_p_qp: Option<f64>,
    last_i_qp: f64,
    pending_target: f64,
}

/// Quantiser scale proportional to the quantiser step size.
fn qscale(qp: f64) -> f64 {
    0.85 * 2f64.powf((qp - 12.0) / 6.0)
}

fn qp_of(qscale: f64) -> f64 {
    12.0 + 6.0 * (qscale / 0.85).log2()
}

impl RateControl {
    pub fn new(mode: RcMode, fps: f64, keyint: usize, width: usize, height: usize) -> Self {
        RateControl {
            mode,
            fps,
            keyint: keyint.max(1),
            pixels: (width * height) as f64,
            total_bits: 0.0,
            total_target: 0.0,
            cplx_p: None,
            i_ratio: 6.0,
            last_p_qp: None,
            last_i_qp: 26.0,
            pending_target: 0.0,
        }
    }

    /// Per-frame budgets for P and I frames so that a whole GOP averages
    /// the requested bitrate given the current I/P cost ratio.
    fn budgets(&self, bps: f64) -> (f64, f64) {
        let per_frame = bps / self.fps;
        let n = self.keyint as f64;
        let p = per_frame * n / (n - 1.0 + self.i_ratio);
        (p, p * self.i_ratio)
    }

    /// QP for the next frame.
    pub fn frame_qp(&mut self, idr: bool) -> u8 {
        match self.mode {
            RcMode::ConstQp { qp, i_offset } => {
                if idr {
                    qp.saturating_sub(i_offset)
                } else {
                    qp
                }
            }
            RcMode::Abr { bps } => {
                let (p_budget, i_budget) = self.budgets(bps);
                let budget = if idr { i_budget } else { p_budget };
                self.pending_target = budget;
                // Pay back (or spend) the accumulated error over about two seconds.
                let error = self.total_bits - self.total_target;
                let target = (budget - error / (2.0 * self.fps)).clamp(budget * 0.25, budget * 3.0);
                let qp = if idr {
                    match self.last_p_qp {
                        // Keyframes a little finer than the P frames around them.
                        Some(p) => p - 3.0,
                        // First frame: guess from bits per pixel.
                        None => {
                            let bpp = bps / self.fps / self.pixels;
                            32.0 - 6.0 * (bpp / 0.1).log2()
                        }
                    }
                } else {
                    let q = match (self.cplx_p, self.last_p_qp) {
                        (Some(c), Some(last)) => qp_of(c / target).clamp(last - 3.0, last + 3.0),
                        _ => self.last_i_qp + 3.0,
                    };
                    self.last_p_qp = Some(q.clamp(10.0, 51.0));
                    q
                };
                let qp = qp.clamp(10.0, 51.0);
                if idr {
                    self.last_i_qp = qp;
                }
                qp.round() as u8
            }
        }
    }

    /// Feeds back the size of the frame just coded.
    pub fn update(&mut self, idr: bool, qp: u8, bits: usize) {
        if let RcMode::Abr { .. } = self.mode {
            self.total_bits += bits as f64;
            self.total_target += self.pending_target;
            let cplx = bits as f64 * qscale(qp as f64);
            if idr {
                if let Some(cp) = self.cplx_p {
                    let ratio = (cplx / cp).clamp(1.5, 30.0);
                    self.i_ratio = 0.5 * self.i_ratio + 0.5 * ratio;
                }
            } else {
                self.cplx_p = Some(match self.cplx_p {
                    Some(c) => 0.6 * c + 0.4 * cplx,
                    None => cplx,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_qp_mode() {
        let mut rc = RateControl::new(RcMode::ConstQp { qp: 28, i_offset: 3 }, 30.0, 250, 352, 288);
        assert_eq!(rc.frame_qp(true), 25);
        rc.update(true, 25, 100_000);
        assert_eq!(rc.frame_qp(false), 28);
        let mut rc = RateControl::new(RcMode::ConstQp { qp: 1, i_offset: 3 }, 30.0, 250, 352, 288);
        assert_eq!(rc.frame_qp(true), 0);
    }

    #[test]
    fn qscale_round_trip() {
        for qp in [10.0, 23.5, 40.0] {
            assert!((qp_of(qscale(qp)) - qp).abs() < 1e-9);
        }
        assert!((qscale(18.0) / qscale(12.0) - 2.0).abs() < 1e-9);
    }

    /// Closed-loop check against a synthetic encoder whose frame size is
    /// complexity / qscale: the controller must converge on the target.
    #[test]
    fn abr_converges_on_a_model_encoder() {
        for &(bps, cplx) in &[(300_000.0, 40_000.0), (1_000_000.0, 40_000.0), (500_000.0, 200_000.0)] {
            let mut rc = RateControl::new(RcMode::Abr { bps }, 30.0, 60, 352, 288);
            let mut total = 0usize;
            let frames = 600;
            for i in 0..frames {
                let idr = i % 60 == 0;
                let qp = rc.frame_qp(idr);
                assert!((10..=51).contains(&qp));
                let c = if idr { cplx * 8.0 } else { cplx };
                let bits = (c / qscale(qp as f64)) as usize;
                rc.update(idr, qp, bits);
                total += bits;
            }
            let achieved = total as f64 * 30.0 / frames as f64;
            let err = (achieved / bps - 1.0).abs();
            assert!(err < 0.08, "target {bps}, achieved {achieved:.0} ({:.1}% off)", err * 100.0);
        }
    }

    #[test]
    fn abr_reacts_to_a_complexity_jump() {
        let bps = 400_000.0;
        let mut rc = RateControl::new(RcMode::Abr { bps }, 25.0, 250, 352, 288);
        let mut qps = Vec::new();
        for i in 0..200 {
            let idr = i == 0;
            let qp = rc.frame_qp(idr);
            let cplx = if i < 100 { 30_000.0 } else { 120_000.0 };
            rc.update(idr, qp, (cplx * if idr { 6.0 } else { 1.0 } / qscale(qp as f64)) as usize);
            qps.push(qp);
        }
        // Four times the complexity needs about 12 QP steps more.
        let before = qps[90] as i32;
        let after = qps[190] as i32;
        assert!((after - before - 12).abs() <= 3, "qp went {before} -> {after}");
    }
}
