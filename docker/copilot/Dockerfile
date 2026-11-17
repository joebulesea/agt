# agt 的 copilot-docker Agent 用的映像檔：在容器中執行 GitHub Copilot CLI（copilot）。
# 建置：docker build -t agt-copilot docker/copilot/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料與設定保存在 volume `agt-copilot-home`（掛在 /home/node），只需要登入一次。
# 專案需要的工具（Rust、Python 套件等）請自行加在下方。
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential util-linux \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g @github/copilot && npm cache clean --force

# 啟動前自動更新 Copilot CLI（見 entrypoint.sh；AGT_COPILOT_UPDATE=0 可略過）
COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

USER node
ENV HOME=/home/node \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    NPM_CONFIG_UPDATE_NOTIFIER=false
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["copilot"]
