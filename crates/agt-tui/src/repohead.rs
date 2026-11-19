//! 新增分頁表單用：讀出 repo 目前所在的 branch 與 remote 的預設 branch，
//! 提醒「Base 留空時會用目前的 HEAD branch」實際是哪一個。
//!
//! 直接讀 `.git` 裡的檔案，不執行 git：表單每次重畫都會呼叫，而且不必在使用者輸入的任意目錄執行程式。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoHead {
    /// 目前所在的 branch；None 表示 detached HEAD。
    pub branch: Option<String>,
    /// remote（origin）的預設 branch；查不到時為 None。
    pub default: Option<String>,
}

/// `path` 所在的 git repo（往上層找 `.git`）的 HEAD 資訊；不是 repo 或讀不到時回傳 None。
pub fn read(path: &Path) -> Option<RepoHead> {
    let (gitdir, common) = find_gitdir(path)?;
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let branch = head
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string);
    Some(RepoHead {
        branch,
        default: remote_default(&common),
    })
}

/// (這個 worktree 的 gitdir, 主 repo 的共用 gitdir)
fn find_gitdir(path: &Path) -> Option<(PathBuf, PathBuf)> {
    for dir in path.ancestors() {
        let dot = dir.join(".git");
        let Ok(meta) = std::fs::metadata(&dot) else {
            continue;
        };
        if meta.is_dir() {
            return Some((dot.clone(), dot));
        }
        // worktree：.git 是指向 gitdir 的檔案，共用的部分在 commondir
        let text = std::fs::read_to_string(&dot).ok()?;
        let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        let gitdir = if target.is_absolute() {
            target
        } else {
            dir.join(target)
        };
        let common = std::fs::read_to_string(gitdir.join("commondir"))
            .ok()
            .map(|c| gitdir.join(c.trim()))
            .unwrap_or_else(|| gitdir.clone());
        return Some((gitdir, common));
    }
    None
}

/// origin 的預設 branch：`refs/remotes/origin/HEAD`（clone 時建立），沒有時看 origin/main、origin/master。
fn remote_default(common: &Path) -> Option<String> {
    let remotes = common.join("refs/remotes/origin");
    if let Ok(t) = std::fs::read_to_string(remotes.join("HEAD"))
        && let Some(b) = t.trim().strip_prefix("ref: refs/remotes/origin/")
    {
        return Some(b.to_string());
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).unwrap_or_default();
    ["main", "master"]
        .into_iter()
        .find(|b| {
            remotes.join(b).is_file()
                || packed
                    .lines()
                    .any(|l| l.ends_with(&format!(" refs/remotes/origin/{b}")))
        })
        .map(str::to_string)
}

/// Base 欄位留空時要顯示的提醒：(欄位裡的說明, 額外的警告列)。
pub fn base_hint(head: Option<&RepoHead>) -> (String, Option<String>) {
    match head {
        None => ("（預設：目前的 HEAD branch）".into(), None),
        Some(RepoHead { branch: None, .. }) => (
            "（目前是 detached HEAD，請填 base）".into(),
            Some("⚠ repo 目前是 detached HEAD，請在 Base 填入要合併到的 branch".into()),
        ),
        Some(RepoHead {
            branch: Some(b),
            default,
        }) => {
            let hint = format!("（預設：目前的 HEAD，{b}）");
            let warn = match default {
                Some(d) if d != b => {
                    Some(format!("⚠ Base 會是 {b}，不是 {d}（PR / merge 的目標）"))
                }
                _ => None,
            };
            (hint, warn)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn reads_branch_and_remote_default() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("r");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/feat/x\n");
        write(
            &repo.join(".git/refs/remotes/origin/HEAD"),
            "ref: refs/remotes/origin/main\n",
        );
        std::fs::create_dir_all(repo.join("sub/dir")).unwrap();
        let h = read(&repo.join("sub/dir")).unwrap();
        assert_eq!(h.branch.as_deref(), Some("feat/x"));
        assert_eq!(h.default.as_deref(), Some("main"));
        let (hint, warn) = base_hint(Some(&h));
        assert!(hint.contains("feat/x"));
        assert!(warn.unwrap().contains("不是 main"));

        // 已在預設 branch：不警告
        write(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
        assert_eq!(base_hint(read(&repo).as_ref()).1, None);

        // detached HEAD
        write(&repo.join(".git/HEAD"), "0123456789abcdef\n");
        assert!(base_hint(read(&repo).as_ref()).1.is_some());
    }

    #[test]
    fn worktree_and_packed_refs() {
        let d = tempfile::tempdir().unwrap();
        let common = d.path().join("main/.git");
        write(
            &common.join("packed-refs"),
            "abc refs/remotes/origin/master\n",
        );
        let gd = common.join("worktrees/w1");
        write(&gd.join("HEAD"), "ref: refs/heads/agt/x\n");
        write(&gd.join("commondir"), "../..\n");
        let wt = d.path().join("w1");
        write(&wt.join(".git"), &format!("gitdir: {}\n", gd.display()));
        let h = read(&wt).unwrap();
        assert_eq!(h.branch.as_deref(), Some("agt/x"));
        assert_eq!(h.default.as_deref(), Some("master"));
    }

    #[test]
    fn not_a_repo() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read(d.path()), None);
        assert_eq!(base_hint(None).1, None);
    }
}
