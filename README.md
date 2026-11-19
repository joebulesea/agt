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

## 安裝（發布版）

```sh
curl -fsSL https://raw.githubusercontent.com/joebulesea/agt/main/scripts/install.sh | sh     # Linux x64
```

```powershell
irm https://raw.githubusercontent.com/joebulesea/agt/main/scripts/install.ps1 | iex           # Windows x64
```

腳本會下載最新 Release、用 `SHA256SUMS` 核對後安裝（Linux：`~/.local/bin`；Windows：`%APPDATA%\agt`，加入使用者 PATH），最後執行 `agt setup`。用環境變數 `AGT_REPO`（`<owner>/<repo>`，預設 `joebulesea/agt`；fork 要測試時改成自己的）、`AGT_VERSION`、`AGT_NO_SETUP=1` 調整。之後用 `agt update` 自我更新（需要系統有 `curl` 與 `tar`；Windows 10 以後內建）。發布方式：把 `Cargo.toml` 的版號改好，打 tag `vX.Y.Z` 並推送，`.github/workflows/release.yml` 會建置各平台壓縮檔與 `SHA256SUMS` 並發布。

## 使用

```sh
agt                                    # 開啟 TUI（必要時自動在背景啟動 agtd）
agt new claude --repo ~/proj --task "登入功能"   # 建立分頁：worktree + branch agt/claude/<slug>-<id>
agt ls                                 # 列出分頁
agt attach <id>                        # 開啟 TUI 並切到該分頁
agt diff <id>
agt rename <id> "新任務名稱"
agt finish <id> merge --strategy squash   # merge | pr | keep | discard
agt kill <id> [--cleanup]
agt restart <id> [--fresh]             # daemon 重啟後，在同一個 worktree 重新啟動 Agent（接續上次的對話；--fresh 開新對話）
agt gc [--prune]                       # 比對 git worktree 與資料庫，列出 / 清理留下的 worktree 副本
agt setup [--yes [--install] [--build]]   # 檢查並安裝 git / Docker / Docker Sandboxes，加入 Agent（第一次執行 agt 時自動執行）
agt doctor                             # 檢查執行環境（daemon、設定檔、git、Docker、gh…）並說明下一步；有失敗項目時 exit 1
agt config check [--file F] [--repo DIR]   # 檢查設定檔（不需要 daemon）：錯誤含行號，另列出找不到的指令、拼錯的變數等警告
agt update [--check] [--version 0.3.0] [-y]   # 從 GitHub Release 更新 agt / agtd（核對 SHA256、重新啟動 daemon）
agt uninstall [--dry-run] [--purge] [--worktrees] [-y]   # 解除安裝（預設保留設定、資料與 worktree）
agt daemon start|stop|restart|status
```

`agt gc` 用來清理殘留的 worktree：它會比對 git worktree 與資料庫，列出已沒有對應分頁的副本，加上 `--prune` 才會實際刪除。

新增分頁時（`Ctrl+A` → `n`）Base 欄位留空會使用主 repo 目前的 HEAD branch，畫面上會顯示實際的 branch；它不是 remote 的預設 branch 時以黃字提醒，避免 PR 開到 feature branch。底部狀態列最右邊顯示 agt 的版本號。Agent 結束後（例如 `/exit`），畫面上會出現可點擊的「重新啟動，接續對話（`R`）」與「開新對話（`N`）」按鈕。

TUI 快捷鍵：前綴鍵 `Ctrl+A`，之後按 `n` 新增、`j/k` 或 `1–9` 切換、`w` 跳到等待輸入的分頁、`d` diff、`m` 收尾、`r` 重新命名、`R` 重新啟動並接續對話、`N` 重新啟動並開新對話、`[` 滾動與 `/` 搜尋、`o` 開啟工作目錄、`x` 關閉（選擇刪除或保留 worktree 副本；保留的副本下次開啟 agt 時會提示清理）、`b` 收合側欄、`?` 說明、`q` detach。

