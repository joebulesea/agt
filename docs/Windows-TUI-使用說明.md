# 在 Windows 上使用 agt TUI

本文件說明如何在 Windows 上開啟與操作 agt 的終端機介面（TUI）。

## 1. 準備

| 項目 | 說明 |
| --- | --- |
| 執行檔 | `agt.exe` 與 `agtd.exe`，必須放在同一個資料夾：安裝在 `%APPDATA%\agt\`（建置輸出另有一份在 `C:\claude\Rust\agt\dist\windows\`） |
| 終端機 | 建議使用 Windows Terminal（PowerShell） |
| git | 2.20 以上，需在 PATH 中 |
| Agent | 要使用的 Agent 指令必須在 PATH 中：`claude`（Claude Code）、`agy`（Antigravity CLI）、`copilot`（GitHub Copilot CLI，`npm install -g @github/copilot`）、`codex`（OpenAI Codex CLI，`npm install -g @openai/codex`）；`shell` 會使用 `pwsh`，沒有時改用 `powershell` |

不需要手動啟動 daemon：執行 `agt` 時若 daemon 尚未執行，會自動在背景啟動 `agtd`。

## 2. 加入 PATH（選用）

加入 PATH 後就可以直接輸入 `agt`。

只在目前的視窗有效：

```powershell
$env:Path += ";$env:APPDATA\agt"
```

永久生效（新開的視窗才會套用）：

```powershell
[Environment]::SetEnvironmentVariable(
  "Path",
  [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:APPDATA\agt",
  "User")
```

以下範例都假設已經加入 PATH；沒有加入時，請把 `agt` 換成完整路徑 `%APPDATA%\agt\agt.exe`。

### 更新（發布執行檔）

從原始碼更新時，在 Git Bash 執行（需要 Docker Desktop）：

```sh
docker/publish.sh             # 編譯 → 複製到 dist/windows → 安裝到 %APPDATA%\agt
docker/publish.sh --restart   # 同上，並重新啟動 daemon 改用新版（會結束所有分頁的 Agent）
```

在 cmd 執行（或在檔案總管雙擊 `docker\publish.cmd`）也可以；它會自動找 Git Bash 的 `bash.exe`、確認 Docker Desktop 在執行，沒有 `agt-dev` 映像時先建置，參數同上：

```bat
docker\publish.cmd
docker\publish.cmd --restart
```

daemon 或 TUI 執行中時 exe 會被鎖住，腳本會先把舊檔改名為 `.old` 再放新檔，下次發布時自動刪除。

## 3. 初始設定（agt setup）

只有 `agt.exe` 與 `agtd.exe` 也能直接使用（在檔案總管雙擊 `agt.exe` 也可以）：第一次執行 `agt`（還沒有設定檔）時，會先出現初始設定畫面：

1. 檢查環境：git、Docker Desktop（含 WSL2）、Docker Sandboxes（`sbx`，含是否已登入、是否已設定網路政策）、Claude Code、GitHub Copilot CLI、OpenAI Codex CLI、Antigravity CLI；有問題的項目標示 **必要 / 建議 / 選用**。
2. 有缺少的項目時，勾選要處理的項目（預設只勾必要的 git），每一項再選擇處理方式：
   - **用 winget 安裝**：會先顯示完整指令（`winget install -e --id <ID> --source winget`），winget 的輸出直接顯示在畫面上；需要系統管理員權限時由安裝程式跳出 UAC。安裝後會重新讀取 PATH，不用重新開視窗。
   - **開啟下載頁**：用預設瀏覽器開啟官方網站。
   - **顯示指令自行安裝**、**略過**。
   - Docker Desktop 沒有在執行時可以選「啟動 Docker Desktop」，會等待 engine 就緒（最多約 90 秒）；sbx 沒有登入時可以直接執行 `sbx login`。
   - WSL2 交給 Docker Desktop 安裝程式處理。安裝程式要求重新開機時選「先結束」，重新開機後再開啟 agt 會重新檢查並從這裡繼續。
   - 沒有 winget（例如 LTSC 版 Windows）時只能開啟下載頁或自行安裝。
3. 用 `↑` / `↓` 移動、`空白鍵` 勾選要加入的 Agent（`claude-sbx`、`claude-docker`、`agy-docker`、`copilot-docker`、`codex-docker`），`Enter` 確認，`Esc` 取消。
4. 選到需要映像檔的 Agent 時，把內建的 Dockerfile 寫到 `%APPDATA%\agt\docker\<claude|agy|copilot|codex>\`，並詢問是否馬上 `docker build`。
5. 選到 `claude-sbx` 而且還沒有沙箱網路政策時，選擇 `balanced` / `allow-all` / `deny-all`。
6. 產生 `%APPDATA%\agt\config.toml`，最後選擇是否開啟 agt。

**缺少 git 時無法開啟 agt**（建立 worktree 需要 git 2.20 以上）：之後每次開啟 agt 都會先快速檢查 PATH 中有沒有 git，沒有時再顯示一次安裝選單。從檔案總管雙擊 `agt.exe` 時，出錯或選「離開」後會顯示「按任意鍵關閉」，不會直接關掉視窗。

**之後每次開啟 agt** 都會快速檢查（只看 PATH，不會拖慢啟動）：如果偵測到設定檔中還沒有的可用 Agent（例如剛安裝好 Docker Desktop 或 Docker Sandboxes），會詢問是否加入；沒勾選的可以選「不要再提醒」（記錄在 `%APPDATA%\agt\setup-skip.txt`，刪掉這個檔案就會再提醒）。

隨時可以手動執行：

```powershell
agt setup              # 互動式設定
agt setup --yes        # 不詢問，採用預設選項（例如腳本中）；不會安裝任何東西，只列出缺少的項目
agt setup --yes --install   # 同上，並用 winget 安裝缺少的必要項目（git）
agt setup --yes --build   # 同上，並直接建置缺少的 Docker 映像檔
```

已存在的設定檔不會被覆蓋，只會在最後面加入新的區段；已經有的 Agent 會略過。

## 3.1 開啟 TUI

```powershell
cd C:\你的\git-repo
agt
```

在 TUI 中新增分頁時，repo 欄位預設為開啟 TUI 時的目前目錄。

也可以先用指令建立分頁，再直接開啟該分頁：

```powershell
agt new claude --repo . --task "登入功能" --attach
```

## 4. 畫面

```text
┌─ 左側欄 ─────────────┬─ 主區域：目前分頁的 Agent 畫面 ───────────┐
│ ▼ proj  (main)       │                                          │
│ 1 ⚠ claude  登入功能 │                                          │
│     agt/claude/... +3│                                          │
│ 2 ● shell   殼層     │                                          │
├──────────────────────┴──────────────────────────────────────────┤
│ 1 running · 1 waiting · 0 done │ Ctrl+A 前綴 │ ? 說明            │
└──────────────────────────────────────────────────────────────────┘
```

- 左側欄依 repo 分組，每個分頁顯示燈號、Agent、任務名稱與 branch。
- `+N` 是領先 base branch 的 commit 數，`✎` 表示有未 commit 的變更。
- 終端機寬度小於 80 欄時，左側欄自動收合成只顯示燈號。

### 狀態燈號

| 燈號 | 狀態 |
| --- | --- |
| ● | 執行中 |
| ◐ | 思考中 |
| ⚠（閃爍） | 等待輸入 |
| ✓ | 完成 |
| ✗ | 錯誤 |
| ○ | 閒置 / 已結束 |

## 5. 快捷鍵

所有指令都是先按 `Ctrl+A`，放開後再按下一個鍵；其他按鍵會原樣送給 Agent。

| 按鍵 | 功能 |
| --- | --- |
| `n` | 新增分頁 |
| `j` / `k`、`↓` / `↑` | 切換分頁 |
| `1` – `9` | 跳到第 N 個分頁 |
| `w` | 跳到下一個等待輸入的分頁 |
| `d` | 顯示 diff |
| `m` | 收尾選單：merge / pr / keep / discard |
| `r` | 重新命名任務 |
| `R` | 在同一個 worktree 重新啟動已結束的 Agent，**接續上次的對話**（例如電腦重新開機後） |
| `N` | 在同一個 worktree 重新啟動已結束的 Agent，開新對話 |

> 接續對話靠 Agent 設定的 `new_args` / `resume_args`（`claude` 用 `--session-id` / `--resume`、`copilot` 用 `--session-id` 指定這個分頁自己的對話；agy 用 `--continue`、`codex` 用 `resume --last`，接續最近一次的對話）。接續失敗（例如上次還沒開始對話就結束，找不到對話）時，agt 會自動改開新對話。舊版 agt 建立的分頁沒有記錄對話 ID，第一次按 `R` 會開新對話，之後就能接續。
| `[` | 滾動模式（`↑↓`、`PgUp`/`PgDn`、`/` 搜尋、`n` 下一個、`q` 離開） |
| `u` | 複製畫面上最新的網址（例如 Agent 印出的登入網址；折成多列的長網址會自動接回） |
| `o` | 用檔案總管開啟分頁的工作目錄（worktree，Agent 改檔案的地方） |
| `x` | 關閉分頁：選擇刪除 worktree 副本與 branch，或保留副本。保留的副本會被註記，下次開啟 agt 時提示清理（也可執行 `agt gc --prune`） |
| `b` | 收合或展開左側欄 |
| `?` | 快捷鍵說明 |
| `q` | 離開 TUI（detach），Agent 繼續在背景執行 |
| `Ctrl+A` | 再按一次 `Ctrl+A` 會送出真正的 `Ctrl+A` |

### 輸入多行提示詞

在 Agent 的輸入框按 `Ctrl+Enter`（或 `Shift+Enter`、`Ctrl+J`）換行，按 `Enter` 送出。

### 滑鼠操作

不想記快捷鍵時，可以直接用滑鼠點：

| 位置 | 按鈕 / 動作 |
| --- | --- |
| 左側欄右上角 | `+ 新增`：開啟新增分頁表單（側欄收合時是最上面的 `+`） |
| 頂端標題列的路徑 | 顯示分頁的工作目錄（worktree）；**雙擊**用檔案總管開啟 |
| 底部狀態列 | `+ 新增`、`Diff`、`收尾`、`目錄`、`改名`、`關閉`、`說明`，和對應的快捷鍵效果相同 |
| 底部狀態列（畫面上有網址時） | 黃色的 `複製網址 Ctrl+A u`：點一下就複製畫面上最新的網址 |
| 左側欄的分頁 | 切換分頁 |
| 新增分頁表單 | 點欄位就能輸入，點 `◀` / `▶` 切換 Agent，點 `瀏覽…` 選擇 Repo 目錄，點 `建立` / `取消` |
| 收尾、merge 策略選單 | 點選項直接執行 |
| 確認視窗 | 點 `是` / `否` |
| 說明、diff、重新命名視窗 | 點視窗外面關閉 |

Agent 沒有開啟滑鼠模式時，在主區域捲動滾輪可以瀏覽歷史。
diff 視窗打開時，滾輪會捲動 diff 內容（一次 3 行）。diff 視窗與錯誤訊息視窗（例如收尾失敗）中按 `c`（或 `y`）會把全部內容複製到剪貼簿；錯誤訊息會依視窗寬度自動換行。

### 新增分頁表單

| 欄位 | 說明 |
| --- | --- |
| Agent | 用 `←` / `→` 切換；預設為目前分頁的 Agent |
| Repo | repo 或任意目錄；不是 git repo 時不建立 worktree |
| 任務 | 任務名稱，會顯示在左側欄並用於 branch 名稱 |
| Base | base branch，留空時使用目前的 HEAD branch（欄位中會顯示是哪一個；主 repo 不在 remote 的預設 branch 上時，下方以黃字提醒 PR / merge 的目標會是它） |

`Tab` 或 `↑` / `↓` 切換欄位，`Enter` 建立，`Esc` 取消。

**用目錄選擇 Repo**：按 `Ctrl+O`（或 `F2`），或點 Repo 欄右邊的「瀏覽…」，會開啟目錄瀏覽器：

| 按鍵 / 滑鼠 | 動作 |
| --- | --- |
| `↑` / `↓`、`PgUp` / `PgDn`、滾輪 | 移動 |
| `Enter` / `→`、點一下 | 進入目錄；在「✓ 使用這個目錄」上按則選定目前目錄 |
| `←` / `Backspace` | 回上一層（Windows 在磁碟根目錄再往上會列出所有磁碟） |
| 直接輸入文字 | 篩選目錄名稱（`Backspace` 刪除篩選字） |
| `Tab` | 直接選定反白的目錄，不必先進入 |
| `Esc` | 關閉瀏覽器，回到表單 |

git repo 會以黃色 `⎇` 標示。選好後游標會自動移到「任務」欄。若 repo 設定了 `setup` 指令，第一次執行前會詢問是否執行。

## 6. 離開與重新連線

- 按 `Ctrl+A` → `q` 只會關閉 UI，Agent 繼續在背景執行。
- 再次執行 `agt`，畫面會完整還原。
- `agt attach <id>`：開啟 TUI 並直接切到指定的分頁。

## 7. 常用指令

```powershell
agt ls                     # 列出分頁
agt daemon status          # daemon 狀態
agt daemon stop            # 停止 daemon（會一併結束所有 Agent，執行前會確認）
agt gc                     # 檢查 worktree 與資料庫是否一致，列出關閉分頁後留下的副本
agt gc --prune             # 清理：有未 commit 變更的副本會保留，已合併的 branch 一併刪除
```

## 8. 檔案位置

| 用途 | 位置 |
| --- | --- |
| 設定檔 | `%APPDATA%\agt\config.toml` |
| 資料庫、終端機紀錄、daemon 日誌 | `%APPDATA%\agt\data\`（舊版在 `%LOCALAPPDATA%\agt\`，新版 daemon 第一次啟動時會自動搬過來） |
| worktree | `%USERPROFILE%\.agt\worktrees\` |
| daemon 連線 | `\\.\pipe\agt-<使用者名稱>` |

設定檔的所有選項與預設值見 [`config/default.toml`](../config/default.toml)。設定檔修改後 daemon 會自動重新載入；格式錯誤時會沿用上一份有效設定，並在 TUI 狀態列顯示錯誤位置。

## 9. Claude Code 的 hooks

建立 `claude` 分頁時，agt 會在該 worktree 內寫入 `.claude\settings.local.json`，讓 Claude Code 回報狀態（執行中、等待輸入、完成）。

- 該檔案已加入 git 的 exclude，不會被 commit。
- 不會修改你的全域 Claude Code 設定。
- 檔案已存在時，只合併 hooks 欄位，保留其他設定。

## 10. 在 Docker 沙箱中執行 Claude（claude-docker）

`claude-docker` 會在 Docker 容器中啟動 Claude Code，並加上 `--dangerously-skip-permissions`，讓它不必逐一詢問權限、自動工作。容器只掛載該分頁的 worktree（容器內的 `/work/<分頁 id>`；每個分頁路徑不同，對話記錄才不會混在一起），看不到主機上的其他檔案；就算 Agent 做錯事，影響也只限於那個 worktree 的 branch，不喜歡結果時用「收尾 → discard」整個丟掉即可。

### 準備（只需一次）

1. 安裝並啟動 Docker Desktop。
2. 建置映像檔：

   ```powershell
   docker build -t agt-claude C:\claude\Rust\agt\docker\claude
   ```

3. 在 `%APPDATA%\agt\config.toml` 加入 `[agents.claude-docker]`（範例見 [`config/default.toml`](../config/default.toml)），然後執行 `agt daemon restart`。
4. 登入：容器內**無法用瀏覽器登入**（登入完成後瀏覽器要連回容器內的 localhost，主機連不到，會顯示「登入未完成」）。請改在主機上產生 token：

   ```powershell
   claude setup-token        # 在主機上執行，用瀏覽器登入後會印出一組 token
   [Environment]::SetEnvironmentVariable("CLAUDE_CODE_OAUTH_TOKEN", "<貼上 token>", "User")
   ```

   開一個**新的** Windows Terminal 視窗（才會讀到新的環境變數），執行 `agt daemon restart`。daemon 會把 `CLAUDE_CODE_OAUTH_TOKEN` 傳進容器。使用 API 金鑰的話，改設定 `ANTHROPIC_API_KEY`，做法相同。

   分頁中出現登入網址時，不要用滑鼠選取（TUI 會攔截滑鼠，長網址也被折成多列），按 `Ctrl+A` → `u` 就會把完整網址複製到剪貼簿。
5. 新增分頁時把 Agent 切到 `claude-docker`。第一次啟動時會出現 Claude Code 的初始設定（主題、確認略過權限模式），設定保存在 Docker volume `agt-claude-home`，之後不會再出現。

### 注意事項

| 項目 | 說明 |
| --- | --- |
| git | 容器內**無法使用 git**（worktree 的 `.git` 指向主機路徑）。Claude 只修改檔案；設定 `auto_commit = true` 後，收尾選 merge 或 pr 時，agt 會先在主機上把所有變更 commit（訊息為 Claude 寫的 `.agt/COMMIT_MSG`，沒有時用任務名稱），再合併或建立 PR，見下方「用 Claude 寫的說明 commit 並建立 PR」 |
| 狀態燈號 | 容器內無法使用 hooks，改用畫面規則判斷，準確度比一般 `claude` 分頁略低 |
| 關閉分頁 | 會執行 `cleanup` 指令（`docker rm -f agt-<id>`）移除容器；daemon 結束時也會清掉 |
| 網路 | 容器可以連外網，沒有限制 |
| 工具 | 映像檔內只有 Node、git、Python、build-essential。專案需要的其他工具（Rust 等）請加在 `docker/claude/Dockerfile` 後重新建置 |
| 速度 | Windows 目錄掛載進容器時，大量檔案讀寫會比較慢 |
| Claude 版本 | 設定 `update`（見範例）後，agt daemon 啟動時會在背景檢查 npm 上的最新版並安裝到 volume `agt-claude-tools`；**每次新建分頁**時也會先在分頁中執行一次同樣的更新（畫面上會看到 `[agt] 檢查 Agent 更新…`，已是最新版時只多花幾秒），再啟動 Claude Code；沒網路或更新失敗時直接用現有版本。不想在新建分頁時更新可設定 `update_on_new = false`。分頁內的容器把這個 volume 掛成唯讀，Agent 改不到它；按 `R` 重新啟動不會更新。沒有掛載 `agt-claude-tools` 的舊設定會使用映像檔內建的版本（重新建置映像檔才會更新） |
| 安全性 | 範例的 `--cap-drop=ALL`、`--security-opt=no-new-privileges`、`--pids-limit` 限制容器的權限與行程數；Agent 的執行檔在分頁中唯讀。`agt-claude-home` 是所有分頁共用且可寫的家目錄，要完全隔離的做法與 token 的注意事項見 `config/default.toml` 中 `claude-docker` 範例上方的說明。收尾（merge / pr）時，若分頁改了合併後會自動執行的設定檔（`.claude/`、`.agt.toml`、`.github/workflows/` 等），agt 會先列出來要求確認 |

### 用 Claude 寫的說明 commit 並建立 PR

`config/default.toml` 範例的 system prompt 會請 Claude 在每次修改後寫兩個檔案：`.agt/COMMIT_MSG`（commit 訊息，第一行是標題）與 `.agt/PR_BODY.md`（PR 說明）。舊的設定要自行把範例中 `--append-system-prompt` 後面那段文字換成新版。

Claude 改完後，在 agt 中按收尾選 **pr** 即可（需要 `auto_commit = true` 與 [GitHub CLI](https://cli.github.com/)）：

1. 用 `.agt/COMMIT_MSG` commit 所有變更（沒有這個檔案時用任務名稱），commit 後刪除該檔。
2. push 後建立 PR：標題是 commit 訊息的第一行，說明用 `.agt/PR_BODY.md`（沒有時由 gh 從 commit 填入），成功後刪除該檔。
3. 這個 branch 已經有開啟中的 PR 時（review 後的第二輪），只 push，有新的 `PR_BODY.md` 時更新 PR 說明。分頁不會被關閉，可以繼續請 Claude 修改、再選一次 pr。

`.agt/` 已加入主 repo 的 `.git/info/exclude`，不會出現在 diff 中，也不會被 commit。說明檔只會被當成文字交給 git / gh，不會被執行；檔案是連結、不是一般檔案、超過 64KB 或不是 UTF-8 時，agt 會拒絕並停止。

不開 TUI 時，也可以在主機上執行 `C:\claude\agt\docker\claude\agt-pr.cmd <worktree 路徑>`，效果相同（詳見 [agt-pr 操作步驟](agt-pr-操作步驟.md)）。
設定中可用的變數：`{cwd}` = worktree 路徑、`{session}` = 分頁 id、`{gitdir}` = 主 repo 的 `.git` 目錄（見下方 claude-sbx）。`adapter = "claude"` 讓自訂 Agent 沿用 Claude Code 的狀態判斷。`{conversation}` = 分頁目前的對話 ID：`claude-docker` 設定 `new_args = ["--session-id", "{conversation}"]` 與 `resume_args = ["--resume", "{conversation}"]` 後，按 `R` 會接續該分頁自己的對話（見第 5 節）。

### Docker Sandboxes（claude-sbx）

[Docker Sandboxes](https://docs.docker.com/ai/sandboxes/)（`sbx`）把每個分頁的 Claude Code 放在獨立的 microVM 裡，隔離比容器更強，也**不需要 Docker Desktop**。和 `claude-docker` 的差別：

| 項目 | claude-docker | claude-sbx |
| --- | --- | --- |
| 隔離 | 容器 | microVM（獨立 kernel 與 Docker daemon） |
| git | 不能用 | 主 repo 的 `.git` 以唯讀掛入，可以 `git status` / `diff` / `log`；commit 仍由 `auto_commit` 處理 |
| 重新啟動（`R`） | 重建容器 | 沿用同一個沙箱，對話記錄、裝過的套件都還在 |
| 網路 | 不限制 | 經主機 proxy，依 `sbx policy` 允許清單（例如 `balanced`） |

準備（只需一次）：

1. Windows 11，以系統管理員 PowerShell 開啟 Windows Hypervisor Platform（可能需要重開機）：
   `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All`
2. 安裝並登入：`winget install Docker.sbx`、`sbx login`（開新視窗才找得到 `sbx`）。
3. 設定網路政策：`sbx policy init balanced`（或 `allow-all` / `deny-all`）。沒設定時建立沙箱會失敗。
4. 把 `config/default.toml` 裡的 `[agents.claude-sbx]` 複製到 `%APPDATA%\agt\config.toml`（存檔後自動重新載入）。
5. 新增分頁時把 Agent 切到 `claude-sbx`。**第一次**要下載 Claude 映像檔（約 400 MB，數分鐘），之後每個新沙箱約 6 秒。
6. 新沙箱的 Claude 還沒登入（分頁會顯示 ⚠ 等待輸入）：在分頁中輸入 `/login`，用 `Ctrl+A` → `u` 複製登入網址到主機瀏覽器。使用 API 金鑰時改執行一次 `sbx secret set anthropic`。

設定說明：

| 欄位 | 作用 |
| --- | --- |
| `args` | 第一次啟動：`sbx run --name agt-{session} claude {cwd} {gitdir}:ro` 建立沙箱並掛載 worktree 與主 repo 的 `.git` |
| `restart_args` | 重新啟動（`R` / `N`）時取代 `args`。沙箱已存在時不能再指定掛載路徑，只用名稱接回 |
| `cleanup` | `sbx stop`：Agent 結束、daemon 停止時停止沙箱，釋放記憶體（啟動前也會先執行一次） |
| `on_close` | `sbx rm -f`：關閉分頁（關閉、收尾）時刪除沙箱。重新啟動或 daemon 停止時**不會**執行 |
| `{gitdir}` | 主 repo 的 `.git` 目錄。用到它時，agt 會把 worktree 的 `.git` 改成相對路徑，沙箱內才解析得到 |

注意事項：

- 記憶體：每個執行中的沙箱在主機上固定約佔 1.2–1.4 GB（microVM 本身與映像檔快取），和上限設多少無關。
  沒有上限時（預設為主機記憶體的一半）會隨著工作慢慢長大、不太會還給主機，所以範例設定加了
  `"--cpus", "4", "-m", "3g"`（需要編譯大型專案時可提高）。上限在建立沙箱時決定，修改後只對新分頁有效。
- Agent 結束後 `cleanup` 會停止沙箱，停止的沙箱不佔記憶體；同時開的 `claude-sbx` 分頁越多，用量越高。
- 沙箱內的路徑與主機對應：`C:\Users\...` → `/c/Users/...`。
- 狀態燈號沒有 hooks，改由畫面判斷：出現 `esc to interrupt` 為執行中，消失後為完成，`Not logged in` 為等待輸入。
- 殘留的沙箱可以用 `sbx ls` 檢查、`sbx rm <名稱>` 刪除。

### Antigravity CLI（agy-docker）

做法相同，映像檔在 `docker/agy/`，設定範例是 `config/default.toml` 裡的 `[agents.agy-docker]`：

1. 建置映像檔：`docker build -t agt-agy C:\claude\Rust\agt\docker\agy`
2. 把 `[agents.agy-docker]` 複製到 `%APPDATA%\agt\config.toml`，執行 `agt daemon restart`。
3. 登入：新增 `agy-docker` 分頁後，agy 會印出 Google 登入網址。按 `Ctrl+A` → `u` 複製網址，在主機的瀏覽器開啟並登入，再把頁面顯示的授權碼貼回分頁、按 Enter。登入資料保存在 volume `agt-agy-home`，之後不需要再登入。使用 Gemini API 金鑰時，改在主機設定 `GEMINI_API_KEY` 環境變數（daemon 會傳進容器），就不必登入。

| 項目 | 說明 |
| --- | --- |
| git | 容器內無法使用 git。第一次啟動時會建立 `~/.gemini/GEMINI.md`（全域規則）告訴 agy 不要使用 git；已有內容時不會覆蓋。commit 由 `auto_commit` 處理 |
| 版本 | 設定 `update` 後，daemon 啟動時在背景執行 `agy update`（裝進 volume `agt-agy-tools`，分頁中唯讀），新建分頁時也會先更新一次（`update_on_new = false` 可關閉） |
| 權限 | 加上 `--dangerously-skip-permissions` 自動核准所有工具；agy 的終端機沙箱在容器內無法使用，由 Docker 隔離取代 |
| 狀態燈號 | 使用畫面規則判斷（`adapter = "antigravity"`） |

### GitHub Copilot CLI（copilot-docker）

做法相同，映像檔在 `docker/copilot/`，設定範例是 `config/default.toml` 裡的 `[agents.copilot-docker]`：

1. 建置映像檔：`docker build -t agt-copilot C:\claude\Rust\agt\docker\copilot`
2. 把 `[agents.copilot-docker]` 複製到 `%APPDATA%\agt\config.toml`，執行 `agt daemon restart`。
3. 登入：新增 `copilot-docker` 分頁後輸入 `/login`，畫面會顯示 GitHub 的網址與一次性代碼。按 `Ctrl+A` → `u` 複製網址，在主機的瀏覽器開啟並輸入代碼。登入資料保存在 volume `agt-copilot-home`，之後不需要再登入。也可以在主機設定 `GH_TOKEN`（或 `GITHUB_TOKEN`，具備 Copilot Requests 權限的 fine-grained PAT），daemon 會傳進容器。

| 項目 | 說明 |
| --- | --- |
| git | 容器內無法使用 git。第一次啟動時會建立 `~/.copilot/copilot-instructions.md`（全域指示）告訴 Copilot 不要使用 git；已有內容時不會覆蓋。commit 由 `auto_commit` 處理 |
| 版本 | 設定 `update` 後，daemon 啟動時在背景檢查 npm 上的 `@github/copilot` 並更新（裝進 volume `agt-copilot-tools`，分頁中唯讀），新建分頁時也會先更新一次（`update_on_new = false` 可關閉） |
| 權限 | 加上 `--allow-all-tools` 自動核准所有工具，由 Docker 隔離取代逐一核准 |
| 接續對話 | 開新對話時以 `--session-id` 指定分頁自己的對話 ID，`R` 用同一個 ID 接續；同時開多個 `copilot-docker` 分頁也不會接到別的分頁的對話 |
| 狀態燈號 | 使用畫面規則判斷（`adapter = "copilot"`）：出現工具核准選單（`No, and tell Copilot ...`）或資料夾信任詢問為等待輸入 |

### OpenAI Codex CLI（codex-docker）

做法相同，映像檔在 `docker/codex/`，設定範例是 `config/default.toml` 裡的 `[agents.codex-docker]`：

1. 建置映像檔：`docker build -t agt-codex C:\claude\Rust\agt\docker\codex`
2. 把 `[agents.codex-docker]` 複製到 `%APPDATA%\agt\config.toml`，執行 `agt daemon restart`。
3. 登入：新增 `codex-docker` 分頁後執行 `codex login --device-auth`，按 `Ctrl+A` → `u` 複製網址，在主機的瀏覽器開啟並輸入代碼。登入資料保存在 volume `agt-codex-home`，之後不需要再登入。也可以在主機設定 `OPENAI_API_KEY`，daemon 會傳進容器。

| 項目 | 說明 |
| --- | --- |
| git | 容器內無法使用 git。第一次啟動時會建立 `~/.codex/AGENTS.md`（全域指示）告訴 Codex 不要使用 git；已有內容時不會覆蓋。commit 由 `auto_commit` 處理 |
| 版本 | 設定 `update` 後，daemon 啟動時在背景檢查 npm 上的 `@openai/codex` 並更新（裝進 volume `agt-codex-tools`，分頁中唯讀），新建分頁時也會先更新一次（`update_on_new = false` 可關閉） |
| 權限 | 加上 `--dangerously-bypass-approvals-and-sandbox` 略過 Codex 自己的核准與沙箱，由 Docker 隔離取代 |
| 接續對話 | `R` 以 `resume --last` 接續容器內最近一次的對話；同時開多個 `codex-docker` 分頁時可能接到別的分頁的對話 |
| 狀態燈號 | 使用畫面規則判斷（`adapter = "codex"`）：出現指令／修改核准選單或資料夾信任詢問為等待輸入 |

## 11. 疑難排解

| 狀況 | 處理方式 |
| --- | --- |
| 新增分頁時出現「無法啟動 `claude`」 | `claude` 不在 PATH 中，請確認可以在 PowerShell 直接執行 `claude` |
| 出現協定版本不同的提示 | 執行 `agt daemon restart` |
| TUI 沒有回應 | 開另一個視窗執行 `agt daemon status`；必要時 `agt daemon restart` |
| 需要回報問題 | 附上 `%APPDATA%\agt\data\agtd.log`；設定環境變數 `AGT_LOG=debug` 後重啟 daemon 可以取得更詳細的日誌 |

> 目前 Windows 上的 CLI 與 daemon 已經實測過；TUI 只在 Linux 容器中做過自動化測試，尚未在 Windows Terminal 中手動操作。遇到畫面錯亂或按鍵沒有反應時，請回報當時的狀況與日誌。
