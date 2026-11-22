### 新增

- `agt update --nightly` 先列出變更再問是否更新：從目前版本到最新測試版之間的 commit 標題（新的在前，最多 15 筆，其餘顯示「還有 N 個」）。起點是目前測試版建置編號裡的 commit；正式版用它的標籤 `vX.Y.Z`（本機編譯的版本沒有建置編號，起點只是近似）。`agt update --check --json --nightly` 在可更新時多一個 `changes`：`{since, total, commits: [{sha, title}]}`，查不到時沒有這個欄位。資料來自 GitHub 的 compare API（有頻率限制，所以只在可更新時才查，逾時 15 秒；連不上、起點不存在等任何失敗都靜默略過，不影響更新）。標題來自 commit 訊息（不可信）：只取第一行、去掉控制字元、限制 100 字；`nightly.json` 新增的 `sha` 只接受十六進位（會放進網址）
