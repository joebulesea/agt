#!/bin/sh
# agt 安裝腳本（Linux x86_64）
#   curl -fsSL https://raw.githubusercontent.com/joebulesea/agt/main/scripts/install.sh | sh
#
# 環境變數：
#   AGT_REPO        GitHub repo（預設 joebulesea/agt）
#   AGT_VERSION     指定版本（例如 v0.3.0），預設最新版；nightly = main 的最新測試版
#   AGT_INSTALL_DIR 安裝位置，預設 ~/.local/bin
#   AGT_NO_SETUP=1  安裝後不執行 agt setup
set -eu

REPO=${AGT_REPO:-joebulesea/agt}
DIR=${AGT_INSTALL_DIR:-$HOME/.local/bin}

die() { echo "錯誤：$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "需要 $1"; }
need curl; need tar; need uname
case $REPO in */*/*|*/|/*|"") die "AGT_REPO 必須是 <owner>/<repo>（目前是 $REPO）" ;; */*) ;; *) die "AGT_REPO 必須是 <owner>/<repo>（目前是 $REPO）" ;; esac

os=$(uname -s); arch=$(uname -m)
[ "$os" = Linux ] || die "目前只提供 Linux 與 Windows 版（偵測到 $os）"
case $arch in
  x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
  *) die "目前只提供 x86_64（偵測到 $arch）" ;;
esac

version=${AGT_VERSION:-}
if [ -z "$version" ]; then
  # /releases/latest 會重新導向到 /releases/tag/<版本>，不使用有頻率限制的 API
  url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
    || die "查詢最新版本失敗"
  version=${url##*/}
fi
case $version in [0-9]*) version=v$version ;; esac
# nightly = main 的最新測試版（預發行版；/releases/latest 不包含它，所以要明確指定）
case $version in v[0-9]*|nightly) ;; *) die "無法判斷版本：$version" ;; esac

name="agt-$version-$target"
base="https://github.com/$REPO/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "==> 下載 $name"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || die "下載失敗：$base/$name.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "下載 SHA256SUMS 失敗"

echo "==> 核對 SHA256"
want=$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || die "SHA256SUMS 裡找不到 $name.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp/$name.tar.gz" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  got=$(shasum -a 256 "$tmp/$name.tar.gz" | awk '{print $1}')
else
  die "需要 sha256sum 或 shasum"
fi
[ "$want" = "$got" ] || die "SHA256 不符（預期 $want，實際 $got），已中止"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
[ -f "$tmp/$name/agt" ] || die "壓縮檔內容不完整"

# 舊版 daemon 還在執行時先停掉，避免新舊版本混用
if [ -x "$DIR/agt" ]; then
  "$DIR/agt" daemon stop -y >/dev/null 2>&1 || true
fi

mkdir -p "$DIR"
# 先放成暫存檔再改名：覆蓋執行中的檔案時不會出現 "Text file busy"
cp "$tmp/$name/agt" "$DIR/.agt.new"
chmod 755 "$DIR/.agt.new"
mv -f "$DIR/.agt.new" "$DIR/agt"
# 舊版有獨立的 agtd（已併入 agt）：刪除
rm -f "$DIR/agtd" "$DIR/agtd.old"
echo "==> 已安裝 $version 到 $DIR"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "注意：$DIR 不在 PATH 中，請加入 shell 設定檔：export PATH=\"$DIR:\$PATH\"" ;;
esac

if [ "${AGT_NO_SETUP:-}" != 1 ] && [ -t 1 ] && [ -r /dev/tty ]; then
  echo "==> 執行 agt setup"
  "$DIR/agt" setup </dev/tty || echo "agt setup 未完成，之後可自行執行 agt setup"
else
  echo "完成。請執行 agt setup 檢查環境。"
fi
