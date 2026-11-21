# Agent 分組收尾說明

給 Agent（以及設定 Agent 提示的人）看的說明：一個任務裡有好幾個互不相關的主題時，
不要全部塞進同一個 commit / PR，改成**分組**，收尾時每組各自一個 commit（pr 則是各自一個 PR）。

沒有 `.agt/groups/` 時，行為跟以前完全一樣（單一 `.agt/COMMIT_MSG`、`.agt/PR_BODY.md`）。

agt 建立分頁時會在 worktree 寫入 `.agt/README.md`：給 Agent 看的收尾規則（本文的精簡版，
內容在 `crates/agt-git/src/agent_guide.md`）。Agent 的提示只需要叫它先讀這個檔案，
規則更新時不必修改每個人的設定檔。

## 什麼時候分組

- 變更可以獨立審查、獨立合併（例如「修 bug」與「重構另一個模組」）。
- 各組不要改到同一個檔案；同一個檔案只能屬於一組。
- 各組各自從 base 建立，彼此不包含對方的變更：某組用到另一組新增的程式碼時，那組單獨建出來會無法編譯。
- 做不到上面兩點的拆分（多個主題改到同一個檔案，或彼此有依賴）：放在同一組，並在 `PR_BODY.md` 說明包含哪些主題。**不要**另外寫拆分計畫（例如 `.agt/split/`），agt 不會使用它。
- 只有一個主題時，不要分組。

## 目錄結構

在專案目錄（worktree 根目錄）下建立，每組一個子目錄：

```
.agt/
└── groups/
    ├── diff-tabs/
    │   ├── FILES          # 這組包含的路徑，一行一個
    │   ├── COMMIT_MSG     # 這組的 commit 訊息
    │   └── PR_BODY.md     # 這組的 PR 說明
    └── scrollbar/
        ├── FILES
        ├── COMMIT_MSG
        └── PR_BODY.md
```

### FILES

一行一個路徑，相對於專案根目錄；寫目錄代表底下全部檔案。刪除的檔案也要列出。

```
crates/agt-tui/src/ui/popup.rs
crates/agt-tui/src/app/
```

### COMMIT_MSG、PR_BODY.md

格式與單一收尾相同：`COMMIT_MSG` 第一行是 50 字以內的標題（同時是 PR 標題），空一行後是內文；
`PR_BODY.md` 是 Markdown（做了什麼、為什麼、如何測試）。兩者都可省略，
省略時 commit 與 PR 標題用「任務名稱（組名）」。

## 規則

- 組名只能用英數字與 `.`、`_`、`-`，最長 40 字，不能以 `.` 或 `-` 開頭，**不能叫 `rest`**。
- 路徑不可是絕對路徑、不可含 `..` 或 `.git`、不可在 `.agt/` 底下。
- `.agt/groups` 與各組目錄必須是一般目錄（不能是符號連結）；單一檔案不超過 64KB、UTF-8。
- 每組的 `FILES` 不可為空，且那些路徑必須真的有變更。
- 沒被任何組列出的變更，會自動成為最後一組 `rest`（commit 訊息用任務名稱）。
- 不要放巢狀的 git repo（含 `.git` 的子目錄），會被拒絕。

## 收尾時發生什麼

使用者在 agt 按 `m` 收尾：

| 動作 | 每組的結果 |
| --- | --- |
| **pr** | 建立 branch `<任務 branch>-<組名>`（內容 = merge-base + 該組檔案，只有一個 commit），各自 push 並開 PR；已有 PR 時只追加 commit、更新說明，不 force push。各 PR 互不包含對方的變更 |
| **merge** | 依組名順序合進 base：merge＝每組一個 merge commit、squash＝每組一個 commit、rebase＝逐組 cherry-pick。任一組衝突就停下，已合進去的組保留 |

收尾前，收尾視窗會預覽預計產生的 commit / PR。以下情況會在最前面以紅色 `⚠` 提醒：

- `.agt/` 裡有 agt 不認得的項目（只認得 `README.md`、`COMMIT_MSG`、`PR_BODY.md`、`groups/`）。
- 沒有分組，但相對 merge-base 的改動超過 40 個檔案或 1500 行：可能混了好幾個主題。

完成後視窗會顯示「分成 N 組：N 個 commit、N 個 PR」與各組的標題、branch、PR 網址。
成功處理的組，其 `.agt/groups/<組名>/` 會被刪除；`rest` 不需要說明檔。

## 範例

```
.agt/groups/diff-tabs/FILES
  crates/agt-tui/src/app/popup.rs
  crates/agt-tui/src/ui/popup.rs
.agt/groups/diff-tabs/COMMIT_MSG
  diff 視窗改為分頁顯示

  依已 commit / 未 commit / 未追蹤切成分頁，Tab 或 1-9 切換。
.agt/groups/diff-tabs/PR_BODY.md
  ## 做了什麼
  ...
```

## 可貼進 Agent 提示的版本

建立於支援 `.agt/README.md` 之後的分頁，提示只要一句：

> 收尾規則（commit 訊息、PR 說明、多個主題時如何分組）在 `.agt/README.md`，開始修改前先讀，每次修改完照它的規則更新說明檔。

`agt config check` 會提醒 `auto_commit = true` 卻沒有提到 `.agt/README.md` 的 Agent。
