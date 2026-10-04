//! Constant tables transcribed from ITU-T Rec. H.264: CAVLC code tables
//! (clause 9.2), scan order, coded_block_pattern mapping, quantiser tables,
//! chroma QP mapping, deblocking thresholds and level limits.
//!
//! The VLC tables are written as bit strings, the same way the specification
//! prints them, and converted to (length, value) pairs once at start-up.

use std::sync::OnceLock;

/// A variable-length code: `len` bits holding `bits` (MSB first).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Vlc {
    pub len: u8,
    pub bits: u16,
}

const fn parse(s: &str) -> Vlc {
    let b = s.as_bytes();
    let mut v = 0u16;
    let mut i = 0;
    while i < b.len() {
        v = (v << 1) | (b[i] - b'0') as u16;
        i += 1;
    }
    Vlc {
        len: b.len() as u8,
        bits: v,
    }
}

/// 4x4 zig-zag (frame) scan: scan position -> raster index (y*4+x). Figure 8-8.
pub const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// luma4x4BlkIdx (decoding order) -> block x, y in units of 4 samples. Figure 6-10.
pub const BLK_X: [usize; 16] = [0, 1, 0, 1, 2, 3, 2, 3, 0, 1, 0, 1, 2, 3, 2, 3];
pub const BLK_Y: [usize; 16] = [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3];

