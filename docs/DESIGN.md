# Design

squeeze264 is an H.264 (ITU-T Rec. H.264 | ISO/IEC 14496-10) Constrained Baseline encoder written from the text
of the specification. This document explains how it is put together, which parts were hard, and what was
decided against. Clause numbers refer to the 2021 edition of the Recommendation.

## The one rule everything follows

A video encoder contains a decoder. Every P frame is predicted from the *decoded* previous frame, so the encoder
must compute exactly the picture the decoder will compute: same prediction, same inverse transform, same
deblocking, to the last bit. If one sample differs, the two sides predict from different references and the
error grows frame after frame.

That splits the code into two kinds:

| Kind | Examples | If it is wrong |
|---|---|---|
| **Normative**: must match the specification bit for bit | syntax writing, CAVLC tables, intra predictors, interpolation filters, inverse transform and scaling, deblocking, motion vector prediction | The stream is invalid or drifts. A real decoder disagrees with the encoder. |
| **Encoder's choice**: any outcome is a legal stream | forward transform and quantiser rounding, motion search, mode decision, coefficient thresholding, rate control | Compression gets worse. Nothing breaks. |

The test strategy follows from this. Normative code is checked against independent decoders (ffmpeg's libavcodec
and Apple VideoToolbox): the reconstruction the encoder holds in memory must be byte-identical to what they
output. Encoder's-choice code is checked by behaviour (does motion search find a known displacement, does the
rate controller hit its target) and measured by rate-distortion against x264.

## Data flow for one frame

```
 Y4M reader ──► source frame (padded to 16x16 multiples)
                    │
   ┌────────────────▼────────────────────────────────────────────────┐
   │ for each macroblock, in raster order                            │
   │                                                                 │
   │   analysis.rs  decide: skip / inter (partitions, vectors) /     │
   │                intra 16x16 / intra 4x4 (+ chroma mode)          │
   │        │            ▲ me.rs: candidates → hexagon → sub-sample  │
   │        ▼            ▲ mvpred.rs: predicted vector               │
   │   mb.rs        prediction  (intra.rs / inter.rs)                │
   │                residual → transform.rs: DCT, quantise           │
   │                levels → dequantise, inverse DCT → reconstruction│──► unfiltered picture
   │                levels → cavlc.rs → bitstream.rs                 │    (intra neighbours)
   │                update contexts in state.rs                      │
   └─────────────────────────────────────────────────────────────────┘
                    │
        deblock.rs: filter the whole picture (in-loop)
                    │
        inter.rs: extend borders, build the three half-sample planes
                    │
        reference picture for the next frame
                    │
   slice NAL ──► Annex B file  and/or  mp4.rs (length-prefixed samples + moov)
```

`encoder.rs` drives this loop, chooses the frame type and QP (`ratecontrol.rs`) and collects statistics.

## Module guide

| File | Lines of responsibility |
|---|---|
| `bitstream.rs` | Bit writer, Exp-Golomb `ue/se/te`, RBSP trailing bits, NAL header and emulation prevention. |
| `tables.rs` | Everything transcribed from tables in the specification: CAVLC code words, zig-zag scan, coded_block_pattern mapping, quantiser tables, chroma QP mapping, deblocking thresholds, level limits. |
| `headers.rs` | SPS (with VUI timing and bitstream restrictions), PPS, slice header. |
| `transform.rs` | Forward/inverse 4x4 integer transform, Hadamard transforms for DC coefficients, quantisation and scaling. |
| `intra.rs` | Nine 4x4 predictors, four 16x16 predictors, four chroma predictors. |
| `inter.rs` | Reference picture with half-sample planes, quarter-sample luma and eighth-sample chroma prediction. |
| `mvpred.rs` | Median/directional motion vector prediction and the P_Skip vector. |
| `me.rs`, `cost.rs` | Motion search and distortion measures (SAD, SATD, lambda). |
| `cavlc.rs` | Residual block entropy coding; a test-only decoder for round trips. |
| `mb.rs` | Turns one decision into reconstructed samples and macroblock syntax; neighbour contexts; I_PCM fallback. |
| `analysis.rs` | Mode decision, and the random-decision generator used for fuzzing. |
| `deblock.rs` | Boundary strength and edge filters. |
| `ratecontrol.rs` | Constant QP and single-pass average bitrate. |
| `encoder.rs` | Frame loop, slice assembly, statistics. |
| `mp4.rs` | ISO BMFF muxer: `ftyp`, `mdat`, `moov` with one `avc1` track. |
| `y4m.rs`, `frame.rs` | Streaming input, planes with replicated borders, PSNR. |
| `verify.rs` | Runs ffmpeg as an external process and compares frames. |
| `synth.rs`, `rng.rs`, `md5.rs` | Synthetic clips, deterministic PRNG, MD5 for `framemd5`-style fingerprints. |

