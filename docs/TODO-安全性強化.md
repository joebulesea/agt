# 待辦：安全性強化（剩餘項目）

來源：對整個專案做的一次安全審查。階段 1～5 的程式部分已完成；這份文件記錄**還沒做**、**刻意不做**的部分，以及測試缺口。

## 已完成（供對照）

| 階段 | 內容 | 位置 |
| --- | --- | --- |
| 1 | worktree 的 `.git` 驗證與 `GIT_DIR` 固定、關閉 `core.fsmonitor` | PR #8 |
| 1 | `add_exclude` 不再執行 git、巢狀 repo（gitlink）處理、`.git` 被改動時提示並可丟棄 | PR #9 |
| 2 | `copy_files` 拒絕符號連結 | PR #7 |
| 2 | Agent 執行檔移到唯讀 volume、容器加固參數、收尾時對自動執行的設定檔示警 | PR #9 |
| 3 | IPC 只接受同一位使用者（Unix peer uid / socket 目錄；Windows pipe ACL / 擁有者檢查）、`Resize` 上限、Hello 逾時 | PR #9 |
| 4.1 | Windows 紀錄檔改放 `%LOCALAPPDATA%\agt\logs`（不隨漫遊設定檔同步），啟動時搬移舊檔、刪除不屬於任何分頁的紀錄檔 | `fix/security-phase4-5` |
| 4.2 | 關閉超過 30 天、且已沒有 worktree 的分頁連同事件一起清除 | `fix/security-phase4-5` |
| 4.3 | `general.osc52 = off \| ask \| on`（預設 ask）、上限降為 64 KiB | `fix/security-phase4-5` |
| 5.1 | base image 釘 digest、Claude Code / Copilot CLI 可固定版本（建置參數或 `AGT_*_VERSION`） | `fix/security-phase4-5` |
| 5.2 | CI 唯讀權限、action 釘 SHA、`cargo audit`、Dependabot；`rusqlite` 升級修正 RUSTSEC-2026-0459 | `fix/security-phase4-5` |
| 5.3 | PowerShell 智慧引號跳脫、`.cmd` 拒絕 cmd 特殊字元、diff / 訊息 / 通知過濾控制字元 | `fix/security-phase4-5` |
| 缺口 | `general.sensitive_paths` 追加示警路徑、同時連線數上限 64、範例加上資源與網路說明 | `fix/security-phase4-5` |

---

## 尚未完成

### agy 安裝腳本無法核對

`docker/agy/Dockerfile` 仍以 `curl ... | bash` 安裝 agy。Google 沒有公布安裝腳本或執行檔的 SHA256，沒有可以核對的值。

- [ ] 官方提供版本化的下載網址與雜湊後，改成下載指定版本並核對 SHA256
- [x] 在那之前的替代做法：`docker build --build-arg AGY_INSTALL_SHA256=<雜湊>` 核對安裝腳本（雜湊需要人工更新；沒設定時建置會印出警告）

### 共用家目錄

`agt-*-home` volume 由所有分頁共用且可寫（登入憑證、對話記錄、Agent 的全域設定）。被操控的分頁可以改裡面的全域設定來影響之後的分頁。原本想把憑證改成唯讀掛載，但 Agent 需要寫回家目錄更新登入 token，在沒辦法測試登入流程的情況下沒有冒險改。

- [ ] 實測各 Agent 的登入流程，找出哪些檔案必須可寫，其餘（例如 `settings.json`、`CLAUDE.md`、hooks、MCP 設定）改成唯讀或在每次啟動時由 entrypoint 重新寫入
- [ ] 或提供「每個分頁獨立家目錄」的正式選項（`config/default.toml` 範例上方已寫了手動做法，但沒有實際跑過）：每個分頁一個 volume `agt-home-{session}` + `on_close` 刪除 + 環境變數 token 登入
- [ ] token 傳遞：`-e GH_TOKEN` 等會讓容器內所有程式讀到；評估改用 Docker secret 或只在需要的指令上注入

### 網路限制

容器可以連外網。Docker 本身無法只允許特定網域，範例設定已說明改用 `claude-sbx`（`sbx policy`）或自行架設 proxy。

- [x] 提供 proxy 範例：`docker/proxy/`（squid + internal 網路；**尚未在實際 Docker 環境測試**）

### 相依套件的維護性警告

`cargo audit` 目前沒有弱點，但有 4 個不會讓 CI 失敗的警告：`bincode` 1.x、`paste` 不再維護；`lru` 0.12（經由 `ratatui`）有 unsound 的 API。

- [x] `bincode` 換成 `postcard`（協定版本 1 → 2）；`paste` 警告仍在（`ratatui` 0.29 的相依）
- [ ] 升級 `ratatui`：0.30 可以編譯、測試通過且 `lru` / `paste` 警告消失，但（1）`Terminal::clear()` 會查詢游標位置（已找到替代做法）、（2）含中文與 `◀ ▶` 的列，冒煙測試的畫面與 0.29 不同：右邊框偏移、部分空格消失。無法在真實終端機確認是否只是 pyte 的差異，中文環境風險大，先留在 0.29；之後可在實際的 Windows Terminal 上試過再決定

### 以系統管理員身分執行 daemon

提升權限執行 daemon 時，pipe 擁有者可能是 Administrators 群組，client 目前接受。這條路徑沒有實測。

- [ ] 實測提升權限的 daemon 與一般權限的 client 是否能正常連線。程式碼檢查結果：pipe 的 ACL 只含 SYSTEM 與「目前使用者的 SID」（提升權限後仍是同一個 SID）；client 接受擁有者為本人、Administrators、SYSTEM。理論上可行，但沒有在 Windows 上實測；系統管理員本來就能操作你的行程，不另外收緊

---

## 刻意不做

### 同一使用者內再區分權限（分頁 token）

在主機上直接執行（非沙箱）的 Agent 與 `agt` 屬於同一個使用者，可透過 `AGT_SOCKET` 操作任何分頁。原本考慮給每個分頁一組 token、限制 hook 連線只能呼叫 `Notify`。

不做的原因：這樣的 Agent 本來就能以使用者身分直接執行 `agt kill`、讀寫所有檔案，token 擋不住它，只會增加複雜度。真正的隔離是讓 Agent 在沙箱裡執行（沙箱內拿不到 socket）。

### 回應佇列上限

每條連線的回應佇列（`out`）沒有上限。IPC 只接受同一位使用者，而且已限制同時連線數，剩下的是自己對自己的 DoS，不值得增加複雜度。

---

## 測試缺口

- [x] OSC 52 的 `ask` 對話框：`Client::detached()` 讓 `App` 不需要 daemon 就能建立，已有單元測試（`agt-tui` 的 `app::tests`）
- [ ] Windows 紀錄檔搬移只有單元測試，沒有在真實的 `%APPDATA%` / `%LOCALAPPDATA%` 上實跑
- [ ] 用第二個 Windows 帳號實測「其他使用者被 pipe ACL 拒絕」（目前只驗證了 ACL 內容）
- [ ] 各 Agent 在加固參數（`--cap-drop=ALL` 等）與唯讀 tools volume 下的完整登入與使用流程（目前只驗證了容器能啟動、執行檔能執行、版本固定有效）
- [ ] 把「Agent 竄改 `.git` / 巢狀 repo 後主機不執行任何東西」做成端對端測試（目前是 `agt-git` 的單元測試）
