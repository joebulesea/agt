//! 給 Agent 看的收尾規則（`.agt/README.md`），以及收尾預覽前的檢查（不認得的說明檔、改動很大卻沒有分組）。

use crate::*;
use std::path::Path;

/// 收尾規則寫在 `.agt/` 底下的檔名。
pub const GUIDE_FILE: &str = "README.md";

/// 收尾規則的內容（建立分頁時寫入 `.agt/README.md`）。
pub const AGENT_GUIDE: &str = include_str!("agent_guide.md");

/// `.agt/` 底下 agt 認得的項目，其餘的會在收尾預覽時提醒。
/// 後四個是 Claude Code 的 status line 用的（用量資料與沙箱裡的腳本，見 agt-agents 的 `claude.rs`）。
const KNOWN_NOTES: [&str; 8] = [
    GUIDE_FILE,
    COMMIT_MSG_FILE,
    PR_BODY_FILE,
    GROUPS_DIR,
    "usage.json",
    "usage.json.tmp",
    "statusline.sh",
    "statusline.js",
];

/// 沒有分組時，改動超過這麼多行（新增 + 刪除，不含未追蹤的檔案）就提醒。
pub const LARGE_CHANGE_LINES: usize = 1500;
/// 沒有分組時，改動超過這麼多個檔案（含未追蹤的）就提醒。
pub const LARGE_CHANGE_FILES: usize = 40;

/// 把收尾規則寫到 `<worktree>/.agt/README.md`（已存在就覆寫）。
/// `.agt` 是連結或不是目錄時拒絕；舊檔先刪除再以 `create_new` 建立，不會跟隨連結寫到別的地方。
pub fn write_guide(worktree: &Path) -> Result<()> {
    use std::io::Write;
    let dir = worktree.join(NOTES_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(GitError::Refused(format!(
                "{} 是連結或不是目錄，不寫入收尾規則",
                dir.display()
            )));
        }
        Ok(_) => {}
        Err(_) => std::fs::create_dir(&dir)?,
    }
    let path = dir.join(GUIDE_FILE);
    if std::fs::symlink_metadata(&path).is_ok() {
        std::fs::remove_file(&path)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(AGENT_GUIDE.as_bytes())?;
    Ok(())
}

/// 收尾預覽要顯示的提醒（每則一行，以 `⚠` 開頭）。只讀取，不改動任何東西。
/// - `.agt/` 裡有 agt 不認得的項目（例如 Agent 自己寫的 `split/` 拆分計畫，收尾時不會被用到）
/// - 沒有分組，但改動很大（可能混了好幾個主題，會全部放進同一個 commit / PR）
pub fn finish_warnings(
    repo: &Path,
    worktree: &Path,
    base: &str,
    grouped: bool,
) -> Result<Vec<String>> {
    let mut out = vec![];
    let unknown = unknown_notes(worktree);
    if !unknown.is_empty() {
        out.push(format!(
            "⚠ .agt/ 裡有 agt 不認得的項目：{}（不會被用到；分組要放在 .agt/groups/，規則見 .agt/{GUIDE_FILE}）",
            unknown.join("、")
        ));
    }
    if !grouped {
        let (files, lines) = change_size(repo, worktree, base)?;
        if files > LARGE_CHANGE_FILES || lines > LARGE_CHANGE_LINES {
            out.push(format!(
                "⚠ 改動很大（{files} 個檔案、{lines} 行）卻沒有分組，會全部放進同一個 commit / PR；\
                 若包含多個主題，請 Agent 依 .agt/{GUIDE_FILE} 分組後再收尾"
            ));
        }
    }
    Ok(out)
}

