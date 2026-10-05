# squeeze264

An H.264 (AVC) video encoder written from scratch in Rust, from the text of the ITU-T H.264 specification,
with no third-party crates. It takes raw Y4M video and writes `.mp4` files that play in QuickTime Player
(checked through AVFoundation, the framework behind it) and in players built on libavcodec such as VLC and
ffplay (checked through ffmpeg).

The point of the project is not the encoder alone but the proof that it is correct: for every test stream, two
independent decoders (ffmpeg's libavcodec and Apple's VideoToolbox) must output frames that are **byte-identical**
to the reconstruction the encoder computed itself, without printing a single warning.

This is a learning rebuild of a twenty-year-old standard, modelled on a course project
([MIT 6.205, "H.264 Video Compression and Transmission", Fall 2022](https://fpga.mit.edu/6205/F25/final_project_archive)),
done here in software. It does not compete with x264; the [results](#results-against-x264) say by how much it
loses and why. [繁體中文版 README](README.zh-TW.md) · [白話導讀](docs/導讀.zh-TW.md)

```
$ ./target/release/squeeze264 check data/foreman_cif.y4m --qp 28
300 frames 352x288 @ 29.97 fps, level 1.3
bitrate   503.9 kbit/s (630544 bytes)
PSNR      Y 35.99  U 40.52  V 41.92 dB
macroblocks  I4x4 2.2%  I16x16 1.1%  inter 59.7%  skip 37.0%
speed     106.0 fps (2.83 s)
PASS  ffmpeg (libavcodec h264)                   Annex B  300/300 frames identical, decoder silent
PASS  ffmpeg (libavcodec h264)                   MP4      300/300 frames identical, decoder silent
PASS  Apple VideoToolbox (via ffmpeg hwaccel)    Annex B  300/300 frames identical, decoder silent
PASS  Apple VideoToolbox (via ffmpeg hwaccel)    MP4      300/300 frames identical, decoder silent
bit-exact: the decoders reproduce the encoder's reconstruction exactly
```

45 MB of raw video becomes 0.6 MB. The picture below is frame 150 of that encode with the encoder's decisions
drawn on it: blue blocks are predicted from the previous frame (lines are motion vectors, small squares are
partitions), orange and green are intra-coded, untinted blocks are skipped.

![Frame 150 of foreman with macroblock types and motion vectors](docs/frame-viz.jpg)

The full report, with per-frame statistics and hover details for every macroblock, is
[`docs/results.html`](docs/results.html) (open it in a browser after cloning).

## What is implemented

Constrained Baseline profile, everything real, nothing stubbed:

| Area | Details |
|---|---|
| Input | Y4M, 4:2:0, 8-bit, read frame by frame. Any even size; non-multiples of 16 are padded and cropped via the SPS. |
| Frame types | I (IDR) and P slices, one reference frame, one slice per picture. |
| Intra | 16x16 (vertical, horizontal, DC, plane), 4x4 (all nine modes), chroma (four modes), SATD mode decision. I_PCM as a fallback. |
| Inter | 16x16, 16x8, 8x16, 8x8 and 8x4 / 4x8 / 4x4 partitions; P_Skip; motion vector prediction; vectors may point outside the picture. |
| Motion search | Neighbour candidates, hexagon search, half- and quarter-sample refinement. 6-tap luma and eighth-sample bilinear chroma interpolation. |
| Residual | 4x4 integer transform, Hadamard for Intra16x16 and chroma DC, quantisation, CAVLC with all tables. |
| Loop filter | In-loop deblocking, all boundary strengths, alpha/beta offsets. |
| Rate control | Constant QP, or a target average bitrate (single pass). |
| Output | SPS/PPS/slice headers with VUI timing, Annex B byte stream, and an MP4 muxer of its own. |
| Tooling | CLI with progress and per-frame statistics, HTML report, benchmark against x264. |

About 8,200 lines of Rust in `src/` (including unit tests), 1,100 in `tests/`.

## Quick start

Needs Rust (stable). `ffmpeg` is needed only for verification, `x264` only for the benchmark.

```sh
make build        # cargo build --release
make test         # 114 tests; the conformance ones need ffmpeg on PATH
make data         # download the standard test clips (about 275 MB, SHA-256 checked)
./target/release/squeeze264 encode data/foreman_cif.y4m -o foreman.mp4 --qp 28
open foreman.mp4
```

On a Mac, double-clicking `跑跑看.command` does all of it: build, fetch one clip (or generate a synthetic one
when offline), encode, verify against the decoders, build the report, and open the video and the report.

```
squeeze264 encode <in.y4m> -o <out.mp4|out.h264> [--qp N | --bitrate KBPS] [--keyint N] ...
squeeze264 check  <in.y4m> [same options]     encode, decode with ffmpeg and VideoToolbox, compare
squeeze264 gen    <out.y4m> [--size WxH] [--frames N] [--pattern moving|noise|still|extremes]
squeeze264 --help                             all options
```

## How it works

```
 source frame ─► per macroblock: decide ─► predict ─► residual ─► transform + quantise ─► CAVLC ─► bitstream
                      ▲              (intra / inter)                      │
                      │                                    dequantise + inverse transform
                      │                                                   ▼
                 reference frame ◄─ half-sample planes ◄─ deblocking ◄─ reconstruction
```

The lower loop is a complete decoder living inside the encoder: each frame is predicted from what the decoder
will have, not from the original. That reconstruction is what the tests compare with real decoders.
[`docs/DESIGN.md`](docs/DESIGN.md) explains the architecture, the hard parts (table transcription, neighbour
availability, exact interpolation, QP inheritance, level limits) and the trade-offs that were rejected.

## Verification

All of this is reproducible with the commands shown. Results below are from an Apple M5 running macOS 27 with
ffmpeg 8.0.1; "VideoToolbox" means ffmpeg's `-hwaccel videotoolbox` with the hardware surface format forced, so
a silent fallback to software decoding is impossible.

**1. Bit-exact decode of real clips** — `make check`

Every clip × {QP 22, QP 37, 800 kbit/s target} × {ffmpeg software, VideoToolbox} × {Annex B, MP4}:

| Clip | Frames | Result |
|---|---|---|
| foreman CIF | 300 | 12/12 runs identical, decoders silent |
| akiyo CIF | 300 | 12/12 |
| mobile CIF | 300 | 12/12 |
| shields 720p50 (first 100 frames) | 100 | 12/12 |

**2. Fuzzing with random decisions** — `cargo test --release --test conformance fuzz`

The encoder's decisions are replaced by random legal ones (macroblock type, prediction modes, partition shapes,
vectors anywhere in the allowed range, coded_block_pattern, QP changes on a third of the macroblocks, I_PCM)
over noise, synthetic motion and saturated content, 28 seeds, sizes from 16x16 to 176x144, QP 0 to 51,
deblocking on and off with alpha offsets from −6 to +6, chroma QP offsets from −12 to +12. Every stream must decode bit-exactly. The test also asserts that the corpus
reached all 262 coeff_token table entries, all 48 coded_block_pattern values for intra and inter, every
prediction mode and partition shape, all 16 sub-sample positions, and both QP extremes. Six more seeds at
320x240 go through VideoToolbox.

**3. Unit tests against the specification** — `cargo test --release --lib` (86 tests)

Exp-Golomb codes against Table 9-2; CAVLC tables for prefix-freeness and exact Kraft sums, round trips through a
separate test-only decoder (60,000 random blocks) and the two worked examples in Iain Richardson's Vcodex white paper on CAVLC; the inverse
transform against the equations of clause 8.5.12; quantisation round-trip error bounds at all 52 QPs; all 16
quarter-sample positions against a second, sample-by-sample implementation of clause 8.4.2.2; intra predictors
on hand-computed examples; deblocking filter outputs and boundary strengths on hand-computed edges; motion
vector prediction including the sub-partition availability cases; the MP4 box structure; MD5 against RFC 1321.

**4. A third decoder path and the container** — `make avcheck` (macOS)

`tools/avcheck.swift` opens the MP4 with AVFoundation, the framework QuickTime Player uses:

```
playable=true size=352x288 fps=29.970 duration=10.010 frames=300 reader=completed
AVFoundation decoded 300 frames; 300/300 identical to the encoder's reconstruction
```

**5. Anyone can repeat the comparison by hand**

```sh
./target/release/squeeze264 encode data/foreman_cif.y4m -o out.h264 --framemd5 out.md5
ffmpeg -v error -i out.h264 -f framemd5 - | grep -v '^#' | awk '{print $6}' | diff - out.md5 && echo identical
```

**6. Do the tests notice bugs?** — `python3 tools/mutants.py`

22 one-line bugs planted in normative code (a swapped table entry, a rounding constant, a neighbour rule): all
22 are caught ([`docs/mutants.md`](docs/mutants.md)). The first run caught 21; the survivor (frame_num wrapping
at the wrong value, which decoders silently conceal) exposed a blind spot of the decode oracle and led to a
direct test of the slice headers.

Things a decoder would not complain about are tested directly: the 3200-bit macroblock limit (I_PCM fallback),
vector ranges per level, the motion-vector count limit from level 3.1, frame numbering, level selection.

## Results against x264

`make bench` encodes each clip at four quantiser settings with squeeze264 and with x264 0.165 in three
configurations, and measures luma PSNR and SSIM of every decoded stream against the source with ffmpeg.
BD-rate is the average extra bitrate squeeze264 needs for the same PSNR (positive = worse).

| Clip | vs x264 with matched tools | vs x264 Baseline, medium preset | vs x264 defaults (High profile) |
|---|---|---|---|
| foreman CIF | +0.5 % | +13.1 % | +74.0 % |
| akiyo CIF | −4.2 % | −0.5 % | +69.2 % |
| mobile CIF | −1.4 % | +20.8 % | +115.4 % |
| shields 720p | +1.6 % | +14.2 % | +111.9 % |

![PSNR against bitrate for four clips and four encoders](docs/rd-curves.png)

- **Matched tools** restricts x264 to what this encoder has: Baseline, one reference frame, hexagon search,
  SATD-based decisions without rate-distortion optimisation or trellis (`--subme 5 --trellis 0 --ref 1`).
  Here the two are within a few percent of each other.
- **Baseline, medium preset** lets x264 use its rate-distortion optimised mode decision, trellis quantisation and
  three reference frames, still within Baseline. That is worth 0–21 % bitrate, which this encoder does not have.
- **Defaults** add CABAC, B frames, the 8x8 transform and macroblock-tree rate control (High profile). This
  encoder needs roughly 1.7× to 2.2× the bitrate for the same PSNR. Those are the tools listed under future work.

**Speed** (single thread, same machine, shared with other jobs while measuring, process start-up included):

| Clip, QP 27 | squeeze264 | x264 matched | x264 Baseline medium | x264 defaults |
|---|---|---|---|---|
| foreman CIF | 82 fps | 793 fps | 409 fps | 381 fps |
| akiyo CIF | 333 fps | 2313 fps | 1170 fps | 1044 fps |
| mobile CIF | 64 fps | 755 fps | 197 fps | 300 fps |
| shields 720p | 23 fps | 93 fps | 42 fps | 59 fps |

x264 is several times faster (about 4× to 12× against the matched configuration): it has hand-written SIMD
for every kernel; this encoder is scalar Rust. Speed varies with QP (13–54 fps at 720p, 57–843 fps at CIF);
all measurements are in [`docs/bench.json`](docs/bench.json) and the table at the bottom of the report.

Bitrate mode landed within 7 % of the target on the test clips (foreman at 300 and 800 kbit/s: 315 and 818;
akiyo at 100: 104; mobile at 1500: 1525; shields 720p at 4000: 4276 over only 100 frames).

## Limitations

- Baseline tools only: no B frames, no CABAC, no 8x8 transform, no interlace, no weighted prediction, one
  reference frame, one slice per picture. No multi-threading, no SIMD.
- Mode decisions use SATD estimates, not true rate-distortion costs; no trellis quantisation, no adaptive
  quantisation, no psycho-visual tuning, no look-ahead or scene-cut detection (a scene change inside a GOP is
  coded as a P frame full of intra macroblocks).
- The level is chosen from picture size and frame rate (plus the target in bitrate mode). In constant-QP mode
  the bitrate is not known in advance and can exceed the level's limit: shields 720p50 at QP 22 produced
  34.7 Mbit/s under level 3.2, whose limit is 20 Mbit/s. HRD/CPB conformance and the minimum compression ratio
  are not enforced. The decoders used here do not check these.
- Rate control is frame-level and single-pass; it has no buffer model and can overshoot on short clips.
- Input must be 8-bit 4:2:0 Y4M with even dimensions. The sample aspect ratio of the input is ignored
  (square pixels assumed), as are colour primaries.
- VideoToolbox refused pictures smaller than 64x64 here (32x32 and 64x48 were rejected), so for those only the ffmpeg software decoder is checked
  (`check` prints SKIP). The hardware comparison needs macOS on real hardware; on Linux only ffmpeg is used.
- On virtual machines, including GitHub's macOS runners, the VideoToolbox comparison is skipped: frames come back
  through a paravirtual surface path and differed in chroma from real hardware for the same stream. CI therefore
  proves bit-exactness against ffmpeg only; the VideoToolbox results above are from a physical Apple M5.
- The MP4 muxer writes exactly one video track with constant frame duration and puts the index at the end of the
  file (fine for local playback, not for progressive download).
- Conformance was established with decoders, not with the official JVT conformance bitstreams (those test
  decoders, not encoders) and not with a stream analyser.

Future work, in the order it would pay off: rate-distortion optimised mode decision, multiple reference frames,
CABAC, B frames, the 8x8 transform (High profile), SIMD kernels, slice-level threading.

## Related work

- **[x264](https://www.videolan.org/developers/x264.html)** is the production-quality open-source H.264 encoder
  and the baseline for the comparison above. squeeze264 implements a small subset of its tools and is far slower.
- **[OpenH264](https://www.openh264.org/)** (Cisco) is a real-time Constrained Baseline encoder and decoder used
  in browsers for WebRTC.
- **JM**, the Joint Video Team's reference software, is the reference implementation of the standard.
- **[minih264](https://github.com/lieff/minih264)** (lieff) is a compact single-header C encoder with SIMD,
  aimed at embedded use; it is the closest in spirit and considerably more practical.
- **[hello264](https://www.cardinalpeak.com/blog/worlds-smallest-h-264-encoder)** ("World's smallest H.264
  encoder", Ben Mesander) and Jordi Cenzano's
  [minimal encoder](https://jordicenzano.name/2014/08/31/the-source-code-of-a-minimal-h264-encoder-c/) write
  uncompressed I_PCM macroblocks only: valid streams, no compression.
- The MIT 6.205 project that inspired this one implemented H.264 building blocks on an FPGA.

This project claims no novelty. It is a from-scratch educational implementation: no code was taken from any of
the above; it was written from the ITU-T Recommendation (freely available from itu.int) and textbook
descriptions, with the decoders used purely as black-box oracles. What it adds is a worked example of how to
*prove* such an encoder correct and an honest measurement of what each missing tool costs.

## Repository layout

```
src/            encoder library and CLI (see docs/DESIGN.md for a module guide)
tests/          conformance (needs ffmpeg), encoder behaviour, CLI
tools/          bench.py, report.py, mutants.py, fetch_data.py, avcheck.swift / avcheck.sh
docs/           DESIGN.md, 導讀.zh-TW.md, results.html, bench.json, mutants.md
跑跑看.command   double-click demo (macOS)
data/           test clips (not in git; `make data`)
```

`make` targets: `build`, `test`, `test-debug` (same tests with debug assertions), `lint`, `data`, `check`,
`bench`, `report`, `avcheck`, `demo`, `clean`. CI builds and runs the tests on Linux (ffmpeg from apt, synthetic
frames generated at test time) and macOS.

## Patents and licence

H.264 is covered by patents in many countries. This repository is educational source code and conveys no patent
licence. The source code is released under the [MIT licence](LICENSE). Test clips come from the
[Xiph.org collection](https://media.xiph.org/video/derf/) and are downloaded, not redistributed.
