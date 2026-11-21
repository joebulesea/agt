//! 分組收尾：Agent 在 `.agt/groups/<名稱>/` 描述多組變更，每組各自一個 branch、commit 與 PR。
//!
//! 每個分組目錄可含：
//! - `FILES`：這組包含的路徑，一行一個（相對於 worktree 根目錄；目錄代表底下全部）
//! - `COMMIT_MSG`：這組的 commit 訊息（第一行是標題，也當 PR 標題）
//! - `PR_BODY.md`：這組的 PR 說明
//! - `AFTER`：（選用）這組接在哪一組後面（組名）。用到另一組新增的程式碼時使用
//!
//! 沒有被任何分組認領的變更自成一組 `rest`。每組的 branch 是 `<任務 branch>-<名稱>`，
//! 內容 = merge-base 加上該組的檔案，所以各 PR 互不包含對方的變更。
//! 有 `AFTER` 的組改為接在那組的 branch 後面（內容 = 那組的 branch 加上這組的檔案），
//! PR 的 base 也是那組的 branch（堆疊 PR）；那組合併後 GitHub 會自動把 base 改回原本的 branch。

use crate::*;
use std::path::{Path, PathBuf};

/// 分組目錄（相對於 worktree）。
pub const GROUPS_DIR: &str = "groups";
/// 分組裡記錄「接在哪一組後面」的檔案。
pub const AFTER_FILE: &str = "AFTER";

#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub files: Vec<String>,
    pub commit_msg: Option<String>,
    pub pr_body: Option<String>,
    /// 接在哪一組後面（`AFTER`）。None = 從 merge-base 建立。
    pub after: Option<String>,
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 40
        && !n.starts_with(['.', '-'])
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with(['/', ':', '\\'])
        && !p.contains('\0')
        && !p.split('/').any(|s| s == ".." || s == ".git")
        && !p.starts_with(".agt")
        && !(p.len() >= 2 && p.as_bytes()[1] == b':')
}

fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn groups_root(worktree: &Path) -> PathBuf {
    worktree.join(NOTES_DIR).join(GROUPS_DIR)
}

/// 讀取所有分組（依名稱排序）。沒有 `.agt/groups` 時回傳空。
/// 目錄是連結、名稱或路徑不合規定時拒絕（說明檔由 Agent 控制）。
pub fn load_groups(worktree: &Path) -> Result<Vec<Group>> {
    let root = groups_root(worktree);
    if std::fs::symlink_metadata(&root).is_err() {
        return Ok(vec![]);
    }
    if !real_dir(&worktree.join(NOTES_DIR)) || !real_dir(&root) {
        return Err(GitError::Refused(format!(
            "{} 是連結或不是目錄，不處理",
            root.display()
        )));
    }
    let mut names: Vec<String> = std::fs::read_dir(&root)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = vec![];
    for name in names {
        if !valid_name(&name) || name == "rest" {
            return Err(GitError::Refused(format!(
                "分組名稱「{name}」不合規定（只能用英數、. _ -，不可為 rest）"
            )));
        }
        if !real_dir(&root.join(&name)) {
            return Err(GitError::Refused(format!("分組 {name} 不是一般目錄")));
        }
        let rel = |f: &str| format!("{GROUPS_DIR}/{name}/{f}");
        let files_txt = read_note(worktree, &rel("FILES"))?.unwrap_or_default();
        let mut files = vec![];
        for l in files_txt.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let l = l.trim_start_matches("./").trim_end_matches('/');
            if !valid_path(l) {
                return Err(GitError::Refused(format!(
                    "分組 {name} 的路徑「{l}」不合規定"
                )));
            }
            files.push(l.to_string());
        }
        if files.is_empty() {
            return Err(GitError::Refused(format!("分組 {name} 的 FILES 是空的")));
        }
        let after = read_note(worktree, &rel(AFTER_FILE))?.and_then(|s| {
            s.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        });
        if let Some(a) = &after
            && (!valid_name(a) || a == "rest" || *a == name)
        {
            return Err(GitError::Refused(format!(
                "分組 {name} 的 AFTER「{a}」不合規定（要是另一組的名稱，不可為 rest 或自己）"
            )));
        }
        out.push(Group {
            commit_msg: read_note(worktree, &rel(COMMIT_MSG_FILE))?,
            pr_body: read_note(worktree, &rel(PR_BODY_FILE))?,
            after,
            name,
            files,
        });
    }
    order_by_after(out)
}

