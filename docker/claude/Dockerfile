# agt 的 claude-docker Agent 用的映像檔：在容器中執行 Claude Code。
# 建置：docker build -t agt-claude docker/claude/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料保存在 volume `agt-claude-home`（掛在 /home/node），只需要登入一次。
# Claude Code 的更新版保存在 volume `agt-claude-tools`（掛在 /opt/agt-tools，分頁中唯讀）。
# 專案需要的工具（Rust、Python 套件等）請自行加在下方。
# base image 釘在 digest（tag 可能被改指到別的內容）；更新時改 digest，或交給 Dependabot。
FROM node:26-bookworm-slim@sha256:5df79989ed685e09f0a98019db4cebfb22fb9df1a53c0161664a011f7ff6b0f1

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential \
 && rm -rf /var/lib/apt/lists/*

# 版本：預設 latest；要固定時 docker build --build-arg CLAUDE_CODE_VERSION=<版本>
ARG CLAUDE_CODE_VERSION=latest
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" && npm cache clean --force

# Claude Code 的更新版放在 volume agt-claude-tools（掛在 /opt/agt-tools）。目錄先建好並交給執行身分，
# 新的 volume 第一次掛載時會沿用這個擁有者；分頁的容器把它掛成唯讀（見 entrypoint.sh）。
RUN mkdir -p /opt/agt-tools && chown node:node /opt/agt-tools

# 更新 Claude Code 的方式見 entrypoint.sh（AGT_CLAUDE_UPDATE=0 可略過）
COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

# 以非 root 身分執行（--dangerously-skip-permissions 不允許 root）
USER node
ENV HOME=/home/node \
    PATH=/opt/agt-tools/bin:$PATH \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    # 告訴 Claude Code 這是隔離環境
    IS_SANDBOX=1 \
    # 更新改由 entrypoint 處理（內建更新器沒有權限寫入全域 npm 目錄）
    DISABLE_AUTOUPDATER=1 \
    NPM_CONFIG_UPDATE_NOTIFIER=false
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["claude"]
