#!/bin/sh
# 用法：docker/cargo.sh <cargo 參數...>   在 agt-dev 容器中執行 cargo
cd "$(dirname "$0")/.." || exit 1
MSYS_NO_PATHCONV=1 exec docker run --rm -i \
  -v "$(pwd -W 2>/dev/null || pwd):/work" \
  -v agt-cargo-registry:/usr/local/cargo/registry \
  -v agt-cargo-git:/usr/local/cargo/git \
  -v agt-target:/target \
  -e CARGO_TERM_COLOR=never \
  agt-dev cargo "$@"
