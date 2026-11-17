#!/bin/sh
# agt-agy 容器的進入點：決定要用哪一份 agy，必要時更新。
#
# agy 放在 /opt/agt-tools/bin（volume agt-agy-tools），第一次從映像檔內的 /opt/agy 複製過去，
# 之後由 `agy update` 更新。這個 volume 只有「更新用」的容器掛成可寫（設定檔的 update 指令，
# agt daemon 啟動時執行）；分頁的容器一律掛成唯讀（:ro），所以分頁裡的 Agent 改不到
# 之後所有分頁都會執行的 agy 執行檔。
#
#   - /opt/agt-tools 是可寫的 volume → 安裝並執行 agy update（更新用容器）
#   - 唯讀                           → 直接使用裡面的版本（分頁）；還沒有就用映像檔內建的
#   - 沒有掛載                       → 使用映像檔內建的版本
#
# 多個更新同時進行時用鎖檔排隊。設定環境變數 AGT_AGY_UPDATE=0 可略過更新。
set -u

TOOLS=/opt/agt-tools
export PATH="$TOOLS/bin:/opt/agy:$PATH"

# 全域規則：告訴 agy 這是沒有 git 的沙箱（只在檔案不存在時建立，不覆蓋使用者的內容）
RULES="$HOME/.gemini/GEMINI.md"
if [ ! -s "$RULES" ]; then
  mkdir -p "$HOME/.gemini"
  cat >"$RULES" <<'EOF'
你在 Docker 沙箱中執行，目前所在的目錄就是專案目錄。容器內無法使用 git，不要執行或修復任何 git 指令；只修改檔案，commit 與合併由外部負責。
EOF
fi

if [ "${1:-}" = "agy" ] && [ "${AGT_AGY_UPDATE:-1}" != "0" ]; then
  if ! mountpoint -q "$TOOLS"; then
    echo "[agt] 沒有掛載 agt-agy-tools，使用映像檔內建的 agy（設定方式見 config/default.toml 的 agy-docker 範例）"
  elif [ -w "$TOOLS" ]; then
    mkdir -p "$TOOLS/bin"
    {
      if flock -w 200 9; then
        [ -x "$TOOLS/bin/agy" ] || cp /opt/agy/agy "$TOOLS/bin/agy"
        before=$(agy --version 2>/dev/null)
        timeout 180 agy update >/dev/null 2>&1 || echo "[agt] agy 更新失敗，使用現有版本 ${before:-?}"
        after=$(agy --version 2>/dev/null)
        [ "$before" != "$after" ] && echo "[agt] agy 已更新：${before:-?} -> $after"
      else
        echo "[agt] 等待其他更新逾時，略過"
      fi
    } 9>"$TOOLS/.agt-update.lock"
  fi
fi

exec "$@"
