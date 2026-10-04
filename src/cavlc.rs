//! CAVLC residual block coding (clause 9.2). The writer takes one block of
//! levels in scan order; a decoder following the parsing process of the
//! specification is included for round-trip tests only.

use crate::bitstream::BitWriter;
use crate::tables::cavlc;

/// Which coeff_token table an nC value selects (Table 9-5 column).
#[inline]
pub fn token_table(nc: i32) -> usize {
    match nc {
        -1 => 4,
        0..=1 => 0,
        2..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
}

/// Counts how often each coeff_token entry was written, so tests can show
/// that the fuzz corpus reaches the whole table.
#[derive(Clone, Default)]
pub struct Coverage {
    pub coeff_token: [[[u32; 4]; 17]; 5],
}

impl Coverage {
    /// (entries used, entries that exist) over all five tables.
    pub fn summary(&self) -> (usize, usize) {
        let (mut used, mut total) = (0, 0);
        for (t, table) in self.coeff_token.iter().enumerate() {
            let max_tc = if t == 4 { 4 } else { 16 };
            for tc in 0..=max_tc {
                for t1 in 0..=tc.min(3) {
                    total += 1;
                    used += (table[tc][t1] > 0) as usize;
                }
            }
        }
        (used, total)
    }

    pub fn merge(&mut self, other: &Coverage) {
        for t in 0..5 {
            for tc in 0..17 {
                for t1 in 0..4 {
                    self.coeff_token[t][tc][t1] += other.coeff_token[t][tc][t1];
                }
            }
        }
    }
}

/// Writes one residual block. `coeffs` holds the levels in scan order
/// (16 for a full 4x4 block, 15 for an AC block, 4 for chroma DC).
/// Returns TotalCoeff, which the caller stores for neighbour context.
pub fn write_block(w: &mut BitWriter, coeffs: &[i32], nc: i32, cov: &mut Coverage) -> u8 {
    let t = cavlc();
    let max_coeff = coeffs.len();
    // Non-zero levels from highest frequency down, with the run of zeros
    // that precedes each one in scan order.
    let mut levels = [0i32; 16];
    let mut runs = [0u8; 16];
    let mut total = 0usize;
    let mut last = 0usize;
    let mut i = max_coeff;
    let mut pending_zeros = 0u8;
    while i > 0 {
        i -= 1;
        if coeffs[i] != 0 {
            if total == 0 {
                last = i;
            } else {
                runs[total - 1] = pending_zeros;
            }
            levels[total] = coeffs[i];
            total += 1;
            pending_zeros = 0;
        } else if total > 0 {
            pending_zeros += 1;
        }
    }
    let table = token_table(nc);
    if total == 0 {
        let c = t.coeff_token[table][0][0];
        w.put(c.len as u32, c.bits as u32);
        cov.coeff_token[table][0][0] += 1;
        return 0;
    }
    runs[total - 1] = pending_zeros;
    let total_zeros = last + 1 - total;

    // Up to three trailing +/-1 levels are sent as sign bits only.
    let mut t1 = 0;
    while t1 < total.min(3) && levels[t1].abs() == 1 {
        t1 += 1;
    }
    let c = t.coeff_token[table][total][t1];
    debug_assert!(c.len > 0);
    w.put(c.len as u32, c.bits as u32);
    cov.coeff_token[table][total][t1] += 1;
    for &l in &levels[..t1] {
        w.put1(l < 0);
    }

    // Remaining levels with the adaptive suffix length.
    let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };
    for (k, &l) in levels[t1..total].iter().enumerate() {
        let mut code = if l > 0 { 2 * l - 2 } else { -2 * l - 1 } as u32;
        // The first level after fewer than three trailing ones cannot be
        // +/-1, so its code is shifted down by two.
        if k == 0 && t1 < 3 {
            code -= 2;
        }
        if suffix_len == 0 {
            if code < 14 {
                w.put(code + 1, 1);
            } else if code < 30 {
                w.put(15, 1);
                w.put(4, code - 14);
            } else {
                debug_assert!(code - 30 < 4096, "level too large for Baseline CAVLC");
                w.put(16, 1);
                w.put(12, code - 30);
            }
        } else if code < (15 << suffix_len) {
            w.put((code >> suffix_len) + 1, 1);
            w.put(suffix_len, code & ((1 << suffix_len) - 1));
        } else {
            debug_assert!(code - (15 << suffix_len) < 4096, "level too large for Baseline CAVLC");
            w.put(16, 1);
            w.put(12, code - (15 << suffix_len));
        }
        if suffix_len == 0 {
            suffix_len = 1;
        }
        if l.abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
            suffix_len += 1;
        }
    }

    if total < max_coeff {
        let c = if max_coeff == 4 {
            t.total_zeros_chroma_dc[total - 1][total_zeros]
        } else {
            t.total_zeros[total - 1][total_zeros]
        };
        w.put(c.len as u32, c.bits as u32);
    }
    let mut zeros_left = total_zeros;
    for &run in &runs[..total - 1] {
        if zeros_left == 0 {
            break;
        }
        let c = t.run_before[zeros_left.min(7) - 1][run as usize];
        w.put(c.len as u32, c.bits as u32);
        zeros_left -= run as usize;
    }
    total as u8
}

