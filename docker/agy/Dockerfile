# agt 的 agy-docker Agent 用的映像檔：在容器中執行 Antigravity CLI（agy）。
# 建置：docker build -t agt-agy docker/agy/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料與設定保存在 volume `agt-agy-home`（掛在 /home/agent），只需要登入一次。
# 專案需要的工具（Rust、Node 等）請自行加在下方。
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential util-linux \
 && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -s /bin/bash agent

# 先安裝一份到 /opt/agy 當作種子；第一次啟動時由 entrypoint 複製到 volume 內的 ~/.local/bin，
# 之後 agy 在那裡自我更新（使用者可寫入）。
RUN curl -fsSL https://antigravity.google/cli/install.sh | HOME=/opt/agy-home bash -s -- --dir /opt/agy \
 && /opt/agy/agy --version \
 && rm -rf /opt/agy-home

COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

USER agent
ENV HOME=/home/agent \
    PATH=/home/agent/.local/bin:$PATH \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["agy"]
