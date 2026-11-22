### 新增

- 側欄與 `agt landing` 顯示 PR 的 CI 狀態：收尾開出、還沒合併的 PR，daemon 在每分鐘的檢查裡另外用 `gh pr view --json statusCheckRollup` 查 CI；失敗時側欄紅字 `✗ CI 失敗：clippy, test`（失敗的檢查名稱）並發系統通知（沿用 `notify`，只通知失敗，執行中與通過不通知），執行中灰字 `… CI 執行中`，全部通過綠字 `✓ CI 通過`，沒有檢查時不顯示；`agt landing` 在還沒合併的 PR 後面列出 CI 狀態，`agt ls --json` 的 `landing` 有 `ci`、`ci_detail`。CI 查詢與「有沒有進 base」分開執行，查不到（`gh` 版本不支援、離線）時維持上次的狀態，不影響進 base 的判斷。資料庫 migration 6（`landings` 新增 `ci`、`ci_detail`），協定版本不變
