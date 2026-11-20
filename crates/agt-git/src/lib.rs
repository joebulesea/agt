//! Worktree 管理（規格 7）。直接呼叫系統的 `git` 指令，行為和使用者手動操作一致。
//!
//! 安全規則（規格 7.4）：絕不執行 `git push --force`、`git reset --hard`，
//! 也絕不刪除非 `agt/` 開頭的 branch。

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use thiserror::Error;

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

// 信任邊界：worktree 的內容（含它的 `.git`）是 Agent 可寫、不可信的，尤其是在容器 / 沙箱中執行的 Agent。
// 對 worktree 執行 git 一律走 `git_wt`：先確認 `.git` 是指向主 repo 登記的 gitdir 的檔案，
// 再用 `GIT_DIR` / `GIT_WORK_TREE` 固定，git 就不會去採用 worktree 裡被替換過的 `.git`
// （例如換成自己的 `.git` 目錄、塞進會執行外部程式的設定）。主 repo 的 git 指令用 `git`。

fn command(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        // 額外保險：背景指令不需要 fsmonitor，也不該因為任何設定來源而啟動外部程式
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        // 背景輪詢的 git status 不搶 index.lock，避免 Agent 自己的 git add / commit 失敗
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        // daemon 沒有 console，避免每個 git 指令都跳出視窗
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn run_raw(dir: &Path, args: &[&str]) -> Result<Output> {
    Ok(command(dir).args(args).output()?)
}

/// 執行 git，成功時回傳去掉結尾空白的 stdout。只用於可信的目錄（主 repo）；
/// worktree 請用 [`git_wt`]。
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run(command(dir), args)
}

fn run(mut c: Command, args: &[&str]) -> Result<String> {
    debug_assert!(
        !(args.first() == Some(&"push")
            && args.iter().any(|a| a.starts_with("--force") || *a == "-f")),
        "agt 不執行 git push --force"
    );
    debug_assert!(
        !(args.first() == Some(&"reset") && args.contains(&"--hard")),
        "agt 不執行 git reset --hard"
    );
    let out = c.args(args).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let mut stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            stderr = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(GitError::Command {
            args: args.join(" "),
            stderr,
        })
    }
}

/// 確認 `wt` 的 `.git` 是指向主 repo 登記的 gitdir（`<common>/worktrees/<id>`）的檔案，回傳該 gitdir。
///
/// 拒絕：`.git` 不是一般檔案（被換成目錄或連結）、或它指向的不是主 repo `worktrees/` 底下的目錄
/// （例如指到 worktree 內自己準備的目錄）。`repo` 必須是可信的主 repo 路徑。
pub fn trusted_gitdir(repo: &Path, wt: &Path) -> Result<PathBuf> {
    let refuse = |why: &str| {
        GitError::Refused(format!(
            "worktree {} 的 .git 不可信（{why}），不對它執行 git；請檢查 Agent 是否改動過 .git",
            wt.display()
        ))
    };
    let gitfile = wt.join(".git");
    let meta = std::fs::symlink_metadata(&gitfile).map_err(|_| refuse("找不到 .git"))?;
    if !meta.is_file() {
        return Err(refuse(".git 不是檔案"));
    }
    let text = std::fs::read_to_string(&gitfile).map_err(|_| refuse("無法讀取 .git"))?;
    let target = text
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| refuse(".git 格式不符"))?;
    let target = PathBuf::from(target);
    let gitdir = normalize(&if target.is_absolute() {
        target
    } else {
        wt.join(target)
    });

    // 主 repo 的共用 gitdir。一般 repo 的 `.git` 就是目錄，直接用，不必啟動 git（背景輪詢每次都會呼叫，
    // Windows 上啟動行程很慢）；`.git` 是檔案（repo 本身是 worktree、或 --separate-git-dir）時才問 git。
    let dot_git = repo.join(".git");
    let common = if std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir()) {
        dot_git
    } else {
        let c = PathBuf::from(git(repo, &["rev-parse", "--git-common-dir"])?);
        if c.is_absolute() { c } else { repo.join(c) }
    };
    let common = normalize(&common);
    let registered = common.join("worktrees");
    if !gitdir.is_dir() || !gitdir.parent().is_some_and(|p| same_path(p, &registered)) {
        return Err(refuse(".git 沒有指向主 repo 登記的 gitdir"));
    }
    Ok(gitdir)
}

/// 不進入 submodule / 巢狀 repo。
const SUBMODULE_OFF: [&str; 4] = [
    "-c",
    "diff.ignoreSubmodules=all",
    "-c",
    "submodule.recurse=false",
];

/// 對 worktree 執行 git 用的指令：先驗證，再以 `GIT_DIR` / `GIT_WORK_TREE` 固定。
fn wt_command(repo: &Path, wt: &Path) -> Result<Command> {
    let gitdir = trusted_gitdir(repo, wt)?;
    let mut c = command(wt);
    c.env("GIT_DIR", gitdir).env("GIT_WORK_TREE", wt);
    // 巢狀 repo：Agent 可以在 worktree 裡放一個含惡意設定的 repo，被 `git add -A` 當成 gitlink 加入後，
    // status / diff 會進到裡面執行 git（讀它的 config、執行其中的 fsmonitor）。一律不進 submodule。
    c.args(SUBMODULE_OFF);
    Ok(c)
}

/// 對 worktree 執行 git（`repo` 是可信的主 repo 路徑）。
pub fn git_wt(repo: &Path, wt: &Path, args: &[&str]) -> Result<String> {
    run(wt_command(repo, wt)?, args)
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    run_raw(dir, args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 檢查 git 版本 ≥ 2.20。
pub fn check_version() -> Result<String> {
    let v = git(Path::new("."), &["--version"])?;
    let nums: Vec<u32> = v
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() >= 2 && (nums[0], nums[1]) < MIN_GIT {
        return Err(GitError::Refused(format!(
            "需要 git {}.{} 以上，目前是 {v}",
            MIN_GIT.0, MIN_GIT.1
        )));
    }
    Ok(v)
}

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

fn base36(mut n: u64, len: usize) -> String {
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

fn fnv1a(s: &str) -> u64 {
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
fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
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

fn status_args(include_untracked: bool) -> [&'static str; 3] {
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

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// `base` 之下的 `rel` 路徑上（含每一層上層目錄）是否有符號連結。
fn path_has_symlink(base: &Path, rel: &Path) -> bool {
    let mut p = base.to_path_buf();
    rel.components().any(|c| {
        p.push(c);
        is_symlink(&p)
    })
}

/// 複製目錄；來源或目的地是符號連結的項目略過（理由見 [`copy_files`]），
/// 例如 node_modules/.bin 裡的連結。呼叫端已確認 `src`、`dst` 本身不是連結。
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
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
fn is_inside(child: &Path, parent: &Path) -> bool {
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
fn own_gitdir(wt: &Path) -> Result<(PathBuf, PathBuf)> {
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

pub fn longpaths_enabled(repo: &Path) -> bool {
    git(repo, &["config", "--get", "core.longpaths"])
        .map(|v| v == "true")
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Merge,
    Squash,
    Rebase,
}

const CONFLICT_ARGS: [&str; 3] = ["diff", "--name-only", "--diff-filter=U"];

fn conflicted_files(dir: &Path) -> String {
    git(dir, &CONFLICT_ARGS).unwrap_or_default()
}

fn conflicted_files_wt(repo: &Path, wt: &Path) -> String {
    git_wt(repo, wt, &CONFLICT_ARGS).unwrap_or_default()
}

mod finish;
pub use finish::*;

#[cfg(test)]
mod tests;
