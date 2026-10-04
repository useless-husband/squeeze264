//! squeeze264: an H.264 Constrained Baseline encoder written from the
//! ITU-T H.264 specification. See docs/DESIGN.md for the architecture.

pub mod analysis;
pub mod bitstream;
pub mod cavlc;
pub mod cost;
pub mod deblock;
pub mod encoder;
pub mod frame;
pub mod headers;
pub mod inter;
pub mod intra;
pub mod mb;
pub mod md5;
pub mod me;
pub mod mvpred;
pub mod ratecontrol;
pub mod rng;
pub mod state;
pub mod synth;
pub mod tables;
pub mod transform;
pub mod verify;
pub mod y4m;
