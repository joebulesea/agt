#!/bin/sh
# agt-claude 容器的進入點：決定要用哪一份 Claude Code，必要時更新。
#
# 新版安裝在 /opt/agt-tools（volume agt-claude-tools）。這個 volume 只有「更新用」的容器
# 掛成可寫（設定檔的 update 指令，agt daemon 啟動時執行）；分頁的容器一律掛成唯讀（:ro），
# 所以分頁裡的 Agent 改不到之後所有分頁都會執行的 claude 執行檔。
#
#   - /opt/agt-tools 是可寫的 volume → 檢查 npm 上的最新版並安裝（更新用容器）
#   - 唯讀                           → 直接使用裡面的版本（分頁）
#   - 沒有掛載                       → 使用映像檔內建的版本
#
# 多個更新同時進行時用鎖檔排隊。設定環境變數 AGT_CLAUDE_UPDATE=0 可略過檢查。
set -u

TOOLS=/opt/agt-tools
export PATH="$TOOLS/bin:$PATH"

update() {
  current=$(claude --version 2>/dev/null | awk '{print $1}')
  latest=$(timeout 10 npm view @anthropic-ai/claude-code version 2>/dev/null)
  if [ -n "$latest" ] && [ "$latest" != "$current" ]; then
    echo "[agt] 更新 Claude Code ${current:-?} -> $latest ..."
    if timeout 300 npm install -g --prefix "$TOOLS" --no-audit --no-fund --loglevel=error \
         "@anthropic-ai/claude-code@$latest"; then
      echo "[agt] 更新完成：$(claude --version 2>/dev/null)"
    else
      echo "[agt] 更新失敗，使用現有版本 ${current:-?}"
    fi
  fi
}

if [ "${1:-}" = "claude" ] && [ "${AGT_CLAUDE_UPDATE:-1}" != "0" ]; then
  if ! mountpoint -q "$TOOLS"; then
    echo "[agt] 沒有掛載 agt-claude-tools，使用映像檔內建的 Claude Code（設定方式見 config/default.toml 的 claude-docker 範例）"
  elif [ -w "$TOOLS" ]; then
    # fd 9 = 鎖檔；等前一個更新結束後再檢查一次版本（通常已是最新，直接略過）
    {
      flock -w 330 9 || echo "[agt] 等待其他更新逾時，略過檢查"
      update
    } 9>"$TOOLS/.agt-update.lock"
  fi
fi

exec "$@"
