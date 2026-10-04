#!/bin/sh
# Encodes a clip to MP4, decodes it with AVFoundation (tools/avcheck.swift)
# and compares the MD5 of every decoded frame with the encoder's own
# reconstruction. macOS only. Usage: sh tools/avcheck.sh [clip.y4m]
set -eu
cd "$(dirname "$0")/.."
BIN=target/release/squeeze264
mkdir -p out
CLIP="${1:-}"
if [ -z "$CLIP" ]; then
  if [ -f data/foreman_cif.y4m ]; then
    CLIP=data/foreman_cif.y4m
  else
    CLIP=out/avcheck-synth.y4m
    "$BIN" gen "$CLIP" --size 352x288 --frames 60 -q
  fi
fi
"$BIN" encode "$CLIP" -o out/avcheck.mp4 --framemd5 out/avcheck.md5 -q
swift tools/avcheck.swift out/avcheck.mp4 out/avcheck.yuv
python3 - "$CLIP" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as f:
    fields = {t[:1]: t[1:] for t in f.readline().decode().split()[1:]}
n = int(fields["W"]) * int(fields["H"]) * 3 // 2
want = open("out/avcheck.md5").read().split()
data = open("out/avcheck.yuv", "rb").read()
got = [hashlib.md5(data[i:i + n]).hexdigest() for i in range(0, len(data), n)]
same = sum(a == b for a, b in zip(want, got))
print(f"AVFoundation decoded {len(got)} frames; {same}/{len(want)} identical to the encoder's reconstruction")
sys.exit(0 if got == want else 1)
PY
