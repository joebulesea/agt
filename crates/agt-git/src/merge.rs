//! 收尾：commit、合併進 base。

use crate::*;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Merge,
    Squash,
    Rebase,
}

pub(crate) const CONFLICT_ARGS: [&str; 3] = ["diff", "--name-only", "--diff-filter=U"];

pub(crate) fn conflicted_files(dir: &Path) -> String {
    git(dir, &CONFLICT_ARGS).unwrap_or_default()
}

pub(crate) fn conflicted_files_wt(repo: &Path, wt: &Path) -> String {
    git_wt(repo, wt, &CONFLICT_ARGS).unwrap_or_default()
}

/// 把 worktree 中所有變更（含未追蹤的檔案）commit 起來；沒有變更時回傳 None，
/// 有 commit 時回傳實際使用的 commit 訊息。給無法自行 commit 的 Agent（例如在容器中執行的）使用。
///
/// Agent 寫了 `.agt/COMMIT_MSG` 時用它當 commit 訊息（commit 後刪除），否則用 `fallback`。
/// `.agt/` 一律不 commit（加入主 repo 的 info/exclude）。
pub fn commit_all(repo: &Path, worktree: &Path, fallback: &str) -> Result<Option<String>> {
    let gitdir = trusted_gitdir(repo, worktree)?;
    add_exclude(worktree, NOTES_EXCLUDE)?;
    if !is_dirty_wt(repo, worktree, true)? {
        return Ok(None);
    }
    let note = read_note(worktree, COMMIT_MSG_FILE)?;
    git_wt(repo, worktree, &["add", "-A"])?;
    let nested = new_gitlinks(repo, worktree)?;
    if !nested.is_empty() {
        // 巢狀 repo 的內容由 Agent 控制，主機的 git 之後會進到裡面執行（讀它的 config）：不加入
        for p in &nested {
            // --cached：只從 index 移除，不動工作目錄；-f：index 裡的內容與 HEAD、檔案都不同時 git 才肯移除
            git_wt(repo, worktree, &["rm", "--cached", "-f", "-q", "--", p])?;
        }
        return Err(GitError::Refused(format!(
            "worktree 裡有巢狀的 git repo：{}。agt 不支援把它 commit 進來；\
             請刪除其中的 .git（或把該目錄移走）後再收尾。",
            nested.join("、")
        )));
    }
    let message = match note {
        Some(text) => {
            let staged = stage_note(&gitdir, "AGT_COMMIT_MSG", &text)?;
            let r = git_wt(
                repo,
                worktree,
                &[
                    "commit",
                    "--cleanup=whitespace",
                    "-F",
                    &staged.to_string_lossy(),
                ],
            );
            let _ = std::fs::remove_file(&staged);
            r?;
            remove_note(worktree, COMMIT_MSG_FILE);
            text
        }
        None => {
            git_wt(repo, worktree, &["commit", "-m", fallback])?;
            fallback.to_string()
        }
    };
    Ok(Some(message))
}

/// index 裡有、HEAD 沒有的 gitlink（`git add -A` 把含有 `.git` 的子目錄當成 submodule 加入）。
pub(crate) fn new_gitlinks(repo: &Path, worktree: &Path) -> Result<Vec<String>> {
    const GITLINK: &str = "160000 ";
    let paths = |out: String| -> Vec<String> {
        out.split('\0')
            .filter(|l| l.starts_with(GITLINK))
            .filter_map(|l| l.split_once('\t').map(|(_, p)| p.to_string()))
            .collect()
    };
    let staged = paths(git_wt(repo, worktree, &["ls-files", "-s", "-z"])?);
    if staged.is_empty() {
        return Ok(staged);
    }
    // 還沒有任何 commit 時 HEAD 不存在：視為沒有既有的 gitlink
    let existing =
        paths(git_wt(repo, worktree, &["ls-tree", "-r", "-z", "HEAD"]).unwrap_or_default());
    Ok(staged
        .into_iter()
        .filter(|p| !existing.contains(p))
        .collect())
}

/// 回到主工作目錄將 branch 合併進 base（規格 7.3、7.4）。
/// 主工作目錄有未 commit 的變更時拒絕；發生衝突時停下來，不自動解決。
pub fn merge_into_base(
    main: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
    strategy: Strategy,
    message: &str,
) -> Result<String> {
    if is_dirty(main, false)? {
        return Err(GitError::Dirty(format!(
            "主工作目錄 {} 有未 commit 的變更，拒絕 merge",
            main.display()
        )));
    }
    if is_dirty_wt(main, worktree, false)? {
        return Err(GitError::Dirty(
            "worktree 還有未 commit 的變更，請先讓 Agent commit 或手動 commit".into(),
        ));
    }
    // base 不是本地 branch（例如 origin/main、tag）時 merge 會落在 detached HEAD 上
    if !branch_exists(main, base) {
        return Err(GitError::Refused(format!(
            "base {base} 不是本地 branch，無法 merge；請改用 pr，或手動合併"
        )));
    }
    // 不替使用者切換主工作目錄的 branch，避免之後的 commit 落在意料之外的 branch
    let current = git(main, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    if current.as_deref() != Some(base) {
        return Err(GitError::Refused(format!(
            "主工作目錄 {} 目前在 {}，不是 {base}；請先切換到 {base} 再 merge",
            main.display(),
            current.as_deref().unwrap_or("detached HEAD")
        )));
    }
    match strategy {
        Strategy::Merge => {
            let msg = format!("Merge {branch}: {message}");
            if let Err(e) = git(main, &["merge", "--no-ff", "-m", &msg, branch]) {
                let files = conflicted_files(main);
                if !files.is_empty() {
                    return Err(GitError::Conflict(format!(
                        "merge 發生衝突，已停在主工作目錄 {} 等待處理：\n{files}",
                        main.display()
                    )));
                }
                return Err(e);
            }
        }
        Strategy::Squash => {
            if let Err(e) = git(main, &["merge", "--squash", branch]) {
                let files = conflicted_files(main);
                if !files.is_empty() {
                    return Err(GitError::Conflict(format!(
                        "squash 發生衝突，已停在主工作目錄 {} 等待處理：\n{files}",
                        main.display()
                    )));
                }
                return Err(e);
            }
            if is_dirty(main, false)? {
                git(main, &["commit", "-m", message])?;
            }
        }
        Strategy::Rebase => {
            if let Err(e) = git_wt(main, worktree, &["rebase", base]) {
                let files = conflicted_files_wt(main, worktree);
                if !files.is_empty() {
                    return Err(GitError::Conflict(format!(
                        "rebase 發生衝突，已停在 worktree {} 等待處理：\n{files}",
                        worktree.display()
                    )));
                }
                return Err(e);
            }
            git(main, &["merge", "--ff-only", branch])?;
        }
    }
    Ok(format!("已將 {branch} 合併進 {base}"))
}
