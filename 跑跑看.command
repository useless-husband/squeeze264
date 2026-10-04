#!/bin/bash
# ============================================================
#  跑跑看：用自己寫的 H.264 編碼器壓一段影片
#
#  這個檔案在 Finder 裡雙擊就會打開「終端機」來執行。
#  它會做五件事：
#    1. 用 Rust 編譯編碼器（第一次大約 20 秒）。
#    2. 準備一段測試影片：標準測試片段 foreman（352x288、300 張畫面、約 45 MB）。
#       沒有的話會從 Xiph.org 下載；沒有網路就改用程式自己產生的合成畫面。
#    3. 把影片壓成 H.264，存成 out/demo.mp4。
#    4. 請 ffmpeg 和 Mac 內建的解碼器把壓好的檔案解開，
#       一張一張跟編碼器自己算出來的畫面比對，必須完全一樣。
#       （沒裝 ffmpeg 的話這一步會跳過。）
#    5. 做一份網頁報告，然後用 QuickTime Player 打開影片、用瀏覽器打開報告。
#
#  需要先裝好：
#    Rust（提供 cargo）：到 https://rustup.rs 照指示安裝
#    python3：在終端機執行 xcode-select --install
#    ffmpeg（選用，用來驗證）：brew install ffmpeg
# ============================================================

# 先切換到這個檔案所在的資料夾。資料夾名稱有空白和中文，
# 所以 "$(dirname "$0")" 一定要用雙引號包起來。
cd "$(dirname "$0")" || exit 1

# 讓「指令 | tail」這種寫法在前面的指令失敗時也算失敗。
set -o pipefail

pause_and_exit() {
  # 從終端機用 make demo 執行時不用等按鍵
  if [ -t 0 ]; then
    read -r -p "按 Enter 關閉視窗..."
  fi
  exit "$1"
}

# Homebrew 安裝的 rustup、ffmpeg 不一定在 PATH 裡，先幫忙加上。
for d in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin /opt/homebrew/bin /usr/local/bin; do
  case ":$PATH:" in
    *":$d:"*) ;;
    *) [ -d "$d" ] && PATH="$PATH:$d" ;;
  esac
done
export PATH

for tool in cargo python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "找不到 $tool。"
    echo "  cargo（Rust）：打開 https://rustup.rs ，照網頁上的一行指令安裝"
    echo "  python3：在終端機執行 xcode-select --install"
    pause_and_exit 1
  fi
done

BIN=target/release/squeeze264
mkdir -p out

echo "== 1/5 編譯編碼器（Rust，release 模式）..."
if ! cargo build --release -j 4 2>&1 | tail -3; then
  echo "編譯失敗，上面的訊息會說明原因。"
  pause_and_exit 1
fi
if [ ! -x "$BIN" ]; then
  echo "編譯失敗：找不到 $BIN"
  pause_and_exit 1
fi

echo
echo "== 2/5 準備測試影片"
CLIP=data/foreman_cif.y4m
VIZ=150
if [ ! -f "$CLIP" ]; then
  echo "   data/ 裡還沒有 foreman_cif.y4m，從 Xiph.org 下載（約 45 MB）..."
  if ! python3 tools/fetch_data.py --only foreman_cif.y4m; then
    echo "   下載失敗（可能沒有網路）。改用程式產生的合成畫面，一樣可以看到完整流程。"
    CLIP=out/synthetic.y4m
    VIZ=20
    "$BIN" gen "$CLIP" --size 352x288 --frames 90 --fps 30 || pause_and_exit 1
  fi
fi
echo "   使用 $CLIP"

echo
echo "== 3/5 壓縮成 H.264（固定量化參數 QP 28，數字越大壓得越小、畫質越差）"
if ! "$BIN" encode "$CLIP" -o out/demo.mp4 --qp 28 --recon out/recon.y4m \
      --stats out/stats.json --viz-frame "$VIZ"; then
  echo "編碼失敗。"
  pause_and_exit 1
fi
RAW=$(wc -c < "$CLIP" | tr -d ' ')
ENC=$(wc -c < out/demo.mp4 | tr -d ' ')
echo "   原始大小 $((RAW / 1024)) KB → 壓縮後 $((ENC / 1024)) KB（約 $((RAW / ENC)) 分之一）"

echo
echo "== 4/5 驗證：別人的解碼器解出來的畫面，要和編碼器自己算的完全一樣"
CHECK_ARGS=()
if command -v ffmpeg >/dev/null 2>&1; then
  if "$BIN" check "$CLIP" --qp 28 -q | tee out/check.txt; then
    echo "   PASS = 每一張畫面的每一個像素都相同。"
    CHECK_ARGS=(--check out/check.txt)
  else
    echo "   驗證沒有通過！這代表編碼器有錯，請把上面的訊息回報。"
    pause_and_exit 1
  fi
else
  echo "   沒有安裝 ffmpeg，跳過驗證。（安裝方式：brew install ffmpeg）"
fi

echo
echo "== 5/5 產生報告"
BENCH_ARGS=()
[ -f docs/bench.json ] && BENCH_ARGS=(--bench docs/bench.json)
if ! python3 tools/report.py "${BENCH_ARGS[@]}" --stats out/stats.json --recon out/recon.y4m \
      "${CHECK_ARGS[@]}" --out out/report.html --title "squeeze264 跑跑看結果"; then
  echo "報告產生失敗。"
  pause_and_exit 1
fi

echo
echo "完成。"
echo "  影片：out/demo.mp4（用 QuickTime Player 打開）"
echo "  報告：out/report.html（每張畫面用了多少位元、移動向量圖、和 x264 的比較）"
if [ -z "${SQUEEZE264_NO_OPEN:-}" ] && command -v open >/dev/null 2>&1; then
  open out/demo.mp4
  open out/report.html
fi
pause_and_exit 0
