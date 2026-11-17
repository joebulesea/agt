# agt 的 copilot-docker Agent 用的映像檔：在容器中執行 GitHub Copilot CLI（copilot）。
# 建置：docker build -t agt-copilot docker/copilot/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料與設定保存在 volume `agt-copilot-home`（掛在 /home/node），只需要登入一次。
# Copilot CLI 的更新版保存在 volume `agt-copilot-tools`（掛在 /opt/agt-tools，分頁中唯讀）。
# 專案需要的工具（Rust、Python 套件等）請自行加在下方。
# base image 釘在 digest（tag 可能被改指到別的內容）；更新時改 digest，或交給 Dependabot。
FROM node:22-bookworm-slim@sha256:e9f565d32859f0cbfb52125eafe8be39390f24dac302dc3cdf566e8ba3ab21ee

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential util-linux \
 && rm -rf /var/lib/apt/lists/*

# 版本：預設 latest；要固定時 docker build --build-arg COPILOT_VERSION=<版本>
ARG COPILOT_VERSION=latest
RUN npm install -g "@github/copilot@${COPILOT_VERSION}" && npm cache clean --force

# Copilot CLI 的更新版放在 volume agt-copilot-tools（掛在 /opt/agt-tools）。目錄先建好並交給執行身分，
# 新的 volume 第一次掛載時會沿用這個擁有者；分頁的容器把它掛成唯讀（見 entrypoint.sh）。
RUN mkdir -p /opt/agt-tools && chown node:node /opt/agt-tools

# 更新 Copilot CLI 的方式見 entrypoint.sh（AGT_COPILOT_UPDATE=0 可略過）
COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

USER node
ENV HOME=/home/node \
    PATH=/opt/agt-tools/bin:$PATH \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    NPM_CONFIG_UPDATE_NOTIFIER=false
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["copilot"]
