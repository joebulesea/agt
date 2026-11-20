#!/bin/sh
# 用法：docker/publish.sh [--restart]
#
# 發布執行檔（Windows）：
#   1. 在 agt-dev 容器中編譯 Windows release 版（x86_64-pc-windows-gnu）
#   2. 複製到 dist/windows/
#   3. 安裝到 %APPDATA%\agt\（使用者 PATH 指向這裡）
#   --restart：安裝後執行 `agt daemon restart -y`，讓 daemon 改用新版
#              （會結束所有分頁的 Agent；分頁保留，之後可用 Ctrl+A → R 接續）
#
# daemon 或 TUI 執行中時 exe 會被鎖住、無法覆蓋：先把舊檔改名為 .old 再放新檔
# （Windows 允許改名執行中的檔案），下次發布時再刪除已不再使用的 .old。
# 需要在 Git Bash 中執行，且 Docker Desktop 正在執行；缺 agt-dev 映像時會先建置。
set -eu
RESTART=0
RESTART_FAILED=0
for arg in "$@"; do
  case $arg in
    --restart) RESTART=1 ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "不認得的參數：$arg" >&2; echo "用法：docker/publish.sh [--restart]" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.." || exit 1

TARGET=x86_64-pc-windows-gnu
DIST=dist/windows
INSTALL="${APPDATA:?需要在 Windows 的 Git Bash 中執行}/agt"
INSTALL=$(cygpath -u "$INSTALL" 2>/dev/null || echo "$INSTALL")

if ! docker info >/dev/null 2>&1; then
  echo "Docker Desktop 沒有在執行，請先開啟後再試一次。" >&2
  exit 1
fi
if ! docker image inspect agt-dev >/dev/null 2>&1; then
  echo "==> 建置 agt-dev 映像（第一次執行，需要幾分鐘）"
  docker build -t agt-dev docker
fi

echo "==> 編譯 $TARGET release"
sh docker/cargo.sh build --release --target "$TARGET" --workspace

# 把容器內的建置結果複製到暫存資料夾（建置輸出在 docker volume agt-target 裡）
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
MSYS_NO_PATHCONV=1 docker run --rm -v agt-target:/target \
  -v "$(cd "$STAGE" && (pwd -W 2>/dev/null || pwd)):/out" agt-dev \
  sh -c "cp /target/$TARGET/release/agt.exe /out/"

# 放到目的資料夾；檔案被鎖住時先把舊檔改名
install_to() {
  dir=$1
  mkdir -p "$dir"
  # 上次留下、已不再使用的舊檔（還在使用中的刪不掉，保留）
  rm -f "$dir"/agt*.old 2>/dev/null || true
  # 舊版有獨立的 agtd.exe（已併入 agt.exe，由 `agt daemon run` 執行）：刪掉；使用中就留著
  rm -f "$dir/agtd.exe" 2>/dev/null || true
  for f in agt.exe; do
    if ! cp "$STAGE/$f" "$dir/$f" 2>/dev/null; then
      old="$dir/$f.old"
      # .old 本身也還在使用中（例如 daemon 正從它執行）時，改用不重複的名稱
      [ -e "$old" ] && old="$dir/$f.$$.old"
      mv "$dir/$f" "$old"
      cp "$STAGE/$f" "$dir/$f"
      echo "    $f 使用中，舊檔改名為 $(basename "$old")"
    fi
  done
  echo "==> 已放到 $dir"
}

install_to "$DIST"
install_to "$INSTALL"
"$INSTALL/agt.exe" --version

if [ "$RESTART" = 1 ]; then
  echo "==> 重新啟動 daemon"
  # agt 啟動 daemon 前會關掉自己 std handle 的可繼承旗標（agt-proto spawn_detached），
  # 所以直接輸出到管線（例如 publish.sh | tail）也不會被 daemon 卡住
  # 用暫存檔記下 agt 的結束碼（POSIX sh 沒有 pipefail，管線的結束碼是 sed 的）
  if ! { "$INSTALL/agt.exe" daemon restart -y 2>&1 </dev/null; echo $? >"$STAGE/rc"; } | sed 's/^/    /' \
    || [ "$(cat "$STAGE/rc" 2>/dev/null)" != 0 ]; then
    echo "!!> daemon 重新啟動失敗（見上方訊息）；可手動執行 agt daemon restart" >&2
    RESTART_FAILED=1
  fi
  # 舊版 daemon 已結束：清掉改名留下的 .old（新版 daemon 啟動時也會清；這裡連 dist/ 一起處理）
  for dir in "$DIST" "$INSTALL"; do
    for f in "$dir"/agt*.old; do
      [ -e "$f" ] || continue
      if rm -f "$f" 2>/dev/null; then
        echo "    已刪除 $f"
      else
        echo "    $f 仍在使用中（例如舊版 agt TUI 還開著）；關閉後新版 daemon 會在背景刪除（最多約 6 分鐘內），否則下次發布時刪除"
      fi
    done
  done
else
  echo "提示：daemon 仍在使用舊版時，執行 agt daemon restart（或加上 --restart 重新發布）"
  echo "      新版 daemon 啟動時會自動刪除改名留下的 *.exe.old"
fi

# daemon 重啟失敗時以非 0 結束，讓呼叫端知道（.old 清理照做）
[ "$RESTART_FAILED" = 0 ] || exit 1