/// Number of bits `write_block` would produce.
pub fn block_bits(coeffs: &[i32], nc: i32) -> u32 {
    let mut w = BitWriter::with_capacity(64);
    let mut cov = Coverage::default();
    write_block(&mut w, coeffs, nc, &mut cov);
    w.bit_len() as u32
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bitstream::BitReader;
    use crate::rng::Rng;
    use crate::tables::Vlc;

    fn read_vlc(r: &mut BitReader, codes: &[Vlc]) -> usize {
        let (mut bits, mut len) = (0u16, 0u8);
        loop {
            bits = (bits << 1) | r.bit() as u16;
            len += 1;
            assert!(len <= 16, "no code word matched");
            if let Some(i) = codes.iter().position(|c| c.len == len && c.bits == bits) {
                return i;
            }
        }
    }

    /// Test-only decoder written from the parsing description of clause
    /// 9.2.1 to 9.2.3. Returns the block in scan order.
    pub fn read_block(r: &mut BitReader, max_coeff: usize, nc: i32) -> Vec<i32> {
        let t = cavlc();
        let table = token_table(nc);
        let flat: Vec<Vlc> = t.coeff_token[table].iter().flatten().copied().collect();
        let idx = read_vlc(r, &flat);
        let (total, t1) = (idx / 4, idx % 4);
        let mut out = vec![0i32; max_coeff];
        if total == 0 {
            return out;
        }
        let mut level_val = vec![0i32; total];
        let mut suffix_len = if total > 10 && t1 < 3 { 1 } else { 0 };
        for i in 0..total {
            if i < t1 {
                level_val[i] = 1 - 2 * r.bit() as i32;
                continue;
            }
            let mut level_prefix = 0u32;
            while r.bit() == 0 {
                level_prefix += 1;
            }
            let mut level_code = (level_prefix.min(15) << suffix_len) as i32;
            let suffix_size = if level_prefix == 14 && suffix_len == 0 {
                4
            } else if level_prefix >= 15 {
                level_prefix - 3
            } else {
                suffix_len
            };
            if suffix_size > 0 {
                level_code += r.bits(suffix_size) as i32;
            }
            if level_prefix >= 15 && suffix_len == 0 {
                level_code += 15;
            }
            if level_prefix >= 16 {
                level_code += (1 << (level_prefix - 3)) - 4096;
            }
            if i == t1 && t1 < 3 {
                level_code += 2;
            }
            level_val[i] = if level_code % 2 == 0 {
                (level_code + 2) >> 1
            } else {
                (-level_code - 1) >> 1
            };
            if suffix_len == 0 {
                suffix_len = 1;
            }
            if level_val[i].abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
                suffix_len += 1;
            }
        }
        let mut zeros_left = if total < max_coeff {
            if max_coeff == 4 {
                read_vlc(r, &t.total_zeros_chroma_dc[total - 1][..4 - total + 1])
            } else {
                read_vlc(r, &t.total_zeros[total - 1][..16 - total + 1])
            }
        } else {
            0
        };
        let mut run_val = vec![0usize; total];
        for run in run_val.iter_mut().take(total - 1) {
            if zeros_left > 0 {
                let zl = zeros_left.min(7);
                let n = if zl < 7 { zl + 1 } else { 15 };
                *run = read_vlc(r, &t.run_before[zl - 1][..n]);
                zeros_left -= *run;
            }
        }
        run_val[total - 1] = zeros_left;
        let mut pos: isize = -1;
        for i in (0..total).rev() {
            pos += run_val[i] as isize + 1;
            out[pos as usize] = level_val[i];
        }
        out
    }

    fn encode(coeffs: &[i32], nc: i32) -> (Vec<u8>, usize) {
        let mut w = BitWriter::new();
        let mut cov = Coverage::default();
        write_block(&mut w, coeffs, nc, &mut cov);
        let n = w.bit_len() as usize;
        w.rbsp_trailing();
        (w.into_bytes(), n)
    }

    fn bit_string(coeffs: &[i32], nc: i32) -> String {
        let (bytes, n) = encode(coeffs, nc);
        (0..n)
            .map(|i| if bytes[i / 8] >> (7 - i % 8) & 1 == 1 { '1' } else { '0' })
            .collect()
    }

    // Example 1 of Iain Richardson's Vcodex white paper "H.264 / AVC Context
    // Adaptive Variable Length Coding" (block 0,3,-1,0 / 0,-1,1,0 /
    // 1,0,0,0 / 0,0,0,0 scanned in zig-zag order with nC = 0).
    #[test]
    fn textbook_example_one() {
        let scan = [0, 3, 0, 1, -1, -1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(bit_string(&scan, 0), "000010001110010111101101");
    }

    // Example 2 of the same white paper: -2,4,3,-3,0,0,-1 followed by zeros, nC = 0:
    // coeff_token(5,1) 0000000110, sign 1, levels 0001 | 1 0 (suffix) ...
    #[test]
    fn textbook_example_two() {
        let scan = [-2, 4, 3, -3, 0, 0, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(bit_string(&scan, 0), "000000011010001001000010111001100");
    }

    #[test]
    fn empty_blocks() {
        assert_eq!(bit_string(&[0; 16], 0), "1");
        assert_eq!(bit_string(&[0; 16], 2), "11");
        assert_eq!(bit_string(&[0; 15], 5), "1111");
        assert_eq!(bit_string(&[0; 16], 9), "000011");
        assert_eq!(bit_string(&[0; 4], -1), "01");
    }

    #[test]
    fn single_coefficient_cases() {
        // One trailing one at DC: coeff_token(1,1)="01", sign, total_zeros 0 = "1".
        assert_eq!(bit_string(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0), "0101");
        assert_eq!(
            bit_string(&[-1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0),
            "0111"
        );
        // Chroma DC with a single +1 in the last position: "1", sign 0, total_zeros 3 = "000".
        assert_eq!(bit_string(&[0, 0, 0, 1], -1), "10000");
    }

    #[test]
    fn escape_levels_round_trip() {
        for &big in &[14, 15, 16, 17, 30, 31, 100, 1000, 2047, -15, -16, -2047] {
            for nc in [0, 2, 4, 8] {
                let mut c = [0i32; 16];
                c[0] = big;
                c[3] = -big;
                c[7] = 1;
                let (bytes, _) = encode(&c, nc);
                let got = read_block(&mut BitReader::new(&bytes), 16, nc);
                assert_eq!(got, c, "level {big} nc {nc}");
            }
        }
    }

    #[test]
    fn random_blocks_round_trip() {
        let mut rng = Rng::new(31);
        let mut cov = Coverage::default();
        for case in 0..60000 {
            let (max_coeff, nc) = match case % 6 {
                0 => (16, rng.range(0, 1)),
                1 => (16, rng.range(2, 3)),
                2 => (16, rng.range(4, 7)),
                3 => (16, rng.range(8, 16)),
                4 => (4, -1),
                _ => (15, rng.range(0, 16)),
            };
            // Vary density and magnitude so every table region is reached.
            let density = rng.range(0, 16) as u32;
            let mag = [1, 1, 2, 4, 20, 300, 2047][rng.below(7) as usize];
            let c: Vec<i32> = (0..max_coeff)
                .map(|_| {
                    if rng.below(16) < density {
                        let v = rng.range(1, mag);
                        if rng.chance(1, 2) {
                            -v
                        } else {
                            v
                        }
                    } else {
                        0
                    }
                })
                .collect();
            let mut w = BitWriter::new();
            let total = write_block(&mut w, &c, nc, &mut cov);
            assert_eq!(total as usize, c.iter().filter(|&&v| v != 0).count());
            let n = w.bit_len() as usize;
            w.rbsp_trailing();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            let got = read_block(&mut r, max_coeff, nc);
            assert_eq!(got, c, "case {case} nc {nc}");
            assert_eq!(r.bit_pos(), n, "decoder consumed a different number of bits");
            assert_eq!(block_bits(&c, nc) as usize, n);
        }
        let (used, total) = cov.summary();
        assert_eq!(used, total, "random corpus must reach every coeff_token entry");
    }
}
