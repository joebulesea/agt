# 待辦：雙擊 agt.exe 時檢查環境，缺少的可以選擇自動安裝或自行安裝

目標：在檔案總管雙擊 `agt.exe` 時，檢查 git、Docker Desktop、Docker Sandboxes（sbx）等環境。沒有安裝的項目可以選擇「用 winget 自動安裝」、「開啟下載頁」或「顯示指令自行安裝」。

---

## 現況與缺口

| 項目 | 現況 | 缺口 |
| --- | --- | --- |
| 環境偵測 | `setup::detect()` 已偵測 git / claude / agy / copilot / codex / docker / sbx | 沒有偵測 WSL2（Docker Desktop 需要）與 winget |
| 沒裝的時候 | `print_env()` 只印出 ✗ 和提示 | 不能選擇安裝 |
| 缺少 git | 只在「接下來」列出提示，照樣開啟 TUI | git 是必要的，缺了應該擋下來 |
| 雙擊執行 | 結束或出錯時視窗直接關閉 | 使用者看不到錯誤訊息 |
| 之後每次開啟 | `offer_new_agents()` 只看 PATH 中有沒有新的 Agent | 不會再檢查必要項目 |

## 0. 需要先決定

- [x] 「建議」的項目（sbx）是否預設勾選安裝？→ **只預設勾選「必要」項目（git）**，sbx 與選用項目都不勾
- [x] 主機上的 Claude Code 是否列入可自動安裝的項目 → **列入，選用、預設不勾**
- [x] WSL2 → **交給 Docker Desktop 安裝程式處理**；agt 只偵測，在 Docker Desktop 下方顯示狀態，安裝 Docker Desktop 前提示可能需要重新開機
- [x] Linux / macOS 第一版 → **只顯示安裝指令與官方網址，不自動安裝**

## 1. 檢查項目與安裝方式

| 項目 | 等級 | 偵測 | 自動安裝（winget） | 自行安裝 |
| --- | --- | --- | --- | --- |
| Git | **必要** | `git --version`，2.20 以上 | `winget install -e --id Git.Git` | 開啟 git-scm.com 下載頁 |
| Docker Sandboxes（sbx） | 建議 | 沿用現有偵測 | `winget install -e --id Docker.sbx` | 官方文件連結 |
| └ 登入 | | NotSignedIn | 執行 `sbx login`（同一個視窗互動） | |
| └ 網路政策 | | 沿用 | 沿用現有的 balanced / allow-all / deny-all 選單 | |
| WSL2 | Docker Desktop 前置 | `wsl --status`（只看結束代碼） | 不安裝，交給 Docker Desktop 安裝程式 | |
| Docker Desktop | 選用 | Missing / NotRunning | `winget install -e --id Docker.DockerDesktop` | 開啟下載頁 |
| └ 沒有在執行 | | NotRunning | 啟動 `Docker Desktop.exe`，輪詢 `docker version` 最多約 90 秒 | |
| Claude Code（主機） | 選用 | `on_path("claude")` | `winget install -e --id Anthropic.ClaudeCode` | 官方安裝腳本（`install.ps1` / `install.sh`） |
| Copilot CLI / Codex CLI / agy | 選用 | 沿用 | 只顯示指令（需要 npm） | 顯示指令 |

- [x] winget ID 與下載網址已核對：`Git.Git`、`Anthropic.ClaudeCode`、`Docker.DockerDesktop`、`Docker.sbx` 都存在於 microsoft/winget-pkgs 的 manifests，文件中的下載頁與 agy 安裝腳本網址回應 200（只核對存在，沒有實際執行安裝）

## 2. 使用流程

