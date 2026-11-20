# 更新紀錄

版號採用 [語意化版本](https://semver.org/lang/zh-TW/)，1.0 之前的規則如下：

- **PATCH**（0.2.0 → 0.2.1）：修正錯誤、調整文字或介面細節
- **MINOR**（0.2.x → 0.3.0）：新功能、使用者看得到的行為改變，或不相容的改變（標示「⚠ 不相容」）

每個 PR 不改版號，只在「未發布」加上一行；發布時依其中最大的變更類型決定新版號，修改 `Cargo.toml`、打 tag `vX.Y.Z`，再執行 `docker/publish.sh`。

## 未發布

### 新增

- ⚠ 不相容：畫面封包的編碼由 `bincode` 1.x（已停止維護）改為 `postcard`，協定版本 1 → 2；升級後請執行 `agt daemon restart`（分頁保留）
- 發布流程：推送 `v*` 標籤時由 `.github/workflows/release.yml` 編譯 Windows x64（MSVC）與 Linux x64、打包並附上 `SHA256SUMS` 建立 Release；新增安裝腳本 `scripts/install.ps1`、`scripts/install.sh`（核對 SHA256 後安裝）
- `agt update`（`--check` 只檢查、`--version` 指定版本）：下載、核對 SHA256、結束 daemon 後換掉執行檔（舊檔先改名為 `.old`，失敗會回復）；`agt uninstall`：停止 daemon、刪除 `agt=1` 容器與執行檔，設定、資料與 worktree 另外詢問。發布位置由 `AGT_REPO` 指定
- Agent 的指令是 `docker` 但 Docker 沒有開啟時，建立分頁與重新啟動會明確提示「Docker 沒有開啟」（找不到 docker 指令時提示尚未安裝），不再讓容器指令莫名失敗
- `docker/proxy/`：只放行指定網域的 squid proxy 範例（容器網路限制）；`docker/agy` 可用 `AGY_INSTALL_SHA256` 建置參數核對安裝腳本
- CI 新增 TUI 冒煙測試與安裝腳本檢查；TUI 新增不需要 daemon 的 `App` 單元測試（OSC 52 確認、前綴鍵、分頁切換、輸入轉送）
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

- 偵測到版本不同的 daemon 還在執行（例如從有獨立 agtd 的舊版升級）時，`agt`、`agt daemon stop/restart` 會說明原因，並依平台列出結束它的指令，不再誤報「daemon 沒有在執行」

### 內部調整

- ⚠ 不相容：`agtd` 併入 `agt`，只剩單一執行檔；daemon 改以 `agt daemon run` 執行（通常由 agt 自動在背景啟動）。升級後舊的 `agtd` / `agtd.exe` 不再使用，`agt update`、安裝腳本與 `docker/publish.sh` 會把它刪除；端對端測試移到 `crates/agt-cli/tests/`。**從舊版升級時**，舊的 daemon 還在執行時（協定版本不同，`agt daemon stop` 找不到它），要先手動結束它：Windows `taskkill /IM agtd.exe /F`、Linux `pkill agtd`，再執行新版 agt（分頁資料保留，可用 Ctrl+A → R 接續）
- 拆分大檔，行為不變：`agt-git`（`finish.rs`、`tests.rs`）、daemon 的 `state`（`create` / `ops` / `maintenance` / `tests`）、TUI 的 `app`（`events` / `requests` / `input` / `popup` / `mouse` / `tests`）與 `ui`（`panes` / `popup` / `tests`）、`agt setup`（`agents` / `offer` / `tests`）

### 修正

- TUI 冒煙測試改用逐格讀取畫面，不再因為 pyte 處理寬字元的限制而丟出 `IndexError`

- TUI 冒煙測試：OSC 52 預設改為 `ask` 後測試設定沒跟著更新，改成 `osc52 = "on"`
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