No third-party crates are used.

## Hard problems

### 1. Getting the CAVLC tables right without copying them from another codec

The coeff_token, total_zeros and run_before tables hold about 500 code words. A single wrong bit produces a
stream that still parses for a while and then turns into garbage. They were typed in from the specification in
the same layout it prints them (bit strings), and three independent nets catch typos:

1. **Structure.** A unit test checks that every table is prefix-free and pins its Kraft sum. In these tables
   only the all-zero code word of the longest length is unused, so the sums are known in advance
   (for example 1 − 2/65536 for the first coeff_token table). A typo that changes a length or creates a duplicate
   fails here.
2. **Round trip.** A separate test-only decoder, written from the *parsing* description (clause 9.2), decodes
   60,000 random blocks, plus two worked examples from the literature whose expected bit strings are in the tests.
3. **The oracle.** The fuzz corpus must write every one of the 262 coeff_token entries (the encoder counts them)
   and the resulting streams must decode bit-exactly in ffmpeg and VideoToolbox. A swap of two equal-length code
   words passes nets 1 and 2 but not this one. `tools/mutants.py` plants exactly such swaps to prove it.

### 2. Neighbour availability

Several predictions depend on blocks that may or may not have been decoded yet: the above-right samples of an
Intra4x4 block, and neighbour C of a motion partition. The rule in the specification is "available if it lies in
the picture and precedes the current block in decoding order", and decoding order inside a macroblock is a
Z-pattern, not raster order. Both places reduce to one comparison of `luma4x4BlkIdx` values
(`tables::BLK_IDX`): a block inside the current macroblock is available iff its index is smaller. Above the
macroblock it depends on the picture edge; to the right it never is. When C is unavailable the specification
substitutes D (above-left), and when only A exists it stands in for B and C; `mvpred.rs` keeps "unavailable" and
"available but intra" apart because the two cases behave differently.

### 3. Quarter-sample interpolation that is fast and still exact

The specification defines the 15 fractional positions through named samples: half-sample values come from a
6-tap filter, the centre one from filtering unrounded intermediates, and every quarter position is the rounded
average of two neighbours. Computing that per candidate vector would make motion search slow. Instead each
reference picture gets three extra planes (horizontal, vertical and centre half-samples), computed once per frame.
Any quarter position is then one or two fetches (the `QPEL` table in `inter.rs`). References are stored with a
32-sample border of replicated edge samples; a vector may point up to 24 samples outside the picture, which is
exactly equivalent to the coordinate clamping the specification prescribes, and leaves room for the filter taps.

The unit tests compare this against a second implementation written sample-by-sample from clause 8.4.2.2.1 with
clamped fetches, which computes the centre sample the *other* way round (from horizontal intermediates), for
every fraction and for blocks reaching outside the picture.

### 4. The decoder's QP is not always the encoder's QP

`mb_qp_delta` is only transmitted when a macroblock has coefficients (or is Intra16x16). A skipped macroblock, or
one whose coefficients all quantised to zero, therefore *inherits* the previous QP in the decoder, whatever the
encoder had in mind, and the deblocking filter uses that inherited value. I_PCM macroblocks are filtered as QP 0
but do not change the running QP. `mb.rs` tracks the value the decoder will derive (`PicState::qp`) separately
from the value used for quantisation. The fuzzer changes QP on a third of the macroblocks to exercise this.

### 5. Deblocking order and strength

The filter runs over the finished picture, macroblock by macroblock: vertical edges left to right, then
horizontal edges top to bottom, each reading samples already modified by earlier edges. Intra prediction,
however, must use *unfiltered* neighbours, so filtering cannot happen as macroblocks are produced; the encoder
keeps the unfiltered reconstruction until the frame is complete, exactly like a decoder. Boundary strength
depends on intra/inter, on whether either 4x4 block has coefficients (the transmitted count, zero for blocks
dropped by coded_block_pattern), and on vector differences of at least one whole sample. Chroma edges reuse the
luma strengths but average chroma QPs.

### 6. Limits that no decoder complains about

A decoder will happily play a stream that breaks level limits, so the oracle cannot find these; they are handled
by construction and tested directly:

- **3200 bits per macroblock** (Annex A.3.1). After writing a macroblock the encoder checks its size; if it is
  over the limit the bits are rewound and the macroblock is re-sent as I_PCM (raw samples, 3072 bits plus header).
  This happens for noise at QP 0 and is tested in both slice types.
