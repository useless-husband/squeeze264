#!/usr/bin/env python3
"""Build the self-contained HTML report.

Inputs (all optional, sections appear when their data is given):
  --bench docs/bench.json     rate-distortion results from tools/bench.py
  --stats out/stats.json      per-frame statistics from `squeeze264 encode --stats`
  --recon out/recon.y4m       the encoder's reconstruction (for the frame picture)
  --check out/check.txt       output of `squeeze264 check` to quote

Output: one HTML file with inline SVG, no external resources.
Python standard library only.
"""
import argparse
import base64
import html
import json
import math
import struct
import zlib

# Categorical slots (validated for colour-vision-deficiency separation in both
# modes, assigned in fixed order) and neutral tokens.
CSS = """
:root{color-scheme:light;--surface:#fcfcfb;--panel:#f3f2ee;--grid:#e4e2dc;--ink:#0b0b0b;--ink2:#52514e;--ink3:#7a7872;
--s1:#2a78d6;--s2:#eb6834;--s3:#1baf7a;--s4:#eda100;}
@media (prefers-color-scheme:dark){:root{color-scheme:dark;--surface:#1a1a19;--panel:#242422;--grid:#383835;--ink:#ffffff;--ink2:#c3c2b7;--ink3:#8f8e85;
--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;}}
*{box-sizing:border-box}
body{margin:0;background:var(--surface);color:var(--ink);font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif}
main{max-width:1080px;padding:28px 20px 64px;margin:0}
h1{font-size:26px;margin:0 0 6px}
h2{font-size:19px;margin:44px 0 6px;padding-top:18px;border-top:1px solid var(--grid)}
h3{font-size:15px;margin:0 0 2px}
p{margin:6px 0 12px;max-width:78ch}
.note{color:var(--ink2);font-size:13.5px}
code{font:13px ui-monospace,Menlo,Consolas,monospace;background:var(--panel);padding:1px 5px;border-radius:4px}
pre{font:12.5px/1.5 ui-monospace,Menlo,Consolas,monospace;background:var(--panel);padding:12px 14px;border-radius:6px;overflow-x:auto;margin:8px 0 14px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(420px,1fr));gap:26px 28px;margin-top:14px}
@media (max-width:520px){.grid{grid-template-columns:1fr}}
.tiles{display:flex;flex-wrap:wrap;gap:12px;margin:14px 0 6px}
.tile{background:var(--panel);border-radius:8px;padding:12px 16px;min-width:150px}
.tile .label{font-size:12.5px;color:var(--ink2)}
.tile .value{font-size:24px;font-weight:600}
.tile .unit{font-size:13px;color:var(--ink2);font-weight:400;margin-left:3px}
.legend{display:flex;flex-wrap:wrap;gap:6px 18px;font-size:13px;color:var(--ink2);margin:8px 0 2px}
.legend span{display:inline-flex;align-items:center;gap:6px}
.key{display:inline-block;width:14px;height:3px;border-radius:2px}
.swatch{display:inline-block;width:11px;height:11px;border-radius:2px}
svg{display:block;width:100%;height:auto;overflow:visible}
svg text{font:11.5px -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;fill:var(--ink2)}
.axis{stroke:var(--grid);stroke-width:1}
.line{fill:none;stroke-width:2;stroke-linejoin:round;stroke-linecap:round}
.dot{stroke:var(--surface);stroke-width:2}
.hit{fill:transparent;cursor:default}
table{border-collapse:collapse;font-size:13.5px;margin:10px 0}
th,td{text-align:right;padding:4px 12px 4px 0;border-bottom:1px solid var(--grid);font-variant-numeric:tabular-nums}
th{font-weight:600;color:var(--ink2)}
th:first-child,td:first-child{text-align:left}
details{margin:8px 0}
summary{cursor:pointer;color:var(--ink2);font-size:13.5px}
#tip{position:fixed;pointer-events:none;background:var(--ink);color:var(--surface);font-size:12.5px;line-height:1.4;padding:6px 9px;border-radius:5px;display:none;white-space:pre;z-index:9}
.framewrap{max-width:880px;margin-top:10px}
.framewrap image{image-rendering:pixelated}
.toggles{display:flex;flex-wrap:wrap;gap:4px 18px;font-size:13.5px;margin:10px 0 4px}
.hide-types .t,.hide-vectors .v,.hide-parts .p{display:none}
"""