/// Inverse of BLK_X/BLK_Y: raster (y*4+x) -> luma4x4BlkIdx.
pub const BLK_IDX: [usize; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

// ---------------------------------------------------------------------------
// coeff_token, Table 9-5. Rows are TotalCoeff 0..=16, columns TrailingOnes 0..=3.
// ---------------------------------------------------------------------------

type CoeffTokenSrc = [[&'static str; 4]; 17];

const CT_NC0: CoeffTokenSrc = [
    ["1", "", "", ""],
    ["000101", "01", "", ""],
    ["00000111", "000100", "001", ""],
    ["000000111", "00000110", "0000101", "00011"],
    ["0000000111", "000000110", "00000101", "000011"],
    ["00000000111", "0000000110", "000000101", "0000100"],
    ["0000000001111", "00000000110", "0000000101", "00000100"],
    ["0000000001011", "0000000001110", "00000000101", "000000100"],
    ["0000000001000", "0000000001010", "0000000001101", "0000000100"],
    ["00000000001111", "00000000001110", "0000000001001", "00000000100"],
    ["00000000001011", "00000000001010", "00000000001101", "0000000001100"],
    ["000000000001111", "000000000001110", "00000000001001", "00000000001100"],
    [
        "000000000001011",
        "000000000001010",
        "000000000001101",
        "00000000001000",
    ],
    [
        "0000000000001111",
        "000000000000001",
        "000000000001001",
        "000000000001100",
    ],
    [
        "0000000000001011",
        "0000000000001110",
        "0000000000001101",
        "000000000001000",
    ],
    [
        "0000000000000111",
        "0000000000001010",
        "0000000000001001",
        "0000000000001100",
    ],
    [
        "0000000000000100",
        "0000000000000110",
        "0000000000000101",
        "0000000000001000",
    ],
];

const CT_NC2: CoeffTokenSrc = [
    ["11", "", "", ""],
    ["001011", "10", "", ""],
    ["000111", "00111", "011", ""],
    ["0000111", "001010", "001001", "0101"],
    ["00000111", "000110", "000101", "0100"],
    ["00000100", "0000110", "0000101", "00110"],
    ["000000111", "00000110", "00000101", "001000"],
    ["00000001111", "000000110", "000000101", "000100"],
    ["00000001011", "00000001110", "00000001101", "0000100"],
    ["000000001111", "00000001010", "00000001001", "000000100"],
    ["000000001011", "000000001110", "000000001101", "00000001100"],
    ["000000001000", "000000001010", "000000001001", "00000001000"],
    ["0000000001111", "0000000001110", "0000000001101", "000000001100"],
    ["0000000001011", "0000000001010", "0000000001001", "0000000001100"],
    ["0000000000111", "00000000001011", "0000000000110", "0000000001000"],
    ["00000000001001", "00000000001000", "00000000001010", "0000000000001"],
    ["00000000000111", "00000000000110", "00000000000101", "00000000000100"],
];

const CT_NC4: CoeffTokenSrc = [
    ["1111", "", "", ""],
    ["001111", "1110", "", ""],
    ["001011", "01111", "1101", ""],
    ["001000", "01100", "01110", "1100"],
    ["0001111", "01010", "01011", "1011"],
    ["0001011", "01000", "01001", "1010"],
    ["0001001", "001110", "001101", "1001"],
    ["0001000", "001010", "001001", "1000"],
    ["00001111", "0001110", "0001101", "01101"],
    ["00001011", "00001110", "0001010", "001100"],
    ["000001111", "00001010", "00001101", "0001100"],
    ["000001011", "000001110", "00001001", "00001100"],
    ["000001000", "000001010", "000001101", "00001000"],
    ["0000001101", "000000111", "000001001", "000001100"],
    ["0000001001", "0000001100", "0000001011", "0000001010"],
    ["0000000101", "0000001000", "0000000111", "0000000110"],
    ["0000000001", "0000000100", "0000000011", "0000000010"],
];

/// coeff_token for chroma DC of 4:2:0 (nC == -1); TotalCoeff 0..=4.
const CT_CHROMA_DC: [[&str; 4]; 5] = [
    ["01", "", "", ""],
    ["000111", "1", "", ""],
    ["000100", "000110", "001", ""],
    ["000011", "0000011", "0000010", "000101"],
    ["000010", "00000011", "00000010", "0000000"],
];

// ---------------------------------------------------------------------------
// total_zeros, Tables 9-7 and 9-8 (4x4 blocks), indexed [TotalCoeff-1][total_zeros].
// ---------------------------------------------------------------------------

const TZ_4X4: [&[&str]; 15] = [
    &[
        "1",
        "011",
        "010",
        "0011",
        "0010",
        "00011",
        "00010",
        "000011",
        "000010",
        "0000011",
        "0000010",
        "00000011",
        "00000010",
        "000000011",
        "000000010",
        "000000001",
    ],
    &[
        "111", "110", "101", "100", "011", "0101", "0100", "0011", "0010", "00011", "00010", "000011", "000010",
        "000001", "000000",
    ],
    &[
        "0101", "111", "110", "101", "0100", "0011", "100", "011", "0010", "00011", "00010", "000001", "00001",
        "000000",
    ],
    &[
        "00011", "111", "0101", "0100", "110", "101", "100", "0011", "011", "0010", "00010", "00001", "00000",
    ],
    &[
        "0101", "0100", "0011", "111", "110", "101", "100", "011", "0010", "00001", "0001", "00000",
    ],
    &[
        "000001", "00001", "111", "110", "101", "100", "011", "010", "0001", "001", "000000",
    ],
    &[
        "000001", "00001", "101", "100", "011", "11", "010", "0001", "001", "000000",
    ],
    &["000001", "0001", "00001", "011", "11", "10", "010", "001", "000000"],
    &["000001", "000000", "0001", "11", "10", "001", "01", "00001"],
    &["00001", "00000", "001", "11", "10", "01", "0001"],
    &["0000", "0001", "001", "010", "1", "011"],
    &["0000", "0001", "01", "1", "001"],
    &["000", "001", "1", "01"],
    &["00", "01", "1"],
    &["0", "1"],
];

/// total_zeros for chroma DC 2x2 blocks, Table 9-9(a), indexed [TotalCoeff-1][total_zeros].
const TZ_CHROMA_DC: [&[&str]; 3] = [&["1", "01", "001", "000"], &["1", "01", "00"], &["1", "0"]];

/// run_before, Table 9-10, indexed [min(zerosLeft,7)-1][run_before].
const RUN_BEFORE: [&[&str]; 7] = [
    &["1", "0"],
    &["1", "01", "00"],
    &["11", "10", "01", "00"],
    &["11", "10", "01", "001", "000"],
    &["11", "10", "011", "010", "001", "000"],
    &["11", "000", "001", "011", "010", "101", "100"],
    &[
        "111",
        "110",
        "101",
        "100",
        "011",
        "010",
        "001",
        "0001",
        "00001",
        "000001",
        "0000001",
        "00000001",
        "000000001",
        "0000000001",
        "00000000001",
    ],
];

/// All CAVLC tables in (length, bits) form.
pub struct CavlcTables {
    /// [table][total_coeff][trailing_ones]; table 0: 0<=nC<2, 1: 2<=nC<4,
    /// 2: 4<=nC<8, 3: nC>=8 (fixed length), 4: chroma DC.
    pub coeff_token: [[[Vlc; 4]; 17]; 5],
    /// [total_coeff-1][total_zeros]
    pub total_zeros: [[Vlc; 16]; 15],
    pub total_zeros_chroma_dc: [[Vlc; 4]; 3],
    /// [min(zeros_left,7)-1][run_before]
    pub run_before: [[Vlc; 15]; 7],
}

pub fn cavlc() -> &'static CavlcTables {
    static T: OnceLock<CavlcTables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = CavlcTables {
            coeff_token: [[[Vlc::default(); 4]; 17]; 5],
            total_zeros: [[Vlc::default(); 16]; 15],
            total_zeros_chroma_dc: [[Vlc::default(); 4]; 3],
            run_before: [[Vlc::default(); 15]; 7],
        };
        for (ti, src) in [CT_NC0, CT_NC2, CT_NC4].iter().enumerate() {
            for tc in 0..17 {
                for t1 in 0..4 {
                    if !src[tc][t1].is_empty() {
                        t.coeff_token[ti][tc][t1] = parse(src[tc][t1]);
                    }
                }
            }
        }
        // nC >= 8: 6-bit fixed length code. "0000 11" means no coefficients,
        // everything else is (TotalCoeff-1) in 4 bits followed by TrailingOnes.
        t.coeff_token[3][0][0] = Vlc { len: 6, bits: 3 };
        for tc in 1..17u16 {
            for t1 in 0..4u16.min(tc + 1) {
                t.coeff_token[3][tc as usize][t1 as usize] = Vlc {
                    len: 6,
                    bits: ((tc - 1) << 2) | t1,
                };
            }
        }
        for tc in 0..5 {
            for t1 in 0..4 {
                if !CT_CHROMA_DC[tc][t1].is_empty() {
                    t.coeff_token[4][tc][t1] = parse(CT_CHROMA_DC[tc][t1]);
                }
            }
        }
        for (i, row) in TZ_4X4.iter().enumerate() {
            for (j, s) in row.iter().enumerate() {
                t.total_zeros[i][j] = parse(s);
            }
        }
        for (i, row) in TZ_CHROMA_DC.iter().enumerate() {
            for (j, s) in row.iter().enumerate() {
                t.total_zeros_chroma_dc[i][j] = parse(s);
            }
        }
        for (i, row) in RUN_BEFORE.iter().enumerate() {
            for (j, s) in row.iter().enumerate() {
                t.run_before[i][j] = parse(s);
            }
        }
        t
    })
}

