#!/usr/bin/env python3
"""Rate-distortion comparison against x264.

For every clip and quantiser setting this encodes with squeeze264 and with
x264 in two configurations, measures PSNR and SSIM of each decoded stream
against the source with ffmpeg (the same measurement for every encoder),
and computes Bjontegaard delta rates. Results go to a JSON file that
tools/report.py turns into the HTML report.

Needs: ffmpeg and x264 on PATH, the clips from `make data`, a release build.
"""
import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

QPS = [22, 27, 32, 37]

# name -> frames to encode (None: all)
CLIPS = {
    "foreman_cif.y4m": None,
    "akiyo_cif.y4m": None,
    "mobile_cif.y4m": None,
    "shields_720p_100f.y4m": None,
}

# Three x264 configurations, all single-threaded and tuned for PSNR so that
# psycho-visual options and adaptive quantisation do not distort a PSNR
# comparison.
#
# "matched": restricted to the tools this encoder has. Baseline profile
# (CAVLC, no B frames, no 8x8 transform), one reference frame, hexagon
# search, SATD-based sub-pel refinement without rate-distortion optimised
# mode decision or trellis quantisation, constant QP.
X264_MATCHED = ["--profile", "baseline", "--preset", "medium", "--tune", "psnr", "--ref", "1",
                "--me", "hex", "--merange", "16", "--subme", "5", "--trellis", "0",
                "--keyint", "250", "--min-keyint", "250", "--scenecut", "0", "--threads", "1"]
# "baseline": Baseline profile but otherwise x264's medium preset, i.e. with
# its RD mode decision, trellis quantisation, three reference frames and
# scene-cut detection. Constant QP.
X264_BASELINE = ["--profile", "baseline", "--preset", "medium", "--tune", "psnr", "--threads", "1"]
# "default": x264 as it comes (High profile: CABAC, B frames, 8x8 transform,
# macroblock-tree rate control), constant rate factor.
X264_DEFAULT = ["--preset", "medium", "--tune", "psnr", "--threads", "1"]

ENCODERS = ["squeeze264", "x264-matched", "x264-baseline", "x264-default"]


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"command failed: {' '.join(cmd)}\n{r.stderr[-2000:]}")
    return r


def y4m_info(path):
    with open(path, "rb") as f:
        line = f.readline().decode("ascii")
    fields = {t[:1]: t[1:] for t in line.split()[1:]}
    w, h = int(fields["W"]), int(fields["H"])
    num, den = (int(x) for x in fields["F"].split(":"))
    frames = (os.path.getsize(path) - len(line)) // (6 + w * h * 3 // 2)
    return w, h, num / den, frames


def measure(stream, source, frames):
    """PSNR-Y (from the mean squared error over all frames) and SSIM-Y."""
    # The psnr filter passes its first input through, so ssim can follow it.
    # Both inputs get the same time base and frame-number timestamps so that
    # frames pair up by index regardless of container timing.
    graph = ("[0:v]settb=1/1000,setpts=N[a];[1:v]settb=1/1000,setpts=N,split[c][d];"
             "[a][c]psnr=shortest=1[p];[p][d]ssim=shortest=1")
    r = run(["ffmpeg", "-hide_banner", "-nostdin", "-i", stream, "-i", source,
             "-frames:v", str(frames), "-filter_complex", graph, "-f", "null", "-"])
    psnr = re.search(r"PSNR y:([\d.]+|inf)", r.stderr)
    ssim = re.search(r"SSIM Y:([\d.]+)", r.stderr)
    if not psnr or not ssim:
        sys.exit(f"could not parse ffmpeg metrics for {stream}:\n{r.stderr[-1500:]}")
    return float(psnr.group(1)), float(ssim.group(1))