- [x] 偵測：沿用 `detect()`，加入 WSL 與 winget
- [x] 列出結果：沿用 `print_env()`，每一項標示 必要 / 建議 / 選用
- [x] 勾選要處理的項目：沿用 `prompt::multi_select`，預設只勾選缺少的「必要」項目
- [x] 每一項選擇安裝方式：winget 安裝 / 開啟下載頁 / 顯示指令自行安裝 / 略過；執行前顯示完整指令並確認，winget 輸出直接顯示在畫面上
- [x] 安裝後重新整理 PATH：從登錄檔（`HKCU\Environment`、`HKLM\...\Session Manager\Environment`）讀出 PATH，設定到目前的行程，再執行一次 `detect()`
- [x] 依照順序處理：Git → Docker Desktop（WSL2 由安裝程式處理；啟動並等待 engine）→ sbx → `sbx login` → Claude Code / Copilot / Codex / agy → 網路政策 → 原本的「選 Agent / 建置映像檔」
- [x] 需要重新開機時：提示「重新開機後再雙擊 agt.exe 會從這裡繼續」後結束（每次都會重新偵測，不需要另外記錄進度）
- [x] 缺少 git 時不開啟 TUI，說明原因後等待按鍵關閉

## 3. 雙擊執行的處理

- [x] 用 `GetConsoleProcessList` 判斷是否從檔案總管雙擊（console 中只有 agt 一個行程）
- [x] 雙擊時，出錯或選「離開」後顯示「按任意鍵關閉」；從終端機執行時不暫停

## 4. 檢查時機

| 時機 | 行為 |
| --- | --- |
| 第一次執行（沒有設定檔） | 完整檢查＋安裝選單 |
| 之後每次開啟 | 只快速檢查必要項目（`on_path("git")`，不執行程式），缺少時才進入安裝選單；其他沿用 `offer_new_agents()` |
| `agt setup` | 完整檢查＋安裝選單 |
| `agt setup --yes` / 非互動 | 永遠不安裝，只列出結果；需要時另外加 `--install` |

## 5. 安全原則

- [x] 每一項安裝都先確認，顯示完整指令；不在背景安裝、不加 `--silent`
- [x] winget 一律 `-e --id <ID> --source winget`，避免模糊比對
- [x] agt 本身不要求系統管理員，需要時由 winget 或安裝程式跳出 UAC
- [x] 用 `Command::new().args()` 組成指令，不經過 shell 字串
- [x] 沒有 winget（LTSC、舊版 Windows）時，退回「開啟下載頁 / 顯示指令」
- [x] 下載頁只開程式碼中寫死的官方網址

## 6. 程式碼改動

- [x] 新增 `crates/agt-cli/src/deps.rs`：`Requirement` 表（id、名稱、等級、偵測、winget ID、下載網址、手動指令、前置條件）、安裝順序、`refresh_path()`、`wait_docker()`
- [x] `crates/agt-cli/src/setup.rs`：`Env` 加入 `wsl`、`winget`；`print_env` 改用 `Requirement` 表；`run()` 在選 Agent 前插入安裝選單
- [x] `crates/agt-cli/src/main.rs`：缺少 git 時不開啟 TUI；雙擊偵測與按鍵關閉
- [x] `crates/agt-cli/src/prompt.rs`：新增 `pause()`
- [x] 文件：`README.md`、`docs/Windows-TUI-使用說明.md` 第 2 節、`docs/TODO-安裝與發布.md`

## 7. 測試

- [x] 把安裝計畫與指令內容寫成純函式，用假的 `Env` 測試：缺少項目的順序、有 / 沒有 winget 時的選項、`--yes` 不會安裝
- [ ] 在乾淨的 Windows VM（沒有 git、Docker、sbx）上手動實測完整流程，包含重新開機後繼續

## 8. 實作說明

- 安裝計畫：`deps::plan(&Env, Os) -> Vec<Task>`（純函式），每個 `Task` 有問題種類（未安裝 / 版本太舊 / 沒有在執行 / 尚未登入）與可用的處理方式；`deps::auto_methods()` 決定 `--yes` 時要自動做什麼（沒有 `--install` 時一律不做）
- 「需要重新開機」無法可靠地從 winget 的結束代碼判斷：Docker Desktop 安裝後如果 engine 還無法使用，讓使用者選「啟動並等待 / 先結束（重新開機）/ 略過」；選先結束時回傳 `deps::Stopped`，雙擊時等按鍵再關閉，結束代碼為 0
- `refresh_path()` 用 `RegGetValueW`（`RRF_RT_REG_SZ`，會展開 `%VAR%`）讀登錄檔，只把目前行程沒有的目錄加到 PATH 後面