// ---------------------------------------------------------------------------
// coded_block_pattern mapping, Table 9-4 (ChromaArrayType 1 or 2).
// Indexed by codeNum; value is the coded_block_pattern.
// ---------------------------------------------------------------------------

pub const CBP_FROM_CODE_INTRA: [u8; 48] = [
    47, 31, 15, 0, 23, 27, 29, 30, 7, 11, 13, 14, 39, 43, 45, 46, 16, 3, 5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44, 1,
    2, 4, 8, 17, 18, 20, 24, 6, 9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

pub const CBP_FROM_CODE_INTER: [u8; 48] = [
    0, 16, 1, 2, 4, 8, 32, 3, 5, 10, 12, 15, 47, 7, 11, 13, 14, 6, 9, 31, 35, 37, 42, 44, 33, 34, 36, 40, 39, 43, 45,
    46, 17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// codeNum to transmit for a given coded_block_pattern.
pub fn cbp_code(cbp: u8, intra: bool) -> u32 {
    static T: OnceLock<[[u8; 48]; 2]> = OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [[0u8; 48]; 2];
        for code in 0..48 {
            t[0][CBP_FROM_CODE_INTER[code] as usize] = code as u8;
            t[1][CBP_FROM_CODE_INTRA[code] as usize] = code as u8;
        }
        t
    });
    t[intra as usize][cbp as usize] as u32
}