/// `.agt/` 底下 agt 不認得的項目名稱（目錄加上 `/`），依名稱排序，最多列 5 個。
fn unknown_notes(worktree: &Path) -> Vec<String> {
    let dir = worktree.join(NOTES_DIR);
    let Ok(m) = std::fs::symlink_metadata(&dir) else {
        return vec![];
    };
    if m.file_type().is_symlink() || !m.is_dir() {
        return vec![]; // 讀取說明檔時會另外拒絕
    }
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if KNOWN_NOTES.contains(&name.as_str()) {
                return None;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    names.sort();
    if names.len() > 5 {
        let more = names.len() - 5;
        names.truncate(5);
        names.push(format!("另外 {more} 個"));
    }
    names
}

/// 相對 merge-base 的改動量：(檔案數，含未追蹤的；新增 + 刪除的行數，只算追蹤中的文字檔)。
fn change_size(repo: &Path, worktree: &Path, base: &str) -> Result<(usize, usize)> {
    let mb = git_wt(repo, worktree, &["merge-base", base, "HEAD"])?;
    let numstat = git_wt(repo, worktree, &["diff", "--numstat", "-z", mb.trim()])?;
    let (files, lines) = parse_numstat(&numstat);
    let untracked = git_wt(
        repo,
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let untracked = untracked
        .split('\0')
        .filter(|p| !p.is_empty() && *p != NOTES_DIR && !p.starts_with(".agt/"))
        .count();
    Ok((files + untracked, lines))
}

/// 解析 `git diff --numstat -z`：每筆是 `新增\t刪除\t路徑\0`；改名時路徑留空，
/// 後面再接 `舊路徑\0新路徑\0`。二進位檔的行數是 `-`，不計行數。
fn parse_numstat(s: &str) -> (usize, usize) {
    let mut parts = s.split('\0');
    let (mut files, mut lines) = (0, 0);
    while let Some(rec) = parts.next() {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if path.is_empty() {
            // 改名：跳過舊路徑與新路徑
            parts.next();
            parts.next();
        } else if path == NOTES_DIR || path.starts_with(".agt/") {
            continue;
        }
        files += 1;
        lines += a.parse::<usize>().unwrap_or(0) + d.parse::<usize>().unwrap_or(0);
    }
    (files, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numstat_with_renames_and_binaries() {
        let s = "10\t2\ta.rs\0-\t-\tlogo.png\x003\t1\t\0old.rs\0new.rs\0";
        assert_eq!(parse_numstat(s), (3, 16));
        assert_eq!(parse_numstat(""), (0, 0));
        // 說明檔目錄不算
        assert_eq!(parse_numstat("5\t0\t.agt/COMMIT_MSG\0"), (0, 0));
    }

    #[test]
    fn guide_is_written_and_overwritten() {
        let d = tempfile::tempdir().unwrap();
        write_guide(d.path()).unwrap();
        let p = d.path().join(NOTES_DIR).join(GUIDE_FILE);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), AGENT_GUIDE);
        std::fs::write(&p, "被改掉了").unwrap();
        write_guide(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), AGENT_GUIDE);
    }

    #[test]
    fn guide_mentions_every_known_note() {
        for n in [COMMIT_MSG_FILE, PR_BODY_FILE, GROUPS_DIR, "FILES", "rest"] {
            assert!(AGENT_GUIDE.contains(n), "{n}");
        }
    }

    #[test]
    fn guide_refuses_when_notes_dir_is_a_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(NOTES_DIR), "x").unwrap();
        assert!(write_guide(d.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn guide_does_not_follow_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret");
        std::fs::write(&target, "不能被覆寫").unwrap();
        // .agt 是連結：拒絕
        std::os::unix::fs::symlink(outside.path(), d.path().join(NOTES_DIR)).unwrap();
        assert!(write_guide(d.path()).is_err());
        std::fs::remove_file(d.path().join(NOTES_DIR)).unwrap();
        // README.md 是連結：刪掉連結本身再建立，不會寫到連結指向的檔案
        std::fs::create_dir(d.path().join(NOTES_DIR)).unwrap();
        let p = d.path().join(NOTES_DIR).join(GUIDE_FILE);
        std::os::unix::fs::symlink(&target, &p).unwrap();
        write_guide(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "不能被覆寫");
        assert!(
            !std::fs::symlink_metadata(&p)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn unknown_notes_lists_extra_entries() {
        let d = tempfile::tempdir().unwrap();
        assert!(unknown_notes(d.path()).is_empty());
        let n = d.path().join(NOTES_DIR);
        std::fs::create_dir_all(n.join("groups")).unwrap();
        std::fs::create_dir_all(n.join("split/01")).unwrap();
        for f in [GUIDE_FILE, COMMIT_MSG_FILE, PR_BODY_FILE, "PLAN.md"] {
            std::fs::write(n.join(f), "x").unwrap();
        }
        assert_eq!(unknown_notes(d.path()), ["PLAN.md", "split/"]);
        for i in 0..6 {
            std::fs::write(n.join(format!("x{i}")), "x").unwrap();
        }
        let u = unknown_notes(d.path());
        assert_eq!(u.len(), 6);
        assert_eq!(u[5], "另外 3 個");
    }
}