/// 依 `AFTER` 排序：被接的組排在前面，其餘維持名稱順序。有循環時拒絕。
/// `AFTER` 指向不存在的組是允許的（那組可能在上次收尾 pr 時已經處理完、說明檔已刪除），
/// 建立 branch 時才要求那組的 branch 存在。
fn order_by_after(groups: Vec<Group>) -> Result<Vec<Group>> {
    let mut pending = groups;
    let mut out: Vec<Group> = vec![];
    while !pending.is_empty() {
        let ready = pending.iter().position(|g| match &g.after {
            None => true,
            Some(a) => !pending.iter().any(|p| &p.name == a),
        });
        match ready {
            Some(i) => out.push(pending.remove(i)),
            None => {
                let names: Vec<&str> = pending.iter().map(|g| g.name.as_str()).collect();
                return Err(GitError::Refused(format!(
                    "分組的 AFTER 形成循環：{}",
                    names.join("、")
                )));
            }
        }
    }
    Ok(out)
}

/// 收尾檢查：參數是組名與這組 branch 的暫時 checkout 目錄。`Err` 是要顯示給使用者的失敗原因。
pub type GroupCheck<'a> = &'a dyn Fn(&str, &Path) -> std::result::Result<(), String>;

/// 在每組 branch 的暫時 checkout（detached worktree）中執行檢查。任何一組失敗就停下並回報，
/// 呼叫端在這之前不會 push 或合併任何一組。暫時目錄放在 Agent 的 worktree 旁邊
/// （`<worktree>.check-<組名>`），沙箱 Agent 能掛載它的 worktree，也就能掛載這裡。
fn check_groups(
    repo: &Path,
    worktree: &Path,
    groups: &[Group],
    branches: &[String],
    check: GroupCheck,
) -> Result<()> {
    let name = worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agt".into());
    let parent = worktree.parent().unwrap_or(worktree);
    for (g, gb) in groups.iter().zip(branches) {
        let dir = parent.join(format!("{name}.check-{}", g.name));
        let dir_s = dir.to_string_lossy().into_owned();
        let cleanup = || {
            let _ = git(repo, &["worktree", "remove", "--force", &dir_s]);
            if std::fs::symlink_metadata(&dir).is_ok() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            let _ = git(repo, &["worktree", "prune"]);
        };
        cleanup(); // 上次中斷留下的
        git(repo, &["worktree", "add", "--detach", "-q", &dir_s, gb])?;
        let r = check(&g.name, &dir);
        cleanup();
        if let Err(e) = r {
            return Err(GitError::Refused(format!(
                "分組 {} 的收尾檢查沒有通過（branch {gb}），沒有 push 或合併任何一組：\n{e}",
                g.name
            )));
        }
    }
    Ok(())
}

/// 分組的 branch 名稱。
pub(crate) fn group_branch(branch: &str, name: &str) -> String {
    format!("{branch}-{name}")
}

pub(crate) fn claims(files: &[String], path: &str) -> bool {
    files.iter().any(|f| {
        path == f
            || path
                .strip_prefix(f.as_str())
                .is_some_and(|r| r.starts_with('/'))
    })
}

/// 以暫存 index 在 `parent` 上疊加 `files` 的目前內容，回傳 tree；沒有疊加任何東西時 tree 與 parent 相同。
fn build_tree(
    repo: &Path,
    wt: &Path,
    gitdir: &Path,
    parent: &str,
    files: &[String],
) -> Result<String> {
    let index = gitdir.join("AGT_GROUP_INDEX");
    let _ = std::fs::remove_file(&index);
    let run_idx = |args: &[&str]| -> Result<String> {
        let mut c = wt_command(repo, wt)?;
        c.env("GIT_INDEX_FILE", &index);
        run(c, args)
    };
    let r = (|| {
        run_idx(&["read-tree", parent])?;
        let mut args = vec!["--literal-pathspecs", "add", "-A", "--"];
        args.extend(files.iter().map(String::as_str));
        run_idx(&args).map_err(|e| match e {
            GitError::Command { stderr, .. } if stderr.contains("did not match any files") => {
                GitError::Refused(format!(
                    "分組裡有不存在、也沒有被刪除的路徑，沒有任何變更可收尾：{stderr}"
                ))
            }
            e => e,
        })?;
        // 巢狀 repo 會被 add 成 gitlink：不接受（原因同 commit_all）
        let staged = run_idx(&["ls-files", "-s", "-z"])?;
        let base = git_wt(repo, wt, &["ls-tree", "-r", "-z", parent])?;
        for l in staged.split('\0').filter(|l| l.starts_with("160000 ")) {
            if !base.contains(l) {
                return Err(GitError::Refused(format!(
                    "分組裡有巢狀的 git repo：{}",
                    l.split_once('\t').map(|x| x.1).unwrap_or(l)
                )));
            }
        }
        run_idx(&["write-tree"])
    })();
    let _ = std::fs::remove_file(&index);
    r
}

