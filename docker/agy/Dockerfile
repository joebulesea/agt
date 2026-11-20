# agt 的 agy-docker Agent 用的映像檔：在容器中執行 Antigravity CLI（agy）。
# 建置：docker build -t agt-agy docker/agy/
#
# 容器只看得到 agt 掛載進來的 worktree（/work/<分頁 id>），看不到主機上的其他檔案。
# 登入資料與設定保存在 volume `agt-agy-home`（掛在 /home/agent），只需要登入一次。
# agy 的更新版保存在 volume `agt-agy-tools`（掛在 /opt/agt-tools，分頁中唯讀）。
# 專案需要的工具（Rust、Node 等）請自行加在下方。
# base image 釘在 digest（tag 可能被改指到別的內容）；更新時改 digest，或交給 Dependabot。
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep jq less procps python3 python3-pip build-essential util-linux \
 && rm -rf /var/lib/apt/lists/*

# agy 的更新版放在 volume agt-agy-tools（掛在 /opt/agt-tools）。目錄先建好並交給執行身分，
# 新的 volume 第一次掛載時會沿用這個擁有者；分頁的容器把它掛成唯讀（見 entrypoint.sh）。
RUN useradd -m -u 1000 -s /bin/bash agent \
 && mkdir -p /opt/agt-tools && chown agent:agent /opt/agt-tools

# 先安裝一份到 /opt/agy 當作種子；更新用的容器會把它複製到 volume 內的 /opt/agt-tools/bin，
# 之後 agy 在那裡自我更新。
# 官方沒有公布安裝腳本的雜湊。想要固定內容時，自行在可信環境下載一次並記錄 sha256sum，
# 建置時加上 --build-arg AGY_INSTALL_SHA256=<雜湊>：腳本內容不同就中止（腳本更新時雜湊要人工更新）。
ARG AGY_INSTALL_SHA256=
RUN curl -fsSL -o /tmp/agy-install.sh https://antigravity.google/cli/install.sh \
 && if [ -n "$AGY_INSTALL_SHA256" ]; then echo "$AGY_INSTALL_SHA256  /tmp/agy-install.sh" | sha256sum -c -; \
    else echo "警告：沒有設定 AGY_INSTALL_SHA256，安裝腳本未經核對" >&2; fi \
 && HOME=/opt/agy-home bash /tmp/agy-install.sh --dir /opt/agy \
 && /opt/agy/agy --version \
 && rm -rf /opt/agy-home /tmp/agy-install.sh

COPY entrypoint.sh /usr/local/bin/agt-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/agt-entrypoint && chmod 755 /usr/local/bin/agt-entrypoint

USER agent
ENV HOME=/home/agent \
    PATH=/opt/agt-tools/bin:/opt/agy:$PATH \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor
WORKDIR /work
ENTRYPOINT ["agt-entrypoint"]
CMD ["agy"]
