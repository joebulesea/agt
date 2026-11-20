## 做了什麼
- `crates/agt-tui/src/app.rs`：`on_paste` 一律以 bracketed paste（`ESC[200~ … ESC[201~`）轉送給裡面的程式，不再把 `\n` 換成 `\r`。
- 新增 `paste_bytes()`，會先移除貼上內容中夾帶的 `ESC[201~`，避免提早結束貼上區段。
- 新增 3 個單元測試（多行包裝、移除結束標記、空貼上）。

## 為什麼
原本只有在 daemon 傳來的畫面快照帶有 `BRACKETED_PASTE` 旗標時才包裝；快照過期、剛 attach 或裡面的程式尚未開啟該模式時，換行會變成 Enter，多行貼上就被直接執行。

## 如何測試
- `cargo test -p agt-tui`（本次在沙箱中沒有 cargo，未實際執行）。
- 手動：在 Windows Terminal 用 Ctrl+Shift+V 貼上多行文字，確認不會被送出。

## 注意
裡面的程式若完全不支援 bracketed paste，會看到多餘的跳脫序列；Claude Code、codex、copilot 都支援。

## 另外：顯示 auto_commit 狀態
- `UiConfig` 新增 `auto_commit_agents`（`serde(default)`，舊版 daemon / TUI 相容）。
- 新增分頁表單的 Agent 欄位，對有 `auto_commit` 的 Agent 顯示 `auto_commit`（`crates/agt-tui/src/ui.rs`）。
- `agt config check` 列出每個 Agent 展開 `extends` 後的 `auto_commit` on/off（`crates/agt-daemon/src/check.rs`），並補測試 `auto_commit_lines_follow_extends`。
- 同樣沒有在沙箱中編譯或執行測試（沒有 cargo），請本機執行 `cargo test`。
- 左側欄分頁清單第二行行尾以綠色顯示 `auto`，標題列顯示 `[auto_commit]`（`crates/agt-tui/src/ui.rs`）。
- `.gitignore` 加上 `/.agt/`，讓 Agent 說明檔（`COMMIT_MSG`、`PR_BODY.md`）在這個 repo 一定不會被誤加進 commit（agt 原本只靠各 clone 的 `info/exclude`）。