設定檔範例與預設值見 [`config/default.toml`](config/default.toml)。同一個 Agent 想多一個選項（例如指定模型）時，用 `extends` 沿用既有設定、`extra_args` 追加參數：

```toml
[agents.claude-sbx-advisor]
extends = "claude-sbx"
extra_args = ["--model", "sonnet", "--advisor", "opus"]
```

### Advisor 版 Claude Code

Claude Code 以 Sonnet 執行、由 Opus 擔任顧問（`--model sonnet --advisor opus`）。執行 `agt setup`，在「要加入哪些 Agent？」選單中勾選即可，依偵測到的環境出現：

| Agent | 出現條件 | 沿用的設定 |
| --- | --- | --- |
| `claude-advisor` | 主機上有 `claude` | `claude`（主機版） |
| `claude-sbx-advisor` | 有 Docker Sandboxes | `claude-sbx` |
| `claude-docker-advisor` | 有 Docker | `claude-docker`（使用 `agt-claude` 映像檔） |

- 預設不勾選：需要支援 `--advisor` 的 Claude Code，版本太舊時分頁會一啟動就結束（主機上執行 `claude update`；`claude-docker` 開一個新分頁會自動更新）。
- 只勾選 advisor 版時，設定檔中還沒有的 `claude-sbx` / `claude-docker` 會一併加入（`extends` 找不到被繼承的 Agent 時設定檔無法載入）。
- 已經設定過的環境，開啟 agt 時也會提醒加入 advisor 版；選「不要再提醒」後不再詢問，之後仍可執行 `agt setup` 加入。
- 新建分頁時（`Ctrl+A` → `n`）在 Agent 欄位用 ←/→ 切換；branch 名稱取自 Agent 名稱，例如 `agt/claude-sbx-advisor/<slug>-<id>`。

## 專案結構

| crate | 內容 |
| --- | --- |
| `agt-proto` | IPC 訊息、長度前綴封包、傳輸層（named pipe / UDS）、共用 client |
| `agt-core` | Session（PTY + `alacritty_terminal` + 狀態機）、AgentAdapter 介面、`platform` 模組 |
| `agt-agents` | Claude Code（hooks）、Antigravity、GitHub Copilot CLI、OpenAI Codex CLI（規則比對）、Shell、自訂 Agent |
| `agt-git` | WorktreeManager：直接呼叫 `git` |
| `agt-store` | SQLite（WAL、`user_version` migration）、可輪替的終端機紀錄檔 |
| `agt-daemon` | `agtd`：IPC Router、工作階段管理、設定檔熱重載、閒置關閉；端對端測試在 `tests/e2e.rs` |
| `agt-tui` | ratatui 介面，只依賴 `agt-proto` |
| `agt-cli` | `agt` 執行檔 |

## Docker 沙箱（claude-docker、claude-sbx、agy-docker、copilot-docker、codex-docker）

