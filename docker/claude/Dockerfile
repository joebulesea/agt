# agt 的 claude-docker Agent 用的映像檔：在容器中執行 Claude Code。
# 建置：docker build -t agt-claude docker/claude/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料保存在 volume `agt-claude-home`（掛在 /home/node），只需要登入一次。
# 專案需要的工具（Rust、Python 套件等）請自行加在下方。
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code && npm cache clean --force

# 啟動前自動更新 Claude Code（見 entrypoint.sh；AGT_CLAUDE_UPDATE=0 可略過）
COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

# 以非 root 身分執行（--dangerously-skip-permissions 不允許 root）
USER node
ENV HOME=/home/node \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    # 告訴 Claude Code 這是隔離環境
    IS_SANDBOX=1 \
    # 更新改由 entrypoint 在啟動前處理（內建更新器沒有權限寫入全域 npm 目錄）
    DISABLE_AUTOUPDATER=1 \
    NPM_CONFIG_UPDATE_NOTIFIER=false
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["claude"]
