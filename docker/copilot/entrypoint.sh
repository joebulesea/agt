#!/bin/sh
# agt-copilot 容器的進入點：啟動 copilot 前先檢查並更新到最新版。
#
# 新版安裝在 ~/.npm-global（位於 volume agt-copilot-home 內），更新一次後之後都沿用，
# 只有 npm 上出現新版本時才會重新安裝。沒有網路或更新失敗時，直接使用現有版本。
# 多個容器同時啟動時用鎖檔排隊，不會重複安裝。
# 設定環境變數 AGT_COPILOT_UPDATE=0 可略過檢查。
set -u

PREFIX="$HOME/.npm-global"
export PATH="$PREFIX/bin:$PATH"

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
  latest=$(timeout 10 npm view @github/copilot version 2>/dev/null)
  if [ -n "$latest" ] && [ "$latest" != "$current" ]; then
    echo "[agt] 更新 Copilot CLI ${current:-?} -> $latest ..."
    if timeout 300 npm install -g --prefix "$PREFIX" --no-audit --no-fund --loglevel=error \
         "@github/copilot@$latest"; then
      echo "[agt] 更新完成：$(copilot --version 2>/dev/null | head -n 1)"
    else
      echo "[agt] 更新失敗，使用現有版本 ${current:-?}"
    fi
  fi
}

if [ "${1:-}" = "copilot" ] && [ "${AGT_COPILOT_UPDATE:-1}" != "0" ]; then
  mkdir -p "$PREFIX"
  {
    flock -w 330 9 || echo "[agt] 等待其他更新逾時，略過檢查"
    update
  } 9>"$PREFIX/.agt-update.lock"
fi

exec "$@"
