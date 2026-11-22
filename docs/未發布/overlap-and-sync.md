### 新增

- 分頁衝突預警：同一個 repo 的兩個分頁動了同一個檔案（已 commit、未 commit、未追蹤的都算，`.agt/` 不算；`--share` 共用 worktree 的分頁不算）時，側欄多一行黃字 `⚠ 與 a3f 重疊（3 檔）`，`agt ls` 也會標示，`agt ls --json` 的 `overlaps` 列出檔案。由 daemon 在輪詢 git 狀態時順便比對，只存在記憶體。
- 落後 base 的提示：側欄 branch 行在 `+N` 後面顯示 `↓N`（落後 base 幾個 commit）。
- 同步 base：`Ctrl+A` → `s` 或 `agt sync <id>` 把 base 的最新 commit 合併進分頁的 branch（`git merge`，不改寫歷史）。Agent 忙碌或有未 commit 的變更時拒絕；衝突時還原並列出衝突的檔案。新增協定訊息 `ClientMsg::Sync`、`SessionInfo.overlaps`。
