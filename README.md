# agt：AI Agent 終端機多工器

依照《agt：AI Agent 終端機設計規格》v1.0 實作的 Rust workspace。每個 AI Agent 工作階段是左側欄的一個分頁，各自在獨立的 git worktree / branch 上工作；背景的 `agtd` 持有所有 PTY，UI 關閉後 Agent 照常執行。

## 建置

需要 Rust 1.89 以上（edition 2024、`File::try_lock`）。

```sh
cargo build --release          # 產出 target/release/agt 與 agtd（放在同一個目錄）
cargo test --workspace         # 單元測試 + 端對端測試（Unix）
cargo clippy --workspace --all-targets -- -D warnings
```

沒有本機 Rust 環境時，可以用 Docker（`docker/Dockerfile` 內含 Linux 工具鏈與 Windows 交叉編譯用的 MinGW）：

```sh
docker build -t agt-dev docker/
docker/cargo.sh test --workspace
docker/cargo.sh build --release --target x86_64-pc-windows-gnu --workspace   # Windows 版
docker/publish.sh [--restart]   # 發布：編譯 Windows 版並安裝到 %APPDATA%\agt（Git Bash）
docker\publish.cmd [--restart]  # 同上，在 cmd 執行或雙擊（會自動找 Git Bash、缺 agt-dev 映像時先建置）
```

TUI 冒煙測試（在容器中以 pty + pyte 驅動 TUI 並截取畫面）：`python3 tests/tui_smoke.py <bin 目錄>`。

## 使用

```sh
agt                                    # 開啟 TUI（必要時自動在背景啟動 agtd）
agt new claude --repo ~/proj --task "登入功能"   # 建立分頁：worktree + branch agt/claude/<slug>-<id>
agt ls                                 # 列出分頁
agt attach <id>                        # 開啟 TUI 並切到該分頁
agt diff <id>
agt finish <id> merge --strategy squash   # merge | pr | keep | discard
agt kill <id> [--cleanup]
agt restart <id> [--fresh]             # daemon 重啟後，在同一個 worktree 重新啟動 Agent（接續上次的對話；--fresh 開新對話）
agt gc [--prune]                       # 比對 git worktree 與資料庫，列出 / 清理留下的 worktree 副本
agt setup [--yes [--install] [--build]]   # 檢查並安裝 git / Docker / Docker Sandboxes，加入 Agent（第一次執行 agt 時自動執行）
agt daemon start|stop|restart|status
```

TUI 快捷鍵：前綴鍵 `Ctrl+A`，之後按 `n` 新增、`j/k` 或 `1–9` 切換、`w` 跳到等待輸入的分頁、`d` diff、`m` 收尾、`r` 重新命名、`R` 重新啟動並接續對話、`N` 重新啟動並開新對話、`[` 滾動與 `/` 搜尋、`o` 開啟工作目錄、`x` 關閉（選擇刪除或保留 worktree 副本；保留的副本下次開啟 agt 時會提示清理）、`b` 收合側欄、`?` 說明、`q` detach。

設定檔範例與預設值見 [`config/default.toml`](config/default.toml)。

## 專案結構

| crate | 內容 |
| --- | --- |
| `agt-proto` | IPC 訊息、長度前綴封包、傳輸層（named pipe / UDS）、共用 client |
| `agt-core` | Session（PTY + `alacritty_terminal` + 狀態機）、AgentAdapter 介面、`platform` 模組 |
| `agt-agents` | Claude Code（hooks）、Antigravity、GitHub Copilot CLI（規則比對）、Shell、自訂 Agent |
| `agt-git` | WorktreeManager：直接呼叫 `git` |
| `agt-store` | SQLite（WAL、`user_version` migration）、可輪替的終端機紀錄檔 |
| `agt-daemon` | `agtd`：IPC Router、工作階段管理、設定檔熱重載、閒置關閉；端對端測試在 `tests/e2e.rs` |
| `agt-tui` | ratatui 介面，只依賴 `agt-proto` |
| `agt-cli` | `agt` 執行檔 |

## Docker 沙箱（claude-docker、claude-sbx、agy-docker、copilot-docker）

