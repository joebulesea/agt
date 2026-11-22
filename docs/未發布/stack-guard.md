### 新增

- 堆疊 PR 防護（stack-guard）：分組收尾 pr 時，只要有組用了 `AFTER`，收尾預覽與結果都會提醒合併順序（由前往後，每合一個先確認下一個 PR 的 base 已改成 base branch、且 base 真的包含那組的內容），並建議開啟 repo 的 Automatically delete head branches；堆疊的組的 PR 說明最前面也會加一段警示橫幅，說明它接在哪個 branch 後面。原因：堆疊 PR 合併後只進了堆疊 branch、沒進 `main`，`main` 才會建置的測試版就拿不到內容。只加提醒，不改變收尾行為
