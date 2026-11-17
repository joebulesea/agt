# 待辦：像 Claude CLI 一樣用指令安裝 agt

目標：

```powershell
irm https://<網址>/install.ps1 | iex       # Windows
curl -fsSL https://<網址>/install.sh | sh    # Linux / macOS
agt update                                    # 之後自我更新
```

程式本身大致已就緒（`agt setup` 已內嵌 Dockerfile 與 Agent 範例，單一執行檔就能完成設定），缺的是**發布流程**與**安裝 / 更新機制**。

---

## 0. 需要先決定

- [ ] **repo 位置與公開與否**：GitHub 公開 repo（Releases 可匿名下載，最簡單）或私有（下載位置需另外放 Cloudflare R2 / S3 等）
- [ ] **授權條款**：目前 `Cargo.toml` 為 `UNLICENSED`，公開發布前需決定
- [ ] **支援平台**：第一版建議 Windows x64 + Linux x64；macOS（需公證，Apple 開發者帳號 $99/年）與 ARM 之後再加
- [ ] **是否把 agtd 併入 agt.exe**（例如 `agt daemon run`）：單一執行檔讓安裝、更新都更簡單，但需要改程式

## Windows 目錄配置

| 用途 | 位置 |
| --- | --- |
| 執行檔（`agt.exe`、`agtd.exe`） | `%APPDATA%\agt\`（已在使用；開發時用 `docker/publish.sh` 安裝） |
| 設定檔 | `%APPDATA%\agt\config.toml` |
| Dockerfile（`agt setup` 產生） | `%APPDATA%\agt\docker\<claude\|agy>\` |
| 「不要再提醒」清單 | `%APPDATA%\agt\setup-skip.txt` |
| 資料庫、終端機紀錄、daemon 日誌 | `%APPDATA%\agt\data\`（已實作；舊版的 `%LOCALAPPDATA%\agt` 會自動搬移） |
| worktree | `%USERPROFILE%\.agt\worktrees\`（不變） |

注意：`%APPDATA%` 是 Roaming 目錄，在公司網域的漫遊設定檔環境下會跟著帳號同步（執行檔約 10 MB）。個人電腦沒有影響。

## 1. 前置作業

- [ ] 把 `C:\claude\Rust\agt` 初始化為 git repo 並推到遠端（目前不是 git repo，`.github/workflows/ci.yml` 不會執行）
- [ ] 確認 CI（clippy + test，Linux / macOS / Windows）在遠端可以通過

## 2. 發布流程（約 0.5–1 天）

- [ ] 新增 `.github/workflows/release.yml`：推送 `v*` 標籤時觸發
- [ ] 各平台編譯 release 版
  - [ ] Windows 改用 GitHub Windows runner + MSVC（目前是 Docker 內 MinGW 交叉編譯 `x86_64-pc-windows-gnu`）
  - [ ] Linux x64（必要時用 musl 做靜態連結）
- [ ] 打包：`agt-<版本>-<平台>.zip` / `.tar.gz`，內含 `agt` 與 `agtd`
- [ ] 產生 `SHA256SUMS`，連同壓縮檔上傳到 Releases
- [ ] 提供「最新版本」查詢方式（Releases API 或固定的 `latest.json`）
- [ ] 版本號管理：建立 CHANGELOG；沿用既有的協定版本檢查（版本不同時提示 `agt daemon restart`）

## 3. 安裝腳本（約 1 天）

- [ ] `install.ps1`（Windows）
  - [ ] 判斷 CPU 架構，下載對應壓縮檔（只用 HTTPS）
  - [ ] **核對 SHA256**，不符就中止
  - [ ] 已安裝且 daemon 在執行時，先 `agt daemon stop -y`
  - [ ] 解壓到 `%APPDATA%\agt\`（不需要系統管理員；和設定檔、`docker\`、`data\` 放在一起，與目前手動安裝的位置相同）
  - [ ] 把目錄加進使用者 PATH（已存在則略過）
  - [ ] 檢查 git 是否存在，沒有就提示安裝
  - [ ] 最後執行 `agt setup`
- [ ] `install.sh`（Linux / macOS）：同上，安裝到 `~/.local/bin`
- [ ] 在乾淨的 Windows（沒有 Rust、沒有舊版 agt）上實測

## 4. `agt update` 與移除（約 1 天）

- [ ] `agt update`：查詢最新版本 → 下載並核對 SHA256 → 停止 daemon → 換掉執行檔 → 重新啟動 daemon
  - [ ] **執行中的 exe 無法覆蓋**（開發時多次遇到）：先把舊檔改名為 `.old` 再放新檔，下次啟動時清除 `.old`
  - [ ] 有分頁正在執行時先提醒（重啟 daemon 會結束所有 Agent）
- [ ] （選用）啟動時背景檢查新版本，在狀態列提示
- [ ] `agt uninstall`（或文件說明）：
  - [ ] 停止 daemon
  - [ ] 刪除沙箱 / 容器（`sbx rm agt-*`、`docker rm -f` 標記 `agt=1` 的容器）
  - [ ] 刪除執行檔 `%APPDATA%\agt\agt.exe`、`agtd.exe` 與 PATH 設定
  - [ ] 詢問是否刪除設定（`%APPDATA%\agt` 其餘內容）、worktree `~/.agt/worktrees`

## 5. 之後再做

- [ ] 程式碼簽章（避免 Windows Defender / SmartScreen 警告；例如 Azure Trusted Signing，約 $10/月）
- [ ] Scoop：自己的 bucket repo（一個 JSON 檔，難度低）
- [ ] winget：向 microsoft/winget-pkgs 送 PR（最好先有簽章）
- [ ] Homebrew tap（支援 macOS 時）
- [ ] macOS 實機測試與公證（目前只有 Windows 實際使用過，Linux 只在容器內做過自動化測試）
- [ ] Windows ARM64

## 其他安裝方式比較

| 方式 | 指令 | 需要 | 難度 |
| --- | --- | --- | --- |
| 安裝腳本 | `irm … \| iex` | 公開下載位置 + 腳本 | 中（本待辦的主線） |
| Scoop | `scoop install agt` | bucket repo + JSON | 低 |
| winget | `winget install agt` | winget-pkgs PR、最好有簽章 | 中 |
| cargo | `cargo install --git …` | 使用者自備 Rust | 低（只適合開發者） |
| Homebrew | `brew install …/agt` | tap repo | 低 |
| npm 包裝 | `npm i -g agt` | 依平台分成多個套件 | 中（不建議） |