JS = """
const tip=document.getElementById('tip');
document.addEventListener('mousemove',e=>{
  const t=e.target.closest('[data-tip]');
  if(!t){tip.style.display='none';return}
  tip.textContent=t.getAttribute('data-tip');
  tip.style.display='block';
  const w=tip.offsetWidth,h=tip.offsetHeight;
  let x=e.clientX+14,y=e.clientY+14;
  if(x+w>innerWidth-8)x=e.clientX-w-14;
  if(y+h>innerHeight-8)y=e.clientY-h-14;
  tip.style.left=x+'px';tip.style.top=y+'px';
});
document.querySelectorAll('[data-toggle]').forEach(c=>c.addEventListener('change',()=>{
  document.getElementById('frame').classList.toggle('hide-'+c.dataset.toggle,!c.checked);
}));
"""

ENCODER_LABELS = {
    "squeeze264": "squeeze264 (this encoder)",
    "x264-matched": "x264, Baseline, matched tools",
    "x264-baseline": "x264, Baseline, medium preset",
    "x264-default": "x264 defaults (High profile)",
}
SLOTS = {"squeeze264": "--s1", "x264-matched": "--s2", "x264-baseline": "--s3", "x264-default": "--s4"}


def esc(s):
    return html.escape(str(s), quote=True)


def nice_ticks(lo, hi, n=5):
    span = hi - lo
    if span <= 0:
        return [lo]
    step = 10 ** math.floor(math.log10(span / n))
    for m in (1, 2, 2.5, 5, 10):
        if span / (step * m) <= n:
            step *= m
            break
    first = math.ceil(lo / step) * step
    out = []
    v = first
    while v <= hi + 1e-9:
        out.append(round(v, 6))
        v += step
    return out


def log_ticks(lo, hi):
    out = []
    e = math.floor(math.log10(lo))
    while 10 ** e <= hi:
        for m in (1, 2, 5):
            v = m * 10 ** e
            if lo <= v <= hi:
                out.append(v)
        e += 1
    return out


def fmt_num(v):
    if v >= 1000:
        return f"{v:,.0f}"
    if v >= 100:
        return f"{v:.0f}"
    return f"{v:g}"


