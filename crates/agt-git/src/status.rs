//! worktree 的狀態與 diff。

use crate::*;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitStatus {
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
}

/// 相對 base 的領先 / 落後 commit 數與是否有未 commit 的變更。
pub fn status(repo: &Path, worktree: &Path, base: &str) -> Result<GitStatus> {
    let counts = git_wt(
        repo,
        worktree,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...HEAD"),
        ],
    )?;
    let mut it = counts
        .split_whitespace()
        .map(|s| s.parse::<u32>().unwrap_or(0));
    let behind = it.next().unwrap_or(0);
    let ahead = it.next().unwrap_or(0);
    let dirty = is_dirty_wt(repo, worktree, true)?;
    Ok(GitStatus {
        ahead,
        behind,
        dirty,
    })
}

pub(crate) fn status_args(include_untracked: bool) -> [&'static str; 3] {
    [
        "status",
        "--porcelain",
        if include_untracked {
            "--untracked-files=normal"
        } else {
            "--untracked-files=no"
        },
    ]
}

/// 可信目錄（主 repo）是否有未 commit 的變更；worktree 請用 [`is_dirty_wt`]。
pub fn is_dirty(dir: &Path, include_untracked: bool) -> Result<bool> {
    Ok(!git(dir, &status_args(include_untracked))?.is_empty())
}

/// worktree 是否有未 commit 的變更。
pub fn is_dirty_wt(repo: &Path, wt: &Path, include_untracked: bool) -> Result<bool> {
    Ok(!git_wt(repo, wt, &status_args(include_untracked))?.is_empty())
}

/// 是否有不在 base、也不在任何 remote 上的 commit。
pub fn has_unpushed(repo: &Path, worktree: &Path, base: &str) -> Result<bool> {
    let n = git_wt(
        repo,
        worktree,
        &["rev-list", "--count", "HEAD", "--not", base, "--remotes"],
    )?;
    Ok(n.trim().parse::<u32>().unwrap_or(0) > 0)
}

/// `git diff <base>...HEAD` 加上未 commit 的變更與未追蹤的檔案。
pub fn diff(repo: &Path, worktree: &Path, base: &str) -> Result<String> {
    // --no-ext-diff：不執行任何設定的外部 diff 程式
    let committed = git_wt(
        repo,
        worktree,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            &format!("{base}...HEAD"),
        ],
    )?;
    let uncommitted = git_wt(
        repo,
        worktree,
        &["diff", "--no-color", "--no-ext-diff", "HEAD"],
    )?;
    let untracked = git_wt(
        repo,
        worktree,
        &["ls-files", "--others", "--exclude-standard"],
    )?;
    let mut out = String::new();
    out.push_str(&format!("### 已 commit（{base}...HEAD）\n"));
    out.push_str(if committed.is_empty() {
        "（無）\n"
    } else {
        &committed
    });
    out.push_str("\n\n### 未 commit 的變更\n");
    out.push_str(if uncommitted.is_empty() {
        "（無）\n"
    } else {
        &uncommitted
    });
    if !untracked.is_empty() {
        out.push_str("\n\n### 未追蹤的檔案\n");
        out.push_str(&untracked);
    }
    Ok(out)
}
