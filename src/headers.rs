//! Sequence and picture parameter sets and slice headers
//! (clauses 7.3.2.1, 7.3.2.2, 7.3.3 and Annex E for the VUI).

use crate::bitstream::{BitWriter, Nal, NalType};

/// log2_max_frame_num: frame_num is sent in this many bits.
pub const LOG2_MAX_FRAME_NUM: u32 = 8;

#[derive(Clone, Debug)]
pub struct StreamParams {
    /// Visible picture size in luma samples.
    pub width: usize,
    pub height: usize,
    pub mb_w: usize,
    pub mb_h: usize,
    pub fps_num: u32,
    pub fps_den: u32,
    pub level_idc: u8,
    /// QP the PPS advertises; slices send a delta against it.
    pub init_qp: u8,
    pub chroma_qp_offset: i8,
    /// Largest |mv| component in quarter samples the encoder will produce.
    pub max_mv_x: u32,
    pub max_mv_y: u32,
}

fn log2_ceil(v: u32) -> u32 {
    32 - v.leading_zeros()
}

pub fn write_sps(p: &StreamParams) -> Nal {
    let mut w = BitWriter::new();
    w.put(8, 66); // profile_idc: Baseline
    w.put1(true); // constraint_set0_flag: obeys Baseline
    w.put1(true); // constraint_set1_flag: also obeys Main -> Constrained Baseline
    w.put(6, 0); // constraint_set2..5 and reserved_zero_2bits
    w.put(8, p.level_idc as u32);
    w.ue(0); // seq_parameter_set_id
    w.ue(LOG2_MAX_FRAME_NUM - 4); // log2_max_frame_num_minus4
    w.ue(2); // pic_order_cnt_type: output order equals decoding order
    w.ue(1); // max_num_ref_frames
    w.put1(false); // gaps_in_frame_num_value_allowed_flag
    w.ue(p.mb_w as u32 - 1); // pic_width_in_mbs_minus1
    w.ue(p.mb_h as u32 - 1); // pic_height_in_map_units_minus1
    w.put1(true); // frame_mbs_only_flag
    w.put1(true); // direct_8x8_inference_flag
    let crop_r = p.mb_w * 16 - p.width;
    let crop_b = p.mb_h * 16 - p.height;
    if crop_r > 0 || crop_b > 0 {
        w.put1(true); // frame_cropping_flag
                      // Crop units are two luma samples for 4:2:0 frames.
        w.ue(0);
        w.ue((crop_r / 2) as u32);
        w.ue(0);
        w.ue((crop_b / 2) as u32);
    } else {
        w.put1(false);
    }
    w.put1(true); // vui_parameters_present_flag
                  // --- VUI ---
    w.put1(false); // aspect_ratio_info_present_flag
    w.put1(false); // overscan_info_present_flag
    w.put1(false); // video_signal_type_present_flag
    w.put1(false); // chroma_loc_info_present_flag
    w.put1(true); // timing_info_present_flag
    w.put(32, p.fps_den); // num_units_in_tick
    w.put(32, p.fps_num * 2); // time_scale (two ticks per frame)
    w.put1(true); // fixed_frame_rate_flag
    w.put1(false); // nal_hrd_parameters_present_flag
    w.put1(false); // vcl_hrd_parameters_present_flag
    w.put1(false); // pic_struct_present_flag
    w.put1(true); // bitstream_restriction_flag
    w.put1(true); // motion_vectors_over_pic_boundaries_flag
    w.ue(0); // max_bytes_per_pic_denom: unlimited
    w.ue(0); // max_bits_per_mb_denom: unlimited
    w.ue(log2_ceil(p.max_mv_x)); // log2_max_mv_length_horizontal
    w.ue(log2_ceil(p.max_mv_y)); // log2_max_mv_length_vertical
    w.ue(0); // max_num_reorder_frames: no reordering
    w.ue(1); // max_dec_frame_buffering
    w.rbsp_trailing();
    Nal::new(NalType::Sps, 3, &w.into_bytes())
}

pub fn write_pps(p: &StreamParams) -> Nal {
    let mut w = BitWriter::new();
    w.ue(0); // pic_parameter_set_id
    w.ue(0); // seq_parameter_set_id
    w.put1(false); // entropy_coding_mode_flag: CAVLC
    w.put1(false); // bottom_field_pic_order_in_frame_present_flag
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.put1(false); // weighted_pred_flag
    w.put(2, 0); // weighted_bipred_idc
    w.se(p.init_qp as i32 - 26); // pic_init_qp_minus26
    w.se(0); // pic_init_qs_minus26
    w.se(p.chroma_qp_offset as i32); // chroma_qp_index_offset
    w.put1(true); // deblocking_filter_control_present_flag
    w.put1(false); // constrained_intra_pred_flag
    w.put1(false); // redundant_pic_cnt_present_flag
    w.rbsp_trailing();
    Nal::new(NalType::Pps, 3, &w.into_bytes())
}

#[derive(Clone, Copy, Debug)]
pub struct SliceHeader {
    pub idr: bool,
    pub frame_num: u32,
    pub idr_pic_id: u32,
    pub qp: u8,
    pub init_qp: u8,
    pub deblock: bool,
    pub alpha_offset_div2: i8,
    pub beta_offset_div2: i8,
}

