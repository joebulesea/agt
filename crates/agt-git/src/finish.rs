//! 收尾相關：Agent 寫的說明檔、commit、merge、PR。

use super::*;

/// 無法自行 commit 的 Agent（例如在容器中執行的）寫說明檔的目錄（相對於 worktree）。
pub const NOTES_DIR: &str = ".agt";
/// commit 訊息：第一行是標題，空一行後是內文。
pub const COMMIT_MSG_FILE: &str = "COMMIT_MSG";
/// PR 說明（Markdown）。
pub const PR_BODY_FILE: &str = "PR_BODY.md";
/// 讓 git 忽略說明檔目錄的 info/exclude 規則。
pub const NOTES_EXCLUDE: &str = "/.agt/";
pub(super) const NOTE_MAX: u64 = 64 * 1024;

/// 讀取 Agent 寫在 `<worktree>/.agt/<name>` 的說明檔。沒有檔案、或內容只有空白時回傳 None。
///
/// 說明檔由 Agent 控制，只當成文字使用。拒絕：目錄或檔案是連結（可能指向主機上的其他檔案，例如金鑰，
/// 內容會被當成 commit / PR 說明送出去）、不是一般檔案、超過 64KB、不是 UTF-8。
/// 開檔時不跟隨連結，並以開啟後的 handle 再檢查一次，避免檢查與讀取之間被換成連結。
pub fn read_note(worktree: &Path, name: &str) -> Result<Option<String>> {
    use std::io::Read;
    let refuse = |p: &Path, why: &str| GitError::Refused(format!("{} {why}，不處理", p.display()));
    let dir = worktree.join(NOTES_DIR);
    match std::fs::symlink_metadata(&dir) {
        Err(_) => return Ok(None),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(refuse(&dir, "是連結或不是目錄"));
        }
        Ok(_) => {}
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Err(_) => return Ok(None),
        Ok(m) if m.file_type().is_symlink() => return Err(refuse(&path, "是連結")),
        Ok(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc_o_nofollow());
    }
    let mut f = opts.open(&path).map_err(|_| refuse(&path, "無法開啟"))?;
    let meta = f.metadata()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(refuse(&path, "是連結或不是一般檔案"));
    }
    if meta.len() > NOTE_MAX {
        return Err(refuse(&path, "超過 64KB，不像是說明文字"));
    }
    let mut buf = Vec::new();
    f.by_ref().take(NOTE_MAX + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > NOTE_MAX {
        return Err(refuse(&path, "超過 64KB，不像是說明文字"));
    }
    let text = String::from_utf8(buf).map_err(|_| refuse(&path, "不是 UTF-8 文字"))?;
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .replace("\r\n", "\n");
    Ok(if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_string() + "\n")
    })
}

#[cfg(unix)]
pub(super) fn libc_o_nofollow() -> i32 {
    // 不為了一個常數引入 libc：Linux 0o400000、macOS / BSD 0x100
    if cfg!(target_os = "linux") {
        0o400000
    } else {
        0x100
    }
}

/// 刪除用過的說明檔（下一輪由 Agent 重寫）；失敗只記錄。
pub fn remove_note(worktree: &Path, name: &str) {
    let p = worktree.join(NOTES_DIR).join(name);
    if let Err(e) = std::fs::remove_file(&p) {
        tracing::warn!("無法刪除 {}：{e}", p.display());
    }
}

/// 把說明文字寫到可信的 gitdir 裡，給 `git commit -F` / `gh --body-file` 使用
/// （不直接傳 worktree 裡的檔案：讀取之後 Agent 仍可能把它換掉）。
pub(super) fn stage_note(gitdir: &Path, name: &str, text: &str) -> Result<PathBuf> {
    let p = gitdir.join(name);
    std::fs::write(&p, text)?;
    Ok(p)
}

