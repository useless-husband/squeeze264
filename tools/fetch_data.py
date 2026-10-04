#!/usr/bin/env python3
"""Download the test clips into data/ and verify their SHA-256.

The clips come from the Xiph.org "derf" collection of standard test
sequences (https://media.xiph.org/video/derf/). They are not committed to
the repository. The 720p clip is large, so only its first frames are
fetched with an HTTP range request; the checksum covers exactly that prefix.

Usage: python3 tools/fetch_data.py [--only NAME] [--dir data]
"""
import argparse
import hashlib
import os
import subprocess
import sys

BASE = "https://media.xiph.org/video/derf/y4m/"

# name -> (remote file, frames to keep or None for the whole file, sha256)
CLIPS = {
    "foreman_cif.y4m": ("foreman_cif.y4m", None,
                        "b0e3c35eff9fe1da91521a02eb9f0e3ef4bc5aec341ecef1567cd87f1711267b"),
    "akiyo_cif.y4m": ("akiyo_cif.y4m", None,
                      "0f7a1f997930217d601ab324b1428859fdb599dde8d33772a24bfe2803a9b067"),
    "mobile_cif.y4m": ("mobile_cif.y4m", None,
                       "8b32924ea00ef3bd52f7ddafd3ceb55d2bb331610b59957c5c3cc9628f9e0d90"),
    "shields_720p_100f.y4m": ("720p50_shields_ter.y4m", 100,
                              "a3a0e7f07564a55360d7903cd27319af2e8b3e08723ff96ee34d9fd4174189f8"),
}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def curl(url, byte_range, out_path=None):
    """Runs curl (system certificate store, works where Python's does not)."""
    cmd = ["curl", "-fsSL", "--retry", "3"]
    if byte_range:
        cmd += ["-r", byte_range]
    if out_path:
        cmd += ["-o", out_path]
    r = subprocess.run(cmd + [url], capture_output=out_path is None)
    if r.returncode != 0:
        raise OSError(f"curl exited with status {r.returncode}")
    return r.stdout if out_path is None else None


def prefix_length(url, frames):
    """Byte length of the Y4M header plus the first `frames` frames."""
    head = curl(url, "0-1023")
    line = head[:head.index(b"\n") + 1]
    fields = {t[:1]: t[1:] for t in line.decode("ascii").split()[1:]}
    w, h = int(fields["W"]), int(fields["H"])
    return len(line) + frames * (len(b"FRAME\n") + w * h * 3 // 2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="data")
    ap.add_argument("--only", action="append")
    args = ap.parse_args()
    os.makedirs(args.dir, exist_ok=True)
    failed = False
    for name, (remote, frames, digest) in CLIPS.items():
        if args.only and name not in args.only:
            continue
        path = os.path.join(args.dir, name)
        if os.path.exists(path) and sha256(path) == digest:
            print(f"ok       {name}")
            continue
        url = BASE + remote
        print(f"fetching {name} from {url}", flush=True)
        tmp = path + ".part"
        try:
            if frames is None:
                curl(url, None, tmp)
            else:
                curl(url, f"0-{prefix_length(url, frames) - 1}", tmp)
        except OSError as e:
            print(f"FAILED   {name}: {e}", file=sys.stderr)
            failed = True
            continue
        got = sha256(tmp)
        if got != digest:
            print(f"FAILED   {name}: sha256 {got}, expected {digest}", file=sys.stderr)
            os.replace(tmp, path + ".bad")
            failed = True
            continue
        os.replace(tmp, path)
        print(f"ok       {name}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
