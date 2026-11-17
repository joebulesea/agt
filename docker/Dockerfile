# agt 開發/建置環境：Linux 原生建置 + 交叉編譯 Windows (x86_64-pc-windows-gnu)
# base image 釘在 digest（tag 可能被改指到別的內容）；更新時改 digest，或交給 Dependabot。
FROM rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 git \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu \
 && rustup component add clippy rustfmt
RUN git config --global user.email "agt@example.invalid" && git config --global user.name "agt-test" \
 && git config --global init.defaultBranch main
ENV CARGO_TARGET_DIR=/target
WORKDIR /work