def rd_chart(clip):
    """PSNR over bitrate (log scale), one line per encoder."""
    W, H, L, R, T, B = 460, 312, 44, 14, 26, 38
    pts = [(p["kbps"], p["psnr_y"]) for e in clip["encoders"].values() for p in e]
    xlo, xhi = min(p[0] for p in pts) * 0.9, max(p[0] for p in pts) * 1.1
    ylo, yhi = math.floor(min(p[1] for p in pts) - 0.5), math.ceil(max(p[1] for p in pts) + 0.5)
    sx = lambda v: L + (math.log(v) - math.log(xlo)) / (math.log(xhi) - math.log(xlo)) * (W - L - R)
    sy = lambda v: T + (yhi - v) / (yhi - ylo) * (H - T - B)
    o = [f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="PSNR against bitrate for {esc(clip["name"])}">']
    for t in nice_ticks(ylo, yhi, 6):
        o.append(f'<line class="axis" x1="{L}" x2="{W - R}" y1="{sy(t):.1f}" y2="{sy(t):.1f}"/>')
        o.append(f'<text x="{L - 6}" y="{sy(t) + 4:.1f}" text-anchor="end">{t:g}</text>')
    for t in log_ticks(xlo, xhi):
        o.append(f'<line class="axis" x1="{sx(t):.1f}" x2="{sx(t):.1f}" y1="{H - B}" y2="{H - B + 4}"/>')
        o.append(f'<text x="{sx(t):.1f}" y="{H - B + 16}" text-anchor="middle">{fmt_num(t)}</text>')
    o.append(f'<text x="{L}" y="{H - 4}">bitrate, kbit/s (log scale)</text>')
    o.append(f'<text x="{L - 34}" y="11">PSNR-Y, dB</text>')
    for name, points in clip["encoders"].items():
        col = f"var({SLOTS[name]})"
        path = " ".join(f"{'M' if i == 0 else 'L'}{sx(p['kbps']):.1f},{sy(p['psnr_y']):.1f}" for i, p in enumerate(points))
        o.append(f'<path class="line" d="{path}" stroke="{col}"/>')
    # Dots last so they sit above every line; each with a generous hit target.
    for name, points in clip["encoders"].items():
        col = f"var({SLOTS[name]})"
        for p in points:
            x, y = sx(p["kbps"]), sy(p["psnr_y"])
            tip = (f"{ENCODER_LABELS[name]}\n{'CRF' if name == 'x264-default' else 'QP'} {p['q']}: "
                   f"{p['kbps']:,.1f} kbit/s, {p['psnr_y']:.2f} dB\nSSIM {p['ssim_y']:.4f}, {p['fps']:g} fps")
            o.append(f'<circle class="dot" cx="{x:.1f}" cy="{y:.1f}" r="4" fill="{col}"/>')
            o.append(f'<circle class="hit" cx="{x:.1f}" cy="{y:.1f}" r="11" data-tip="{esc(tip)}"/>')
    o.append("</svg>")
    return "".join(o)


def bench_section(bench):
    o = ['<h2>Rate–distortion against x264</h2>']
    o.append(
        "<p>Each clip was encoded at four quantiser settings by this encoder and by x264 in three configurations. "
        "Quality is the luma PSNR of the decoded stream against the source, measured with ffmpeg's "
        "<code>psnr</code> filter for every encoder alike. Higher and further left is better.</p>")
    o.append('<div class="legend">' + "".join(
        f'<span><i class="key" style="background:var({SLOTS[k]})"></i>{esc(v)}</span>' for k, v in ENCODER_LABELS.items()) + "</div>")
    o.append('<div class="grid">')
    for clip in bench["clips"]:
        o.append(f'<div><h3>{esc(clip["name"].replace(".y4m", ""))}</h3>'
                 f'<div class="note">{clip["width"]}×{clip["height"]}, {clip["frames"]} frames, {clip["fps"]:.4g} fps</div>'
                 f'{rd_chart(clip)}</div>')
    o.append("</div>")
    o.append("<h3 style='margin-top:26px'>Bjøntegaard delta rate of squeeze264</h3>")
    o.append("<p class='note'>Extra bitrate this encoder needs for the same PSNR, averaged over the overlapping "
             "quality range (cubic fit of log-rate over PSNR). Positive means squeeze264 is worse.</p>")
    o.append("<table><tr><th>Clip</th><th>vs x264 matched tools</th><th>vs x264 Baseline medium</th><th>vs x264 defaults</th></tr>")
    for clip in bench["clips"]:
        bd = clip["bd_rate"]
        cell = lambda k: "n/a" if bd.get(k) is None else f"{bd[k]:+.1f}%"
        o.append(f"<tr><td>{esc(clip['name'].replace('.y4m', ''))}</td><td>{cell('x264-matched')}</td>"
                 f"<td>{cell('x264-baseline')}</td><td>{cell('x264-default')}</td></tr>")
    o.append("</table>")
    o.append("<details><summary>All measurements as a table</summary><table>"
             "<tr><th>Clip</th><th>Encoder</th><th>QP / CRF</th><th>kbit/s</th><th>PSNR-Y dB</th><th>SSIM-Y</th><th>fps</th></tr>")
    for clip in bench["clips"]:
        for name, points in clip["encoders"].items():
            for p in points:
                o.append(f"<tr><td>{esc(clip['name'].replace('.y4m', ''))}</td><td>{esc(name)}</td><td>{p['q']}</td>"
                         f"<td>{p['kbps']:,.1f}</td><td>{p['psnr_y']:.2f}</td><td>{p['ssim_y']:.4f}</td><td>{p['fps']:g}</td></tr>")
    o.append("</table></details>")
    o.append(f"<p class='note'>x264: <code>{esc(bench['x264_version'])}</code>. Matched tools: "
             f"<code>x264 {esc(' '.join(bench['x264_matched_args']))} --qp N</code>. Baseline medium: "
             f"<code>x264 {esc(' '.join(bench['x264_baseline_args']))} --qp N</code>. Defaults: "
             f"<code>x264 {esc(' '.join(bench['x264_default_args']))} --crf N</code>. "
             f"Speeds are single-threaded on a shared machine and include process start-up; measured {esc(bench['date'])}.</p>")
    return "".join(o)


def frame_charts(stats):
    frames = stats["frames"]
    n = len(frames)
    W, H, L, R, T, B = 1040, 224, 48, 10, 24, 30
    sx = lambda i: L + (i + 0.5) / n * (W - L - R)
    bw = max(1.0, min(24.0, (W - L - R) / n - (2 if n <= 150 else 1)))
    # --- frame sizes ---
    ymax = max(f["bytes"] for f in frames) * 8 / 1000
    yt = nice_ticks(0, ymax, 4)
    ytop = max(yt[-1], ymax)
    sy = lambda v: T + (ytop - v) / ytop * (H - T - B)
    o = ['<h3 style="margin-top:22px">Size of every frame</h3>']
    o.append('<div class="legend"><span><i class="swatch" style="background:var(--s2)"></i>I frame (intra only)</span>'
             '<span><i class="swatch" style="background:var(--s1)"></i>P frame (predicted from the previous one)</span></div>')
    o.append(f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="Frame sizes in kilobits">')
    for t in yt:
        o.append(f'<line class="axis" x1="{L}" x2="{W - R}" y1="{sy(t):.1f}" y2="{sy(t):.1f}"/>')
        o.append(f'<text x="{L - 6}" y="{sy(t) + 4:.1f}" text-anchor="end">{fmt_num(t)}</text>')
    for f in frames:
        kb = f["bytes"] * 8 / 1000
        x, y = sx(f["i"]) - bw / 2, sy(kb)
        h = H - B - y
        r = min(4, bw / 2, h)
        col = "var(--s2)" if f["type"] == "I" else "var(--s1)"
        tip = f"frame {f['i']} ({f['type']}), QP {f['qp']}\n{kb:.1f} kbit, PSNR-Y {f['psnr'][0]:.2f} dB\nskip {f['skip']}, inter {f['inter']}, intra {f['i4'] + f['i16']}"
        o.append(f'<path d="M{x:.2f},{H - B}V{y + r:.2f}q0,{-r:.2f} {r:.2f},{-r:.2f}h{bw - 2 * r:.2f}q{r:.2f},0 {r:.2f},{r:.2f}V{H - B}z" fill="{col}"/>')
        o.append(f'<rect class="hit" x="{L + f["i"] / n * (W - L - R):.2f}" y="{T}" width="{(W - L - R) / n:.2f}" height="{H - T - B}" data-tip="{esc(tip)}"/>')
    for t in nice_ticks(0, n - 1, 10):
        o.append(f'<text x="{sx(t):.1f}" y="{H - B + 15}" text-anchor="middle">{int(t)}</text>')
    o.append(f'<text x="{L}" y="{H - 2}">frame number</text><text x="4" y="11">kbit</text></svg>')
    # --- PSNR ---
    ys = [f["psnr"][0] for f in frames]
    ylo, yhi = math.floor(min(ys) - 0.3), math.ceil(max(ys) + 0.3)
    sy = lambda v: T + (yhi - v) / (yhi - ylo) * (H - T - B)
    o.append('<h3 style="margin-top:22px">Luma PSNR of every frame</h3>')
    o.append(f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="PSNR per frame">')
    for t in nice_ticks(ylo, yhi, 4):
        o.append(f'<line class="axis" x1="{L}" x2="{W - R}" y1="{sy(t):.1f}" y2="{sy(t):.1f}"/>')
        o.append(f'<text x="{L - 6}" y="{sy(t) + 4:.1f}" text-anchor="end">{t:g}</text>')
    path = " ".join(f"{'M' if i == 0 else 'L'}{sx(f['i']):.1f},{sy(f['psnr'][0]):.1f}" for i, f in enumerate(frames))
    o.append(f'<path class="line" d="{path}" stroke="var(--s1)"/>')
    for f in frames:
        tip = f"frame {f['i']} ({f['type']}): Y {f['psnr'][0]:.2f} dB, U {f['psnr'][1]:.2f}, V {f['psnr'][2]:.2f}"
        o.append(f'<rect class="hit" x="{L + f["i"] / n * (W - L - R):.2f}" y="{T}" width="{(W - L - R) / n:.2f}" height="{H - T - B}" data-tip="{esc(tip)}"/>')
    for t in nice_ticks(0, n - 1, 10):
        o.append(f'<text x="{sx(t):.1f}" y="{H - B + 15}" text-anchor="middle">{int(t)}</text>')
    o.append(f'<text x="{L}" y="{H - 2}">frame number</text><text x="4" y="11">dB</text></svg>')
    return "".join(o)


def png_gray(width, height, rows):
    """Minimal 8-bit greyscale PNG."""
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def read_y4m_luma(path, index):
    with open(path, "rb") as f:
        header = f.readline()
        fields = {t[:1]: t[1:] for t in header.decode("ascii").split()[1:]}
        w, h = int(fields["W"]), int(fields["H"])
        f.seek(len(header) + index * (6 + w * h * 3 // 2) + 6)
        data = f.read(w * h)
    return w, h, [data[y * w:(y + 1) * w] for y in range(h)]


SUB_SHAPES = {0: [(0, 0, 2, 2)], 1: [(0, 0, 2, 1), (0, 1, 2, 1)], 2: [(0, 0, 1, 2), (1, 0, 1, 2)],
              3: [(0, 0, 1, 1), (1, 0, 1, 1), (0, 1, 1, 1), (1, 1, 1, 1)]}
I4_NAMES = ["V", "H", "DC", "DDL", "DDR", "VR", "HD", "VL", "HU"]
PART_NAMES = ["16x16", "16x8", "8x16", "8x8"]


def partitions(mb):
    """(bx, by, w, h) in 4-sample units, decoding order."""
    part = mb["part"]
    if part == 0:
        return [(0, 0, 4, 4)]
    if part == 1:
        return [(0, 0, 4, 2), (0, 2, 4, 2)]
    if part == 2:
        return [(0, 0, 2, 4), (2, 0, 2, 4)]
    out = []
    for q in range(4):
        qx, qy = (q % 2) * 2, (q // 2) * 2
        out += [(qx + x, qy + y, w, h) for x, y, w, h in SUB_SHAPES[mb["sub"][q]]]
    return out


def frame_section(stats, recon):
    viz = stats["viz"]
    idx = viz["frame"]
    mb_w = stats["mb_w"]
    w, h, rows = read_y4m_luma(recon, idx)
    png = base64.b64encode(png_gray(w, h, rows)).decode("ascii")
    o = [f'<h2>Inside frame {idx}</h2>']
    o.append("<p>The decoded picture with the encoder's decisions drawn on top. Each 16×16 macroblock is either "
             "skipped (copied from the previous frame along a predicted motion vector, no bits beyond a counter), "
             "predicted from the previous frame with its own motion vectors, or coded from its neighbours inside "
             "the same frame (intra). Lines show where each block was taken from; hover a block for details.</p>")
    o.append('<div class="toggles">'
             '<label><input type="checkbox" checked data-toggle="types"> macroblock types</label>'
             '<label><input type="checkbox" checked data-toggle="vectors"> motion vectors</label>'
             '<label><input type="checkbox" checked data-toggle="parts"> partition borders</label></div>')
    o.append('<div class="legend">'
             '<span><i class="swatch" style="background:var(--s1)"></i>inter (motion compensated)</span>'
             '<span><i class="swatch" style="background:var(--s2)"></i>intra 4×4</span>'
             '<span><i class="swatch" style="background:var(--s3)"></i>intra 16×16</span>'
             '<span><i class="swatch" style="background:var(--s4)"></i>raw samples (I_PCM)</span>'
             '<span>skipped macroblocks are left untinted</span></div>')
    o.append(f'<div class="framewrap" id="frame"><svg viewBox="0 0 {w} {h}" role="img" aria-label="Frame {idx} with macroblock modes and motion vectors">')
    o.append(f'<image href="data:image/png;base64,{png}" width="{w}" height="{h}"/>')
    tints, borders, vectors, hits = [], [], [], []
    for i, mb in enumerate(viz["mbs"]):
        x0, y0 = (i % mb_w) * 16, (i // mb_w) * 16
        t = mb["t"]
        tip = f"macroblock ({i % mb_w}, {i // mb_w})  QP {mb['qp']}  {mb['bits']} bits\n"
        if t == "S":
            tip += f"skipped, vector ({mb['mv'][0] / 4:g}, {mb['mv'][1] / 4:g})"
            if mb["mv"] != [0, 0]:
                cx, cy = x0 + 8, y0 + 8
                vectors.append((cx, cy, cx + mb["mv"][0] / 4, cy + mb["mv"][1] / 4))
        elif t == "P":
            tints.append((x0, y0, "--s1"))
            parts = partitions(mb)
            tip += f"inter {PART_NAMES[mb['part']]}, {len(parts)} vector{'s' if len(parts) > 1 else ''}:"
            for bx, by, pw, ph in parts:
                mv = mb["mv"][by * 4 + bx]
                cx, cy = x0 + bx * 4 + pw * 2, y0 + by * 4 + ph * 2
                vectors.append((cx, cy, cx + mv[0] / 4, cy + mv[1] / 4))
                tip += f" ({mv[0] / 4:g}, {mv[1] / 4:g})"
                if pw < 4 or ph < 4:
                    borders.append((x0 + bx * 4, y0 + by * 4, pw * 4, ph * 4))
        elif t == "I4":
            tints.append((x0, y0, "--s2"))
            tip += "intra 4x4, modes: " + " ".join(I4_NAMES[m] for m in mb["modes"])
        elif t == "I16":
            tints.append((x0, y0, "--s3"))
            tip += "intra 16x16, mode " + ["vertical", "horizontal", "DC", "plane"][mb["mode"]]
        else:
            tints.append((x0, y0, "--s4"))
            tip += "raw samples (I_PCM)"
        hits.append(f'<rect class="hit" x="{x0}" y="{y0}" width="16" height="16" data-tip="{esc(tip)}"/>')
    o.append('<g class="t">' + "".join(
        f'<rect x="{x + 0.5}" y="{y + 0.5}" width="15" height="15" fill="var({c})" fill-opacity="0.38"/>' for x, y, c in tints) + "</g>")
    o.append('<g class="p" fill="none" stroke="#fff" stroke-opacity="0.55" stroke-width="0.4">' + "".join(
        f'<rect x="{x}" y="{y}" width="{pw}" height="{ph}"/>' for x, y, pw, ph in borders) + "</g>")
    # Vectors: a dark casing under a light line keeps them visible on any picture content.
    seg = "".join(f"M{a:g},{b:g}L{c:g},{d:g}" for a, b, c, d in vectors)
    dots = "".join(f'<circle cx="{a:g}" cy="{b:g}" r="0.9"/>' for a, b, _, _ in vectors)
    o.append(f'<g class="v"><path d="{seg}" stroke="#000" stroke-opacity="0.65" stroke-width="1.5" stroke-linecap="round" fill="none"/>'
             f'<path d="{seg}" stroke="#fff" stroke-width="0.6" stroke-linecap="round" fill="none"/><g fill="#fff">{dots}</g></g>')
    o.append("".join(hits))
    o.append("</svg></div>")
    f = stats["frames"][idx]
    o.append(f"<p class='note'>Frame {idx} is a {f['type']} frame at QP {f['qp']}: {f['bytes']:,} bytes, "
             f"{f['skip']} skipped, {f['inter']} inter and {f['i4'] + f['i16']} intra macroblocks "
             f"(partitions 16×16 / 16×8 / 8×16 / 8×8: {' / '.join(str(p) for p in f['parts'])}). "
             "A dot marks the centre of a block and the line ends where its prediction comes from in the previous frame.</p>")
    return "".join(o)


def encode_section(stats, recon, check):
    frames = stats["frames"]
    n = len(frames)
    mbs = stats["mb_w"] * stats["mb_h"] * n
    share = lambda k: 100 * sum(f[k] for f in frames) / mbs
    o = ["<h2>One encode in detail</h2>"]
    o.append(f"<p><code>{esc(stats['input'])}</code>, {stats['width']}×{stats['height']}, {n} frames at "
             f"{stats['fps']:.4g} fps, rate control: {esc(stats['rate_control'])}.</p>")
    tiles = [("Bitrate", f"{stats['kbps']:,.0f}", "kbit/s"), ("Luma PSNR", f"{stats['psnr'][0]:.2f}", "dB"),
             ("Encode speed", f"{stats['encode_fps']:.0f}", "frames/s"), ("Skipped macroblocks", f"{share('skip'):.0f}", "%")]
    o.append('<div class="tiles">' + "".join(
        f'<div class="tile"><div class="label">{esc(a)}</div><div class="value">{esc(b)}<span class="unit">{esc(c)}</span></div></div>'
        for a, b, c in tiles) + "</div>")
    o.append(f"<p class='note'>Macroblock types over the whole clip: skip {share('skip'):.1f}%, inter {share('inter'):.1f}%, "
             f"intra 4×4 {share('i4'):.1f}%, intra 16×16 {share('i16'):.1f}%, I_PCM {share('pcm'):.2f}%. "
             "PSNR is computed from the encoder's own reconstruction, which the check below shows to be exactly what decoders output.</p>")
    if check:
        o.append("<h3 style='margin-top:18px'>Decoder check for this encode</h3>")
        o.append(f"<pre>{esc(check.strip())}</pre>")
    o.append(frame_charts(stats))
    out = "".join(o)
    if "viz" in stats and recon:
        out += frame_section(stats, recon)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench")
    ap.add_argument("--stats")
    ap.add_argument("--recon")
    ap.add_argument("--check")
    ap.add_argument("--out", required=True)
    ap.add_argument("--title", default="squeeze264 results")
    args = ap.parse_args()
    body = [f"<h1>{esc(args.title)}</h1>",
            "<p>squeeze264 is an H.264 Constrained Baseline encoder written from the specification. "
            "This page shows how well it compresses compared with x264 and what it decided for one clip.</p>"]
    if args.stats:
        with open(args.stats) as f:
            stats = json.load(f)
        check = open(args.check).read() if args.check else None
        body.append(encode_section(stats, args.recon, check))
    if args.bench:
        with open(args.bench) as f:
            body.append(bench_section(json.load(f)))
    page = ("<!doctype html><html lang='en'><head><meta charset='utf-8'>"
            "<meta name='viewport' content='width=device-width,initial-scale=1'>"
            f"<title>{esc(args.title)}</title><style>{CSS}</style></head><body><main>"
            + "".join(body) + f"</main><div id='tip'></div><script>{JS}</script></body></html>")
    with open(args.out, "w") as f:
        f.write(page)
    print(f"wrote {args.out} ({len(page) // 1024} KiB)")


if __name__ == "__main__":
    main()
