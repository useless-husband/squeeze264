#!/usr/bin/env python3
"""Mutation check: plant one-line bugs in normative code and require the
test suite to notice each of them.

Every mutant changes something a decoder would disagree with (a table
entry, a rounding constant, a neighbour rule). The suite passes on the
original code; it must fail on every mutant. Works on a temporary copy of
the repository. Needs cargo and ffmpeg. Takes a few minutes.

Usage: python3 tools/mutants.py [--write docs/mutants.md] [--only N]
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time

# (file, original text, replacement, what the bug is)
MUTANTS = [
    ("src/tables.rs", '["0000000111", "000000110", "00000101", "000011"]',
     '["0000000110", "000000111", "00000101", "000011"]',
     "coeff_token table (nC 0-1): two code words of TotalCoeff 4 exchange their last bits"),
    ("src/tables.rs", "0, 16, 1, 2, 4, 8, 32, 3,", "0, 16, 2, 1, 4, 8, 32, 3,",
     "coded_block_pattern mapping: two inter entries swapped"),
    ("src/tables.rs", "[13, 17, 25],", "[13, 17, 24],", "deblocking tC0 table: last entry off by one"),
    ("src/tables.rs", '&["0000", "0001", "01", "1", "001"],', '&["0001", "0000", "01", "1", "001"],',
     "total_zeros table (TotalCoeff 12): two code words swapped"),
    ("src/transform.rs", "out[c] = (g0 + g3 + 32) >> 6;", "out[c] = (g0 + g3 + 31) >> 6;",
     "inverse transform: rounding constant 31 instead of 32 (first row)"),
    ("src/transform.rs", "(*v * ls + (1 << (5 - per))) >> (6 - per)", "(*v * ls + (1 << (4 - per))) >> (6 - per)",
     "Intra16x16 DC scaling: wrong rounding term (only visible below QP 12)"),
    ("src/inter.rs", "hp.data[i] = ((v + 16) >> 5).clamp(0, 255) as u8;", "hp.data[i] = ((v + 15) >> 5).clamp(0, 255) as u8;",
     "luma half-sample filter: rounding 15 instead of 16"),
    ("src/inter.rs", "d[i] = ((v + 32) >> 6) as u8;", "d[i] = ((v + 31) >> 6) as u8;",
     "chroma interpolation: rounding 31 instead of 32"),
    ("src/deblock.rs", "let small_gap = (p0 - q0).abs() < ((alpha >> 2) + 2);", "let small_gap = (p0 - q0).abs() <= ((alpha >> 2) + 2);",
     "deblocking bS 4: strong-filter condition uses <= instead of <"),
    ("src/deblock.rs", "if (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4 {",
     "if (a[0] as i32 - b[0] as i32).abs() > 4 || (a[1] as i32 - b[1] as i32).abs() >= 4 {",
     "deblocking bS 1: horizontal vector difference threshold off by one"),
    ("src/mvpred.rs", "if count == 1 {", "if count == 3 {",
     "motion vector prediction: single-matching-neighbour rule disabled"),
    ("src/mvpred.rs", "BLK_IDX[(by - 1) * 4 + cx] < BLK_IDX[by * 4 + bx]", "(by - 1) * 4 + cx < by * 4 + bx",
     "motion vector prediction: above-right availability by raster instead of decoding order"),
    ("src/cavlc.rs", "let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };",
     "let mut suffix_len: u32 = if total > 9 && t1 < 3 { 1 } else { 0 };",
     "CAVLC: initial suffixLength rule uses TotalCoeff > 9"),
    ("src/intra.rs", "let b = (34 * h + 32) >> 6;", "let b = (33 * h + 32) >> 6;",
     "chroma plane prediction: gradient factor 33 instead of 34"),
    ("src/intra.rs", "(t(6) + 3 * t(7) + 2) >> 2", "(t(6) + 2 * t(7) + t(7) + 1) >> 2",
     "Intra4x4 diagonal-down-left: corner sample rounding"),
    ("src/mb.rs", "self.st.i4mode[y4 * w4 + x4 - 1].min(self.st.i4mode[(y4 - 1) * w4 + x4])",
     "self.st.i4mode[y4 * w4 + x4 - 1].max(self.st.i4mode[(y4 - 1) * w4 + x4])",
     "Intra4x4 mode prediction: max instead of min of the neighbours"),
    ("src/mb.rs", "(Some(a), Some(b)) => (a + b + 1) >> 1,", "(Some(a), Some(b)) => (a + b) >> 1,",
     "CAVLC context nC: average of neighbours rounded down"),
    ("src/mb.rs", "        self.st.qp[mb] = 0;\n", "        self.st.qp[mb] = self.prev_qp;\n",
     "I_PCM macroblocks deblocked with the running QP instead of QP 0"),
    ("src/mb.rs", "BLK_IDX[(by - 1) * 4 + bx + 1] < BLK_IDX[by * 4 + bx]", "bx < 3",
     "Intra4x4: above-right samples treated as available whenever inside the macroblock"),
    ("src/encoder.rs", "            if c.is_p && c.skip_run > 0 {", "            if c.is_p && c.skip_run > 1 {",
     "slice data: a single trailing skipped macroblock loses its mb_skip_run"),
    ("src/headers.rs", "w.put(LOG2_MAX_FRAME_NUM, h.frame_num & ((1 << LOG2_MAX_FRAME_NUM) - 1));",
     "w.put(LOG2_MAX_FRAME_NUM, (h.frame_num & 127) & ((1 << LOG2_MAX_FRAME_NUM) - 1));",
     "slice header: frame_num wraps at 128 instead of 256"),
    ("src/bitstream.rs", "if zeros >= 2 && b <= 3 {\n                bytes.push(3);", "if zeros >= 2 && b <= 2 {\n                bytes.push(3);",
     "NAL escaping: 00 00 03 in the payload is not escaped"),
]


def run_tests(workdir):
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(workdir, "target"))
    r = subprocess.run(["cargo", "test", "--release", "--no-fail-fast", "-j", "4"], cwd=workdir, env=env,
                       capture_output=True, text=True)
    out = r.stdout + r.stderr
    failed = sorted({line.split()[1] for line in out.splitlines()
                     if line.startswith("test ") and line.rstrip().endswith("FAILED")})
    if r.returncode != 0 and not failed:
        failed = ["(build error)"] if "error[" in out or "error:" in out else ["(unknown failure)"]
    return r.returncode == 0, failed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--write")
    ap.add_argument("--only", type=int)
    args = ap.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    work = tempfile.mkdtemp(prefix="squeeze264-mutants-")
    try:
        for item in ("src", "tests", "Cargo.toml", "Cargo.lock", "rustfmt.toml"):
            src = os.path.join(root, item)
            if os.path.isdir(src):
                shutil.copytree(src, os.path.join(work, item))
            elif os.path.exists(src):
                shutil.copy(src, work)
        ok, failed = run_tests(work)
        if not ok:
            sys.exit(f"the unmodified code does not pass its tests: {failed}")
        print("baseline: all tests pass", flush=True)
        rows = []
        survivors = 0
        for i, (path, old, new, what) in enumerate(MUTANTS, 1):
            if args.only and i != args.only:
                continue
            full = os.path.join(work, path)
            text = open(full).read()
            if text.count(old) != 1:
                sys.exit(f"mutant {i}: pattern occurs {text.count(old)} times in {path}; update tools/mutants.py")
            open(full, "w").write(text.replace(old, new))
            t0 = time.time()
            ok, failed = run_tests(work)
            open(full, "w").write(text)
            verdict = "SURVIVED" if ok else "caught"
            survivors += ok
            unit = [f for f in failed if "::tests::" in f]
            other = [f for f in failed if "::tests::" not in f]
            rows.append((i, path, what, verdict, len(unit), len(other)))
            print(f"{i:2} {verdict:8} {path:18} {what}  [unit tests failing: {len(unit)}, "
                  f"integration tests failing: {len(other)}] {time.time() - t0:.0f}s", flush=True)
        if args.write:
            with open(os.path.join(root, args.write), "w") as f:
                f.write("# Mutation check\n\n")
                f.write("Generated by `python3 tools/mutants.py --write docs/mutants.md`. Each row is a one-line bug planted in\n"
                        "normative code; the test suite (`cargo test --release`) must fail. \"Unit\" counts failing tests inside\n"
                        "`src/`, \"integration\" counts failing tests under `tests/` (mostly the bit-exact decode against ffmpeg\n"
                        "and VideoToolbox).\n\n")
                f.write("| # | File | Planted bug | Result | Unit | Integration |\n|---|---|---|---|---|---|\n")
                for i, path, what, verdict, unit, other in rows:
                    f.write(f"| {i} | `{path}` | {what} | {verdict} | {unit} | {other} |\n")
                f.write(f"\n{len(rows) - survivors} of {len(rows)} mutants caught.\n")
        print(f"{len(rows) - survivors} of {len(rows)} mutants caught")
        return 1 if survivors else 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