def bd_rate(ref, test):
    """Bjontegaard delta rate of `test` against `ref` in percent
    (cubic fit of log-rate over PSNR, integrated over the common PSNR range).
    Each argument is a list of (kbps, psnr). Positive: test needs more bits."""
    def fit(points):
        xs = [p[1] for p in points]
        ys = [math.log(p[0]) for p in points]
        n = len(xs)
        # Least-squares cubic through the points (exact for four points).
        a = [[sum(x ** (i + j) for x in xs) for j in range(4)] for i in range(4)]
        b = [sum(y * x ** i for x, y in zip(xs, ys)) for i in range(4)]
        for i in range(4):
            piv = max(range(i, 4), key=lambda r: abs(a[r][i]))
            a[i], a[piv] = a[piv], a[i]
            b[i], b[piv] = b[piv], b[i]
            for r in range(i + 1, 4):
                f = a[r][i] / a[i][i]
                for c in range(i, 4):
                    a[r][c] -= f * a[i][c]
                b[r] -= f * b[i]
        coef = [0.0] * 4
        for i in range(3, -1, -1):
            coef[i] = (b[i] - sum(a[i][j] * coef[j] for j in range(i + 1, 4))) / a[i][i]
        assert n >= 4
        return coef

    def integral(coef, lo, hi):
        prim = lambda x: sum(c * x ** (i + 1) / (i + 1) for i, c in enumerate(coef))
        return prim(hi) - prim(lo)

    lo = max(min(p[1] for p in ref), min(p[1] for p in test))
    hi = min(max(p[1] for p in ref), max(p[1] for p in test))
    if hi <= lo:
        return None
    # Centre PSNR values to keep the normal equations well conditioned.
    mid = (lo + hi) / 2
    cr = fit([(r, p - mid) for r, p in ref])
    ct = fit([(r, p - mid) for r, p in test])
    avg = (integral(ct, lo - mid, hi - mid) - integral(cr, lo - mid, hi - mid)) / (hi - lo)
    return (math.exp(avg) - 1) * 100


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="data")
    ap.add_argument("--bin", default="target/release/squeeze264")
    ap.add_argument("--out", default="docs/bench.json")
    ap.add_argument("--clip", action="append", help="limit to these clips")
    ap.add_argument("--frames", type=int, help="limit frames per clip (quick run)")
    args = ap.parse_args()
    for tool in ("ffmpeg", "x264"):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found on PATH")
    if not os.path.exists(args.bin):
        sys.exit(f"{args.bin} not found: run `make build` first")

    x264_version = run(["x264", "--version"]).stdout.split("\n")[0]
    ffmpeg_version = run(["ffmpeg", "-version"]).stdout.split("\n")[0]
    results = {"qps": QPS, "x264_version": x264_version, "ffmpeg_version": ffmpeg_version,
               "x264_matched_args": X264_MATCHED, "x264_baseline_args": X264_BASELINE,
               "x264_default_args": X264_DEFAULT,
               "date": time.strftime("%Y-%m-%d"), "clips": []}
    tmp = tempfile.mkdtemp(prefix="squeeze264-bench-")
    try:
        for name, limit in CLIPS.items():
            if args.clip and name not in args.clip:
                continue
            src = os.path.join(args.data, name)
            if not os.path.exists(src):
                print(f"skip {name}: not in {args.data} (run `make data`)")
                continue
            w, h, fps, total = y4m_info(src)
            frames = min(total, limit or total, args.frames or total)
            clip = {"name": name, "width": w, "height": h, "fps": fps, "frames": frames, "encoders": {}}
            for enc in ENCODERS:
                points = []
                for qp in QPS:
                    out = os.path.join(tmp, "out.h264")
                    t0 = time.time()
                    if enc == "squeeze264":
                        run([args.bin, "encode", src, "-o", out, "--qp", str(qp), "--frames", str(frames), "-q"])
                    elif enc in ("x264-matched", "x264-baseline"):
                        xargs = X264_MATCHED if enc == "x264-matched" else X264_BASELINE
                        run(["x264", "--quiet"] + xargs + ["--qp", str(qp), "--frames", str(frames), "-o", out, src])
                    else:
                        run(["x264", "--quiet"] + X264_DEFAULT + ["--crf", str(qp), "--frames", str(frames), "-o", out, src])
                    secs = time.time() - t0
                    kbps = os.path.getsize(out) * 8 * fps / frames / 1000
                    psnr, ssim = measure(out, src, frames)
                    points.append({"q": qp, "kbps": round(kbps, 2), "psnr_y": psnr, "ssim_y": ssim,
                                   "fps": round(frames / secs, 1)})
                    print(f"{name:24} {enc:14} q{qp}: {kbps:9.1f} kbit/s  {psnr:6.2f} dB  ssim {ssim:.4f}  {frames / secs:6.1f} fps", flush=True)
                clip["encoders"][enc] = points
            mine = [(p["kbps"], p["psnr_y"]) for p in clip["encoders"]["squeeze264"]]
            clip["bd_rate"] = {}
            for ref in ENCODERS[1:]:
                rp = [(p["kbps"], p["psnr_y"]) for p in clip["encoders"][ref]]
                bd = bd_rate(rp, mine)
                clip["bd_rate"][ref] = None if bd is None else round(bd, 1)
                print(f"{name:24} BD-rate vs {ref}: {clip['bd_rate'][ref]} %")
            results["clips"].append(clip)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(results, f, indent=1)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
