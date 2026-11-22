//! 收尾：push 並建立或更新 GitHub PR（透過 `gh`）。

use crate::*;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// 結果：PR 網址或要使用者手動執行的指令。
pub fn create_pr(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
    title: &str,
) -> Result<(String, Option<PrTarget>)> {
    // 先驗證；gh 自己也會在 worktree 裡呼叫 git，下面用同一個 gitdir 固定它
    let gitdir = trusted_gitdir(repo, worktree)?;
    let has_gh = has_gh();
    let remote = pick_remote(repo, worktree)?;
    if !has_gh {
        return Ok((
            format!(
                "找不到 gh，請手動執行：\n  git -C \"{}\" push -u {remote} {branch}",
                worktree.display()
            ),
            None,
        ));
    }
    // 先讀說明檔、確認 PR 的目標：不合規定（連結、base 不在 remote 上等）時在 push 之前就停下
    let body = read_note(worktree, PR_BODY_FILE)?;
    let (pr_base, note) = resolve_pr_base(repo, worktree, &remote, base)?;
    let base = pr_base.as_str();
    git_wt(repo, worktree, &["push", "-u", &remote, branch])?;
    let staged = match &body {
        Some(text) => Some(stage_note(&gitdir, "AGT_PR_BODY.md", text)?),
        None => None,
    };
    let r = open_or_update_pr(worktree, &gitdir, branch, base, title, staged.as_deref());
    if let Some(p) = &staged {
        let _ = std::fs::remove_file(p);
    }
    if r.is_ok() && body.is_some() {
        remove_note(worktree, PR_BODY_FILE);
    }
    let target = PrTarget {
        name: "pr".into(),
        branch: branch.to_string(),
        base: base.to_string(),
    };
    r.map(|out| {
        let text = match note {
            Some(n) => format!("{out}\n{n}"),
            None => out,
        };
        (text, Some(target))
    })
}

pub(crate) fn has_gh() -> bool {
    let mut probe = Command::new("gh");
    probe
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        probe.creation_flags(0x0800_0000);
    }
    probe.status().map(|s| s.success()).unwrap_or(false)
}

pub(crate) fn pick_remote(repo: &Path, worktree: &Path) -> Result<String> {
    let remote = git_wt(repo, worktree, &["remote"])?;
    remote
        .lines()
        .find(|r| *r == "origin")
        .or(remote.lines().next())
        .map(str::to_string)
        .ok_or_else(|| GitError::Refused("這個 repo 沒有設定 remote，無法建立 PR".into()))
}

/// PR 的目標 branch。分頁的 base 常是主 repo 當時所在、只在本機的 branch（例如還沒 push 的 feat/…），
/// GitHub 上沒有它就無法建立 PR：這時若它的內容都已在 remote 的預設 branch 裡，改用預設 branch
/// （PR 的內容仍只有這個分頁的 commit），並回傳說明；否則拒絕（避免 PR 混進 base 上未發布的 commit）。
pub(crate) fn resolve_pr_base(
    repo: &Path,
    worktree: &Path,
    remote: &str,
    base: &str,
) -> Result<(String, Option<String>)> {
    let heads = git_wt(
        repo,
        worktree,
        &[
            "ls-remote",
            "--heads",
            remote,
            &format!("refs/heads/{base}"),
        ],
    )?;
    if !heads.trim().is_empty() {
        return Ok((base.to_string(), None));
    }
    let head = git_wt(repo, worktree, &["ls-remote", "--symref", remote, "HEAD"])?;
    let default = head.lines().find_map(|l| {
        l.strip_prefix("ref: refs/heads/")
            .and_then(|r| r.strip_suffix("\tHEAD"))
            .map(str::to_string)
    });
    let sha = head
        .lines()
        .find(|l| !l.starts_with("ref:") && l.ends_with("\tHEAD"))
        .and_then(|l| l.split('\t').next())
        .map(str::to_string);
    if let (Some(default), Some(sha)) = (default, sha)
        && default != base
        && git_wt(repo, worktree, &["merge-base", "--is-ancestor", base, &sha]).is_ok()
    {
        let note = format!(
            "（{base} 不在 {remote} 上，它的內容都已在 {default} 中，PR 改以 {default} 為目標）"
        );
        return Ok((default, Some(note)));
    }
    Err(GitError::Refused(format!(
        "這個分頁的 base branch {base} 不在 {remote} 上，而且有 {remote} 還沒有的 commit，無法建立 PR。\
         請先 push {base}（git push -u {remote} {base}）再收尾 → pr。"
    )))
}

/// 已有開啟中的 PR：push 已經更新了它，有新的說明時改寫說明；否則建立 PR。
pub(crate) fn open_or_update_pr(
    worktree: &Path,
    gitdir: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body_file: Option<&Path>,
) -> Result<String> {
    let view = gh_run(
        worktree,
        gitdir,
        &[
            "pr",
            "view",
            branch,
            "--json",
            "url,state",
            "--jq",
            ".state + \" \" + .url",
        ],
    );
    if let Ok(line) = &view
        && let Some(url) = line.strip_prefix("OPEN ")
    {
        if let Some(f) = body_file {
            gh_run(
                worktree,
                gitdir,
                &["pr", "edit", branch, "--body-file", &f.to_string_lossy()],
            )?;
            return Ok(format!("已 push 並更新 PR 說明：{url}"));
        }
        return Ok(format!("已 push，PR 已更新：{url}"));
    }
    let mut args = vec![
        "pr", "create", "--base", base, "--head", branch, "--title", title,
    ];
    let body_path;
    match body_file {
        Some(f) => {
            body_path = f.to_string_lossy().into_owned();
            args.extend(["--body-file", &body_path]);
        }
        None => args.push("--fill"),
    }
    gh_run(worktree, gitdir, &args)
}

pub(crate) fn gh_run(worktree: &Path, gitdir: &Path, args: &[&str]) -> Result<String> {
    let mut gh = gh_command(worktree, args);
    gh.env("GIT_DIR", gitdir).env("GIT_WORK_TREE", worktree);
    gh_output(gh, args, None)
}

/// 在可信的主 repo 目錄執行 `gh`（不經過 worktree，分頁關閉後也能用）。
/// 背景輪詢用，所以有逾時：連不上網路時不能一直卡住。
pub(crate) fn gh_run_repo(repo: &Path, args: &[&str], limit: Duration) -> Result<String> {
    gh_output(gh_command(repo, args), args, Some(limit))
}

fn gh_command(dir: &Path, args: &[&str]) -> Command {
    let mut gh = Command::new("gh");
    gh.current_dir(dir)
        // gh 自己呼叫的 git 也不能進巢狀 repo、不能啟動 fsmonitor（-c 只管我們直接執行的 git）
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "diff.ignoreSubmodules")
        .env("GIT_CONFIG_VALUE_1", "all")
        .env("GIT_CONFIG_KEY_2", "submodule.recurse")
        .env("GIT_CONFIG_VALUE_2", "false")
        .args(args)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        gh.creation_flags(0x0800_0000);
    }
    gh
}

fn gh_output(mut gh: Command, args: &[&str], limit: Option<Duration>) -> Result<String> {
    let out = match limit {
        Some(limit) => output_timeout(gh, limit)?,
        None => gh.output()?,
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(GitError::Command {
            args: format!(
                "gh {}",
                args.iter().take(2).copied().collect::<Vec<_>>().join(" ")
            ),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}
