# Agent 分組收尾說明

給 Agent（以及設定 Agent 提示的人）看的說明：一個任務裡有好幾個互不相關的主題時，
不要全部塞進同一個 commit / PR，改成**分組**，收尾時每組各自一個 commit（pr 則是各自一個 PR）。

沒有 `.agt/groups/` 時，行為跟以前完全一樣（單一 `.agt/COMMIT_MSG`、`.agt/PR_BODY.md`）。

## 什麼時候分組

- 變更可以獨立審查、獨立合併（例如「修 bug」與「重構另一個模組」）。
- 各組不要改到同一個檔案；同一個檔案只能屬於一組。
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

> 如果這次任務包含多個互不相關的主題，請分組：每組建立 `.agt/groups/<組名>/`，裡面放 `FILES`（這組的路徑，一行一個，目錄代表底下全部）、`COMMIT_MSG`（第一行 50 字以內的標題，空一行後是內文）、`PR_BODY.md`（做了什麼、為什麼、如何測試）。同一個檔案只能屬於一組；組名只用英數與 `._-`，不可叫 `rest`。沒列在任何組的變更會自動成為 `rest` 組。只有單一主題時不要分組，照舊寫 `.agt/COMMIT_MSG` 與 `.agt/PR_BODY.md`。收尾時每組各自一個 commit 與 PR。
