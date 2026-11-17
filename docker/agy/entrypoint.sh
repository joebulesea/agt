#!/bin/sh
# agt-agy 容器的進入點：啟動 agy 前先確認已安裝並更新到最新版。
#
# agy 安裝在 ~/.local/bin（位於 volume agt-agy-home 內），第一次從映像檔內的 /opt/agy 複製過去，
# 之後由 `agy update` 更新。沒有網路或更新失敗時，直接使用現有版本。
# 多個容器同時啟動時用鎖檔排隊，不會同時更新。
# 設定環境變數 AGT_AGY_UPDATE=0 可略過更新。
set -u

BIN="$HOME/.local/bin"
mkdir -p "$BIN"
if [ ! -x "$BIN/agy" ]; then
  cp /opt/agy/agy "$BIN/agy"
fi

# 全域規則：告訴 agy 這是沒有 git 的沙箱（只在檔案不存在時建立，不覆蓋使用者的內容）
RULES="$HOME/.gemini/GEMINI.md"
if [ ! -s "$RULES" ]; then
  mkdir -p "$HOME/.gemini"
  cat >"$RULES" <<'EOF'
你在 Docker 沙箱中執行，目前所在的目錄就是專案目錄。容器內無法使用 git，不要執行或修復任何 git 指令；只修改檔案，commit 與合併由外部負責。
EOF
fi

if [ "${1:-}" = "agy" ] && [ "${AGT_AGY_UPDATE:-1}" != "0" ]; then
  {
    if flock -w 200 9; then
      before=$(agy --version 2>/dev/null)
      timeout 180 agy update >/dev/null 2>&1 || echo "[agt] agy 更新失敗，使用現有版本 ${before:-?}"
      after=$(agy --version 2>/dev/null)
      [ "$before" != "$after" ] && echo "[agt] agy 已更新：${before:-?} -> $after"
    else
      echo "[agt] 等待其他更新逾時，略過"
    fi
  } 9>"$BIN/.agt-update.lock"
fi

exec "$@"
