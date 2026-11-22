### 新增

- 確認收尾的 PR 真的進了 base：收尾 pr 後，agt 記下每個 PR（含分組與堆疊的），daemon 每分鐘用 `gh pr view` 查還沒有結果的 PR，合併後再確認合併 commit 在 `<remote>/<base>`（不比對推出去的 SHA，squash / rebase 合併才不會誤判）。堆疊 PR 只合進前一組的 branch 時標成「進錯 branch」。新增 `agt landing [--check] [--json]` 列出結果，有 PR 沒進預期的 base 時 exit 1；只有「進錯 branch」與「找不到合併 commit」發系統通知。追蹤存在新的資料表 `landings`（migration 5），不綁分頁，保留 60 天；查詢在主 repo 執行 `gh` 與 `git fetch`，沙箱分頁也一樣。新增協定訊息 `ListLandings`（舊版 daemon 不認得，升級後執行 `agt daemon restart`），協定版本不變。側欄在分頁多一行顯示結果（異常紅字、進了 base 綠字 24 小時、堆疊進度灰字、PR 都還開著時不顯示）；`SessionInfo` 新增 `landing`（`agt ls --json` 也看得到），只在分頁還開著時帶入，協定版本不變
