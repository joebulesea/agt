#!/bin/sh
# agt 安裝腳本（Linux / macOS 以外的平台請見 scripts/install.ps1）
#   curl -fsSL https://github.com/<owner>/agt/releases/latest/download/install.sh | sh
#
# 環境變數：
#   AGT_REPO     GitHub repo（預設 OWNER/agt，發布前請改成實際位置）
#   AGT_VERSION  指定版本（例如 0.3.0），預設最新版
#   AGT_BIN_DIR  安裝位置，預設 ~/.local/bin
#   AGT_NO_SETUP 設為 1 就不在最後執行 `agt setup`
set -eu

REPO="${AGT_REPO:-OWNER/agt}"
BIN_DIR="${AGT_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

case "$REPO" in OWNER/*) die "請設定 AGT_REPO=<owner>/<repo>（腳本內的預設值還沒改）" ;; esac
command -v curl >/dev/null 2>&1 || die "需要 curl"
command -v tar >/dev/null 2>&1 || die "需要 tar"

os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
  Linux-x86_64 | Linux-amd64) target=linux-x64 ;;
  *) die "目前只提供 Linux x64 的預編譯版本（偵測到 $os $arch）。其他平台請用 cargo build --release" ;;
esac

if [ -n "${AGT_VERSION:-}" ]; then
  version=${AGT_VERSION#v}
else
  # 只取最後一次轉址的網址，不呼叫 API（沒有速率限制）
  url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
    || die "查不到最新版本"
  version=${url##*/v}
  [ -n "$version" ] && [ "$version" != "$url" ] || die "無法從 $url 判斷版本"
fi

name="agt-$version-$target.tar.gz"
base="https://github.com/$REPO/releases/download/v$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "下載 agt $version（$target）…"
curl -fsSL -o "$tmp/$name" "$base/$name" || die "下載失敗：$base/$name"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "下載失敗：$base/SHA256SUMS"

# 核對 SHA256：不符就中止
expected=$(grep " $name\$" "$tmp/SHA256SUMS" | awk '{print $1}' | head -n 1)
[ -n "$expected" ] || die "SHA256SUMS 裡找不到 $name"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$name" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$name" | awk '{print $1}')
else
  die "找不到 sha256sum / shasum，無法核對下載內容"
fi
[ "$expected" = "$actual" ] || die "SHA256 不符（預期 $expected，實際 $actual），已中止"

mkdir -p "$tmp/x" "$BIN_DIR"
tar -xzf "$tmp/$name" -C "$tmp/x"
[ -f "$tmp/x/agt" ] || die "壓縮檔內容不符預期（缺少 agt）"

# 已經安裝且 daemon 在執行時先停止（會結束所有 Agent）
if [ -x "$BIN_DIR/agt" ]; then
  "$BIN_DIR/agt" daemon stop -y >/dev/null 2>&1 || true
fi
# 先改名再放新檔：執行中的檔案不能直接覆蓋
# 舊版有獨立的 agtd（已併入 agt）：刪除
rm -f "$BIN_DIR/agtd" "$BIN_DIR/agtd.old"
for f in agt; do
  [ -e "$BIN_DIR/$f" ] && mv -f "$BIN_DIR/$f" "$BIN_DIR/$f.old"
  install -m 755 "$tmp/x/$f" "$BIN_DIR/$f"
  rm -f "$BIN_DIR/$f.old"
done

say "已安裝到 $BIN_DIR"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "注意：$BIN_DIR 不在 PATH 中，請加入 shell 設定檔：export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

# 腳本是從管線讀進來的（curl | sh），互動輸入要改接 /dev/tty
if [ "${AGT_NO_SETUP:-0}" != "1" ] && [ -r /dev/tty ]; then
  say "執行 agt setup…"
  "$BIN_DIR/agt" setup </dev/tty || true
fi
