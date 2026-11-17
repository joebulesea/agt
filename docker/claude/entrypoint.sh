#!/bin/sh
# agt-claude 容器的進入點：啟動 claude 前先檢查並更新到最新版。
#
# 新版安裝在 ~/.npm-global（位於 volume agt-claude-home 內），更新一次後之後都沿用，
# 只有 npm 上出現新版本時才會重新安裝。沒有網路或更新失敗時，直接使用現有版本。
# 多個容器同時啟動時（例如 agt 啟動時的背景更新與剛開的分頁）用鎖檔排隊，不會重複安裝。
# 設定環境變數 AGT_CLAUDE_UPDATE=0 可略過檢查。
set -u

PREFIX="$HOME/.npm-global"
export PATH="$PREFIX/bin:$PATH"

update() {
  current=$(claude --version 2>/dev/null | awk '{print $1}')
  latest=$(timeout 10 npm view @anthropic-ai/claude-code version 2>/dev/null)
  if [ -n "$latest" ] && [ "$latest" != "$current" ]; then
    echo "[agt] 更新 Claude Code ${current:-?} -> $latest ..."
    if timeout 300 npm install -g --prefix "$PREFIX" --no-audit --no-fund --loglevel=error \
         "@anthropic-ai/claude-code@$latest"; then
      echo "[agt] 更新完成：$(claude --version 2>/dev/null)"
    else
      echo "[agt] 更新失敗，使用現有版本 ${current:-?}"
    fi
  fi
}

if [ "${1:-}" = "claude" ] && [ "${AGT_CLAUDE_UPDATE:-1}" != "0" ]; then
  mkdir -p "$PREFIX"
  # fd 9 = 鎖檔；等前一個更新結束後再檢查一次版本（通常已是最新，直接略過）
  {
    flock -w 330 9 || echo "[agt] 等待其他更新逾時，略過檢查"
    update
  } 9>"$PREFIX/.agt-update.lock"
fi

exec "$@"
