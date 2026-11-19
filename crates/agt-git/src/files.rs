//! 把檔案複製進 worktree（`copy_files`）與維護 `.git/info/exclude`；兩者都要防符號連結與路徑逃逸。

use crate::*;
use std::path::{Path, PathBuf};

/// 依 `copy_files` 從 repo 複製未追蹤的檔案（例如 `.env`）到 worktree。不存在的檔案略過。
pub fn copy_files(from: &Path, to: &Path, files: &[String]) -> Result<Vec<String>> {
    let mut copied = Vec::new();
    for f in files {
        let rel = Path::new(f);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(GitError::Refused(format!(
                "copy_files 只能是 repo 內的相對路徑：{f}"
            )));
        }
        // copy_files 可以來自 repo 的 .agt.toml（不需要使用者確認），repo 也可以 commit 符號連結。
        // 經過符號連結就可能讀到 repo 外的檔案（例如把 ~/.ssh 複製進沙箱看得到的 worktree），
        // 或寫到 repo 外（worktree 裡同名的連結指向 ~/.bashrc 時，複製會先把它清空），一律拒絕。
        if path_has_symlink(from, rel) || path_has_symlink(to, rel) {
            return Err(GitError::Refused(format!(
                "copy_files 不能經過符號連結：{f}"
            )));
        }
        let src = from.join(rel);
        let dst = to.join(rel);
        if src.is_file() {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&src, &dst)?;
            copied.push(f.clone());
        } else if src.is_dir() {
            copy_dir(&src, &dst)?;
            copied.push(f.clone());
        }
    }
    Ok(copied)
}

pub(crate) fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// `base` 之下的 `rel` 路徑上（含每一層上層目錄）是否有符號連結。
pub(crate) fn path_has_symlink(base: &Path, rel: &Path) -> bool {
    let mut p = base.to_path_buf();
    rel.components().any(|c| {
        p.push(c);
        is_symlink(&p)
    })
}

/// 複製目錄；來源或目的地是符號連結的項目略過（理由見 [`copy_files`]），
/// 例如 node_modules/.bin 裡的連結。呼叫端已確認 `src`、`dst` 本身不是連結。
pub(crate) fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let target = dst.join(e.file_name());
        // DirEntry::file_type 不會跟著連結走
        if e.file_type()?.is_symlink() || is_symlink(&target) {
            tracing::warn!("copy_files 略過符號連結：{}", e.path().display());
            continue;
        }
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// `child` 是否就是 `parent` 或在它底下（先解析符號連結；Windows 不分大小寫）。
pub(crate) fn is_inside(child: &Path, parent: &Path) -> bool {
    let key = |p: &Path| {
        let s = normalize(p).to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (c, p) = (key(child), key(parent));
    c == p || c.starts_with(&format!("{p}/"))
}

/// 不需要主 repo 路徑、也不執行 git 的 worktree 檢查，給只知道 worktree 目錄的呼叫端用
/// （例如 [`add_exclude`]）。回傳 worktree 在主 repo 裡登記的 gitdir 與主 repo 的 `.git`。
///
/// 要求：`.git` 是一般檔案、指向 worktree 之外的目錄，而且該目錄的 `gitdir` 檔指回這個 worktree 的
/// `.git`（確實是這個 worktree 的管理目錄，不是別的 repo 或 Agent 自己準備的目錄）。
pub(crate) fn own_gitdir(wt: &Path) -> Result<(PathBuf, PathBuf)> {
    let refuse = |why: &str| {
        GitError::Refused(format!(
            "worktree {} 的 .git 不可信（{why}），不對它執行 git；請檢查 Agent 是否改動過 .git",
            wt.display()
        ))
    };
    let gitfile = wt.join(".git");
    let meta = std::fs::symlink_metadata(&gitfile).map_err(|_| refuse("找不到 .git"))?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(refuse(".git 不是一般檔案"));
    }
    let text = std::fs::read_to_string(&gitfile).map_err(|_| refuse("無法讀取 .git"))?;
    let target = text
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| refuse(".git 格式不符"))?;
    let target = PathBuf::from(target);
    let gitdir = if target.is_absolute() {
        target
    } else {
        wt.join(target)
    };
    if !gitdir.is_dir() {
        return Err(refuse("指向的 git 目錄不存在"));
    }
    let gitdir = normalize(&gitdir);
    if is_inside(&gitdir, wt) {
        return Err(refuse("指向 worktree 內的目錄"));
    }
    let back = std::fs::read_to_string(gitdir.join("gitdir"))
        .map_err(|_| refuse("指向的不是 worktree 的 git 目錄"))?;
    let back = PathBuf::from(back.trim());
    let back = if back.is_absolute() {
        back
    } else {
        gitdir.join(back)
    };
    if !same_path(&back, &gitfile) {
        return Err(refuse("指向別的 worktree 的 git 目錄"));
    }
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| normalize(&gitdir.join(c.trim())))
        .unwrap_or_else(|| gitdir.clone());
    Ok((gitdir, common))
}

/// 把 pattern 加入 `dir` 實際使用的 `info/exclude`（不會被 commit）。
///
/// 不執行 git：`dir` 是 Agent 寫得到的目錄，每次啟動 / 重新啟動分頁都會呼叫，
/// 這時 `.git` 可能已被上一次的 Agent 換掉。一般 repo 用 `.git/info/exclude`；
/// worktree 用主 repo 的 `.git/info/exclude`（先通過 [`own_gitdir`] 的檢查）。
pub fn add_exclude(worktree: &Path, pattern: &str) -> Result<()> {
    let dotgit = worktree.join(".git");
    let meta = std::fs::symlink_metadata(&dotgit)
        .map_err(|_| GitError::NotRepo(worktree.display().to_string()))?;
    let path = if meta.is_dir() {
        dotgit.join("info").join("exclude")
    } else {
        own_gitdir(worktree)?.1.join("info").join("exclude")
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = existing;
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(pattern);
    s.push('\n');
    std::fs::write(&path, s)?;
    Ok(())
}