// ---------------------------------------------------------------------------
// Quantisation.
// ---------------------------------------------------------------------------

/// normAdjust4x4 (clause 8.5.9): rows qP%6, columns position class
/// (0: both indices even, 1: both odd, 2: otherwise).
pub const DEQUANT_V: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// Forward quantiser multipliers matching DEQUANT_V (same class layout).
/// MF * V * (transform norm) ~= 2^21 for class 0; these are the customary values.
pub const QUANT_MF: [[i32; 3]; 6] = [
    [13107, 5243, 8066],
    [11916, 4660, 7490],
    [10082, 4194, 6554],
    [9362, 3647, 5825],
    [8192, 3355, 5243],
    [7282, 2893, 4559],
];

/// Position class of raster index i (y*4+x) in a 4x4 block.
pub const POS_CLASS: [usize; 16] = [0, 2, 0, 2, 2, 1, 2, 1, 0, 2, 0, 2, 2, 1, 2, 1];

/// QPc as a function of qPI, Table 8-15.
pub const CHROMA_QP: [u8; 52] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 29,
    30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
];

/// Chroma QP for a luma QP and chroma_qp_index_offset.
#[inline]
pub fn chroma_qp(qp_y: u8, offset: i8) -> u8 {
    let qpi = (qp_y as i32 + offset as i32).clamp(0, 51);
    CHROMA_QP[qpi as usize]
}

// ---------------------------------------------------------------------------
// Deblocking filter thresholds, Tables 8-16 and 8-17.
// ---------------------------------------------------------------------------

pub const ALPHA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20, 22, 25, 28, 32, 36,
    40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226, 255, 255,
];

pub const BETA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11,
    11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
];

/// tC0 indexed [indexA][bS-1] for bS = 1, 2, 3.
pub const TC0: [[u8; 3]; 52] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 2, 3],
    [1, 2, 3],
    [2, 2, 3],
    [2, 2, 4],
    [2, 3, 4],
    [2, 3, 4],
    [3, 3, 5],
    [3, 4, 6],
    [3, 4, 6],
    [4, 5, 7],
    [4, 5, 8],
    [4, 6, 9],
    [5, 7, 10],
    [6, 8, 11],
    [6, 8, 13],
    [7, 10, 14],
    [8, 11, 16],
    [9, 12, 18],
    [10, 13, 20],
    [11, 15, 23],
    [13, 17, 25],
];

// ---------------------------------------------------------------------------
// Level limits, Table A-1 (subset of columns).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub idc: u8,
    /// Max macroblocks per second.
    pub max_mbps: u32,
    /// Max frame size in macroblocks.
    pub max_fs: u32,
    /// Max video bitrate in kbit/s for Baseline.
    pub max_br: u32,
    /// Vertical MV range is [-max_vmv, max_vmv - 0.25] luma samples.
    pub max_vmv: i32,
    /// Max motion vectors per two consecutive macroblocks (0: unlimited).
    pub max_mvs_per_2mb: u32,
}