最簡單的方式：執行 `agt setup`（第一次執行 `agt` 時會自動出現）。它會偵測 Docker Desktop 與 Docker Sandboxes、用選單勾選要加入的 Agent、產生設定檔，並把內建的 Dockerfile 寫到設定目錄、詢問是否建置映像檔。之後每次開啟 agt 偵測到新的可用 Agent 時也會詢問是否加入。

`claude-sbx` 使用 [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/)（`sbx`，microVM，不需要 Docker Desktop）：沙箱保留到分頁關閉，沙箱內可唯讀使用 git。相關設定 `restart_args`（重新啟動時取代 `args`）、`on_close`（只在關閉分頁時執行）與 `{gitdir}` 變數，範例見 [`config/default.toml`](config/default.toml) 的 `[agents.claude-sbx]`。


在容器中執行 Claude Code，容器只看得到該分頁的 worktree：`docker build -t agt-claude docker/claude/`，再把 [`config/default.toml`](config/default.toml) 裡的 `[agents.claude-docker]` 範例複製到設定檔。Antigravity CLI 同理：`docker build -t agt-agy docker/agy/`，範例是 `[agents.agy-docker]`；GitHub Copilot CLI 是 `docker build -t agt-copilot docker/copilot/`，範例是 `[agents.copilot-docker]`。Agent 設定可用 `adapter`（沿用內建 Agent 的狀態判斷）、`cleanup`（分頁結束時執行的收尾指令）、`update`（daemon 啟動時在背景執行的更新指令；Agent 的執行檔放在分頁中唯讀的 volume，只由這個指令更新）與 `{cwd}`、`{session}` 變數。容器內無法使用 git，commit 由收尾選單處理。詳見 [Windows 使用說明](docs/Windows-TUI-使用說明.md) 第 10 節。

## 環境變數

| 變數 | 用途 |
| --- | --- |
| `AGT_HOME` | 把設定、資料與 worktree 都放在這個目錄（測試、可攜模式） |
| `AGT_SOCKET` | daemon 的 socket / pipe 路徑；Agent 內由 daemon 注入 |
| `AGT_SESSION_ID` | 由 daemon 注入 Agent，`agt notify` 用來找到分頁 |
| `AGT_LOG` | daemon 日誌等級（預設 `info`，寫入資料目錄的 `agtd.log`） |
| `AGT_TRACE_IPC` | 設定後把每條連線的讀寫量印到 stderr（除錯用） |

## 與規格的差異與未完成項目

- **端對端測試位置**：放在 `crates/agt-daemon/tests/`（才能取得 `agtd` 執行檔路徑），不是根目錄的 `tests/`；根目錄的 `tests/` 放 TUI 冒煙測試。
- **Windows 結束行程樹**：用 `taskkill /T /F` 而不是 Job Object。
- **IPC 存取限制**：只接受同一位使用者。Unix：socket 目錄 0700 且必須是自己的（不接受符號連結）、socket 0600，兩端都檢查對方的 uid；Windows：pipe 的 ACL 只允許目前的使用者與 SYSTEM，client 連線後確認 pipe 的擁有者。連線後 10 秒內沒有送出 Hello 會被中斷。同一位使用者的行程之間沒有再細分權限（分頁裡的 Agent 若在主機上執行，可以透過 `AGT_SOCKET` 操作其他分頁）。
- **Windows 路徑長度**：worktree 路徑超過 240 字元且未開啟 `core.longpaths` 時拒絕建立（保留檔案名稱的空間）。
- **Antigravity CLI / GitHub Copilot CLI**：只有規則比對（規格 14.2 待確認是否有 hooks）。Antigravity CLI 的 `--continue` 接續的是這個目錄最近一次的對話；Copilot CLI 用 `--session-id` 接續分頁自己的對話。
- **未實作**：`cargo-dist` 打包、主題設定、`agt daemon stop` 之外的開機自動啟動（規格待決）。
- **Windows 原生**：已在 Windows 11 以交叉編譯的執行檔實測 CLI 與 daemon 全流程；TUI 只在 Linux 容器內做過自動化測試，尚未在 Windows Terminal 中手動操作過。
