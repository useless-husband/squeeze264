//! Per-picture coding state shared by the macroblock coder, the entropy
//! coder (neighbour contexts) and the deblocking filter.

use crate::mvpred::MotionField;

pub const MB_I4X4: u8 = 0;
pub const MB_I16X16: u8 = 1;
pub const MB_INTER: u8 = 2;
pub const MB_SKIP: u8 = 3;

pub struct PicState {
    pub mb_w: usize,
    pub mb_h: usize,
    /// Macroblock kind (MB_*), one per macroblock.
    pub kind: Vec<u8>,
    /// QP_Y in effect for each macroblock (what the decoder derives).
    pub qp: Vec<u8>,
    pub motion: MotionField,
    /// TotalCoeff of each luma 4x4 block (stride mb_w*4).
    pub nnz_y: Vec<u8>,
    /// TotalCoeff of each chroma AC 4x4 block per plane (stride mb_w*2).
    pub nnz_c: [Vec<u8>; 2],
    /// Intra4x4PredMode of each 4x4 block; 2 (DC) for blocks of
    /// macroblocks that are not Intra4x4 (stride mb_w*4).
    pub i4mode: Vec<u8>,
}

impl PicState {
    pub fn new(mb_w: usize, mb_h: usize) -> Self {
        let n = mb_w * mb_h;
        PicState {
            mb_w,
            mb_h,
            kind: vec![0; n],
            qp: vec![0; n],
            motion: MotionField::new(mb_w, mb_h),
            nnz_y: vec![0; n * 16],
            nnz_c: [vec![0; n * 4], vec![0; n * 4]],
            i4mode: vec![2; n * 16],
        }
    }

    #[inline]
    pub fn is_intra(&self, mb: usize) -> bool {
        self.kind[mb] <= MB_I16X16
    }
}
