#!/bin/sh
# agt-copilot 容器的進入點：決定要用哪一份 Copilot CLI，必要時更新。
#
# 新版安裝在 /opt/agt-tools（volume agt-copilot-tools）。這個 volume 只有「更新用」的容器
# 掛成可寫（設定檔的 update 指令，agt daemon 啟動時執行）；分頁的容器一律掛成唯讀（:ro），
# 所以分頁裡的 Agent 改不到之後所有分頁都會執行的 copilot 執行檔。
#
#   - /opt/agt-tools 是可寫的 volume → 檢查 npm 上的最新版並安裝（更新用容器）
#   - 唯讀                           → 直接使用裡面的版本（分頁）
#   - 沒有掛載                       → 使用映像檔內建的版本
#
# 多個更新同時進行時用鎖檔排隊。設定環境變數 AGT_COPILOT_UPDATE=0 可略過檢查。
# 設定 AGT_COPILOT_VERSION=<版本> 時只安裝那個版本（例如審視過的版本），不追最新版。
set -u

TOOLS=/opt/agt-tools
export PATH="$TOOLS/bin:$PATH"

# 全域指示：告訴 Copilot 這是沒有 git 的沙箱（只在檔案不存在時建立，不覆蓋使用者的內容）
RULES="$HOME/.copilot/copilot-instructions.md"
if [ ! -s "$RULES" ]; then
  mkdir -p "$HOME/.copilot"
  cat >"$RULES" <<'RULES_EOF'
你在 Docker 沙箱中執行，目前所在的目錄就是專案目錄。容器內無法使用 git，不要執行或修復任何 git 指令；只修改檔案，commit 與合併由外部負責。
RULES_EOF
fi

update() {
  current=$(copilot --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1)
  latest=${AGT_COPILOT_VERSION:-$(timeout 10 npm view @github/copilot version 2>/dev/null)}
  if [ -n "$latest" ] && [ "$latest" != "$current" ]; then
    echo "[agt] 更新 Copilot CLI ${current:-?} -> $latest ..."
    if timeout 300 npm install -g --prefix "$TOOLS" --no-audit --no-fund --loglevel=error \
         "@github/copilot@$latest"; then
      echo "[agt] 更新完成：$(copilot --version 2>/dev/null | head -n 1)"
    else
      echo "[agt] 更新失敗，使用現有版本 ${current:-?}"
    fi
  fi
}

if [ "${1:-}" = "copilot" ] && [ "${AGT_COPILOT_UPDATE:-1}" != "0" ]; then
  if ! mountpoint -q "$TOOLS"; then
    echo "[agt] 沒有掛載 agt-copilot-tools，使用映像檔內建的 Copilot CLI（設定方式見 config/default.toml 的 copilot-docker 範例）"
  elif [ -w "$TOOLS" ]; then
    {
      flock -w 330 9 || echo "[agt] 等待其他更新逾時，略過檢查"
      update
    } 9>"$TOOLS/.agt-update.lock"
  fi
fi

exec "$@"