- **Vertical vector range** per level (±64 to ±512 samples) and the horizontal ±2048 range are part of the
  per-macroblock vector limits, so search, fuzzing and skip decisions all respect them.
- **Motion vectors per two macroblocks** (16 from level 3.1): sub-8x8 partitions are disabled at those levels.
- **Coefficient magnitude.** Baseline CAVLC cannot code |level| above about 2063; the quantiser clamps at 2047.
- **Level selection** from frame size, macroblock rate and (in bitrate mode) the bitrate; sizes beyond level 5.1
  are rejected.

Not enforced: the level's bitrate/CPB limit in constant-QP mode (the bitrate is not known in advance) and the
minimum compression ratio. See Limitations in the README.

### 7. MP4 that QuickTime accepts

The muxer writes samples as they arrive into an `mdat` box with a 64-bit size that is patched at the end, then the
index. Each sample is one slice NAL unit with a 4-byte length prefix; SPS and PPS live only in `avcC`. One chunk
per sample keeps `stsc` trivial. Low frame rates get a ×1000 timescale. The result is checked three ways: a box
walker in the unit tests, bit-exact decode through ffmpeg and VideoToolbox, and `tools/avcheck.swift`, which
opens the file with AVFoundation (the framework behind QuickTime Player) and compares every decoded frame.

## Encoder decisions

- **I frames**: best Intra16x16 mode by SATD versus the sum of the best Intra4x4 modes (which requires coding
  the 4x4 blocks one by one, because each predicts from the reconstruction of the previous ones).
- **P frames**: first try P_Skip (predict with the skip vector; if every coefficient quantises to zero, done).
  Otherwise search 16x16; if the residual is not already small, also 8x8 (with 8x4/4x8/4x4 where the level allows),
  16x8 and 8x16, each seeded with the vectors found so far; then compare with intra. Costs are SATD plus
  lambda times an estimate of header and vector bits, with lambda = 2^((QP−12)/6).
- **Motion search**: predictor and neighbour candidates, hexagon steps on integer positions (SAD), a final
  radius-1 square, then half- and quarter-sample refinement by SATD.
- **Quantisation**: dead-zone rounding (1/3 intra, 1/6 inter). Inter blocks whose only coefficients are a few
  isolated ±1 are zeroed (they cost more bits than the distortion they remove).
- **Rate control**: constant QP (I frames 3 lower), or one QP per frame from a running estimate of
  bits × quantiser scale, with the accumulated budget error paid back over two seconds.

## Verification layers

| Layer | What it proves | Where |
|---|---|---|
| Unit tests against the specification's equations | each normative building block in isolation | `src/*.rs` |
| Fuzz with random decisions | every macroblock type, partition, mode, CBP, QP change, vector position decodes exactly | `tests/conformance.rs::fuzz_random_decisions` |
| Real decisions on synthetic and real clips | the encoder as actually used decodes exactly | `tests/conformance.rs`, `make check` |
| Two decoder implementations, two containers | no reliance on one decoder's tolerance | ffmpeg software, VideoToolbox, AVFoundation |
| Mutation check | the suite notices one-line normative bugs | `tools/mutants.py`, `docs/mutants.md` |
| Debug-assertion run | every sample fetch stays inside its buffer, value ranges hold | `make test-debug` |

The harness itself is tested: `harness_detects_a_single_wrong_sample` flips one chroma sample in the expected
data and requires the comparison to report that frame and plane.

## Trade-offs rejected

- **CABAC, B frames, 8x8 transform, multiple references.** They are where most of x264's remaining advantage
  comes from, but each adds a large normative surface. A complete, provably conformant Baseline encoder was
  preferred over a wider one with gaps.
- **Rate-distortion optimised mode decision and trellis quantisation.** These need exact bit counts per candidate
  (several times more work per macroblock). The benchmark against x264 shows what they are worth: 0–21 % bitrate
  on the test clips.
- **Multiple slices, error resilience, FMO/ASO.** The MIT project this is modelled on was about transmission;
  this one stops at a correct file.
- **SIMD and threads.** Plain scalar Rust that the compiler can vectorise where it sees fit. x264 is roughly ten
  times faster with hand-written assembly.
- **A built-in decoder for self-checking.** It would share the author's misreadings of the specification with
  the encoder. Independent decoders are a stronger oracle.
- **Linking ffmpeg.** It is run as a separate process so that the encoder stays dependency-free and the oracle
  stays independent.

## Patents

H.264 is covered by patents in many jurisdictions. This repository is educational source code; it grants no
patent rights. Anyone shipping a product would need to look at licensing (for example through Via LA, formerly
MPEG LA).
