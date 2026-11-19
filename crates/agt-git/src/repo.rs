//! repo 資訊、路徑比較，以及 branch / 目錄 / 短 ID 的命名規則。

use crate::*;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub name: String,
    /// 目前的 HEAD branch；detached HEAD 時為 None。
    pub head_branch: Option<String>,
}

/// 規格 7.5：不是 git repo 時回傳 `Ok(None)`；bare repo 與 submodule 直接報錯。
pub fn repo_info(path: &Path) -> Result<Option<RepoInfo>> {
    if !path.is_dir() {
        return Err(GitError::NotRepo(path.display().to_string()));
    }
    if !git_ok(path, &["rev-parse", "--git-dir"]) {
        return Ok(None);
    }
    if git(path, &["rev-parse", "--is-bare-repository"])? == "true" {
        return Err(GitError::Bare);
    }
    if !git(path, &["rev-parse", "--show-superproject-working-tree"])?.is_empty() {
        return Err(GitError::Submodule);
    }
    let root = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?);
    let root = normalize(&root);
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let head_branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    Ok(Some(RepoInfo {
        root,
        name,
        head_branch,
    }))
}

/// 正規化路徑（解析符號連結；Windows 去掉 `\\?\` 前綴）。
pub fn normalize(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\")
        && !rest.starts_with("UNC\\")
    {
        return PathBuf::from(rest);
    }
    c
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    if cfg!(windows) {
        a.to_string_lossy().replace('\\', "/").to_lowercase()
            == b.to_string_lossy().replace('\\', "/").to_lowercase()
    } else {
        a == b
    }
}

pub(crate) fn base36(mut n: u64, len: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; len];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out).unwrap()
}

/// 6 碼 base36 短 id（規格 5.1）。
pub fn short_id(random: u64) -> String {
    base36(random, 6)
}

pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 任務名稱轉 slug：ASCII 小寫、以 `-` 連接、最多 32 字元；無法轉換時回傳空字串（規格 7.2）。
pub fn slugify(task: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in task.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.truncate(32);
    out.trim_end_matches('-').to_string()
}

/// branch 名稱 `agt/<agent>/<slug>-<id>`；沒有 slug 時為 `agt/<agent>/<id>`。
pub fn branch_name(agent: &str, task: &str, id: &str) -> String {
    let slug = slugify(task);
    if slug.is_empty() {
        format!("{BRANCH_PREFIX}{agent}/{id}")
    } else {
        format!("{BRANCH_PREFIX}{agent}/{slug}-{id}")
    }
}

/// worktree 的上層目錄名稱 `<repo 名稱>-<路徑雜湊 4 碼>`。
pub fn repo_dir_name(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let key = root.to_string_lossy().replace('\\', "/").to_lowercase();
    format!("{name}-{}", base36(fnv1a(&key), 4))
}

pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub fn ref_exists(repo: &Path, r: &str) -> bool {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ],
    )
}
