# 更新紀錄

版號採用 [語意化版本](https://semver.org/lang/zh-TW/)，1.0 之前的規則如下：

- **PATCH**（0.2.0 → 0.2.1）：修正錯誤、調整文字或介面細節
- **MINOR**（0.2.x → 0.3.0）：新功能、使用者看得到的行為改變，或不相容的改變（標示「⚠ 不相容」）

每個 PR 不改版號，只在「未發布」加上一行；發布時依其中最大的變更類型決定新版號，修改 `Cargo.toml`、打 tag `vX.Y.Z`，再執行 `docker/publish.sh`。

## 未發布

### 新增

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
