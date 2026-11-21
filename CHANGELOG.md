# 更新紀錄

版號採用 [語意化版本](https://semver.org/lang/zh-TW/)，1.0 之前的規則如下：

- **PATCH**（0.2.0 → 0.2.1）：修正錯誤、調整文字或介面細節
- **MINOR**（0.2.x → 0.3.0）：新功能、使用者看得到的行為改變，或不相容的改變（標示「⚠ 不相容」）

每個 PR 不改版號，只在「未發布」加上一行；發布時依其中最大的變更類型決定新版號，修改 `Cargo.toml`、打 tag `vX.Y.Z`，再執行 `docker/publish.sh`。若條目與其他 PR 共用 `CHANGELOG.md` 而容易衝突，可先寫成 `docs/未發布/<主題>.md`，發布時併入本檔並刪除該檔。

## 未發布

### 新增

- 狀態列顯示用量：Claude Code 的分頁在狀態列右邊顯示目前分頁的 context 用量，以及 Pro / Max 訂閱方案的 5 小時、每週額度與重置倒數，達到 80% 時變成紅色。資料來自 Claude Code 的 status line：建立分頁時在 worktree 的 `.claude/settings.local.json` 寫入 `statusLine`（主機上是新的 `agt statusline`，沙箱裡是 `.agt/statusline.sh`，有 node 時另外在 Claude 畫面上顯示一行摘要），JSON 寫到 `.agt/usage.json`，daemon 每 3 秒檢查修改時間、有變化才讀取（視為不可信：限制大小、不跟隨連結、只取數字並限制範圍）。已有使用者自己的 `statusLine` 時不覆寫。`SessionInfo` 新增 `usage` 欄位（`agt ls --json` 也看得到），協定版本不變
- 同一個檔案分到不同組：分組目錄可放 `SNAPSHOT/<路徑>`（這個檔案在這組 branch 上的完整內容），前面的組放只含自己修改的版本，後面的組用 `AFTER` 接上去並在 FILES 拿最終內容，PR 只顯示各自的部分。只有 SNAPSHOT 的組可以省略 FILES；拿最終內容的組沒有接在 SNAPSHOT 的組後面時拒絕並說明；只有 SNAPSHOT 的檔案最終內容歸 `rest`，`rest` 自動接在那組後面。收尾預覽列出每組的 SNAPSHOT 數量。說明見 `docs/Agent-分組收尾說明.md`，`.agt/README.md` 的規則也已更新
- 收尾檢查：設定檔 `[repos."<路徑>"] finish_check`（例如 `"cargo build --workspace"`），分組收尾時在每組 branch 的乾淨 checkout 各執行一次，任何一組失敗就不 push、不合併，並顯示哪一組失敗與輸出的最後 40 行。沙箱 Agent（docker / sbx / podman）在 Agent 的 `check_command`（例如同一個映像檔的容器）裡執行，沒設定時略過，不在主機上執行它寫的程式碼。只從使用者設定檔讀取，`.agt.toml` 裡的不會生效；`agt config check` 會提醒這些情況，收尾預覽也會說明是否執行檢查
- 堆疊分組：分組目錄可放 `AFTER`（另一組的組名），這組接在那組的 branch 後面建立，收尾 pr 時 PR 的 base 也是那組的 branch，用到另一組新增的程式碼時也能拆成多個 PR；merge 依 `AFTER` 順序合進 base。那組後來追加 commit 時，這組以 merge commit 追上（不 force push）。收尾預覽標出每組接在哪一組後面。說明見 `docs/Agent-分組收尾說明.md`，`.agt/README.md` 的規則也已更新

### 修正

- 分組收尾 merge 選 squash 時，改為套用該組自己的 commit 再壓成一個，不再用 `merge --squash`：接在別組後面、又改到同一個檔案的組，原本會把前一組已合進去的修改再算一次而衝突
- 分組收尾 merge 選 rebase 時，分組 branch 上若已有多個 commit（例如之前收尾 pr 時追加過）會全部接上去，不再只取最後一個

## 0.3.1 - 2026-11-21

### 改善

- 背景 git 輪詢只在分頁忙碌、有人在看或狀態剛改變時每 3 秒查一次，其他分頁（等待輸入、完成、已結束）改為每 30 秒，分頁多時少啟動很多 git 行程；長時間執行提醒改用獨立的計時器，不再被慢的 git 拖延
- 資料庫改用 `synchronous = NORMAL`（WAL 模式），分頁狀態頻繁切換時不再每筆寫入都等硬碟同步

### 修正

- `agt update`：沒有設定 `AGT_REPO` 時預設使用 `joebulesea/agt`（與安裝腳本相同），本機用 `docker/publish.sh` 編譯的版本不再出現「不知道要從哪個 GitHub repo 更新」

## 0.3.0 - 2026-11-21

### 新增

- 新增分頁的範本：設定檔 `[templates.<名稱>]`（`agent`、`task`、`base`、`tags`、`description`）。新增表單按 `F3` / `Ctrl+T` 切換，`agt new --template <名稱>` 可省略 Agent；自己填的值優先，標籤合併。`agt config check` 會列出範本並檢查 Agent 與標籤
- 分頁標籤：每個分頁最多 8 個標籤，`Ctrl+A` → `t` 或 `agt tag <id> --add/--remove/--set/--clear` 修改，顯示在左側欄第三行，`agt ls --tag <標籤>` 篩選，`agt new --tag` 建立時加上。資料庫 schema 升到 4（新增 `sessions.tags`；舊版 agt 無法開啟升級後的資料庫）
- `agt stats [--days N] [--json]`：統計最近 N 天（預設 7，最多保留 30 天）建立的分頁花在各狀態的時間（忙碌＝執行中＋思考中、等待輸入），並依 Agent 合計。只有時間；目前沒有 token 或費用的資料來源
- 長時間執行提醒：`[general] long_running_notify_secs`（預設 0 = 關閉），分頁連續執行中／思考中超過這段時間就通知一次
- `agt log <id>`：匯出分頁的終端機紀錄。預設輸出去掉控制序列的純文字（處理 `\r` 進度列與退格造成的覆寫），`--tail N` 只取最後 N 行，`--format md` 輸出 Markdown，`-o` 寫到檔案（已存在需 `--force`）；`--raw -o` 輸出原始位元組，不直接顯示在終端機。需要 `log_output = true`（預設）
- `agt update --check --json`：輸出 `{current, latest, tag, status, update_available}`（`status`：`up_to_date`、`update_available`、`ahead`、`downgrade`），查詢失敗時輸出 `{error}` 並 exit 1
- `agt gc --json` 與 `agt config check --json`：`gc` 輸出 `{pruned, items:[{kind, repo, path, branch, session}]}`；`config check` 成功時輸出 `{ok: true, config_file, config_exists, repo, worktree_root, agents, auto_commit, warnings}`，錯誤時輸出 `{ok: false, error}`，exit code 與文字模式相同（錯誤 1、警告 0）
- `agt doctor --json` 與 `agt daemon status --json`：以 JSON 輸出給腳本或狀態列使用（`ls --json` 原本就有）。doctor 輸出 `{ok, failures, warnings, items:[{level, text, hint}]}`，有失敗項目時仍 exit 1；daemon status 輸出 `daemon_running`、`socket`，執行中時另有 `pid`、`version`、`sessions`、`running`（執行中的分頁數）、`clients`、`data_dir`、`config_path`
- 自訂顏色：設定檔 `[colors]` 區段可覆寫主題中的個別顏色（`#rrggbb`、0–255、ANSI 名稱或 `default`），疊加在 `theme` 之上，也可以只改單一狀態的顏色（`status_running`、`status_error` 等）；`agt config check` 會檢查名稱與寫法。
- 主題設定：`[general] theme = "dark" | "light" | "colorblind"`。`light` 給亮色背景的終端機，`colorblind` 避開紅／綠對比；預設 `dark` 與原本配色相同。設定值會經由 `UiConfig` 傳給 TUI（新欄位有預設值，協定版本不變），設定檔熱重載後立即生效，寫錯時 `agt config check` 會報錯
- Windows：點擊系統通知會開新的終端機視窗執行 `agt attach <id>`（優先 Windows Terminal，否則新的 console）。需要 daemon 仍在執行；其他平台行為不變
- 系統通知可選擇狀態：`[general] notify_on = ["waiting", "done", "error"]`（預設前三者；另可加 `"exited"` 在 Agent 行程結束時通知，`notify = false` 仍是全部關閉）。分頁發生錯誤時現在也會通知（原本只有等待輸入與完成）；設定值寫錯時 `agt config check` 會報錯
- `agt completions <shell>`：輸出 bash、zsh、fish、PowerShell、elvish 的補全腳本（子命令與選項；新增相依 `clap_complete`）
- CI 新增 `cargo deny`（`deny.toml`）：檢查相依套件的授權白名單、`*` 版本需求與來源（只允許 crates.io）；弱點檢查仍由 `cargo audit` 負責
- 收尾視窗（`m`）顯示預計產生的 commit / PR 數量與標題、說明（有分組時列出每組）。
- 分組收尾：Agent 在 `.agt/groups/<組名>/` 放 `FILES`、`COMMIT_MSG`、`PR_BODY.md`，收尾 pr / merge 時每組各自一個 commit（pr 各自一個 PR），未認領的變更自成 `rest` 組。說明見 `docs/Agent-分組收尾說明.md`
- diff 視窗依「已 commit／未 commit 的變更／未追蹤的檔案」分頁（`Tab`、`←` `→`、`h` `l` 或 `1`–`9` 切換，切換後捲回頂端；`c` 複製目前這一頁），區段改用色塊標示，內容超出視窗高度時右側顯示捲軸
- ⚠ 不相容：`agtd` 併入 `agt`，只剩單一執行檔；daemon 改以 `agt daemon run` 執行（通常由 agt 自動在背景啟動），`agt config check` 與 `agt doctor` 也不再另外啟動 `agtd`。release 壓縮檔、安裝腳本、`agt update`、`docker/publish.sh` 都改為單檔並刪除舊的 `agtd`；端對端測試移到 `crates/agt-cli/tests/`。**從舊版升級時**，舊的 daemon 還在執行（協定版本不同，`agt daemon stop` 找不到它）要先手動結束：Windows `taskkill /IM agtd.exe /F`、Linux `pkill agtd`，再執行新版 agt（分頁資料保留，可用 Ctrl+A → R 接續）
- ⚠ 不相容：畫面封包的編碼由 `bincode` 1.x（已停止維護）改為 `postcard`，協定版本 1 → 2；升級後請執行 `agt daemon restart`（分頁保留）
- 偵測到版本不同的 daemon 還在執行（例如從有獨立 agtd 的舊版升級）時，`agt`、`agt daemon stop/restart` 會說明原因，並依平台列出結束它的指令，不再誤報「daemon 沒有在執行」
- Agent 的指令是 `docker` 但 Docker 沒有開啟時，建立分頁與重新啟動會明確提示「Docker 沒有開啟」（找不到 docker 指令時提示尚未安裝），不再讓容器指令莫名失敗
- `docker/proxy/`：只放行指定網域的 squid proxy 範例（容器網路限制）；`docker/agy` 可用 `AGY_INSTALL_SHA256` 建置參數核對安裝腳本
- CI 新增 TUI 冒煙測試與安裝腳本檢查；TUI 新增不需要 daemon 的 `App` 單元測試（OSC 52 確認、前綴鍵、分頁切換、輸入轉送）
- 收尾規則改由 agt 提供：建立分頁時在 worktree 寫入 `.agt/README.md`（說明檔格式、多個主題時如何分組），Agent 提示只需要叫它先讀這個檔案。收尾視窗會提醒 `.agt/` 裡 agt 不認得的項目（例如 Agent 自己寫的 `split/`），以及改動很大卻沒有分組的情況；`agt config check` 會提醒提示沒有提到收尾規則的 `auto_commit` Agent

### 修正

- daemon、核心與 client 的取鎖改用不會中毒的 helper（`agt_proto::sync`），某個執行緒 panic 後不會讓其他分頁跟著連鎖 panic
- TUI 冒煙測試：OSC 52 預設改為 `ask` 後測試設定沒跟著更新，改成 `osc52 = "on"`，並改為逐格讀取畫面，不再因為 pyte 處理寬字元的限制而丟出 `IndexError`

## 0.2.1 - 2026-11-20

### 新增

- 新增分頁表單的 Agent 欄位、左側欄分頁清單（第二行行尾的 `auto`）與標題列（`[auto_commit]`）會標出 `auto_commit`（該 Agent 收尾前會自動 commit）；`agt config check` 也列出每個 Agent 展開 `extends` 後的 `auto_commit` 值
- 左側欄標題列加上可點的「«」收合按鈕，收合後顯示「»」展開按鈕（與 `b` 鍵相同）；空間太窄時「«」會隱藏
- `agt doctor`：一次檢查執行環境，每個問題附上下一步。項目：agt 與 agtd 是否在同一個資料夾、daemon 狀態（版本與通訊協定是否一致、是否因設定檔錯誤而使用預設值）、設定檔（沿用 `agt config check`）、資料目錄可否寫入與 `agtd.log` 最近的錯誤、git（版本、`user.name` / `user.email`）、GitHub CLI 登入、Docker / Docker Sandboxes、`curl` 與 `tar`（`agt update` 需要）。只讀取不修改；有失敗項目時 exit 1，選用工具沒安裝只列為資訊
- `agt config check [--file F] [--repo DIR]`：不啟動 daemon 就檢查設定檔。語法與欄位錯誤顯示檔名與行號（exit 1）；合法但可疑的項目列為警告（exit 0）：Agent 的 `command` 不在 PATH 中、拼錯的 `{變數}`（`update` 不支援變數）、`status_source = "hooks"` 但不是 claude、`resume_args` 用了 `{conversation}` 卻沒有 `new_args`、`[repos]` 的目錄不存在、`copy_files` 不是 repo 內的相對路徑；`--repo` 一併檢查該 repo 的 `.agt.toml`。只檢查使用者設定檔裡寫到的 Agent，沒安裝的內建 Agent 不會一直警告
- `agt uninstall`：停止 daemon、移除 agt 的 Docker 容器（`label=agt=1`）與沙箱（`agt-*`）、刪除 `agt` / `agtd`、從 Windows 使用者 PATH 移除安裝位置。設定與資料（`--purge`）、worktree（`--worktrees`）預設不刪，互動模式會另外詢問；`--dry-run` 只列出會做什麼，`-y` 略過確認。Docker 映像檔與 volume、repo 裡的 `agt/*` branch 不會動
- `agt update`：查詢 GitHub Release 最新版、下載並核對 SHA256 後換掉 `agt` 與 `agtd`，daemon 原本在執行時會用新版重新啟動；Agent 執行中會先詢問（`-y` 略過）。`--check` 只檢查、`--version` 指定版本。執行中的舊檔改名為 `.old`，下次啟動時清除
- 發布流程 `.github/workflows/release.yml`：推送 `v*` 標籤時驗證版號、在 Windows（MSVC）與 Linux x64 建置、上傳壓縮檔與 `SHA256SUMS` 並發布 Release；安裝腳本 `scripts/install.sh`、`scripts/install.ps1`（核對 SHA256、不需系統管理員、加入 PATH、最後執行 `agt setup`）
- OpenAI Codex CLI adapter（`codex`）與 `codex-docker` 沙箱（`docker/codex/`），`agt setup` 可以勾選加入
- 新建分頁時先執行 Agent 的 `update` 指令再啟動 Agent，`claude-docker` 等沙箱分頁會自動使用最新版；可用 `update_on_new = false` 關閉
- 底部狀態列最右邊顯示 agt 的版本號
- Agent 結束（例如 `/exit`）後，畫面上顯示可點擊的「重新啟動，接續對話（`R`）」與「開新對話（`N`）」按鈕
- Agent 設定新增 `extends`（沿用另一個 Agent 的設定）與 `extra_args`（追加參數），例如 `extends = "claude-sbx"` 加上 `extra_args = ["--model", "sonnet", "--advisor", "opus"]` 就能多一個選項，不必複製整串 `args`；daemon 啟動時相同的 `update` 指令只執行一次
- `agt setup` 可以勾選加入 advisor 版的 Claude Code（`claude-advisor`、`claude-sbx-advisor`、`claude-docker-advisor`：Sonnet 執行、Opus 擔任顧問），只選 advisor 版時會一併加入它沿用的 `claude-sbx` / `claude-docker`；開啟 agt 時也會提醒加入（可選「不要再提醒」）
- 沙箱分頁收尾選 pr 時使用 Agent 寫的 `.agt/COMMIT_MSG`（commit 訊息與 PR 標題）與 `.agt/PR_BODY.md`（PR 說明）；branch 已有開啟中的 PR 時只 push 並更新說明，不再建立失敗。分頁的 base 只在本機（例如還沒 push 的 feat branch）但內容都已在 remote 預設 branch 時，PR 改以預設 branch 為目標；否則在 push 前停下並說明。`claude-docker` 與 `claude-sbx` 範例的提示會請 Claude 寫這兩個檔案；另附主機用的 `docker/claude/agt-pr.cmd`
- `.agt/` 加入主 repo 的 info/exclude，說明檔不會出現在 diff，也不會被 auto_commit 加進 commit
- 新增分頁表單的 Base 欄位留空時顯示實際會用的 branch（主 repo 目前的 HEAD）；它不是 remote 的預設 branch 時以黃字提醒，避免 PR 開到 feature branch
- 收尾（merge / pr / discard）失敗時寫進 `agtd.log` 與該分頁的事件記錄，錯誤視窗關掉後也查得到原因
- 在 workspace 與各 crate 的 `Cargo.toml` 加上 `rust-version = "1.89"`，Rust 太舊時 cargo 會直接指出需要的版本，不再出現難懂的編譯錯誤

### 修正

- 資料庫版本比 agt 新時（例如降版後）拒絕開啟並說明原因，不再靜默使用而可能寫壞資料
- 錯誤訊息等文字視窗會依視窗寬度自動換行，長訊息不再被切掉（diff 視窗維持不換行）；按 `c`（或 `y`）可把全部內容複製到剪貼簿（例如收尾 pr 失敗的訊息）

## 0.2.0 - 2026-11-18

第一個正式標記的版本，包含 0.1.0 之後累積的所有變更。

### 新增

- 關閉分頁時可選擇刪除 worktree 副本與 branch，或保留副本；保留的副本會被註記，開啟 agt 時提示清理，`agt gc --prune` 也會一併清理（#23）
- diff 視窗可以用滑鼠滾輪捲動（#22）
- 開啟 agt 時檢查環境（git、Docker、Docker Sandboxes），缺少的項目可以選擇安裝（#21）
- 顯示分頁的 worktree 路徑，並可在檔案總管中開啟（#20）
- GitHub Copilot CLI adapter 與 `copilot-docker` 沙箱（#2）

### 修正

- 確認框的內容折行時，按鈕不再被擠到框外（#21）
- Copilot 分頁接續自己的對話；setup 正確辨識已加入的 Agent（#5）
- `publish.cmd` 改為只用 ASCII，避免 cmd 把中文註解當成指令執行（#4）
- 重新啟動後忽略前一次 Agent 晚到的事件（#3）
- merge 安全檢查、IPC 大小上限、重新啟動的競爭狀況（#1）

### 安全

- 本機資料保護（紀錄檔位置與保留期限、OSC 52 確認、文字過濾）、供應鏈與引號處理強化（#10）
- 補齊沙箱缺口；IPC 只接受同一位使用者（#9）
- 固定 worktree 的 gitdir，避免沙箱內的 Agent 換掉 `.git` 讓主機執行程式（#8）
- `copy_files` 拒絕經過符號連結的路徑（#7）

### 測試

- 端對端測試也在 Windows 上執行（#6）

## 0.1.0 - 2026-11-17

- 初始版本：依《agt：AI Agent 終端機設計規格》v1.0 實作的 Rust workspace
