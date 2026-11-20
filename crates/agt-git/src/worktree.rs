//! worktree 的建立、移除與列舉。

use crate::*;
use std::path::{Path, PathBuf};

/// `git worktree add <path> -b <branch> <base>`
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let p = path.to_string_lossy();
    git(repo, &["worktree", "add", &p, "-b", branch, base])?;
    Ok(())
}

/// 讓 worktree 在容器 / 沙箱裡也能使用 git，回傳要一起掛載的主 repo `.git` 目錄。
///
/// worktree 的 `.git` 檔案預設記錄主 repo 的絕對路徑（Windows 上是 `C:/...`），
/// 在 Linux 沙箱裡解析不到。這裡把它改寫成相對路徑（git 本來就支援），
/// 只要沙箱把 worktree 和主 repo 的 `.git` 都掛在與主機對應的位置（例如 sbx 的
/// `C:\x` → `/c/x`），相對路徑就能正確解析。兩者不在同一個磁碟時無法改成相對路徑，維持原樣。
///
/// `wt` 不是 worktree（沒有 `.git` 檔案，或 `.git` 是目錄）時回傳 None。
pub fn prepare_container_gitdir(wt: &Path) -> Option<PathBuf> {
    let gitfile = wt.join(".git");
    if !gitfile.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&gitfile).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let target = PathBuf::from(target);
    let gitdir = if target.is_absolute() {
        target.clone()
    } else {
        wt.join(&target)
    };
    let gitdir = normalize(&gitdir);
    // 主 repo 的 .git：worktree 的 gitdir 裡有 commondir（通常是 "../.."）
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| normalize(&gitdir.join(c.trim())))
        .unwrap_or_else(|| gitdir.clone());
    if target.is_absolute()
        && let Some(rel) = relative_path(&normalize(wt), &gitdir)
    {
        let rel = rel.to_string_lossy().replace('\\', "/");
        if let Err(e) = std::fs::write(&gitfile, format!("gitdir: {rel}\n")) {
            tracing::warn!("無法把 {} 改成相對路徑：{e}", gitfile.display());
        }
    }
    Some(common)
}

/// `to` 相對於 `from` 的路徑；不在同一個根目錄（例如不同磁碟）時回傳 None。
pub(crate) fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let key = |c: &Component| {
        let s = c.as_os_str().to_string_lossy().into_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    // 根（磁碟代號 / 根目錄）必須相同
    let roots = |v: &[Component]| {
        v.iter()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .map(key)
            .collect::<Vec<_>>()
    };
    if roots(&a) != roots(&b) || roots(&a).is_empty() {
        return None;
    }
    let common = a
        .iter()
        .zip(b.iter())
        .take_while(|(x, y)| key(x) == key(y))
        .count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// 移除 worktree。`force` 會丟棄未 commit 的變更，只能在使用者確認後使用。
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    match git(repo, &args) {
        Ok(_) => Ok(()),
        Err(e) if !path.exists() => {
            let _ = git(repo, &["worktree", "prune"]);
            tracing::debug!("worktree 已不存在：{e}");
            Ok(())
        }
        // `.git` 被改動過時 git 會拒絕移除。使用者已確認要丟棄（force），而且主 repo 仍記錄著
        // 這個 worktree：直接刪除目錄（remove_dir_all 不會順著符號連結刪到外面）再 prune。
        Err(e)
            if force
                && trusted_gitdir(repo, path).is_err()
                && list_worktrees(repo)
                    .is_ok_and(|l| l.iter().any(|w| same_path(&w.path, path))) =>
        {
            tracing::warn!(
                "worktree {} 的 .git 被改動過，改為直接刪除目錄：{e}",
                path.display()
            );
            std::fs::remove_dir_all(path)?;
            git(repo, &["worktree", "prune"]).map(|_| ())
        }
        Err(e) => Err(e),
    }
}

/// 刪除 branch；拒絕刪除非 `agt/` 開頭的 branch。
pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    if !branch.starts_with(BRANCH_PREFIX) {
        return Err(GitError::Refused(format!(
            "agt 不會刪除非 {BRANCH_PREFIX} 開頭的 branch：{branch}"
        )));
    }
    if branch_exists(repo, branch) {
        git(repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

pub fn prune(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"]).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// 解析 `git worktree list --porcelain`。
pub fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&out))
}

pub fn parse_worktree_list(out: &str) -> Vec<WorktreeEntry> {
    let mut list = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                list.push(c);
            }
            cur = Some(WorktreeEntry {
                path: PathBuf::from(p),
                ..Default::default()
            });
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(c) = cur.as_mut()
        {
            c.branch = Some(b.trim_start_matches("refs/heads/").to_string());
        }
    }
    if let Some(c) = cur {
        list.push(c);
    }
    list
}

pub fn longpaths_enabled(repo: &Path) -> bool {
    git(repo, &["config", "--get", "core.longpaths"])
        .map(|v| v == "true")
        .unwrap_or(false)
}
