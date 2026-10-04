//! squeeze264: an H.264 Constrained Baseline encoder written from the
//! ITU-T H.264 specification. See docs/DESIGN.md for the architecture.

pub mod bitstream;
pub mod cavlc;
pub mod frame;
pub mod inter;
pub mod intra;
pub mod md5;
pub mod mvpred;
pub mod rng;
pub mod tables;
pub mod transform;
pub mod y4m;
