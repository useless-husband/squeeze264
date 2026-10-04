# Changelog

## 0.1.0 - 2026-10-05

First version.

### Encoder
- H.264 Constrained Baseline: I and P slices, one reference frame, CAVLC.
- Intra 16x16 (4 modes), Intra 4x4 (9 modes), chroma intra (4 modes), I_PCM fallback when a macroblock would
  exceed 3200 bits.
- Inter 16x16, 16x8, 8x16, 8x8 and 8x4 / 4x8 / 4x4 sub-partitions; P_Skip; median and directional motion vector
  prediction; hexagon search with half- and quarter-sample refinement.
- 4x4 integer transform, Hadamard DC paths, dead-zone quantisation, coefficient thresholding.
- In-loop deblocking filter with alpha/beta offsets.
- Constant QP and single-pass average bitrate rate control.
- Annex B output and a minimal MP4 muxer.
- Y4M (4:2:0, 8-bit) streaming input; cropping for sizes that are not multiples of 16.

### Tools
- `squeeze264 encode | check | gen` command line.
- `tools/bench.py` (rate-distortion and BD-rate against x264), `tools/report.py` (HTML report),
  `tools/mutants.py` (mutation check), `tools/avcheck.swift` (AVFoundation decode check),
  `tools/fetch_data.py` (test clips with SHA-256).

### Verification
- Bit-exact reconstruction against ffmpeg (libavcodec) and Apple VideoToolbox for every test stream.
- Fuzzing with random encoder decisions; the corpus reaches every coeff_token entry, coded_block_pattern,
  prediction mode, partition shape and sub-sample position.