/// 建立 / 更新一組的 branch（只疊加新 commit，不改寫歷史）。回傳 (branch 名稱, 是否有新 commit)。
pub(crate) fn make_group_branch(
    repo: &Path,
    wt: &Path,
    gitdir: &Path,
    branch: &str,
    base: &str,
    g: &Group,
    fallback_title: &str,
) -> Result<(String, bool)> {
    let gb = group_branch(branch, &g.name);
    let rev = |b: &str| {
        git_wt(
            repo,
            wt,
            &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")],
        )
        .ok()
    };
    let existing = rev(&gb);
    // 堆疊：接在另一組的 branch 後面
    let start = match &g.after {
        Some(a) => Some(rev(&group_branch(branch, a)).ok_or_else(|| {
            GitError::Refused(format!(
                "分組 {} 要接在 {a} 後面，但找不到 {a} 的 branch（{a} 已經合併或刪除了嗎？）",
                g.name
            ))
        })?),
        None => None,
    };
    // 已有 branch、但接的那組後來又多了 commit：以那組的最新內容為底做一個 merge commit
    // 把它併進來（不改寫歷史、不需要 force push），PR 的差異才不會混進那組的變更。
    let catch_up = match (&existing, &start) {
        (Some(tip), Some(s)) => !is_ancestor(repo, wt, s, tip)?,
        _ => false,
    };
    let (tree_base, parents): (String, Vec<String>) = match (&existing, &start) {
        (Some(tip), Some(s)) if catch_up => (s.clone(), vec![tip.clone(), s.clone()]),
        (Some(tip), _) => (tip.clone(), vec![tip.clone()]),
        (None, Some(s)) => (s.clone(), vec![s.clone()]),
        (None, None) => {
            let mb = git_wt(repo, wt, &["merge-base", base, "HEAD"])?;
            (mb.clone(), vec![mb])
        }
    };
    let tree = build_tree(repo, wt, gitdir, &tree_base, &g.files)?;
    let base_tree = git_wt(repo, wt, &["rev-parse", &format!("{tree_base}^{{tree}}")])?;
    if tree == base_tree && !catch_up {
        if existing.is_none() {
            return Err(GitError::Refused(format!(
                "分組 {} 沒有任何變更（FILES 的路徑沒有差異）",
                g.name
            )));
        }
        return Ok((gb, false));
    }
    let msg = g
        .commit_msg
        .clone()
        .unwrap_or_else(|| format!("{fallback_title}（{}）\n", g.name));
    let msg = match (&g.after, catch_up) {
        (Some(a), true) => format!("Merge {} into {gb}\n", group_branch(branch, a)),
        _ => msg,
    };
    let staged = stage_note(gitdir, "AGT_GROUP_MSG", &msg)?;
    let staged_s = staged.to_string_lossy().into_owned();
    let mut args = vec!["commit-tree", tree.as_str()];
    for p in &parents {
        args.extend(["-p", p.as_str()]);
    }
    args.extend(["-F", staged_s.as_str()]);
    let sha = git_wt(repo, wt, &args);
    let _ = std::fs::remove_file(&staged);
    git_wt(
        repo,
        wt,
        &["update-ref", &format!("refs/heads/{gb}"), &sha?],
    )?;
    Ok((gb, true))
}

