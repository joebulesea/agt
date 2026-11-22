### 新增

- `finish_check` 的建議：repo 根目錄有 `Cargo.toml`（一般檔案，只看存在，不讀內容）、又沒設定 `finish_check` 時，收尾預覽最後會提示可以設定 `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`（附上可直接貼進使用者設定檔的 `[repos."<路徑>"]` 段落），`agt config check --repo <路徑>` 也會列出「建議：…」。這是 CI 會跑的檢查，能在本機先攔住 fmt 與 clippy 的失敗。只提示，不會自動設定或執行；沙箱 Agent 仍需要 `check_command`
