//! Agent 留下的說明檔（`.agt/COMMIT_MSG`、`.agt/PR_BODY.md`）：安全地讀取與移除。

use crate::*;
use std::path::{Path, PathBuf};

/// 無法自行 commit 的 Agent（例如在容器中執行的）寫說明檔的目錄（相對於 worktree）。
pub const NOTES_DIR: &str = ".agt";

/// commit 訊息：第一行是標題，空一行後是內文。
pub const COMMIT_MSG_FILE: &str = "COMMIT_MSG";

/// PR 說明（Markdown）。
pub const PR_BODY_FILE: &str = "PR_BODY.md";

/// 讓 git 忽略說明檔目錄的 info/exclude 規則。
pub const NOTES_EXCLUDE: &str = "/.agt/";

pub(crate) const NOTE_MAX: u64 = 64 * 1024;

/// 讀取 Agent 寫在 `<worktree>/.agt/<name>` 的說明檔。沒有檔案、或內容只有空白時回傳 None。
///
/// 說明檔由 Agent 控制，只當成文字使用。拒絕：目錄或檔案是連結（可能指向主機上的其他檔案，例如金鑰，
/// 內容會被當成 commit / PR 說明送出去）、不是一般檔案、超過 64KB、不是 UTF-8。
/// 開檔時不跟隨連結，並以開啟後的 handle 再檢查一次，避免檢查與讀取之間被換成連結。
pub fn read_note(worktree: &Path, name: &str) -> Result<Option<String>> {
    use std::io::Read;
    let refuse = |p: &Path, why: &str| GitError::Refused(format!("{} {why}，不處理", p.display()));
    let dir = worktree.join(NOTES_DIR);
    match std::fs::symlink_metadata(&dir) {
        Err(_) => return Ok(None),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(refuse(&dir, "是連結或不是目錄"));
        }
        Ok(_) => {}
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Err(_) => return Ok(None),
        Ok(m) if m.file_type().is_symlink() => return Err(refuse(&path, "是連結")),
        Ok(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc_o_nofollow());
    }
    let mut f = opts.open(&path).map_err(|_| refuse(&path, "無法開啟"))?;
    let meta = f.metadata()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(refuse(&path, "是連結或不是一般檔案"));
    }
    if meta.len() > NOTE_MAX {
        return Err(refuse(&path, "超過 64KB，不像是說明文字"));
    }
    let mut buf = Vec::new();
    f.by_ref().take(NOTE_MAX + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > NOTE_MAX {
        return Err(refuse(&path, "超過 64KB，不像是說明文字"));
    }
    let text = String::from_utf8(buf).map_err(|_| refuse(&path, "不是 UTF-8 文字"))?;
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .replace("\r\n", "\n");
    Ok(if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_string() + "\n")
    })
}

#[cfg(unix)]
pub(crate) fn libc_o_nofollow() -> i32 {
    // 不為了一個常數引入 libc：Linux 0o400000、macOS / BSD 0x100
    if cfg!(target_os = "linux") {
        0o400000
    } else {
        0x100
    }
}

/// 刪除用過的說明檔（下一輪由 Agent 重寫）；失敗只記錄。
pub fn remove_note(worktree: &Path, name: &str) {
    let p = worktree.join(NOTES_DIR).join(name);
    if let Err(e) = std::fs::remove_file(&p) {
        tracing::warn!("無法刪除 {}：{e}", p.display());
    }
}

/// 把說明文字寫到可信的 gitdir 裡，給 `git commit -F` / `gh --body-file` 使用
/// （不直接傳 worktree 裡的檔案：讀取之後 Agent 仍可能把它換掉）。
pub(crate) fn stage_note(gitdir: &Path, name: &str, text: &str) -> Result<PathBuf> {
    let p = gitdir.join(name);
    std::fs::write(&p, text)?;
    Ok(p)
}

/// 說明檔第一個非空白行，當成標題。
pub fn subject_line(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}
