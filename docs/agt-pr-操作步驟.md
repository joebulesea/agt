# 在沙箱寫完程式後 commit 並建立 PR：操作步驟

容器內的 Claude 不能使用 git。它改完程式後，把 commit 訊息與 PR 說明寫成兩個檔案；你在 agt 按收尾 → **pr**，由 agt 在主機上 commit、push 並建立 PR。

```
容器內的 Claude                         主機上的 agt（你按收尾 → pr）
──────────────────                      ──────────────────────────────
修改程式
寫 .agt/COMMIT_MSG   ───────────────▶   commit（用 COMMIT_MSG）
寫 .agt/PR_BODY.md                       push → 建立 PR（標題 = commit 標題，說明 = PR_BODY.md）
```

---

## 一、準備（只需要做一次）

### 1. 安裝並登入 GitHub CLI

```powershell
winget install -e --id GitHub.cli
gh auth login
```

沒有 `gh` 時，agt 只會 commit，並顯示要手動執行的 push 指令。

### 2. 更新 claude-docker 的設定

打開 `%APPDATA%\agt\config.toml`，在 `[agents.claude-docker]`：

- 確認有 `auto_commit = true`。
- 把 `--append-system-prompt` 後面那段文字換成：

```
你在 Docker 沙箱中執行，目前所在的目錄就是專案目錄。容器內無法使用 git，不要執行或修復任何 git 指令；只修改檔案，commit 與合併由外部負責。每次完成修改後，把 commit 訊息寫到 .agt/COMMIT_MSG（第一行是 50 字以內的標題，空一行後是內文），把 PR 說明寫到 .agt/PR_BODY.md（Markdown：做了什麼、為什麼、如何測試）；檔案已存在時改寫成涵蓋目前所有未 commit 修改的內容。
```

> 如果你的設定把 worktree 掛在 `/work`，提示的第一句可以維持原本的「/work 是專案目錄」，只要把後半段加上去即可。
>
> 用 `extends = "claude-docker"` 的 Agent（例如 `claude-docker-advisor`）會一起沿用。

存檔後重新啟動 daemon：

```powershell
agt daemon restart
```

---

## 二、每次使用

### 1. 開分頁、交代工作

在 agt 新增分頁，Agent 選 `claude-docker`，照平常一樣交代任務。

### 2. 檢查修改內容

Claude 完成後，在 agt 中用 diff 視窗檢查修改內容。`.agt/` 裡的說明檔不會出現在 diff 中。

想先看說明檔時，可以請 Claude 把內容貼出來，或直接打開 worktree 裡的 `.agt\COMMIT_MSG`、`.agt\PR_BODY.md`。如果 Claude 沒寫，跟它說「請寫 commit 訊息和 PR 說明」。

### 3. 收尾 → pr

在 agt 中按收尾，選 **pr**。完成後狀態列會顯示 PR 的網址。

| 情況 | agt 的動作 |
| --- | --- |
| 有 `.agt/COMMIT_MSG` | 用它 commit，之後刪除該檔 |
| 沒有 `.agt/COMMIT_MSG` | 用任務名稱當 commit 訊息 |
| 有 `.agt/PR_BODY.md` | 用它當 PR 說明，成功後刪除該檔 |
| 沒有 `.agt/PR_BODY.md` | 由 gh 從 commit 填入說明 |
| 分頁改了 `.claude/`、`.github/workflows/` 等設定檔 | 先列出來要求確認 |

PR 建立後分頁仍然開著。不需要再修改時，直接關閉分頁即可。

---

## 三、PR 要修改時（第二輪）

1. 在同一個分頁請 Claude 依 review 意見修改；Claude 會重新寫 `.agt/COMMIT_MSG`（與 `PR_BODY.md`）。
2. 再按一次收尾 → **pr**。
3. 這個 branch 已經有開啟中的 PR，agt 只會 commit 並 push（PR 自動更新）；有新的 `PR_BODY.md` 時也會更新 PR 說明。不會重複建立 PR。

---

## 四、常見訊息

| 訊息 | 原因與處理 |
| --- | --- |
| `.agt/…` 是連結 / 不是一般檔案，不處理 | 說明檔可能指向主機上的其他檔案。刪除後請 Claude 重寫成一般檔案 |
| `.agt/…` 超過 64KB / 不是 UTF-8 文字 | 不像是說明文字。刪除後請 Claude 重寫 |
| worktree 的 .git 不可信 | worktree 的 `.git` 被改過。不要繼續；檢查該分頁 Agent 做了什麼，必要時收尾 → discard |
| worktree 裡有巢狀的 git repo | Agent 在 worktree 裡建立了另一個 git repo。刪除該目錄中的 `.git`（或整個目錄）後再收尾 |
| 找不到 gh，請手動執行 … | 已 commit，照訊息中的指令 push，再到 GitHub 上建立 PR；或安裝並登入 GitHub CLI |
| commit 訊息變成任務名稱 | Claude 沒寫 `.agt/COMMIT_MSG`，或設定裡的提示還是舊版（見「一、準備」第 2 步） |

---

## 五、不開 TUI 時：agt-pr.cmd

在主機上也可以用腳本做同樣的事，執行前會列出要 commit 的檔案、commit 訊息與 PR 說明並詢問：

```powershell
C:\claude\agt\docker\claude\agt-pr.cmd <worktree 路徑>
```

worktree 路徑在 `%USERPROFILE%\.agt\worktrees\<repo>\<分頁 id>`；已經 `cd` 到 worktree 裡時可以省略。

| 選項 | 說明 |
| --- | --- |
| `-Yes` | 不詢問，直接執行 |
| `-NoPr` | 只 commit，不 push、不建立 PR |
| `-Base <branch>` | PR 的目標 branch（預設為 GitHub 上的預設 branch） |
| `-WorktreeRoot <路徑>` | 設定檔改過 `worktree_root` 時指定（預設 `~\.agt\worktrees`） |

> 請執行主 repo 中的這份腳本，不要執行 worktree 裡的副本，它可能被容器內的 Agent 改過。用腳本建立 PR 之後再按收尾 → pr 也沒關係，agt 會發現 PR 已存在，只 push。

---

## 六、安全說明

- 說明檔只會被當成文字交給 git / gh，**不會被執行**；push 用的 token 也不必傳進容器。
- 讀取說明檔時不跟隨連結，並限制大小與編碼；內容會先複製到主 repo 的 gitdir 再交給 git / gh，避免讀取後被 Agent 換掉。
- 操作前會先確認 worktree 的 `.git` 指向主 repo 登記的位置、沒有巢狀 git repo，任何一項不符就停止。
- 收尾 → pr 省掉的是打指令的時間，**review 修改內容這一步還是要做**。
