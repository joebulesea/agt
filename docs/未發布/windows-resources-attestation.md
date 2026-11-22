### 新增

- Windows 的 `agt.exe` 嵌入版本資訊與 manifest（`crates/agt-cli/build.rs`，用 `winresource`）：檔案內容的「詳細資料」看得到名稱與版本，測試版的版號第四段是 nightly 的 run 編號；manifest 宣告 `asInvoker`（不要求系統管理員）與支援 Windows 10 以後。目的是降低防毒的誤判，不保證有效（執行檔仍沒有程式碼簽章）。在 Windows 從原始碼編譯需要 Windows SDK 的 `rc.exe`，用 mingw 則需要 `windres`，缺少時建置會失敗
- 正式版與測試版的發布檔附 build attestation（GitHub artifact attestation，公開 repo 免費）：`gh attestation verify <檔案> --repo joebulesea/agt` 可以確認壓縮檔或執行檔是由這個 repo 的 workflow 建置的。上傳前先產生，產生失敗就不發布；發布前也會檢查 Windows 執行檔有版本資訊與 manifest
