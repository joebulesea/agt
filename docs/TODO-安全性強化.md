# 待辦：安全性強化（階段 4、5 與已知缺口）

來源：對整個專案做的一次安全審查。階段 1～3 已完成；這份文件記錄**還沒做**的部分，以及已完成階段中刻意留下的缺口。

## 已完成（供對照）

| 階段 | 內容 | 狀態 |
| --- | --- | --- |
| 1 | worktree 的 `.git` 驗證與 `GIT_DIR` 固定、關閉 `core.fsmonitor` | PR #8 |
| 1 | `add_exclude` 不再執行 git、巢狀 repo（gitlink）處理、`.git` 被改動時提示並可丟棄 | `fix/sandbox-hardening` |
| 2 | `copy_files` 拒絕符號連結 | PR #7 |
| 2 | Agent 執行檔移到唯讀 volume、容器加固參數、收尾時對自動執行的設定檔示警 | `fix/sandbox-hardening` |
| 3 | IPC 只接受同一位使用者（Unix peer uid / socket 目錄；Windows pipe ACL / 擁有者檢查）、`Resize` 上限、Hello 逾時 | `fix/sandbox-hardening` |

---

## 階段 4：本機資料

### 4.1 終端機紀錄檔位置與保留期限

現況：紀錄檔放在 `%APPDATA%\agt\data\logs\`（Windows 的 Roaming），每個分頁最多約 100 MB（`LOG_MAX_BYTES` 50 MB × 兩份輪替），可能含有機密（token、貼上的內容）。網域環境的漫遊設定檔會把它同步到伺服器。

- [ ] **決定位置**：
  - A. 只把 `logs\` 搬到 `%LOCALAPPDATA%\agt\logs`（資料庫與設定維持原位）
  - B. 整個資料目錄搬回 `%LOCALAPPDATA%\agt`
  - 注意：先前是刻意從 `%LOCALAPPDATA%` 搬到 `%APPDATA%` 的（見 `Paths::legacy_data_dir` 與 `migrate_from`），B 等於反轉那個決定。偏向 A
- [ ] 搬移既有紀錄檔（比照 `migrate_from` 的作法：舊 daemon 還在執行時沿用舊位置）
- [ ] 紀錄檔保留期限：daemon 啟動時與定期清掉超過 N 天、或對應分頁早已關閉的檔案（現在只有關閉分頁時刪，daemon 異常結束會留下孤兒檔）
- [ ] 設定項：`general.log_output`（已有）之外，考慮 `log_retention_days`
- [ ] Windows 上建立 `logs\` 時設定只有目前使用者可存取的 ACL（Unix 已是 0600）

涉及檔案：`crates/agt-daemon/src/config.rs`（`Paths`）、`crates/agt-store/src/lib.rs`（`LogWriter`）、`crates/agt-daemon/src/state.rs`（`remove_entry`）

### 4.2 `events` 資料表只增不減

- [ ] 分頁關閉後保留一段時間再清（例如 30 天），或限制每個分頁的筆數
- [ ] `sessions` 資料表中已結束的分頁同理
- [ ] `events.data` 內含 hook 回報的 `message` / `tool` 文字，確認不會記到機密

涉及檔案：`crates/agt-store/src/lib.rs`（新增 migration 與清理函式）、`crates/agt-daemon/src/main.rs`（啟動時清理）

### 4.3 OSC 52 剪貼簿

現況：分頁中的程式可用 OSC 52 把最多 1 MiB 寫進使用者的剪貼簿（只限目前正在看的分頁，`app.rs` 已擋背景分頁）。沙箱內的 Agent 因此可以覆蓋剪貼簿內容（例如換成惡意指令，等使用者貼到終端機）。

- [ ] 新增設定 `general.osc52 = "off" | "ask" | "on"`（預設建議 `ask`）
- [ ] `ask`：TUI 顯示「分頁要求複製 N 字到剪貼簿，內容開頭：…」讓使用者確認
- [ ] 降低上限（1 MiB 對一般用途太大，例如 64 KiB）
- [ ] 拒絕包含控制字元（尤其是換行）的內容，或在確認畫面完整顯示

涉及檔案：`crates/agt-daemon/src/state.rs`（`SessionEvent::Clipboard`）、`crates/agt-tui/src/app.rs`、`crates/agt-tui/src/lib.rs`、`crates/agt-daemon/src/config.rs`

---

## 階段 5：供應鏈與零星加固

### 5.1 映像檔與安裝來源

- [ ] `docker/agy/Dockerfile`：`curl ... | bash` 安裝 agy，改成下載指定版本並核對 SHA256
- [ ] npm 套件（`@anthropic-ai/claude-code`、`@github/copilot`）改成可指定版本；更新指令（`update`）要有「只升級到已審視過的版本」的選項，而不是無條件裝最新版
- [ ] base image（`node:22-bookworm-slim`、`debian:bookworm-slim`、`rust:1-bookworm`）釘 digest
- [ ] `docker/Dockerfile`（開發用）中全域 git 設定的假身分只用於測試，確認不會被帶進發布流程

### 5.2 CI

- [ ] `.github/workflows/ci.yml`：`actions/checkout@v4`、`dtolnay/rust-toolchain@stable` 改釘 commit SHA
- [ ] 新增 `cargo audit`（或 `cargo deny`）檢查相依套件的已知弱點與授權
- [ ] 設定 `permissions: contents: read`（目前使用預設權限）
- [ ] Dependabot 或 Renovate 更新 `Cargo.lock`、GitHub Actions、Dockerfile base image

### 5.3 引號與輸出過濾

- [ ] `platform::ps_quote` 只處理 `'`，PowerShell 也把 `‘` `’` `‚` `‛` 視為單引號，需一併跳脫
- [ ] `.cmd` / `.bat` 經 `cmd.exe /d /c` 執行時，參數裡的 `&` `|` `%` `^` 沒有跳脫（`platform::resolve_program`）；評估改為拒絕這類參數，或限制只在已知可信的指令上使用
- [ ] `agt diff` 直接把 diff 內容印到終端機：檔案內容裡的控制序列（含 OSC 52）會被終端機執行。輸出前過濾 ESC 以外的控制字元，TUI 的 diff 檢視器同理
- [ ] 分頁標題、通知內容（OSC 9 / 777、hook 的 `message`）進入系統通知與 TUI 前先過濾控制字元