/// 說明檔第一個非空白行，當成標題。
pub fn subject_line(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
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
pub(super) fn new_gitlinks(repo: &Path, worktree: &Path) -> Result<Vec<String>> {
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

/// branch 相對 base 變更過的檔案中，合併後會在主機或 CI 上**自動執行**的設定檔
/// （hooks、CI workflow、編輯器 task、agt 的 setup 等）。收尾前要讓使用者再確認一次：
/// 沙箱裡的 Agent 改不到主機，但這些檔案一旦合併就等於在沙箱外執行它寫的內容。
///
/// `extra`：使用者追加的路徑（設定 `general.sensitive_paths`），規則見 [`matches_extra`]。
pub fn sensitive_changes(
    repo: &Path,
    worktree: &Path,
    base: &str,
    extra: &[String],
) -> Result<Vec<String>> {
    let out = git_wt(
        repo,
        worktree,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            &format!("{base}...HEAD"),
        ],
    )?;
    Ok(out
        .split('\0')
        .filter(|p| is_sensitive_path(p) || matches_extra(p, extra))
        .map(str::to_string)
        .collect())
}

/// 使用者追加的示警路徑（不分大小寫）：
/// - 以 `/` 結尾：該目錄底下的所有檔案，例如 `scripts/`
/// - 含有 `/`：repo 根目錄起算的完整路徑，例如 `tools/build.sh`
/// - 其他：任何目錄下同名的檔案，例如 `package.json`、`Makefile`
pub fn matches_extra(path: &str, extra: &[String]) -> bool {
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    extra.iter().any(|e| {
        let e = e.trim().trim_start_matches("./").to_ascii_lowercase();
        if e.is_empty() {
            false
        } else if e.ends_with('/') {
            p.starts_with(&e)
        } else if e.contains('/') {
            p == e
        } else {
            name == e
        }
    })
}

/// 見 [`sensitive_changes`]。路徑以 repo 根目錄為準、用 `/` 分隔。
pub fn is_sensitive_path(path: &str) -> bool {
    /// 整個目錄都算。
    const DIRS: &[&str] = &[
        ".claude/",
        ".gemini/",
        ".cursor/",
        ".github/workflows/",
        ".github/actions/",
        ".circleci/",
        ".githooks/",
        ".husky/",
        ".vscode/",
        ".devcontainer/",
    ];
    /// repo 根目錄的檔案。
    const ROOT_FILES: &[&str] = &[".agt.toml", ".mcp.json", ".gitlab-ci.yml", ".gitmodules"];
    /// 任何目錄下的檔案名稱。
    const NAMES: &[&str] = &[".gitattributes", ".envrc"];
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    DIRS.iter().any(|d| p.starts_with(d))
        || ROOT_FILES.contains(&p.as_str())
        || NAMES.contains(&name)
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

/// 結果：PR 網址或要使用者手動執行的指令。
pub fn create_pr(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
    title: &str,
) -> Result<String> {
    // 先驗證；gh 自己也會在 worktree 裡呼叫 git，下面用同一個 gitdir 固定它
    let gitdir = trusted_gitdir(repo, worktree)?;
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
    let has_gh = probe.status().map(|s| s.success()).unwrap_or(false);
    let remote = git_wt(repo, worktree, &["remote"])?;
    let remote = remote
        .lines()
        .find(|r| *r == "origin")
        .or(remote.lines().next());
    let Some(remote) = remote.map(str::to_string) else {
        return Err(GitError::Refused(
            "這個 repo 沒有設定 remote，無法建立 PR".into(),
        ));
    };
    if !has_gh {
        return Ok(format!(
            "找不到 gh，請手動執行：\n  git -C \"{}\" push -u {remote} {branch}",
            worktree.display()
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
    r.map(|out| match note {
        Some(n) => format!("{out}\n{n}"),
        None => out,
    })
}

/// PR 的目標 branch。分頁的 base 常是主 repo 當時所在、只在本機的 branch（例如還沒 push 的 feat/…），
/// GitHub 上沒有它就無法建立 PR：這時若它的內容都已在 remote 的預設 branch 裡，改用預設 branch
/// （PR 的內容仍只有這個分頁的 commit），並回傳說明；否則拒絕（避免 PR 混進 base 上未發布的 commit）。
pub(super) fn resolve_pr_base(
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
pub(super) fn open_or_update_pr(
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

pub(super) fn gh_run(worktree: &Path, gitdir: &Path, args: &[&str]) -> Result<String> {
    let mut gh = Command::new("gh");
    gh.current_dir(worktree)
        .env("GIT_DIR", gitdir)
        .env("GIT_WORK_TREE", worktree)
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
    let out = gh.output()?;
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
