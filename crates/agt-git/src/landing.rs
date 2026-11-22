//! 收尾開出的 PR 合併之後，確認內容真的進了 base（例如 `main`）。
//!
//! 不能用 `merge-base --is-ancestor <推出去的 commit>` 判斷：GitHub 的 squash / rebase 合併會產生新的
//! commit，原本的 SHA 不會出現在 base。這裡改問 `gh pr view` 的合併資訊（狀態、PR 現在的 base、
//! 合併 commit），再確認合併 commit 在 `<remote>/<base>` 裡。堆疊 PR 合進前一組的 branch 而不是
//! `main` 時，會回報「進錯 branch」。
//!
//! `gh` 的輸出視為不可信：只取三個欄位，並驗證格式（狀態是固定字串、branch 名稱字元合法、SHA 是十六進位）。

use crate::*;
use std::path::Path;
use std::time::Duration;

/// `gh pr view` 與 `git fetch` 各自最多等多久（背景輪詢，連不上網路時不能卡住）。
const NET_TIMEOUT: Duration = Duration::from_secs(30);

/// 收尾時開出的一個 PR，之後要追蹤它有沒有進 base。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrTarget {
    /// 組名；沒有分組時是 `pr`。
    pub name: String,
    /// PR 的 head branch。
    pub branch: String,
    /// 內容最後應該進到的 branch（remote 上的預設 branch 或分頁的 base，不是堆疊的前一組）。
    pub base: String,
}

/// 追蹤一個 PR 的結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// PR 還開著（含堆疊在前一組後面、還在等前一個合併）。
    Open,
    /// 已合併，而且合併 commit 在 `<remote>/<base>` 裡。
    Landed,
    /// 已合併，但 PR 當時的 base 不是預期的 branch（內容只進了那個 branch）。
    WrongBase(String),
    /// PR 被關閉，沒有合併。
    Closed,
    /// 已合併到預期的 base，但合併 commit 不在 `<remote>/<base>`（例如被 revert 或改寫歷史）。
    Missing,
}

/// `gh pr view` 的三個欄位：狀態、PR 的 base、合併 commit（沒有時是空字串）。
pub(crate) fn parse_pr_view(line: &str) -> Option<(&str, &str, &str)> {
    let mut it = line.split(' ');
    let state = it.next()?;
    let base = it.next()?;
    let oid = it.next().unwrap_or("");
    if it.next().is_some() {
        return None;
    }
    let state_ok = matches!(state, "OPEN" | "MERGED" | "CLOSED");
    let base_ok = !base.is_empty()
        && base.len() <= 200
        && !base.starts_with(['-', '/'])
        && !base.contains("..")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
    let oid_ok = oid.is_empty()
        || ((oid.len() == 40 || oid.len() == 64) && oid.chars().all(|c| c.is_ascii_hexdigit()));
    (state_ok && base_ok && oid_ok).then_some((state, base, oid))
}

/// 查 `branch` 這個 PR 現在的狀況。`is_in_base` 回答「合併 commit 在不在 base 裡」（要碰 git，所以由呼叫方給）。
pub(crate) fn classify(
    line: &str,
    expected_base: &str,
    is_in_base: impl FnOnce(&str) -> Result<bool>,
) -> Result<Landing> {
    let Some((state, base, oid)) = parse_pr_view(line) else {
        return Err(GitError::Refused(
            "gh pr view 的輸出格式不符，略過這次檢查".into(),
        ));
    };
    Ok(match state {
        "OPEN" => Landing::Open,
        "CLOSED" => Landing::Closed,
        _ if base != expected_base => Landing::WrongBase(base.to_string()),
        _ if oid.is_empty() => Landing::Missing,
        _ => {
            if is_in_base(oid)? {
                Landing::Landed
            } else {
                Landing::Missing
            }
        }
    })
}

