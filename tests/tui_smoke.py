"""TUI 冒煙測試：在 pty 中執行 agt，用 pyte 模擬終端機並截取畫面。
用法（容器內）：python3 tests/tui_smoke.py /target/debug
"""
import os, pty, sys, time, select, subprocess, tempfile
import pyte

bindir = os.path.abspath(sys.argv[1])  # 之後會 chdir，先轉成絕對路徑
home = tempfile.mkdtemp()
env = dict(os.environ, AGT_HOME=home, AGT_SOCKET=os.path.join(home, "agt.sock"),
           TERM="xterm-256color", COLORTERM="truecolor", SHELL="/bin/bash")
open(os.path.join(home, "config.toml"), "w").write("[general]\nnotify = false\nosc52 = \"on\"\n")
repo = os.path.join(home, "proj"); os.makedirs(repo)
for c in (["git", "init", "-q", "-b", "main"], ["git", "commit", "-q", "--allow-empty", "-m", "init"]):
    subprocess.run(c, cwd=repo, check=True)
agt = os.path.join(bindir, "agt")
subprocess.run([agt, "new", "shell", "--repo", repo, "--task", "殼層測試"], env=env, check=True, stdout=subprocess.DEVNULL)

COLS, ROWS = 110, 26
screen = pyte.Screen(COLS, ROWS); stream = pyte.ByteStream(screen)
pid, fd = pty.fork()
if pid == 0:
    os.chdir(repo)
    os.execve(agt, [agt], env)
import fcntl, termios, struct
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

raw = bytearray()  # TUI 送給外層終端機的原始位元組

def pump(t):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError: return
            raw.extend(data); stream.feed(data)

def lines():
    # 不用 pyte 的 screen.display：寬字元的前半被覆蓋、後半的空格子還留著時，它會丟出 IndexError
    # （真實的終端機會一併清掉後半）。逐格串接，空字串的格子（寬字元的後半）本來就不佔字元。
    return ["".join(screen.buffer[y][x].data for x in range(COLS)) for y in range(ROWS)]

def show(title):
    print(f"===== {title} =====")
    for line in lines(): print(line.rstrip())

def text(): return "\n".join(lines())

pump(1.5)
os.write(fd, "echo hello 中文 $((6*7))\r".encode()); pump(1.0)
show("shell 分頁")
assert "殼層測試" in text() and "hello 中文 42" in text(), "畫面應有分頁與輸出"
os.write(fd, b"\x01?"); pump(0.6); show("說明")
assert "快捷鍵" in text()
os.write(fd, b"\x1b"); pump(0.3)
os.write(fd, b"\x01n"); pump(0.5)
os.write(fd, "第二個殼層".encode()); pump(0.2)
pump(0.3)  # Agent 預設為目前分頁的 shell
show("新增分頁表單")
assert "◀ shell ▶" in text()
os.write(fd, b"\r"); pump(1.5)
show("新增後")
assert "第二個殼層" in text() and "建立" not in text().splitlines()[-1], "應建立第二個分頁並清掉狀態訊息"
os.write(fd, b"\x011"); pump(0.6)
show("切回第 1 個分頁")
assert "hello 中文 42" in text(), "切回分頁應還原畫面"

# ── 滑鼠操作：不用快捷鍵，點按鈕建立與關閉分頁 ──
def find(label):
    """回傳 label 在畫面上的 (欄, 列)，全形字元佔 2 欄。"""
    for y in range(ROWS):
        row = screen.buffer[y]
        chars, cols = [], []
        for x in range(COLS):
            d = row[x].data
            if d == "":
                continue  # 全形字元的第二欄
            chars.append(d); cols.append(x)
        i = "".join(chars).find(label)
        if i >= 0:
            return cols[i], y
    raise AssertionError(f"畫面上找不到「{label}」")

def click(label):
    x, y = find(label)
    os.write(fd, f"\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m".encode())
    pump(0.6)

click("+ 新增")
show("點側欄的 + 新增")
assert "◀ shell ▶" in text() and "建立 (Enter)" in text(), "點按鈕應開啟新增表單"
os.write(fd, "滑鼠建立".encode()); pump(0.2)
click("建立 (Enter)"); pump(1.0)
show("點建立後")
assert "滑鼠建立" in text() and "建立 (Enter)" not in text(), "點建立應新增分頁並關閉表單"
click("說明")
assert "快捷鍵" in text(), "點說明應開啟快捷鍵說明"
click("agt")  # 點任意處關閉
assert "點任意處關閉" not in text()
# OSC 52：分頁中的程式要求複製時，TUI 要轉給外層終端機
raw.clear()
os.write(fd, b"printf '\\033]52;c;aHR0cHM6Ly94\\a'\r"); pump(1.0)
assert b"\x1b]52;c;aHR0cHM6Ly94\x07" in raw, "OSC 52 應轉送給外層終端機"
# 狀態列在 110 欄放不下「已複製到剪貼簿」訊息，只檢查轉送


click("關閉")
if "是 (y)" in text():
    click("是 (y)")
pump(1.0)
show("點關閉後")
assert "滑鼠建立" not in text(), "點關閉應移除目前分頁"

os.write(fd, b"\x01q"); pump(0.8)
_, status = os.waitpid(pid, 0)
print("TUI exit status", status)
out = subprocess.run([agt, "ls"], env=env, capture_output=True, text=True).stdout
print(out)
subprocess.run([agt, "daemon", "stop", "-y"], env=env, stdout=subprocess.DEVNULL)
assert status == 0 and out.count("running") >= 2, "detach 後兩個分頁都應繼續執行"
print("TUI 冒煙測試通過")