/// Writes a slice header covering the whole picture.
pub fn write_slice_header(w: &mut BitWriter, h: &SliceHeader) {
    w.ue(0); // first_mb_in_slice
    w.ue(if h.idr { 7 } else { 5 }); // slice_type: all slices of the picture are I (7) or P (5)
    w.ue(0); // pic_parameter_set_id
    w.put(LOG2_MAX_FRAME_NUM, h.frame_num & ((1 << LOG2_MAX_FRAME_NUM) - 1));
    if h.idr {
        w.ue(h.idr_pic_id);
    } else {
        w.put1(false); // num_ref_idx_active_override_flag
        w.put1(false); // ref_pic_list_modification_flag_l0
    }
    // dec_ref_pic_marking(): every picture is a reference picture.
    if h.idr {
        w.put1(false); // no_output_of_prior_pics_flag
        w.put1(false); // long_term_reference_flag
    } else {
        w.put1(false); // adaptive_ref_pic_marking_mode_flag: sliding window
    }
    w.se(h.qp as i32 - h.init_qp as i32); // slice_qp_delta
    if h.deblock {
        w.ue(0); // disable_deblocking_filter_idc: filter all edges
        w.se(h.alpha_offset_div2 as i32);
        w.se(h.beta_offset_div2 as i32);
    } else {
        w.ue(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::{unescape, BitReader};

    fn params() -> StreamParams {
        StreamParams {
            width: 352,
            height: 288,
            mb_w: 22,
            mb_h: 18,
            fps_num: 30000,
            fps_den: 1001,
            level_idc: 13,
            init_qp: 28,
            chroma_qp_offset: 0,
            max_mv_x: 2047,
            max_mv_y: 511,
        }
    }

    #[test]
    fn sps_fields_parse_back() {
        let nal = write_sps(&params());
        assert_eq!(nal.bytes[0], 0x67);
        let rbsp = unescape(&nal.bytes[1..]);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.bits(8), 66);
        assert_eq!(r.bits(8), 0b1100_0000);
        assert_eq!(r.bits(8), 13);
        assert_eq!(r.ue(), 0);
        assert_eq!(r.ue(), 4);
        assert_eq!(r.ue(), 2);
        assert_eq!(r.ue(), 1);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.ue(), 21);
        assert_eq!(r.ue(), 17);
        assert_eq!(r.bits(2), 0b11);
        assert_eq!(r.bit(), 0); // no cropping for CIF
        assert_eq!(r.bit(), 1); // VUI
        assert_eq!(r.bits(4), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(32), 1001);
        assert_eq!(r.bits(32), 60000);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(3), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 1);
        assert_eq!((r.ue(), r.ue()), (0, 0));
        assert_eq!((r.ue(), r.ue()), (11, 9));
        assert_eq!((r.ue(), r.ue()), (0, 1));
        assert_eq!(r.bit(), 1); // stop bit
    }

    #[test]
    fn sps_cropping_for_non_multiple_of_16() {
        let mut p = params();
        p.width = 322;
        p.height = 242;
        p.mb_w = 21;
        p.mb_h = 16;
        let nal = write_sps(&p);
        let rbsp = unescape(&nal.bytes[1..]);
        let mut r = BitReader::new(&rbsp);
        r.bits(24);
        for _ in 0..4 {
            r.ue();
        }
        r.bit();
        assert_eq!((r.ue(), r.ue()), (20, 15));
        r.bits(2);
        assert_eq!(r.bit(), 1);
        assert_eq!((r.ue(), r.ue(), r.ue(), r.ue()), (0, 7, 0, 7));
    }

    #[test]
    fn pps_and_slice_header_parse_back() {
        let nal = write_pps(&params());
        assert_eq!(nal.bytes[0], 0x68);
        let rbsp = unescape(&nal.bytes[1..]);
        let mut r = BitReader::new(&rbsp);
        assert_eq!((r.ue(), r.ue()), (0, 0));
        assert_eq!(r.bits(2), 0);
        assert_eq!((r.ue(), r.ue(), r.ue()), (0, 0, 0));
        assert_eq!(r.bits(3), 0);
        assert_eq!((r.se(), r.se(), r.se()), (2, 0, 0));
        assert_eq!(r.bits(3), 0b100);

        let mut w = BitWriter::new();
        write_slice_header(
            &mut w,
            &SliceHeader {
                idr: false,
                frame_num: 387,
                idr_pic_id: 0,
                qp: 31,
                init_qp: 28,
                deblock: true,
                alpha_offset_div2: -1,
                beta_offset_div2: 2,
            },
        );
        w.rbsp_trailing();
        let b = w.into_bytes();
        let mut r = BitReader::new(&b);
        assert_eq!((r.ue(), r.ue(), r.ue()), (0, 5, 0));
        assert_eq!(r.bits(8), 131); // 387 mod 256
        assert_eq!(r.bits(3), 0);
        assert_eq!(r.se(), 3);
        assert_eq!((r.ue(), r.se(), r.se()), (0, -1, 2));
    }
}
