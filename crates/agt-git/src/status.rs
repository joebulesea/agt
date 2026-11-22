//! worktree 的狀態與 diff。

use crate::*;
use std::collections::BTreeSet;
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

/// 這個 worktree 動過的檔案（相對 repo 根目錄、以 `/` 分隔）：已 commit（`base...HEAD`）、
/// 未 commit 的修改與未追蹤的檔案的聯集。用來偵測兩個分頁是否改了同一個檔案。
/// `.agt/` 底下是 agt 自己的說明檔，不算。
pub fn changed_files(repo: &Path, worktree: &Path, base: &str) -> Result<BTreeSet<String>> {
    let range = format!("{base}...HEAD");
    let mut files = BTreeSet::new();
    for args in [
        // -z：檔名不被引號跳脫（中文、空白），以 NUL 分隔
        &["diff", "--name-only", "-z", "--no-ext-diff", &range][..],
        &["diff", "--name-only", "-z", "--no-ext-diff", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        for f in git_wt(repo, worktree, args)?.split('\0') {
            if !f.is_empty() && !f.starts_with(".agt/") {
                files.insert(f.to_string());
            }
        }
    }
    Ok(files)
}
