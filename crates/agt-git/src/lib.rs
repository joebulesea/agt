//! Worktree 管理（規格 7）。直接呼叫系統的 `git` 指令，行為和使用者手動操作一致。
//!
//! 安全規則（規格 7.4）：絕不執行 `git push --force`、`git reset --hard`，
//! 也絕不刪除非 `agt/` 開頭的 branch。
//!
//! 模組分工（對外的 API 都從 crate 根匯出，使用方不需要知道模組）：
//!
//! | 模組 | 內容 |
//! | --- | --- |
//! | `exec` | 執行 git；信任邊界（`git` / `git_wt`）與版本檢查 |
//! | `repo` | repo 資訊、路徑比較、branch / 目錄 / 短 ID 命名 |
//! | `worktree` | worktree 的建立、移除、列舉 |
//! | `status` | 狀態與 diff |
//! | `files` | `copy_files`、`.git/info/exclude`（防符號連結與路徑逃逸） |
//! | `notes` | `.agt/COMMIT_MSG`、`.agt/PR_BODY.md` 的安全讀取 |
//! | `merge` | commit 與合併進 base |
//! | `sensitive` | 收尾前要求確認的敏感路徑 |
//! | `pr` | push 並建立 / 更新 PR（`gh`） |

use thiserror::Error;

mod exec;
mod files;
mod merge;
mod notes;
mod pr;
mod repo;
mod sensitive;
mod status;
mod worktree;

pub use exec::*;
pub use files::*;
pub use merge::*;
pub use notes::*;
pub use pr::*;
pub use repo::*;
pub use sensitive::*;
pub use status::*;
pub use worktree::*;

/// agt 建立的 branch 一律以此開頭。
pub const BRANCH_PREFIX: &str = "agt/";

/// 最低 git 版本。
pub const MIN_GIT: (u32, u32) = (2, 20);

#[derive(Debug, Error)]
pub enum GitError {
    /// git 的錯誤訊息原樣保留（規格 7.1）。
    #[error("git {args} 失敗：{stderr}")]
    Command { args: String, stderr: String },
    #[error("無法執行 git：{0}")]
    Io(#[from] std::io::Error),
    #[error("{0} 不是 git repo")]
    NotRepo(String),
    #[error("v1 不支援 bare repo")]
    Bare,
    #[error("v1 不支援 submodule")]
    Submodule,
    #[error("{0}")]
    Dirty(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, GitError>;

#[cfg(test)]
mod tests;