/// `a` 是否為 `b` 的祖先（或同一個 commit）。
fn is_ancestor(repo: &Path, wt: &Path, a: &str, b: &str) -> Result<bool> {
    let o = wt_command(repo, wt)?
        .args(["merge-base", "--is-ancestor", a, b])
        .output()?;
    match o.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitError::Refused(format!(
            "git merge-base --is-ancestor 失敗：{}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
    }
}

/// 依分組各自建立 branch、push 並開 PR（已有 PR 時更新）。沒被認領的變更自成 `rest` 組。
/// 回傳每組一行的結果。
pub fn create_group_prs(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
    task: &str,
    groups: Vec<Group>,
    check: Option<GroupCheck>,
) -> Result<String> {
    let gitdir = trusted_gitdir(repo, worktree)?;
    let groups = with_rest(repo, worktree, base, groups)?;
    let remote = pick_remote(repo, worktree)?;
    let gh = has_gh();
    let (pr_base, note) = if gh {
        resolve_pr_base(repo, worktree, &remote, base)?
    } else {
        (base.to_string(), None)
    };
    // 先建好每組的 branch 並檢查，全部通過才開始 push（不會只推出一部分）
    let mut built = vec![];
    for g in &groups {
        let (gb, _) = make_group_branch(repo, worktree, &gitdir, branch, base, g, task)?;
        built.push(gb);
    }
    if let Some(check) = check {
        check_groups(repo, worktree, &groups, &built, check)?;
    }
    let mut lines = vec![];
    for (g, gb) in groups.iter().zip(built) {
        if !gh {
            lines.push(format!(
                "▸ [{}] {}\n    找不到 gh，請手動執行：git -C \"{}\" push -u {remote} {gb}",
                g.name,
                group_title(g, task),
                worktree.display()
            ));
            continue;
        }
        git_wt(repo, worktree, &["push", "-u", &remote, &gb])?;
        let title = group_title(g, task);
        let staged = match &g.pr_body {
            Some(t) => Some(stage_note(&gitdir, "AGT_PR_BODY.md", t)?),
            None => None,
        };
        // 堆疊的組：PR 的 base 是它接的那組的 branch
        let base_for = match &g.after {
            Some(a) => group_branch(branch, a),
            None => pr_base.clone(),
        };
        let r = open_or_update_pr(worktree, &gitdir, &gb, &base_for, &title, staged.as_deref());
        if let Some(p) = &staged {
            let _ = std::fs::remove_file(p);
        }
        let stacked = g
            .after
            .as_ref()
            .map(|a| format!("（接在 [{a}] 後面，PR base {base_for}）"))
            .unwrap_or_default();
        lines.push(format!(
            "▸ [{}] {title}\n    branch {gb}{stacked}\n    {}",
            g.name,
            r?.replace('\n', "\n    ")
        ));
        if g.name != "rest" {
            let dir = groups_root(worktree).join(&g.name);
            if real_dir(&dir) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
    let mut out = format!(
        "分成 {n} 組：{n} 個 commit、{n} 個 PR\n\n{}",
        lines.join("\n\n"),
        n = groups.len()
    );
    if let Some(n) = note {
        out.push_str(&format!("\n\n{n}"));
    }
    Ok(out)
}

fn group_title(g: &Group, task: &str) -> String {
    g.commit_msg
        .as_deref()
        .and_then(subject_line)
        .unwrap_or_else(|| format!("{task}（{}）", g.name))
}

/// 加上「未被認領的變更」自成的 `rest` 組（已 commit 的、未 commit 的、未追蹤的都算）。
fn with_rest(repo: &Path, wt: &Path, base: &str, mut groups: Vec<Group>) -> Result<Vec<Group>> {
    let mb = git_wt(repo, wt, &["merge-base", base, "HEAD"])?;
    let mut changed: Vec<String> = vec![];
    for args in [
        &["diff", "--name-only", "-z", &mb][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        let out = git_wt(repo, wt, args)?;
        changed.extend(
            out.split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        );
    }
    // 說明檔目錄不是變更內容（未追蹤時不會被 info/exclude 擋住）
    changed.retain(|p| p != NOTES_DIR && !p.starts_with(".agt/"));
    changed.sort();
    changed.dedup();
    let rest: Vec<String> = changed
        .into_iter()
        .filter(|p| !groups.iter().any(|g| claims(&g.files, p)))
        .collect();
    if !rest.is_empty() {
        groups.push(Group {
            name: "rest".into(),
            files: rest,
            commit_msg: None,
            pr_body: None,
            after: None,
        });
    }
    Ok(groups)
}

/// 分組 merge：每組先建成獨立 branch（一個 commit），再依序在主工作目錄合進 base。
/// `Merge` 每組一個 merge commit、`Squash` 每組一個 commit、`Rebase` 把每組的 commit cherry-pick 上去
/// （線性歷史）。任何一組衝突就停下（已合進去的組保留），不自動解決。
#[allow(clippy::too_many_arguments)]
pub fn merge_groups(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
    task: &str,
    strategy: Strategy,
    groups: Vec<Group>,
    check: Option<GroupCheck>,
) -> Result<String> {
    let gitdir = trusted_gitdir(repo, worktree)?;
    if is_dirty(repo, false)? {
        return Err(GitError::Dirty(format!(
            "主工作目錄 {} 有未 commit 的變更，拒絕 merge",
            repo.display()
        )));
    }
    if !branch_exists(repo, base) {
        return Err(GitError::Refused(format!(
            "base {base} 不是本地 branch，無法 merge；請改用 pr，或手動合併"
        )));
    }
    let current = git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    if current.as_deref() != Some(base) {
        return Err(GitError::Refused(format!(
            "主工作目錄 {} 目前在 {}，不是 {base}；請先切換到 {base} 再 merge",
            repo.display(),
            current.as_deref().unwrap_or("detached HEAD")
        )));
    }
    let groups = with_rest(repo, worktree, base, groups)?;
    let mb = git_wt(repo, worktree, &["merge-base", base, "HEAD"])?;
    // 先把每組的 branch 都建好：有問題（例如空的分組、巢狀 repo）時在動到 base 之前就停下。
    // 同時記下每組的 commit 範圍（起點 = merge-base，或它接的那組的 branch）：
    // 合併途中會刪掉已合併的分組 branch，之後的組不能再用名稱找起點。
    let mut branches = vec![];
    for g in &groups {
        let (gb, _) = make_group_branch(repo, worktree, &gitdir, branch, base, g, task)?;
        let tip = git_wt(repo, worktree, &["rev-parse", &format!("refs/heads/{gb}")])?;
        let start = match &g.after {
            Some(a) => git_wt(
                repo,
                worktree,
                &[
                    "rev-parse",
                    &format!("refs/heads/{}", group_branch(branch, a)),
                ],
            )?,
            None => mb.clone(),
        };
        branches.push((gb, start, tip));
    }
    if let Some(check) = check {
        let names: Vec<String> = branches.iter().map(|b| b.0.clone()).collect();
        check_groups(repo, worktree, &groups, &names, check)?;
    }
    let mut lines = vec![];
    for (g, (gb, start, tip)) in groups.iter().zip(&branches) {
        let title = group_title(g, task);
        let conflict = |what: &str| {
            let files = git(repo, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            if files.is_empty() {
                None
            } else {
                Some(GitError::Conflict(format!(
                    "分組 {} 的 {what} 發生衝突，已停在主工作目錄 {} 等待處理：\n{files}\n（前面已完成的組：{}）",
                    g.name,
                    repo.display(),
                    lines.len()
                )))
            }
        };
        match strategy {
            Strategy::Merge => {
                let msg = format!("Merge {gb}: {title}");
                if let Err(e) = git(repo, &["merge", "--no-ff", "-m", &msg, gb]) {
                    return Err(conflict("merge").unwrap_or(e));
                }
            }
            Strategy::Squash => {
                if let Err(e) = git(repo, &["merge", "--squash", gb]) {
                    return Err(conflict("squash").unwrap_or(e));
                }
                if is_dirty(repo, false)? {
                    let text = g.commit_msg.clone().unwrap_or_else(|| format!("{title}\n"));
                    let staged = stage_note(&gitdir, "AGT_GROUP_MSG", &text)?;
                    let r = git(
                        repo,
                        &[
                            "commit",
                            "--cleanup=whitespace",
                            "-F",
                            &staged.to_string_lossy(),
                        ],
                    );
                    let _ = std::fs::remove_file(&staged);
                    r?;
                }
            }
            Strategy::Rebase => {
                // 這組自己的 commit（不含它接的那組、也不含追上那組時的 merge commit），依序接上去。
                // 分組 branch 在之前收尾 pr 時可能已經追加過好幾個 commit，只取最後一個會漏掉內容。
                let list = git_wt(
                    repo,
                    worktree,
                    &[
                        "rev-list",
                        "--reverse",
                        "--no-merges",
                        &format!("{start}..{tip}"),
                    ],
                )?;
                let commits: Vec<&str> = list.split_whitespace().collect();
                if !commits.is_empty() {
                    let mut args = vec!["cherry-pick", "--allow-empty"];
                    args.extend(commits);
                    if let Err(e) = git(repo, &args) {
                        return Err(conflict("cherry-pick").unwrap_or(e));
                    }
                }
            }
        }
        // 合進去了：刪掉暫時的分組 branch 與說明
        if gb.starts_with(BRANCH_PREFIX) {
            let _ = git(repo, &["branch", "-D", gb]);
        }
        if g.name != "rest" {
            let dir = groups_root(worktree).join(&g.name);
            if real_dir(&dir) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        lines.push(format!("▸ [{}] {title}\n    已合併進 {base}", g.name));
    }
    Ok(format!(
        "分成 {n} 組：{n} 個 commit 合併進 {base}\n\n{}",
        lines.join("\n\n"),
        n = groups.len()
    ))
}

/// 說明檔前幾個非空白行，當成預覽。
fn preview_lines(text: &str, max: usize) -> Vec<String> {
    let all: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut out: Vec<String> = all.iter().take(max).map(|l| l.to_string()).collect();
    if all.len() > max {
        out.push("…".into());
    }
    out
}

/// 收尾前的預覽：預計會產生幾個 commit / PR，以及各自的標題與說明。
/// `merge` 為 true 時是合併進 base（不開 PR，`strategy_note` 說明 merge 策略）；否則是 push 並開 PR。
/// 有 `.agt/groups` 時每組一個 commit（pr 另外各一個 PR，含未被認領的 `rest`）；否則是單一 commit。
/// 只讀取，不改動任何東西。
pub fn finish_preview(
    repo: &Path,
    worktree: &Path,
    base: &str,
    task: &str,
    merge: bool,
    strategy: &str,
    auto_commit: bool,
) -> Result<String> {
    let groups = load_groups(worktree)?;
    // 提醒放最前面：視窗高度不夠時，後面的內容會被截掉
    let mut out = finish_warnings(repo, worktree, base, !groups.is_empty())?;
    if groups.is_empty() {
        let msg = read_note(worktree, COMMIT_MSG_FILE)?;
        let title = msg
            .as_deref()
            .and_then(subject_line)
            .unwrap_or_else(|| task.to_string());
        if merge {
            // 分支上已有的 commit，加上收尾時會自動 commit 的未提交變更
            let ahead: usize = git_wt(
                repo,
                worktree,
                &["rev-list", "--count", &format!("{base}..HEAD")],
            )?
            .trim()
            .parse()
            .unwrap_or(0);
            let pending = usize::from(auto_commit && is_dirty_wt(repo, worktree, true)?);
            let n = ahead + pending;
            let (count, note) = match strategy {
                "squash" => (1, "squash 成 1 個 commit".to_string()),
                "rebase" => (n, format!("rebase，{n} 個 commit 接在 base 後面")),
                "merge" => (n + 1, format!("merge，{n} 個 commit 加 1 個 merge commit")),
                _ => (n, format!("策略稍後選擇，分支有 {n} 個 commit")),
            };
            out.push(format!(
                "合併進 {base}：預計 {count} 個 commit（{note}）；不開 PR"
            ));
        } else {
            out.push("1 個 commit、1 個 PR".to_string());
        }
        out.push(format!("▸ {title}"));
        if let Some(m) = &msg {
            // 第一個非空白行是標題，其餘是內文
            let body = m.trim_start().split_once('\n').map_or("", |x| x.1);
            out.extend(
                preview_lines(body, 4)
                    .into_iter()
                    .map(|l| format!("    {l}")),
            );
        }
        if !merge && let Some(b) = read_note(worktree, PR_BODY_FILE)? {
            out.push("  PR 說明：".into());
            out.extend(preview_lines(&b, 4).into_iter().map(|l| format!("    {l}")));
        }
        return Ok(out.join("\n"));
    }
    let groups = with_rest(repo, worktree, base, groups)?;
    let n = groups.len();
    if merge {
        let note = match strategy {
            "merge" => "merge：每組一個 merge commit",
            "squash" => "squash：每組壓成一個 commit",
            "rebase" => "rebase：每組一個 commit 接在 base 後面",
            _ => "合併策略稍後選擇",
        };
        out.push(format!(
            "分成 {n} 組依序合進 {base}：{n} 個 commit（{note}）、不開 PR"
        ));
    } else {
        out.push(format!("分成 {n} 組：{n} 個 commit、{n} 個 PR"));
    }
    for g in &groups {
        let stacked = g
            .after
            .as_ref()
            .map(|a| format!("，接在 [{a}] 後面"))
            .unwrap_or_default();
        out.push(format!(
            "▸ [{}] {}（{} 個路徑{stacked}）",
            g.name,
            group_title(g, task),
            g.files.len()
        ));
        if !merge && let Some(b) = &g.pr_body {
            out.extend(preview_lines(b, 2).into_iter().map(|l| format!("    {l}")));
        }
    }
    Ok(out.join("\n"))
}
