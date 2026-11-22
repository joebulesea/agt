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
- 各組預設各自從 base 建立，彼此不包含對方的變更。某組用到另一組新增的程式碼時，在它的 `AFTER` 寫上那一組（見下方「堆疊：AFTER」）。
- 多個主題改到同一個檔案：前面的組用 `SNAPSHOT/<路徑>` 放只含自己修改的版本，後面的組用 `AFTER` 接上去（見下方「SNAPSHOT」）。寫不出合理的中間版本時，放在同一組，並在 `PR_BODY.md` 說明包含哪些主題。**不要**另外寫拆分計畫（例如 `.agt/split/`），agt 不會使用它。
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
        ├── PR_BODY.md
        ├── AFTER          # （選用）接在 diff-tabs 後面
        └── SNAPSHOT/      # （選用）檔案在這組 branch 上的內容
            └── crates/agt-tui/src/ui/popup.rs
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

### 堆疊：AFTER

選用。內容是另一組的組名（一行），表示這組用到那組新增的程式碼，要接在那組後面：

- 這組的 branch 從那組的 branch 建立（內容 = 那組的 branch + 這組的檔案），單獨建出來也能編譯。
- 收尾 pr 時，這組 PR 的 base 是那組的 branch，PR 只顯示這組自己的差異。那組的 PR 合併並刪除 branch 後，
  GitHub 會自動把這組 PR 的 base 改回原本的 branch。
- 收尾 merge 時，依 `AFTER` 的順序合進 base（被接的組先合）。
- 可以串成一條鏈（a ← b ← c）；不能接在自己、`rest` 後面，也不能形成循環。
- 那組後來又追加了 commit（例如再次收尾 pr）：這組會多一個 merge commit 把那組的新內容併進來，不改寫歷史、不需要 force push。
- `AFTER` 指向的組已經處理完（說明檔被刪掉）也可以，只要那組的 branch 還在；branch 不在時收尾會停下並說明。
- 堆疊 PR 防護：收尾 pr 的預覽與結果會提醒「由前往後合併，每合一個先確認下一個 PR 的 base 已改成 base branch，且 base 真的包含那組的內容」；堆疊的組的 PR 說明最前面會加一段警示橫幅。只提醒，不阻擋收尾。
- 收尾 pr 後，agt 會追蹤這些 PR 有沒有真的進 base：用 `agt landing` 查，堆疊 PR 只合進前一組的 branch 時會標成「進錯 branch」並通知。

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
| **pr** | 建立 branch `<任務 branch>-<組名>`（內容 = merge-base + 該組檔案，只有一個 commit；有 `AFTER` 時改為接在那組的 branch 後面），各自 push 並開 PR（有 `AFTER` 時 PR 的 base 是那組的 branch）；已有 PR 時只追加 commit、更新說明，不 force push。沒有 `AFTER` 的 PR 互不包含對方的變更 |
| **merge** | 依組名順序合進 base（有 `AFTER` 時被接的組先合）：merge＝每組一個 merge commit、squash＝每組一個 commit（套用該組自己的 commit 後壓成一個）、rebase＝逐組 cherry-pick 該組的 commit。任一組衝突就停下，已合進去的組保留 |

### SNAPSHOT：同一個檔案分到不同組

選用。`SNAPSHOT/<路徑>` 是某個檔案在這組 branch 上的**完整內容**。多個主題改到同一個檔案時：

1. 排順序，例如 a 在前、b 在後。
2. a 放 `SNAPSHOT/src/app.rs`：只含 a 的修改的完整檔案。a 的 FILES 不列這個檔案（只有 SNAPSHOT 時 FILES 可以省略）。
3. b 的 FILES 列 `src/app.rs`（worktree 裡的最終內容），`AFTER` 寫 `a`。b 的 PR 只顯示它多出來的部分。

規則：

- 同一組裡，一個檔案只能在 FILES 或 SNAPSHOT 其中之一。
- 有檔案最終內容（FILES 列了它）的組，必須經由 `AFTER` 接在每個有它 SNAPSHOT 的組後面，否則拒絕並說明要在哪裡寫 `AFTER`。
- 只有 SNAPSHOT、沒有任何組的 FILES 列出的檔案，最終內容歸 `rest`，`rest` 會自動接在最後一個有它 SNAPSHOT 的組後面；
  有好幾組都有它的 SNAPSHOT 時，那些組要用 `AFTER` 排成一條鏈。
- SNAPSHOT 的路徑規則與 FILES 相同；不能是連結，每個檔案不超過 8MB、每組最多 500 個。
  原本的檔案是可執行檔時沿用它的模式；不能用 SNAPSHOT 取代連結或 submodule。
- 內容經過與 `git add` 相同的轉換（`.gitattributes`，例如換行字元）。
- 搭配下面的收尾檢查，可以確認每個中間版本都能建置。

### 收尾檢查（finish_check）

使用者可以在設定檔 `[repos."<路徑>"]` 設定 `finish_check`（例如 `"cargo build --workspace"`）。
分組收尾時，agt 先把每組的 branch 建好，在每組 branch 的乾淨 checkout（`<worktree>.check-<組名>`）
各執行一次；**任何一組失敗就停下，不 push、不合併任何一組**，錯誤視窗會顯示哪一組失敗與輸出的最後 40 行。
常見原因是某組用到另一組的程式碼卻沒寫 `AFTER`，或共用的檔案被分到別組。

- 只從使用者設定檔讀取；repo 內 `.agt.toml` 的 `finish_check` 不會生效（Agent 可能改過它）。
- 沙箱 Agent（指令是 docker / sbx / podman）在 Agent 設定的 `check_command` 裡執行，例如同一個映像檔的容器，
  `{dir}` 掛載這組的 checkout、`{check}` 是 finish_check 的內容；沒設定時略過檢查（agt 不在主機上執行它寫的程式碼）。
  其他 Agent 在主機上用 shell 執行。
- checkout 裡沒有 `copy_files` 的檔案，也沒有建置快取。每組最多 30 分鐘。
- 沒有分組時不執行（只有一個 branch，Agent 自己的 worktree 就是它的內容）。

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
