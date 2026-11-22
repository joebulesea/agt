//! git 指令的執行：信任邊界（worktree 的 `.git` 不可信）、版本檢查。

use crate::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

// 信任邊界：worktree 的內容（含它的 `.git`）是 Agent 可寫、不可信的，尤其是在容器 / 沙箱中執行的 Agent。
// 對 worktree 執行 git 一律走 `git_wt`：先確認 `.git` 是指向主 repo 登記的 gitdir 的檔案，
// 再用 `GIT_DIR` / `GIT_WORK_TREE` 固定，git 就不會去採用 worktree 裡被替換過的 `.git`
// （例如換成自己的 `.git` 目錄、塞進會執行外部程式的設定）。主 repo 的 git 指令用 `git`。

pub(crate) fn command(dir: &Path) -> Command {
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

/// 在另一個執行緒把管線讀完。
fn drain<R: std::io::Read + Send + 'static>(p: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = vec![];
        if let Some(mut p) = p {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// 執行指令並等它結束，超過 `limit` 就終止並回傳 `TimedOut`。stdin 接空，stdout / stderr 另開執行緒讀，
/// 輸出很多時不會因為管線塞滿而卡住。背景輪詢用：`gh` 或 `git fetch` 卡在網路上時不能拖住整個輪詢。
pub(crate) fn output_timeout(mut c: Command, limit: Duration) -> std::io::Result<Output> {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // 讀取執行緒在管線關閉後自己結束，不等它
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("超過 {} 秒沒有結束，已終止", limit.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

pub(crate) fn run_raw(dir: &Path, args: &[&str]) -> Result<Output> {
    Ok(command(dir).args(args).output()?)
}

/// 執行 git，成功時回傳去掉結尾空白的 stdout。只用於可信的目錄（主 repo）；
/// worktree 請用 [`git_wt`]。
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run(command(dir), args)
}

pub(crate) fn run(mut c: Command, args: &[&str]) -> Result<String> {
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
pub(crate) const SUBMODULE_OFF: [&str; 4] = [
    "-c",
    "diff.ignoreSubmodules=all",
    "-c",
    "submodule.recurse=false",
];

/// 對 worktree 執行 git 用的指令：先驗證，再以 `GIT_DIR` / `GIT_WORK_TREE` 固定。
pub(crate) fn wt_command(repo: &Path, wt: &Path) -> Result<Command> {
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

pub(crate) fn git_ok(dir: &Path, args: &[&str]) -> bool {
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