最簡單的方式：執行 `agt setup`（第一次執行 `agt` 時會自動出現）。它會偵測 Docker Desktop 與 Docker Sandboxes、用選單勾選要加入的 Agent、產生設定檔，並把內建的 Dockerfile 寫到設定目錄、詢問是否建置映像檔。之後每次開啟 agt 偵測到新的可用 Agent 時也會詢問是否加入。選單中也有 advisor 版（`claude-sbx-advisor`、`claude-docker-advisor`），見上方「[Advisor 版 Claude Code](#advisor-版-claude-code)」。

`claude-sbx` 使用 [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/)（`sbx`，microVM，不需要 Docker Desktop）：沙箱保留到分頁關閉，沙箱內可唯讀使用 git。相關設定 `restart_args`（重新啟動時取代 `args`）、`on_close`（只在關閉分頁時執行）與 `{gitdir}` 變數，範例見 [`config/default.toml`](config/default.toml) 的 `[agents.claude-sbx]`。


在容器中執行 Claude Code，容器只看得到該分頁的 worktree：`docker build -t agt-claude docker/claude/`，再把 [`config/default.toml`](config/default.toml) 裡的 `[agents.claude-docker]` 範例複製到設定檔。Antigravity CLI 同理：`docker build -t agt-agy docker/agy/`，範例是 `[agents.agy-docker]`；GitHub Copilot CLI 是 `docker build -t agt-copilot docker/copilot/`，範例是 `[agents.copilot-docker]`；OpenAI Codex CLI 是 `docker build -t agt-codex docker/codex/`，範例是 `[agents.codex-docker]`。Agent 設定可用 `adapter`（沿用內建 Agent 的狀態判斷）、`cleanup`（分頁結束時執行的收尾指令）、`update`（daemon 啟動時在背景執行、新建分頁時先在分頁中執行的更新指令；Agent 的執行檔放在分頁中唯讀的 volume，只由這個指令更新）與 `{cwd}`、`{session}` 變數。容器內無法使用 git，commit 由收尾選單處理。`update` 指令在 daemon 啟動時執行一次（相同指令只跑一次），新建分頁時也會先執行再啟動 Agent，讓沙箱分頁自動使用最新版；設定 `update_on_new = false` 可關閉後者（`R` 重新啟動不會執行）。詳見 [Windows 使用說明](docs/Windows-TUI-使用說明.md) 第 10 節。

### 沙箱分頁的 commit 與 PR

容器內無法使用 git，所以 Agent 改完程式後把說明寫成兩個檔案，由主機上的 agt 在收尾時使用：

- `.agt/COMMIT_MSG`：commit 訊息，第一行（50 字以內）同時是 PR 標題。
- `.agt/PR_BODY.md`：PR 說明（做了什麼、為什麼、如何測試）。

收尾時 **merge 與 pr 擇一**：選 merge 是在本機把 branch 併進 base，選 pr 是 push 並建立 PR、不在本機 merge（等 PR 在 GitHub 上合併）；兩者都會先 commit（Agent 設了 `auto_commit` 時）。

在 agt 按 `m` 收尾並選 **pr** 時，agt 會 commit、push 並建立 PR；該 branch 已有開啟中的 PR 時只 push 並更新說明。需要主機安裝並登入 GitHub CLI（`gh auth login`），沒有 `gh` 時只會 commit 並顯示要手動執行的 push 指令。分頁的 base 只在本機（例如還沒 push 的 feat branch）但內容都已在 remote 預設 branch 時，PR 以預設 branch 為目標；否則在 push 前停下並說明。`claude-docker` 與 `claude-sbx` 的範例提示會請 Claude 寫這兩個檔案（需 `auto_commit = true`），主機用的腳本是 `docker/claude/agt-pr.cmd`。`.agt/` 會被加入主 repo 的 info/exclude，不會出現在 diff 或 auto_commit 的 commit 裡。收尾失敗時原因會寫進 `agtd.log` 與該分頁的事件記錄；錯誤視窗可按 `c`（或 `y`）複製全部內容。操作步驟見 [agt-pr 操作步驟](docs/agt-pr-操作步驟.md)。

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
- **Antigravity CLI / GitHub Copilot CLI / OpenAI Codex CLI**：只有規則比對（規格 14.2 待確認是否有 hooks）。Antigravity CLI 的 `--continue` 接續的是這個目錄最近一次的對話；Copilot CLI 用 `--session-id` 接續分頁自己的對話；Codex CLI 用 `resume --last`，同樣是接續最近一次的對話。
- **未實作**：`cargo-dist` 打包、主題設定、`agt daemon stop` 之外的開機自動啟動（規格待決）。
- **Windows 原生**：已在 Windows 11 以交叉編譯的執行檔實測 CLI 與 daemon 全流程；TUI 只在 Linux 容器內做過自動化測試，尚未在 Windows Terminal 中手動操作過。
