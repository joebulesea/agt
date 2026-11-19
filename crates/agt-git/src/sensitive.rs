//! 收尾前要求確認的敏感路徑（會被自動執行的設定檔等）。

use crate::*;
use std::path::Path;

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