/// 查 PR 有沒有進 `target.base`。在使用者自己的主 repo 執行 `gh` 與 `git fetch`（不經過分頁的 worktree，
/// 分頁關閉、worktree 刪除後也能查），不讀任何 Agent 寫的檔案。
/// 網路或 `gh` 失敗時回傳錯誤（呼叫方稍後重試）。
pub fn check_landing(repo: &Path, target: &PrTarget) -> Result<Landing> {
    let line = gh_run_repo(
        repo,
        &[
            "pr",
            "view",
            &target.branch,
            "--json",
            "state,baseRefName,mergeCommit",
            "--jq",
            ".state + \" \" + .baseRefName + \" \" + (.mergeCommit.oid // \"\")",
        ],
        NET_TIMEOUT,
    )?;
    classify(&line, &target.base, |oid| {
        let remote = pick_remote_repo(repo)?;
        fetch_base(repo, &remote, &target.base)?;
        // fetch 之後 repo 裡仍沒有這個 commit：它不在 base 的歷史裡（不是錯誤，不必重試）
        let known = run_raw(repo, &["cat-file", "-e", &format!("{oid}^{{commit}}")])?;
        if !known.status.success() {
            return Ok(false);
        }
        let o = run_raw(repo, &["merge-base", "--is-ancestor", oid, "FETCH_HEAD"])?;
        match o.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Refused(format!(
                "git merge-base --is-ancestor 失敗：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))),
        }
    })
}

fn fetch_base(repo: &Path, remote: &str, base: &str) -> Result<()> {
    let mut c = crate::exec::command(repo);
    c.args(["fetch", "--no-tags", "--quiet", remote, base]);
    let o = crate::exec::output_timeout(c, NET_TIMEOUT)?;
    if o.status.success() {
        Ok(())
    } else {
        Err(GitError::Command {
            args: "fetch".into(),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        })
    }
}

fn pick_remote_repo(repo: &Path) -> Result<String> {
    let remote = git(repo, &["remote"])?;
    remote
        .lines()
        .find(|r| *r == "origin")
        .or(remote.lines().next())
        .map(str::to_string)
        .ok_or_else(|| GitError::Refused("這個 repo 沒有設定 remote".into()))
}

/// 一個 PR 的 CI 結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    /// 沒有任何檢查。
    None,
    /// 還有檢查在執行或排隊。
    Pending,
    /// 全部通過（或略過）。
    Success,
    /// 有檢查失敗。
    Failure,
}

/// PR 的 CI 狀態與失敗的檢查名稱（以逗號分隔，已限制長度；沒有失敗時是空字串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiStatus {
    pub ci: Ci,
    pub failed: String,
}

/// `gh pr view --json statusCheckRollup --jq` 用的運算式。輸出 `<none|pending|success|failure>\t<失敗的檢查名稱>`。
/// 檢查有兩種：CheckRun（`status` / `conclusion` / `name`）與 StatusContext（`state` / `context`）。
const CI_JQ: &str = r#"(.statusCheckRollup // []) as $c
| ($c | map(select((.conclusion // .state // "") as $r | ["FAILURE","TIMED_OUT","CANCELLED","ERROR","STARTUP_FAILURE","ACTION_REQUIRED"] | any(. == $r)))) as $bad
| ($c | map(select(((.status // "COMPLETED") != "COMPLETED") or ((.state // "") as $s | ["PENDING","EXPECTED"] | any(. == $s))))) as $pend
| (if ($c | length) == 0 then "none" elif ($bad | length) > 0 then "failure" elif ($pend | length) > 0 then "pending" else "success" end)
  + "\t" + ($bad | map(.name // .context // "?") | join(", "))"#;

/// 失敗的檢查名稱最多保留幾個字元。
const CI_FAILED_MAX: usize = 120;

/// 解析 [`CI_JQ`] 的輸出；格式不符時回傳 None。名稱來自 PR 的檢查（不可信）：去掉控制字元並限制長度。
pub(crate) fn parse_ci(line: &str) -> Option<CiStatus> {
    let (head, failed) = line.split_once('\t').unwrap_or((line, ""));
    let ci = match head.trim() {
        "none" => Ci::None,
        "pending" => Ci::Pending,
        "success" => Ci::Success,
        "failure" => Ci::Failure,
        _ => return None,
    };
    let failed: String = failed
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(CI_FAILED_MAX)
        .collect();
    Some(CiStatus { ci, failed })
}

/// 查 PR 的 CI 狀態。和 [`check_landing`] 分開查：這個查詢失敗（例如 `gh` 版本不支援）不影響「有沒有進 base」。
pub fn check_ci(repo: &Path, target: &PrTarget) -> Result<CiStatus> {
    let line = gh_run_repo(
        repo,
        &[
            "pr",
            "view",
            &target.branch,
            "--json",
            "statusCheckRollup",
            "--jq",
            CI_JQ,
        ],
        NET_TIMEOUT,
    )?;
    parse_ci(&line).ok_or_else(|| GitError::Refused("gh 的 CI 輸出格式不符，略過這次檢查".into()))
}