---

## 已完成階段中刻意留下的缺口

### 階段 2 → 共用家目錄

`agt-*-home` volume 由所有分頁共用且可寫（登入憑證、對話記錄、Agent 的全域設定）。被操控的分頁可以改裡面的全域設定來影響之後的分頁。原本想把憑證改成唯讀掛載，但 Agent 需要寫回家目錄更新登入 token，在沒辦法測試登入流程的情況下沒有冒險改。

- [ ] 實測各 Agent 的登入流程，找出哪些檔案必須可寫，其餘（例如 `settings.json`、`CLAUDE.md`、hooks、MCP 設定）改成唯讀或在每次啟動時由 entrypoint 重新寫入
- [ ] 或提供「每個分頁獨立家目錄」的正式選項（`config/default.toml` 範例上方已寫了手動做法，但沒有實際跑過）：每個分頁一個 volume `agt-home-{session}` + `on_close` 刪除 + 環境變數 token 登入
- [ ] token 傳遞：`-e GH_TOKEN` 等會讓容器內所有程式讀到；評估改用 Docker secret 或只在需要的指令上注入

### 階段 2 → 網路

容器目前可以連外網，沒有限制。Agent 被操控時可以把 worktree 內容或傳入的 token 送出去。

- [ ] 提供限制網路的範例設定（只允許 Agent 的 API 網域與套件來源）；`claude-sbx` 的 Docker Sandboxes 有網路政策可用，`agt setup` 已會偵測是否設定
- [ ] `--memory`、`--cpus` 資源上限寫進範例（目前只在 `claude-sbx` 範例裡有）

### 階段 2 → 示警清單

`git::is_sensitive_path` 的清單是固定的，只涵蓋常見的自動執行位置。

- [ ] 評估讓使用者在設定檔追加路徑（`general.sensitive_paths`）
- [ ] `package.json` 的 `scripts`、`Makefile`、`pyproject.toml` 等「之後會被主機執行」的檔案沒有列入（列入會讓示警太頻繁，需要取捨）

### 階段 3 → 同一使用者內的權限

在主機上直接執行（非沙箱）的 Agent 與 `agt` 屬於同一個使用者，可透過 `AGT_SOCKET` 與 `AGT_SESSION_ID` 操作任何分頁（輸入按鍵、收尾、刪除）。

- [ ] 給每個分頁一組隨機 token（`AGT_SESSION_TOKEN`），hook 只能用自己的 token 呼叫 `Notify`；其餘操作（`Input`、`Finish`、`Kill`…）只有 UI 類連線能用
- [ ] 區分 `client_kind`：`notify` 連線只允許 `Hello` + `Notify`

### 階段 3 → 連線資源

- [ ] 回應佇列（`out`，無上限的 channel）與連線數沒有上限。目前只有本人能連線，所以只是自己對自己的 DoS，優先度低
- [ ] 以系統管理員身分（提升權限）執行 daemon 時，pipe 擁有者可能是 Administrators 群組：client 目前接受，這條路徑沒有實測

---

## 測試缺口

- [ ] Windows 上的 daemon 與 TUI 端對端測試（`crates/agt-daemon/tests/e2e.rs` 只在 Unix 執行）；階段 1、3 的單元測試與一次手動實跑已在 Windows 驗證
- [ ] 用第二個 Windows 帳號實測「其他使用者被 pipe ACL 拒絕」（目前只驗證了 ACL 內容）
- [ ] macOS 完全沒有測試環境
- [ ] 各 Agent 在加固參數（`--cap-drop=ALL` 等）與唯讀 tools volume 下的完整登入與使用流程（目前只驗證了容器能啟動、執行檔能執行）
- [ ] 把「Agent 竄改 `.git` / 巢狀 repo 後主機不執行任何東西」做成端對端測試（目前是 `agt-git` 的單元測試）

---

## 建議順序

1. 階段 4.3（OSC 52）與 5.3（輸出過濾）：改動小、風險直接，一起做
2. 階段 4.1 / 4.2（資料保留）：需要先決定紀錄檔位置
3. 共用家目錄與網路限制：需要實測各 Agent 的登入流程，最花時間
4. 階段 5.1 / 5.2（供應鏈與 CI）：可獨立進行
