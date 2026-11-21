//! `agt statusline`：Claude Code 的 `statusLine` 指令（主機上的 Claude）。
//!
//! agt 建立分頁時在 worktree 的 `.claude/settings.local.json` 寫入這個指令。Claude Code 每次收到回覆等時機
//! 執行它並從 stdin 傳入 JSON；這裡把 JSON 寫到專案的 `.agt/usage.json`（daemon 讀取後顯示在狀態列），
//! 再輸出一行摘要給 Claude Code 顯示。沙箱裡的 Claude 改用 `.agt/statusline.sh`，做的事相同。
//!
//! 每次回覆都會執行，必須快速、不連 daemon，任何錯誤都靜默略過（輸出空行）。

use agt_proto::transport::ENV_SESSION;
use agt_proto::usage::{USAGE_FILE, USAGE_MAX, parse, summary};
use std::io::Read;
use std::path::{Path, PathBuf};

/// agt 在 worktree 裡的說明檔目錄（與 agt-git 的 `NOTES_DIR` 相同）。
const NOTES_DIR: &str = ".agt";

pub fn run() {
    let mut buf = Vec::new();
    if std::io::stdin()
        .take(USAGE_MAX + 1)
        .read_to_end(&mut buf)
        .is_err()
        || buf.len() as u64 > USAGE_MAX
    {
        return;
    }
    let text = String::from_utf8_lossy(&buf);
    // 只有 agt 的分頁（daemon 啟動 Agent 時設定的環境變數）才交給 agt
    if std::env::var_os(ENV_SESSION).is_some()
        && let Some(dir) = project_dir(&text)
    {
        let _ = write_usage(&dir, buf.as_slice());
    }
    if let Some(u) = parse(&text, 0) {
        println!("{}", summary(&u));
    }
}

/// 專案目錄：JSON 的 `workspace.project_dir`（Claude Code 啟動的目錄），沒有時用 `cwd`、目前目錄。
fn project_dir(text: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v["workspace"]["project_dir"]
        .as_str()
        .or_else(|| v["cwd"].as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// 寫到 `<dir>/.agt/usage.json`：先寫暫存檔再改名，daemon 不會讀到寫到一半的內容。
/// `.agt` 不存在（不是 agt 建立的 worktree）或是連結時不寫。
fn write_usage(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let notes = dir.join(NOTES_DIR);
    let m = std::fs::symlink_metadata(&notes)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Ok(());
    }
    let tmp = notes.join(format!("{USAGE_FILE}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, notes.join(USAGE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_prefers_workspace_project_dir() {
        assert_eq!(
            project_dir(r#"{"cwd": "/a", "workspace": {"project_dir": "/b"}}"#),
            Some(PathBuf::from("/b"))
        );
        assert_eq!(project_dir(r#"{"cwd": "/a"}"#), Some(PathBuf::from("/a")));
        assert!(project_dir("not json").is_none());
    }

    #[test]
    fn write_usage_only_into_agt_worktree() {
        let d = tempfile::tempdir().unwrap();
        // 沒有 .agt：不寫
        assert!(write_usage(d.path(), b"{}").is_err());
        assert!(!d.path().join(".agt").exists());
        std::fs::create_dir_all(d.path().join(".agt")).unwrap();
        write_usage(d.path(), b"{\"a\":1}").unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(".agt").join(USAGE_FILE)).unwrap(),
            "{\"a\":1}"
        );
        assert!(!d.path().join(".agt").join("usage.json.tmp").exists());
    }
}
