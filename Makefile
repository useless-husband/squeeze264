# All paths are relative: the repository may live in a directory whose name
# contains spaces or non-ASCII characters.
CARGO ?= cargo
PY ?= python3
JOBS ?= 4
BIN := target/release/squeeze264
CLIPS := foreman_cif akiyo_cif mobile_cif shields_720p_100f

.PHONY: build test lint data check bench report avcheck demo clean

build:
	$(CARGO) build --release -j $(JOBS)

# Unit tests, encoder behaviour tests, CLI tests and the bit-exact
# conformance suite. The conformance tests need ffmpeg on PATH and skip
# with a message without it; tests on real clips skip until `make data`.
test:
	$(CARGO) test --release -j $(JOBS)

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --release -j $(JOBS) --all-targets -- -D warnings
	$(PY) -m py_compile tools/bench.py tools/report.py tools/fetch_data.py

# Standard test clips from the Xiph.org collection (about 275 MB), SHA-256 checked.
data:
	$(PY) tools/fetch_data.py

# Encode every clip at two QPs and in bitrate mode; decode with ffmpeg
# (software) and VideoToolbox; require identical frames.
check: build
	@for c in $(CLIPS); do \
		for args in "--qp 22" "--qp 37" "--bitrate 800"; do \
			echo "== $$c $$args"; \
			$(BIN) check data/$$c.y4m $$args -q || exit 1; \
		done; \
	done

# Rate-distortion comparison against x264 (about 3 minutes, one core).
bench: build
	$(PY) tools/bench.py --out docs/bench.json

# Rebuild docs/results.html from docs/bench.json and a fresh encode of foreman.
report: build
	mkdir -p out
	$(BIN) check data/foreman_cif.y4m --qp 28 --stats out/stats.json --viz-frame 150 -q > out/check.txt
	$(BIN) encode data/foreman_cif.y4m -o out/foreman.mp4 --qp 28 --recon out/recon.y4m -q
	$(PY) tools/report.py --bench docs/bench.json --stats out/stats.json --recon out/recon.y4m \
		--check out/check.txt --out docs/results.html

# macOS: open the MP4 with AVFoundation (what QuickTime Player uses) and
# compare every decoded frame with the encoder's reconstruction.
avcheck: build
	sh tools/avcheck.sh

demo:
	sh ./跑跑看.command

clean:
	$(CARGO) clean
	rm -rf out