pub const LEVELS: [Level; 15] = [
    Level {
        idc: 10,
        max_mbps: 1485,
        max_fs: 99,
        max_br: 64,
        max_vmv: 64,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 11,
        max_mbps: 3000,
        max_fs: 396,
        max_br: 192,
        max_vmv: 128,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 12,
        max_mbps: 6000,
        max_fs: 396,
        max_br: 384,
        max_vmv: 128,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 13,
        max_mbps: 11880,
        max_fs: 396,
        max_br: 768,
        max_vmv: 128,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 20,
        max_mbps: 11880,
        max_fs: 396,
        max_br: 2000,
        max_vmv: 128,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 21,
        max_mbps: 19800,
        max_fs: 792,
        max_br: 4000,
        max_vmv: 256,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 22,
        max_mbps: 20250,
        max_fs: 1620,
        max_br: 4000,
        max_vmv: 256,
        max_mvs_per_2mb: 0,
    },
    Level {
        idc: 30,
        max_mbps: 40500,
        max_fs: 1620,
        max_br: 10000,
        max_vmv: 256,
        max_mvs_per_2mb: 32,
    },
    Level {
        idc: 31,
        max_mbps: 108000,
        max_fs: 3600,
        max_br: 14000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 32,
        max_mbps: 216000,
        max_fs: 5120,
        max_br: 20000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 40,
        max_mbps: 245760,
        max_fs: 8192,
        max_br: 20000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 41,
        max_mbps: 245760,
        max_fs: 8192,
        max_br: 50000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 42,
        max_mbps: 522240,
        max_fs: 8704,
        max_br: 50000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 50,
        max_mbps: 589824,
        max_fs: 22080,
        max_br: 135000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
    Level {
        idc: 51,
        max_mbps: 983040,
        max_fs: 36864,
        max_br: 240000,
        max_vmv: 512,
        max_mvs_per_2mb: 16,
    },
];

/// Lowest level whose frame size, macroblock rate, picture dimensions and
/// (when known) bitrate limits admit the stream.
pub fn pick_level(mb_w: u32, mb_h: u32, fps: f64, kbps: Option<u32>) -> Level {
    let fs = mb_w * mb_h;
    let mbps = (fs as f64 * fps).ceil() as u32;
    for l in LEVELS {
        let dim_limit = ((l.max_fs * 8) as f64).sqrt() as u32;
        if fs <= l.max_fs
            && mbps <= l.max_mbps
            && mb_w <= dim_limit
            && mb_h <= dim_limit
            && kbps.is_none_or(|k| k <= l.max_br)
        {
            return l;
        }
    }
    LEVELS[LEVELS.len() - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that a set of codes is prefix-free and returns the Kraft sum
    /// scaled by 2^16 (65536 means the code is complete).
    fn kraft(codes: &[Vlc]) -> u64 {
        let mut sum = 0u64;
        for (i, a) in codes.iter().enumerate() {
            assert!(a.len > 0 && a.len <= 16);
            sum += 1 << (16 - a.len);
            for b in &codes[i + 1..] {
                let (s, l) = if a.len <= b.len { (a, b) } else { (b, a) };
                assert!(
                    (l.bits >> (l.len - s.len)) != s.bits,
                    "code {:0w1$b} is a prefix of {:0w2$b}",
                    s.bits,
                    l.bits,
                    w1 = s.len as usize,
                    w2 = l.len as usize
                );
            }
        }
        sum
    }

    #[test]
    fn coeff_token_tables_are_prefix_free() {
        let t = cavlc();
        for table in 0..5 {
            let max_tc = if table == 4 { 4 } else { 16 };
            let mut codes = Vec::new();
            for tc in 0..=max_tc {
                for t1 in 0..=tc.min(3) {
                    let v = t.coeff_token[table][tc][t1];
                    assert!(v.len > 0, "missing entry table {table} tc {tc} t1 {t1}");
                    codes.push(v);
                }
            }
            assert_eq!(codes.len(), if table == 4 { 14 } else { 62 });
            let k = kraft(&codes);
            assert!(k <= 65536, "table {table} over-subscribed");
            // In each table only the all-zero code word of the longest length
            // is unused (and "0000 11xx" patterns of the fixed-length table),
            // so the sums sit just below 1; pin them so typos are caught.
            let expect = [65536 - 2, 65536 - 8, 65536 - 64, 62 * 1024, 65536][table];
            assert_eq!(k, expect, "table {table} Kraft sum");
        }
    }

    #[test]
    fn total_zeros_and_run_before_are_complete_codes() {
        let t = cavlc();
        for tc in 1..=15usize {
            let n = 16 - tc + 1;
            // Only tzVlcIndex 1 leaves a code word (nine zeros) unused.
            let expect = if tc == 1 { 65536 - 128 } else { 65536 };
            assert_eq!(kraft(&t.total_zeros[tc - 1][..n]), expect, "total_zeros tc {tc}");
        }
        for tc in 1..=3usize {
            let n = 4 - tc + 1;
            assert_eq!(kraft(&t.total_zeros_chroma_dc[tc - 1][..n]), 65536);
        }
        for zl in 1..=7usize {
            let n = if zl < 7 { zl + 1 } else { 15 };
            let k = kraft(&t.run_before[zl - 1][..n]);
            if zl < 7 {
                assert_eq!(k, 65536, "run_before zerosLeft {zl}");
            } else {
                // Unary tail stops at 11 bits: 2^-11 of code space is unused.
                assert_eq!(k, 65536 - 32);
            }
        }
    }

    // Worked example in the style of the CAVLC description: specific entries
    // spot-checked against Table 9-5.
    #[test]
    fn coeff_token_spot_checks() {
        let t = cavlc();
        assert_eq!(t.coeff_token[0][5][3], parse("0000100"));
        assert_eq!(t.coeff_token[0][0][0], parse("1"));
        assert_eq!(t.coeff_token[1][2][2], parse("011"));
        assert_eq!(t.coeff_token[3][0][0], parse("000011"));
        assert_eq!(t.coeff_token[3][16][3], parse("111111"));
        assert_eq!(t.coeff_token[3][1][0], parse("000000"));
        assert_eq!(t.coeff_token[4][4][3], parse("0000000"));
    }

    #[test]
    fn cbp_tables_are_permutations() {
        for table in [&CBP_FROM_CODE_INTRA, &CBP_FROM_CODE_INTER] {
            let mut seen = [false; 48];
            for &v in table.iter() {
                assert!(!seen[v as usize]);
                seen[v as usize] = true;
            }
        }
        // Most likely patterns get the shortest codes.
        assert_eq!(cbp_code(47, true), 0);
        assert_eq!(cbp_code(0, false), 0);
        assert_eq!(cbp_code(0, true), 3);
        for cbp in 0..48u8 {
            assert_eq!(CBP_FROM_CODE_INTRA[cbp_code(cbp, true) as usize], cbp);
            assert_eq!(CBP_FROM_CODE_INTER[cbp_code(cbp, false) as usize], cbp);
        }
    }

    #[test]
    fn scan_tables_consistent() {
        let mut seen = [false; 16];
        for &z in &ZIGZAG {
            assert!(!seen[z]);
            seen[z] = true;
        }
        for i in 0..16 {
            assert_eq!(BLK_IDX[BLK_Y[i] * 4 + BLK_X[i]], i);
        }
        // Position classes follow index parity.
        for i in 0..16 {
            let (x, y) = (i % 4, i / 4);
            let class = match (x % 2, y % 2) {
                (0, 0) => 0,
                (1, 1) => 1,
                _ => 2,
            };
            assert_eq!(POS_CLASS[i], class);
        }
    }

    #[test]
    fn quant_tables_are_matched_pairs() {
        // MF * V * norm must be close to 2^21 where norm is 4, 2.5 (i.e. 16*... ) per class:
        // class 0: a^2=1/4 -> MF*V = 2^15*4; class 1: b^2/4=1/10; class 2: ab/2.
        for m in 0..6 {
            let p0 = QUANT_MF[m][0] as f64 * DEQUANT_V[m][0] as f64 * 16.0;
            let p1 = QUANT_MF[m][1] as f64 * DEQUANT_V[m][1] as f64 * 25.0;
            let p2 = QUANT_MF[m][2] as f64 * DEQUANT_V[m][2] as f64 * 20.0;
            for p in [p0, p1, p2] {
                assert!((p / (1u64 << 21) as f64 - 1.0).abs() < 0.002, "{m}: {p}");
            }
        }
    }

    #[test]
    fn level_selection() {
        assert_eq!(pick_level(22, 18, 30.0, None).idc, 13); // CIF 30 fps
        assert_eq!(pick_level(11, 9, 15.0, None).idc, 10); // QCIF 15 fps
        assert_eq!(pick_level(80, 45, 30.0, None).idc, 31); // 720p30
        assert_eq!(pick_level(80, 45, 50.0, None).idc, 32); // 720p50
        assert_eq!(pick_level(120, 68, 30.0, None).idc, 40); // 1080p30
        assert_eq!(pick_level(22, 18, 30.0, Some(1500)).idc, 20);
    }
}
