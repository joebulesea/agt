use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn slug_rules() {
    assert_eq!(slugify("Login Page!!"), "login-page");
    assert_eq!(slugify("登入功能"), "");
    assert_eq!(slugify("修 bug 123"), "bug-123");
    assert!(slugify(&"a".repeat(80)).len() <= 32);
    assert_eq!(
        branch_name("claude", "登入功能", "k3x9a2"),
        "agt/claude/k3x9a2"
    );
    assert_eq!(
        branch_name("claude", "login", "k3x9a2"),
        "agt/claude/login-k3x9a2"
    );
}

#[test]
fn ids() {
    assert_eq!(short_id(0), "000000");
    assert_eq!(short_id(35), "00000z");
    assert_eq!(
        repo_dir_name(Path::new("/home/u/proj")).len(),
        "proj-".len() + 4
    );
}

#[test]
fn parse_porcelain() {
    let s = "worktree /a\nHEAD 1\nbranch refs/heads/main\n\nworktree /b\nHEAD 2\nbranch refs/heads/agt/x\n";
    let l = parse_worktree_list(s);
    assert_eq!(l.len(), 2);
    assert_eq!(l[1].branch.as_deref(), Some("agt/x"));
}

fn init_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]).unwrap();
    git(p, &["config", "user.email", "t@t"]).unwrap();
    git(p, &["config", "user.name", "t"]).unwrap();
    // Windows 的 git 常預設 core.autocrlf=true，checkout 會把 \n 轉成 \r\n，讓內容比對失敗
    git(p, &["config", "core.autocrlf", "false"]).unwrap();
    std::fs::write(p.join("a.txt"), "a\n").unwrap();
    git(p, &["add", "."]).unwrap();
    git(p, &["commit", "-q", "-m", "init"]).unwrap();
    d
}

#[test]
fn worktree_lifecycle_and_merge() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w1");
    add_worktree(root, &wt, "agt/test/x-000001", "main").unwrap();
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    assert!(status(root, &wt, "main").unwrap().dirty);
    git(&wt, &["add", "."]).unwrap();
    git(&wt, &["commit", "-q", "-m", "b"]).unwrap();
    let st = status(root, &wt, "main").unwrap();
    assert_eq!((st.ahead, st.behind, st.dirty), (1, 0, false));
    assert!(has_unpushed(root, &wt, "main").unwrap());
    assert!(diff(root, &wt, "main").unwrap().contains("b.txt"));

    merge_into_base(root, &wt, "agt/test/x-000001", "main", Strategy::Merge, "t").unwrap();
    assert!(root.join("b.txt").exists());
    assert!(!has_unpushed(root, &wt, "main").unwrap());

    assert!(delete_branch(root, "main").is_err());
    remove_worktree(root, &wt, false).unwrap();
    delete_branch(root, "agt/test/x-000001").unwrap();
    assert!(!branch_exists(root, "agt/test/x-000001"));
}

#[test]
fn commit_all_then_merge() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w2");
    add_worktree(root, &wt, "agt/test/y-000002", "main").unwrap();
    assert!(commit_all(root, &wt, "沒有變更").unwrap().is_none());
    std::fs::write(wt.join("new.txt"), "n\n").unwrap();
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    assert!(
        merge_into_base(
            root,
            &wt,
            "agt/test/y-000002",
            "main",
            Strategy::Squash,
            "t"
        )
        .is_err()
    );
    assert_eq!(
        commit_all(root, &wt, "沙箱的修改").unwrap().as_deref(),
        Some("沙箱的修改")
    );
    assert!(!is_dirty_wt(root, &wt, true).unwrap());
    merge_into_base(
        root,
        &wt,
        "agt/test/y-000002",
        "main",
        Strategy::Squash,
        "沙箱的修改",
    )
    .unwrap();
    assert!(root.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "changed\n"
    );
}

#[test]
fn commit_all_uses_agent_commit_msg_and_never_commits_notes() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w-notes");
    add_worktree(root, &wt, "agt/test/n-000003", "main").unwrap();
    std::fs::write(wt.join("x.txt"), "x\n").unwrap();
    let notes = wt.join(NOTES_DIR);
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join(COMMIT_MSG_FILE),
        "\u{feff}新增 x\r\n\r\n# 不是註解，要保留\r\n",
    )
    .unwrap();
    std::fs::write(notes.join(PR_BODY_FILE), "## 說明\n").unwrap();

    let msg = commit_all(root, &wt, "任務名稱").unwrap().unwrap();
    assert_eq!(subject_line(&msg).as_deref(), Some("新增 x"));
    let log = git(root, &["log", "-1", "--format=%B", "agt/test/n-000003"]).unwrap();
    assert_eq!(log.trim(), "新增 x\n\n# 不是註解，要保留");
    // .agt/ 沒有被 commit；COMMIT_MSG 用完刪除，PR_BODY 留給建立 PR
    let files = git(root, &["ls-tree", "-r", "--name-only", "agt/test/n-000003"]).unwrap();
    assert!(!files.contains(".agt"), "{files}");
    assert!(!notes.join(COMMIT_MSG_FILE).exists());
    assert!(notes.join(PR_BODY_FILE).exists());
    assert!(!is_dirty_wt(root, &wt, true).unwrap());

    // 只改了說明檔：沒有東西要 commit
    std::fs::write(notes.join(COMMIT_MSG_FILE), "x\n").unwrap();
    assert!(commit_all(root, &wt, "任務名稱").unwrap().is_none());

    // 說明檔只有空白：改用任務名稱
    std::fs::write(wt.join("y.txt"), "y\n").unwrap();
    std::fs::write(notes.join(COMMIT_MSG_FILE), " \n\n").unwrap();
    assert_eq!(
        commit_all(root, &wt, "任務名稱").unwrap().as_deref(),
        Some("任務名稱")
    );
}

#[test]
fn pr_base_falls_back_to_remote_default_only_when_safe() {
    let repo = init_repo();
    let root = repo.path();
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().join("origin.git");
    git(
        root,
        &["clone", "-q", "--bare", ".", &remote.to_string_lossy()],
    )
    .unwrap();
    git(
        root,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    )
    .unwrap();
    // 只在本機的 feat，內容都已在 origin/main 裡
    git(root, &["branch", "feat"]).unwrap();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w-base");
    add_worktree(root, &wt, "agt/test/b-000004", "feat").unwrap();

    let (b, note) = resolve_pr_base(root, &wt, "origin", "main").unwrap();
    assert_eq!((b.as_str(), note), ("main", None));
    let (b, note) = resolve_pr_base(root, &wt, "origin", "feat").unwrap();
    assert_eq!(b, "main");
    assert!(note.unwrap().contains("feat"));

    // feat 多了 origin 沒有的 commit：拒絕，不能讓 PR 混進去
    git(root, &["checkout", "-q", "feat"]).unwrap();
    std::fs::write(root.join("f.txt"), "f\n").unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-q", "-m", "local only"]).unwrap();
    assert!(resolve_pr_base(root, &wt, "origin", "feat").is_err());
}

#[test]
fn read_note_refuses_bad_files() {
    let d = tempfile::tempdir().unwrap();
    let wt = d.path();
    assert!(read_note(wt, COMMIT_MSG_FILE).unwrap().is_none());
    let notes = wt.join(NOTES_DIR);
    std::fs::create_dir_all(&notes).unwrap();
    assert!(read_note(wt, COMMIT_MSG_FILE).unwrap().is_none());

    std::fs::write(notes.join(COMMIT_MSG_FILE), vec![b'a'; 64 * 1024 + 1]).unwrap();
    assert!(read_note(wt, COMMIT_MSG_FILE).is_err());
    std::fs::write(notes.join(COMMIT_MSG_FILE), [0xff, 0xfe, 0x00]).unwrap();
    assert!(read_note(wt, COMMIT_MSG_FILE).is_err());
    std::fs::remove_file(notes.join(COMMIT_MSG_FILE)).unwrap();
    std::fs::create_dir(notes.join(COMMIT_MSG_FILE)).unwrap();
    assert!(read_note(wt, COMMIT_MSG_FILE).is_err());

    // 連結（可能指向主機上的金鑰）：拒絕。Windows 沒有建立連結的權限時略過
    let secret = d.path().join("secret.txt");
    std::fs::write(&secret, "TOKEN\n").unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&secret, notes.join(PR_BODY_FILE)).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&secret, notes.join(PR_BODY_FILE)).is_ok();
    if linked {
        assert!(read_note(wt, PR_BODY_FILE).is_err());
    }
}

#[test]
fn merge_refuses_dirty_main_and_stops_on_conflict() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/c-000002", "main").unwrap();
    std::fs::write(wt.join("a.txt"), "from agent\n").unwrap();
    git(&wt, &["commit", "-qam", "agent"]).unwrap();

    std::fs::write(root.join("a.txt"), "dirty\n").unwrap();
    assert!(matches!(
        merge_into_base(root, &wt, "agt/t/c-000002", "main", Strategy::Merge, "t"),
        Err(GitError::Dirty(_))
    ));
    git(root, &["commit", "-qam", "main change"]).unwrap();
    assert!(matches!(
        merge_into_base(root, &wt, "agt/t/c-000002", "main", Strategy::Merge, "t"),
        Err(GitError::Conflict(_))
    ));
}

#[test]
fn merge_refuses_non_branch_base_and_other_checked_out_branch() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/d-000005", "main").unwrap();
    std::fs::write(wt.join("d.txt"), "d\n").unwrap();
    git(&wt, &["add", "."]).unwrap();
    git(&wt, &["commit", "-qm", "d"]).unwrap();
    // base 是 tag：拒絕，不能 merge 到 detached HEAD
    git(root, &["tag", "v1"]).unwrap();
    assert!(matches!(
        merge_into_base(root, &wt, "agt/t/d-000005", "v1", Strategy::Merge, "t"),
        Err(GitError::Refused(_))
    ));
    // 主工作目錄在別的 branch：拒絕，且不切換
    git(root, &["checkout", "-q", "-b", "feature"]).unwrap();
    assert!(matches!(
        merge_into_base(root, &wt, "agt/t/d-000005", "main", Strategy::Merge, "t"),
        Err(GitError::Refused(_))
    ));
    assert_eq!(
        git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
        "feature"
    );
}

#[test]
fn exclude_and_copy() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join(".env"), "K=V").unwrap();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/e-000003", "main").unwrap();
    let copied = copy_files(root, &wt, &[".env".into(), "missing".into()]).unwrap();
    assert_eq!(copied, vec![".env".to_string()]);
    add_exclude(&wt, ".env").unwrap();
    add_exclude(&wt, ".env").unwrap();
    assert!(!is_dirty_wt(root, &wt, true).unwrap());
    assert!(copy_files(root, &wt, &["../x".into()]).is_err());
}

/// 惡意 repo commit 指向 repo 外的符號連結，再用 .agt.toml 的 copy_files 指定它：
/// 以前會把外部檔案清空（來源與目的地是同一個檔案）。
#[cfg(unix)]
#[test]
fn copy_files_refuses_symlinks() {
    use std::os::unix::fs::symlink;
    let repo = init_repo();
    let root = repo.path();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("bashrc");
    std::fs::write(&secret, "precious\n").unwrap();
    // 1. 檔案本身是連結；2. 上層目錄是連結；3. 目錄裡有連結
    symlink(&secret, root.join("evil")).unwrap();
    symlink(outside.path(), root.join("evildir")).unwrap();
    std::fs::create_dir_all(root.join("cfg")).unwrap();
    std::fs::write(root.join("cfg/ok.txt"), "ok\n").unwrap();
    symlink(&secret, root.join("cfg/link")).unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-qm", "symlinks"]).unwrap();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/s-000006", "main").unwrap();

    for f in ["evil", "evildir/bashrc"] {
        assert!(
            matches!(
                copy_files(root, &wt, &[f.into()]),
                Err(GitError::Refused(_))
            ),
            "{f}"
        );
    }
    // 目錄：一般檔案照常複製，連結略過
    assert_eq!(copy_files(root, &wt, &["cfg".into()]).unwrap(), ["cfg"]);
    assert_eq!(
        std::fs::read_to_string(wt.join("cfg/ok.txt")).unwrap(),
        "ok\n"
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "precious\n");
}

/// worktree 內的 `.git` 被替換（換成目錄、或指到 worktree 裡自己的目錄）時，
/// 所有對 worktree 的 git 操作都要拒絕，而不是採用它。測試資料都是無害的空目錄，不執行任何程式。
#[test]
fn untrusted_worktree_gitdir_is_refused() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/u-000007", "main").unwrap();

    // 正常的 worktree：驗證通過，操作照常
    let good = trusted_gitdir(root, &wt).unwrap();
    assert!(good.ends_with(Path::new("worktrees").join("w")), "{good:?}");
    assert!(status(root, &wt, "main").is_ok());

    // 1. `.git` 檔案被換成目錄
    let backup = std::fs::read_to_string(wt.join(".git")).unwrap();
    std::fs::remove_file(wt.join(".git")).unwrap();
    std::fs::create_dir(wt.join(".git")).unwrap();
    assert!(matches!(
        trusted_gitdir(root, &wt),
        Err(GitError::Refused(_))
    ));
    assert!(matches!(
        status(root, &wt, "main"),
        Err(GitError::Refused(_))
    ));
    assert!(matches!(diff(root, &wt, "main"), Err(GitError::Refused(_))));
    assert!(matches!(
        commit_all(root, &wt, "x"),
        Err(GitError::Refused(_))
    ));
    assert!(matches!(
        has_unpushed(root, &wt, "main"),
        Err(GitError::Refused(_))
    ));
    assert!(matches!(
        is_dirty_wt(root, &wt, true),
        Err(GitError::Refused(_))
    ));
    assert!(matches!(
        merge_into_base(root, &wt, "agt/t/u-000007", "main", Strategy::Rebase, "t"),
        Err(GitError::Refused(_))
    ));

    // 2. `.git` 仍是檔案，但指向 worktree 內自己的目錄
    std::fs::remove_dir(wt.join(".git")).unwrap();
    std::fs::create_dir(wt.join("mygit")).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: mygit\n").unwrap();
    assert!(matches!(
        trusted_gitdir(root, &wt),
        Err(GitError::Refused(_))
    ));

    // 3. 指向主 repo 的 .git 本身（不是 worktrees/ 底下登記的目錄）
    let main_git = normalize(&root.join(".git"));
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main_git.display())).unwrap();
    assert!(matches!(
        trusted_gitdir(root, &wt),
        Err(GitError::Refused(_))
    ));

    // 還原後恢復正常
    std::fs::write(wt.join(".git"), backup).unwrap();
    assert!(status(root, &wt, "main").is_ok());
}

/// Agent 能寫的 git config：`git status` 時執行指令、留下 marker 檔。
fn evil_config(dir: &Path, marker: &Path) {
    let m = marker.to_string_lossy().replace('\\', "/");
    let cmd = format!("echo x > \"{m}\"; false");
    git(dir, &["config", "core.fsmonitor", &cmd]).unwrap();
}

/// 重新啟動分頁時 `install_hooks` 會呼叫 add_exclude：此時 `.git` 可能已被上一次的 Agent 換掉，
/// 不能因此執行 worktree 裡的設定；而且丟棄分頁仍要能成功。
#[test]
fn replaced_dotgit_runs_nothing_in_add_exclude_and_can_be_discarded() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let marker = wts.path().join("pwned");
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/h-000006", "main").unwrap();
    add_exclude(&wt, ".claude/x").unwrap();
    std::fs::remove_file(wt.join(".git")).unwrap();
    git(&wt, &["init", "-q"]).unwrap();
    evil_config(&wt, &marker);
    // 確認這個設定真的會被未防護的 git 執行（測試本身有效），再清掉 marker
    // （用原生 git：agt 自己的 `git()` 已經帶了 -c core.fsmonitor=false）
    Command::new("git")
        .current_dir(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(marker.exists(), "fsmonitor 應該會被未防護的 git 執行");
    std::fs::remove_file(&marker).unwrap();

    // .git 是目錄：當成一般 repo，只寫入那個目錄裡的 info/exclude（Agent 自己的檔案），不執行 git
    let _ = add_exclude(&wt, ".claude/y");
    assert!(status(root, &wt, "main").is_err());
    assert!(!marker.exists(), "不可以執行 worktree 裡的設定");

    // 指向 worktree 內的目錄、或別的 repo 的 .git：同樣拒絕
    git(&wt, &["init", "-q", "inner"]).unwrap();
    std::fs::remove_dir_all(wt.join(".git")).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: inner/.git\n").unwrap();
    assert!(add_exclude(&wt, ".claude/y").is_err());
    let other = init_repo();
    let other_git = other.path().join(".git");
    let other_git = other_git.to_string_lossy().replace('\\', "/");
    std::fs::write(wt.join(".git"), format!("gitdir: {other_git}\n")).unwrap();
    assert!(add_exclude(&wt, ".claude/y").is_err());

    // 沒有確認（force = false）時不刪；使用者確認丟棄後仍然移除得掉
    assert!(remove_worktree(root, &wt, false).is_err());
    assert!(wt.exists());
    remove_worktree(root, &wt, true).unwrap();
    assert!(!wt.exists());
    delete_branch(root, "agt/t/h-000006").unwrap();
    assert!(!marker.exists());
}

#[test]
fn nested_repo_config_is_not_executed() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let marker = wts.path().join("pwned");
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/k-000009", "main").unwrap();
    // Agent 在 worktree 裡放一個巢狀 repo；auto_commit 會把它當成 gitlink 加入
    let sub = wt.join("sub");
    git(&wt, &["init", "-q", "sub"]).unwrap();
    git(&sub, &["config", "user.email", "t@t"]).unwrap();
    git(&sub, &["config", "user.name", "t"]).unwrap();
    git(&sub, &["commit", "-q", "--allow-empty", "-m", "s"]).unwrap();
    evil_config(&sub, &marker);
    std::fs::write(sub.join("f"), "x").unwrap();
    std::fs::write(wt.join("ok.txt"), "ok").unwrap();
    // auto_commit 拒絕把巢狀 repo 加進來，也不會留在 index 裡
    let r = commit_all(root, &wt, "auto");
    assert!(matches!(r, Err(GitError::Refused(_))), "{r:?}");
    assert!(!git(&wt, &["ls-files", "-s"]).unwrap().contains("160000"));
    // 就算 gitlink 已經進了 index（例如 Agent 自己 commit），status / diff 也不會進到裡面執行
    git(&wt, &["add", "-A"]).unwrap();
    git(&wt, &["commit", "-q", "-m", "gitlink"]).unwrap();
    std::fs::write(sub.join("g"), "y").unwrap();
    assert!(commit_all(root, &wt, "auto").unwrap().is_none());
    status(root, &wt, "main").unwrap();
    diff(root, &wt, "main").unwrap();
    assert!(!is_dirty_wt(root, &wt, false).unwrap());
    assert!(!marker.exists(), "不可以執行巢狀 repo 的設定");
}

#[test]
fn sensitive_files_are_reported() {
    for p in [
        ".agt.toml",
        ".claude/settings.json",
        ".github/workflows/ci.yml",
        ".vscode/tasks.json",
        "sub/.gitattributes",
        ".MCP.json",
    ] {
        assert!(is_sensitive_path(p), "{p}");
    }
    for p in [
        "src/main.rs",
        "docs/.agt.toml",
        "README.md",
        ".github/ISSUE.md",
    ] {
        assert!(!is_sensitive_path(p), "{p}");
    }
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/s-000010", "main").unwrap();
    std::fs::write(wt.join("ok.txt"), "x").unwrap();
    assert!(commit_all(root, &wt, "ok").unwrap().is_some());
    assert!(
        sensitive_changes(root, &wt, "main", &[])
            .unwrap()
            .is_empty()
    );
    std::fs::write(wt.join(".agt.toml"), "setup = \"curl x | sh\"\n").unwrap();
    std::fs::create_dir_all(wt.join(".claude")).unwrap();
    std::fs::write(wt.join(".claude/settings.json"), "{}").unwrap();
    assert!(commit_all(root, &wt, "evil").unwrap().is_some());
    assert_eq!(
        sensitive_changes(root, &wt, "main", &[]).unwrap(),
        [".agt.toml", ".claude/settings.json"]
    );
    // 使用者追加的路徑
    assert!(
        sensitive_changes(root, &wt, "main", &["ok.txt".into()])
            .unwrap()
            .contains(&"ok.txt".to_string())
    );
}

#[test]
fn extra_sensitive_paths() {
    let extra: Vec<String> = [
        "package.json",
        "scripts/",
        "tools/build.sh",
        "  ",
        "./Makefile",
    ]
    .map(String::from)
    .to_vec();
    for p in [
        "package.json",
        "web/package.json",
        "scripts/x.sh",
        "tools/build.sh",
        "Makefile",
    ] {
        assert!(matches_extra(p, &extra), "{p}");
    }
    for p in ["src/scripts.rs", "other/tools/build.sh", "package.json.bak"] {
        assert!(!matches_extra(p, &extra), "{p}");
    }
    assert!(!matches_extra("anything", &[]));
}

#[test]
fn container_gitdir_is_relative_and_still_works() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w");
    add_worktree(root, &wt, "agt/t/g-000004", "main").unwrap();
    let common = prepare_container_gitdir(&wt).unwrap();
    assert!(
        same_path(&common, &root.join(".git")),
        "{}",
        common.display()
    );
    let text = std::fs::read_to_string(wt.join(".git")).unwrap();
    assert!(text.starts_with("gitdir: ../"), "{text}");
    assert!(!text.contains('\\'));
    // 主機上的 git 照常可用；再呼叫一次結果相同
    std::fs::write(wt.join("n.txt"), "x").unwrap();
    assert!(is_dirty_wt(root, &wt, true).unwrap());
    assert!(same_path(&prepare_container_gitdir(&wt).unwrap(), &common));
    // 不是 worktree
    assert!(prepare_container_gitdir(root).is_none());
}

#[test]
fn relative_paths() {
    let r = |a: &str, b: &str| relative_path(Path::new(a), Path::new(b));
    if cfg!(windows) {
        assert_eq!(
            r(r"C:\a\wt\x", r"C:\a\repo\.git"),
            Some(PathBuf::from(r"..\..\repo\.git"))
        );
        assert_eq!(r(r"C:\a", r"D:\b"), None);
    } else {
        assert_eq!(
            r("/a/wt/x", "/a/repo/.git"),
            Some(PathBuf::from("../../repo/.git"))
        );
        assert_eq!(r("/a", "/a/b"), Some(PathBuf::from("b")));
    }
}

#[test]
fn non_repo_is_none() {
    let d = tempfile::tempdir().unwrap();
    assert!(repo_info(d.path()).unwrap().is_none());
}

// ---- 分組收尾 ----

fn write_group(wt: &Path, name: &str, files: &str, msg: Option<&str>) {
    let d = wt.join(NOTES_DIR).join("groups").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("FILES"), files).unwrap();
    if let Some(m) = msg {
        std::fs::write(d.join(COMMIT_MSG_FILE), m).unwrap();
    }
}

/// 主 repo（在 main）加一個 worktree，裡面有三個互不相關的變更：
/// one/（組 one）、two.txt（組 two）、rest.txt（沒被認領）。
fn grouped_worktree() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
    let repo = init_repo();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("wg");
    let branch = "agt/test/g-000009".to_string();
    add_worktree(repo.path(), &wt, &branch, "main").unwrap();
    std::fs::create_dir_all(wt.join("one")).unwrap();
    std::fs::write(wt.join("one/a.txt"), "1\n").unwrap();
    std::fs::write(wt.join("one/b.txt"), "1b\n").unwrap();
    std::fs::write(wt.join("two.txt"), "2\n").unwrap();
    std::fs::write(wt.join("rest.txt"), "r\n").unwrap();
    write_group(&wt, "one", "one\n", Some("加入 one\n\n內文\n"));
    write_group(&wt, "two", "./two.txt\n", None);
    (repo, wts, wt, branch)
}

fn refused(r: Result<Vec<groups::Group>>) -> String {
    r.expect_err("應該被拒絕").to_string()
}

#[test]
fn claims_matches_exact_and_directory_prefix() {
    let f = vec!["src".to_string(), "a.txt".to_string()];
    assert!(groups::claims(&f, "a.txt"));
    assert!(groups::claims(&f, "src/x/y.rs"));
    assert!(!groups::claims(&f, "src2/x.rs"));
    assert!(!groups::claims(&f, "a.txt.bak"));
}

#[test]
fn load_groups_reads_sorted_and_normalizes() {
    let (_r, _w, wt, _b) = grouped_worktree();
    let g = load_groups(&wt).unwrap();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].name, "one");
    assert_eq!(g[0].files, vec!["one"]);
    assert_eq!(
        g[0].commit_msg.as_deref().and_then(subject_line).as_deref(),
        Some("加入 one")
    );
    assert_eq!(g[1].name, "two");
    assert_eq!(g[1].files, vec!["two.txt"]); // 去掉 ./
    assert!(g[1].commit_msg.is_none());

    // 沒有 groups 目錄：空
    let empty = tempfile::tempdir().unwrap();
    assert!(load_groups(empty.path()).unwrap().is_empty());
}

#[test]
fn load_groups_rejects_bad_names_paths_and_empty_files() {
    let d = tempfile::tempdir().unwrap();
    let wt = d.path();
    for name in ["rest", "a b", ".hidden", "-x", "中文"] {
        let _ = std::fs::remove_dir_all(wt.join(NOTES_DIR));
        write_group(wt, name, "x\n", None);
        assert!(refused(load_groups(wt)).contains("名稱"), "{name}");
    }
    for path in [
        "../x",
        "/etc/passwd",
        "a/../b",
        ".git/config",
        ".agt/x",
        ":(top)x",
        "C:/x",
    ] {
        let _ = std::fs::remove_dir_all(wt.join(NOTES_DIR));
        write_group(wt, "ok", &format!("{path}\n"), None);
        assert!(refused(load_groups(wt)).contains("不合規定"), "{path}");
    }
    let _ = std::fs::remove_dir_all(wt.join(NOTES_DIR));
    write_group(wt, "ok", "\n  \n", None);
    assert!(refused(load_groups(wt)).contains("FILES"));
}

#[cfg(unix)]
#[test]
fn load_groups_rejects_symlinked_dirs() {
    let d = tempfile::tempdir().unwrap();
    let wt = d.path();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(wt.join(NOTES_DIR)).unwrap();
    std::os::unix::fs::symlink(other.path(), wt.join(NOTES_DIR).join("groups")).unwrap();
    assert!(load_groups(wt).is_err());

    let _ = std::fs::remove_file(wt.join(NOTES_DIR).join("groups"));
    std::fs::create_dir_all(wt.join(NOTES_DIR).join("groups")).unwrap();
    std::fs::write(other.path().join("FILES"), "x\n").unwrap();
    std::os::unix::fs::symlink(other.path(), wt.join(NOTES_DIR).join("groups/evil")).unwrap();
    assert!(load_groups(wt).is_err());
}

#[test]
fn group_branches_are_independent_and_idempotent() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let groups = load_groups(&wt).unwrap();

    let (gb, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &groups[0], "任務").unwrap();
    assert!(made);
    assert_eq!(gb, format!("{branch}-one"));
    let files = git(root, &["ls-tree", "-r", "--name-only", &gb]).unwrap();
    assert!(
        files.contains("one/a.txt") && files.contains("one/b.txt"),
        "{files}"
    );
    assert!(
        !files.contains("two.txt") && !files.contains("rest.txt"),
        "{files}"
    );
    // 只多一個 commit，親代是 main，訊息用該組的 COMMIT_MSG
    assert_eq!(
        git(root, &["rev-list", "--count", &format!("main..{gb}")]).unwrap(),
        "1"
    );
    assert_eq!(
        git(root, &["log", "-1", "--format=%s", &gb]).unwrap(),
        "加入 one"
    );
    // 沒有 COMMIT_MSG：用「任務（組名）」
    let (gb2, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &groups[1], "任務").unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s", &gb2]).unwrap(),
        "任務（two）"
    );

    // 沒有新變更：不再多一個 commit
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &groups[0], "任務").unwrap();
    assert!(!made);
    assert_eq!(
        git(root, &["rev-list", "--count", &format!("main..{gb}")]).unwrap(),
        "1"
    );
    // 有新變更：在原本的 branch 上追加（不改寫歷史）
    std::fs::write(wt.join("one/a.txt"), "changed\n").unwrap();
    let before = git(root, &["rev-parse", &gb]).unwrap();
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &groups[0], "任務").unwrap();
    assert!(made);
    assert_eq!(
        git(root, &["rev-parse", &format!("{gb}^")]).unwrap(),
        before
    );
}

#[test]
fn empty_group_is_rejected() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    write_group(&wt, "nothing", "no-such-file.txt\n", None);
    let g = load_groups(&wt).unwrap();
    let nothing = g.iter().find(|g| g.name == "nothing").unwrap();
    let e = groups::make_group_branch(root, &wt, &gitdir, &branch, "main", nothing, "t")
        .unwrap_err()
        .to_string();
    assert!(e.contains("沒有任何變更"), "{e}");
}

fn count(root: &Path, args: &[&str]) -> usize {
    git(root, args).unwrap().trim().parse().unwrap()
}

#[test]
fn merge_groups_merge_strategy_one_merge_commit_per_group() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let g = load_groups(&wt).unwrap();
    let out = merge_groups(root, &wt, &branch, "main", "任務", Strategy::Merge, g, None).unwrap();
    assert!(out.contains("分成 3 組"), "{out}");
    // one、two、rest 各一個 merge commit
    assert_eq!(count(root, &["rev-list", "--count", "--merges", "main"]), 3);
    for f in ["one/a.txt", "one/b.txt", "two.txt", "rest.txt"] {
        assert!(root.join(f).exists(), "{f}");
    }
    // 暫時的分組 branch 與說明都清掉了；.agt 沒有被當成 rest 的內容
    let branches = git(root, &["branch", "--list", &format!("{branch}-*")]).unwrap();
    assert!(branches.is_empty(), "{branches}");
    assert!(!wt.join(NOTES_DIR).join("groups/one").exists());
    assert!(!wt.join(NOTES_DIR).join("groups/two").exists());
    assert!(!root.join(".agt").exists());
}

#[test]
fn merge_groups_squash_and_rebase_make_linear_commits() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let g = load_groups(&wt).unwrap();
    merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Squash,
        g,
        None,
    )
    .unwrap();
    assert_eq!(count(root, &["rev-list", "--count", "--merges", "main"]), 0);
    assert_eq!(count(root, &["rev-list", "--count", "main"]), 4); // init + 3 組
    let subjects = git(root, &["log", "--format=%s", "-3", "main"]).unwrap();
    assert!(subjects.contains("加入 one"), "{subjects}");
    assert!(subjects.contains("任務（two）"), "{subjects}");
    assert!(subjects.contains("任務（rest）"), "{subjects}");

    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let g = load_groups(&wt).unwrap();
    merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Rebase,
        g,
        None,
    )
    .unwrap();
    assert_eq!(count(root, &["rev-list", "--count", "--merges", "main"]), 0);
    assert_eq!(count(root, &["rev-list", "--count", "main"]), 4);
    assert!(root.join("two.txt").exists() && root.join("rest.txt").exists());
}

#[test]
fn merge_groups_refuses_before_touching_base() {
    // 主工作目錄有未 commit 的變更
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    std::fs::write(root.join("a.txt"), "dirty\n").unwrap();
    let g = load_groups(&wt).unwrap();
    let e = merge_groups(root, &wt, &branch, "main", "t", Strategy::Merge, g, None).unwrap_err();
    assert!(matches!(e, GitError::Dirty(_)), "{e}");

    // 主工作目錄不在 base 上
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    git(root, &["checkout", "-q", "-b", "other"]).unwrap();
    let g = load_groups(&wt).unwrap();
    let e = merge_groups(root, &wt, &branch, "main", "t", Strategy::Merge, g, None).unwrap_err();
    assert!(e.to_string().contains("不是 main"), "{e}");

    // 其中一組是空的：在合併任何一組之前就停下，base 沒有變
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    write_group(&wt, "zzz", "no-such-file.txt\n", None);
    let g = load_groups(&wt).unwrap();
    let head = git(root, &["rev-parse", "main"]).unwrap();
    assert!(merge_groups(root, &wt, &branch, "main", "t", Strategy::Merge, g, None).is_err());
    assert_eq!(git(root, &["rev-parse", "main"]).unwrap(), head);
}

#[test]
fn merge_groups_stops_on_conflict_and_keeps_finished_groups() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    // 在 base 上先改 one/a.txt 之外也有的檔案：讓組 two 的 two.txt 與 base 衝突
    std::fs::write(root.join("two.txt"), "base 版本\n").unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-q", "-m", "base two"]).unwrap();
    let g = load_groups(&wt).unwrap();
    let e = merge_groups(root, &wt, &branch, "main", "t", Strategy::Merge, g, None).unwrap_err();
    assert!(matches!(e, GitError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("two"), "{e}");
    // 前面的組 one 已合進去
    assert!(root.join("one/a.txt").exists());
}

// ---- 收尾預覽 ----

fn preview(repo: &Path, wt: &Path, merge: bool, strategy: &str, auto: bool) -> String {
    finish_preview(repo, wt, "main", "任務", merge, strategy, auto).unwrap()
}

#[test]
fn preview_pr_grouped_lists_every_group_with_pr_body() {
    let (repo, _w, wt, _b) = grouped_worktree();
    let d = wt.join(NOTES_DIR).join("groups/one");
    std::fs::write(d.join(PR_BODY_FILE), "## 做了什麼\n加入 one\n第三行\n").unwrap();
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(p.starts_with("分成 3 組：3 個 commit、3 個 PR"), "{p}");
    assert!(p.contains("[one] 加入 one（1 個路徑）"), "{p}");
    assert!(p.contains("[two] 任務（two）"), "{p}"); // 沒有 COMMIT_MSG：任務名稱（組名）
    assert!(p.contains("[rest] 任務（rest）"), "{p}"); // 沒被認領的變更
    assert!(p.contains("## 做了什麼") && p.contains("加入 one"), "{p}");
    assert!(p.contains("…") && !p.contains("第三行"), "{p}"); // 只列前兩行
}

#[test]
fn preview_merge_grouped_has_no_pr_and_names_strategy() {
    let (repo, _w, wt, _b) = grouped_worktree();
    std::fs::write(
        wt.join(NOTES_DIR).join("groups/one").join(PR_BODY_FILE),
        "PR 才有的說明\n",
    )
    .unwrap();
    for (s, note) in [
        ("merge", "每組一個 merge commit"),
        ("squash", "每組壓成一個 commit"),
        ("rebase", "每組一個 commit 接在 base 後面"),
        ("ask", "合併策略稍後選擇"),
    ] {
        let p = preview(repo.path(), &wt, true, s, false);
        assert!(p.starts_with("分成 3 組依序合進 main：3 個 commit"), "{p}");
        assert!(p.contains(note) && p.contains("不開 PR"), "{s}: {p}");
        assert!(!p.contains("PR 才有的說明"), "{s}: {p}");
    }
}

#[test]
fn preview_without_groups_uses_notes_or_task() {
    let repo = init_repo();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("wp");
    add_worktree(repo.path(), &wt, "agt/test/p-000010", "main").unwrap();
    std::fs::write(wt.join("x.txt"), "x\n").unwrap();

    // 沒有說明檔：標題用任務名稱
    let p = preview(repo.path(), &wt, false, "ask", true);
    assert_eq!(p, "1 個 commit、1 個 PR\n▸ 任務");

    let notes = wt.join(NOTES_DIR);
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join(COMMIT_MSG_FILE), "新增 x\n\n內文一\n內文二\n").unwrap();
    std::fs::write(notes.join(PR_BODY_FILE), "## 說明\n詳細\n").unwrap();
    let p = preview(repo.path(), &wt, false, "ask", true);
    assert!(p.contains("▸ 新增 x") && p.contains("    內文一"), "{p}");
    assert!(p.contains("PR 說明：") && p.contains("    ## 說明"), "{p}");
    // merge 不開 PR，不顯示 PR 說明
    let p = preview(repo.path(), &wt, true, "squash", true);
    assert!(p.contains("▸ 新增 x") && p.contains("不開 PR"), "{p}");
    assert!(!p.contains("PR 說明"), "{p}");
}

#[test]
fn preview_merge_counts_commits_per_strategy() {
    let repo = init_repo();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("wc");
    add_worktree(repo.path(), &wt, "agt/test/c-000011", "main").unwrap();
    for f in ["b.txt", "c.txt"] {
        std::fs::write(wt.join(f), "x\n").unwrap();
        git(&wt, &["add", "."]).unwrap();
        git(&wt, &["commit", "-q", "-m", f]).unwrap();
    }
    // 分支上有 2 個 commit，乾淨：沒有待自動 commit 的變更
    let n = |s: &str, auto: bool| preview(repo.path(), &wt, true, s, auto);
    assert!(n("rebase", true).contains("預計 2 個 commit"));
    assert!(
        n("merge", true).contains("預計 3 個 commit（merge，2 個 commit 加 1 個 merge commit）")
    );
    assert!(n("squash", true).contains("預計 1 個 commit（squash"));
    assert!(n("ask", true).contains("分支有 2 個 commit"));

    // 有未 commit 的變更：auto_commit 開啟時多 1 個，關閉時不算
    std::fs::write(wt.join("d.txt"), "d\n").unwrap();
    assert!(n("rebase", true).contains("預計 3 個 commit"));
    assert!(n("merge", true).contains("預計 4 個 commit"));
    assert!(n("rebase", false).contains("預計 2 個 commit"));
    assert!(n("squash", true).contains("預計 1 個 commit"));
}

#[test]
fn preview_is_read_only_and_rejects_bad_groups() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let before = git(repo.path(), &["branch", "--list"]).unwrap();
    preview(repo.path(), &wt, false, "ask", true);
    preview(repo.path(), &wt, true, "merge", true);
    // 沒有建立分組 branch、沒有 commit，變更仍在
    assert_eq!(git(repo.path(), &["branch", "--list"]).unwrap(), before);
    assert!(!branch_exists(repo.path(), &format!("{branch}-one")));
    assert!(is_dirty_wt(repo.path(), &wt, true).unwrap());

    // 分組格式錯誤：回傳錯誤，讓視窗顯示原因
    write_group(&wt, "bad", "\n", None);
    let e = finish_preview(repo.path(), &wt, "main", "任務", false, "ask", false)
        .expect_err("FILES 是空的")
        .to_string();
    assert!(e.contains("FILES"), "{e}");
}

#[test]
fn preview_warns_about_unknown_notes_and_large_ungrouped_change() {
    let repo = init_repo();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("ww");
    add_worktree(repo.path(), &wt, "agt/test/w-000012", "main").unwrap();
    std::fs::write(wt.join("x.txt"), "x\n").unwrap();
    // 小改動、沒有多餘的說明檔：沒有提醒
    let p = preview(repo.path(), &wt, false, "ask", true);
    assert!(!p.contains('⚠'), "{p}");

    // Agent 自己寫的拆分計畫：不會被用到
    write_guide(&wt).unwrap();
    std::fs::create_dir_all(wt.join(NOTES_DIR).join("split/01")).unwrap();
    std::fs::write(wt.join(NOTES_DIR).join("split/01/FILES"), "x.txt\n").unwrap();
    let p = preview(repo.path(), &wt, false, "ask", true);
    let first = p.lines().next().unwrap();
    assert!(
        first.starts_with("⚠ .agt/") && first.contains("split/"),
        "{p}"
    );
    // agt 自己寫的 README.md 不算
    assert!(
        !first.split('（').next().unwrap().contains(GUIDE_FILE),
        "{p}"
    );
    assert!(p.contains("1 個 commit、1 個 PR"), "{p}");
    assert!(!p.contains("改動很大"), "{p}");

    // 改動很大（檔案數超過門檻）又沒有分組
    for i in 0..=LARGE_CHANGE_FILES {
        std::fs::write(wt.join(format!("f{i}.txt")), "x\n").unwrap();
    }
    let p = preview(repo.path(), &wt, true, "squash", true);
    assert!(p.contains("改動很大") && p.contains("沒有分組"), "{p}");

    // 行數超過門檻（已 commit 的也算）
    std::fs::remove_dir_all(wt.join(NOTES_DIR).join("split")).unwrap();
    for i in 0..=LARGE_CHANGE_FILES {
        std::fs::remove_file(wt.join(format!("f{i}.txt"))).unwrap();
    }
    std::fs::write(wt.join("big.txt"), "y\n".repeat(LARGE_CHANGE_LINES + 1)).unwrap();
    git(&wt, &["add", "."]).unwrap();
    git(&wt, &["commit", "-q", "-m", "big"]).unwrap();
    let p = preview(repo.path(), &wt, false, "ask", true);
    assert!(p.starts_with("⚠ 改動很大"), "{p}");
    assert_eq!(p.matches('⚠').count(), 1, "{p}");
}

#[test]
fn preview_with_groups_does_not_warn_about_size() {
    let (repo, _w, wt, _b) = grouped_worktree();
    for i in 0..=LARGE_CHANGE_FILES {
        std::fs::write(wt.join(format!("f{i}.txt")), "x\n").unwrap();
    }
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(!p.contains('⚠'), "{p}");
    assert!(p.starts_with("分成 3 組"), "{p}");
}

// ---- 堆疊分組（AFTER）----

fn write_after(wt: &Path, name: &str, after: &str) {
    let d = wt.join(NOTES_DIR).join("groups").join(name);
    std::fs::write(d.join(groups::AFTER_FILE), after).unwrap();
}

/// grouped_worktree 加上：two 接在 one 後面。
fn stacked_worktree() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
    let (repo, wts, wt, branch) = grouped_worktree();
    write_after(&wt, "two", "one\n");
    (repo, wts, wt, branch)
}

#[test]
fn load_groups_orders_by_after() {
    let (_r, _w, wt, _b) = grouped_worktree();
    // a 接在 two 後面、two 接在 one 後面：one → two → a（名稱順序原本是 a、one、two）
    std::fs::write(wt.join("a.txt"), "a\n").unwrap();
    write_group(&wt, "a", "a.txt\n", None);
    write_after(&wt, "a", "  two  \n");
    write_after(&wt, "two", "one\n");
    let g = load_groups(&wt).unwrap();
    let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["one", "two", "a"]);
    assert_eq!(g[1].after.as_deref(), Some("one"));
    assert_eq!(g[2].after.as_deref(), Some("two")); // 去掉空白
    assert!(g[0].after.is_none());

    // 接在不存在的組後面：讀取時允許（那組可能已經處理完），建 branch 時才檢查
    write_after(&wt, "a", "gone\n");
    let g = load_groups(&wt).unwrap();
    assert_eq!(
        g.iter().find(|g| g.name == "a").unwrap().after.as_deref(),
        Some("gone")
    );
}

#[test]
fn load_groups_rejects_bad_after_and_cycles() {
    let (_r, _w, wt, _b) = grouped_worktree();
    for bad in ["rest", "two", "a b", "../x"] {
        write_after(&wt, "two", bad);
        assert!(refused(load_groups(&wt)).contains("AFTER"), "{bad}");
    }
    write_after(&wt, "two", "one");
    write_after(&wt, "one", "two");
    let e = refused(load_groups(&wt));
    assert!(
        e.contains("循環") && e.contains("one") && e.contains("two"),
        "{e}"
    );
}

#[test]
fn stacked_group_branch_builds_on_its_dependency() {
    let (repo, _w, wt, branch) = stacked_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let g = load_groups(&wt).unwrap();
    let (one, two) = (&g[0], &g[1]);

    // 接的那組還沒有 branch：拒絕並說明
    let e = groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務")
        .unwrap_err()
        .to_string();
    assert!(e.contains("找不到 one 的 branch"), "{e}");

    let (ob, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", one, "任務").unwrap();
    let (tb, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務").unwrap();
    assert!(made);
    // two 的親代是 one 的 branch，比 one 多一個 commit，內容包含兩組、不含 rest
    assert_eq!(
        git(root, &["rev-parse", &format!("{tb}^")]).unwrap(),
        git(root, &["rev-parse", &ob]).unwrap()
    );
    assert_eq!(
        count(root, &["rev-list", "--count", &format!("{ob}..{tb}")]),
        1
    );
    let files = git(root, &["ls-tree", "-r", "--name-only", &tb]).unwrap();
    assert!(
        files.contains("one/a.txt") && files.contains("two.txt"),
        "{files}"
    );
    assert!(!files.contains("rest.txt"), "{files}");
    // PR 的差異（相對 one 的 branch）只有 two 自己的檔案
    assert_eq!(
        git(root, &["diff", "--name-only", &ob, &tb]).unwrap(),
        "two.txt"
    );

    // 沒有新變更：不多 commit
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務").unwrap();
    assert!(!made);
}

#[test]
fn stacked_group_catches_up_with_merge_commit_when_dependency_moves() {
    let (repo, _w, wt, branch) = stacked_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let g = load_groups(&wt).unwrap();
    let (one, two) = (&g[0], &g[1]);
    let (ob, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", one, "任務").unwrap();
    let (tb, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務").unwrap();
    let old_two = git(root, &["rev-parse", &tb]).unwrap();

    // one 又改了：one 追加 commit，two 以 merge commit 併入 one 的新內容（不改寫歷史）
    std::fs::write(wt.join("one/a.txt"), "changed\n").unwrap();
    groups::make_group_branch(root, &wt, &gitdir, &branch, "main", one, "任務").unwrap();
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務").unwrap();
    assert!(made);
    assert_eq!(
        git(root, &["rev-parse", &format!("{tb}^1")]).unwrap(),
        old_two
    );
    assert_eq!(
        git(root, &["rev-parse", &format!("{tb}^2")]).unwrap(),
        git(root, &["rev-parse", &ob]).unwrap()
    );
    assert_eq!(
        git(root, &["show", &format!("{tb}:one/a.txt")]).unwrap(),
        "changed"
    );
    assert_eq!(
        git(root, &["diff", "--name-only", &ob, &tb]).unwrap(),
        "two.txt"
    );
    // 再跑一次：已追上，沒有新變更
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", two, "任務").unwrap();
    assert!(!made);
}

#[test]
fn merge_groups_handles_stacked_groups_with_every_strategy() {
    for strategy in [Strategy::Merge, Strategy::Squash, Strategy::Rebase] {
        let (repo, _w, wt, branch) = stacked_worktree();
        let root = repo.path();
        let g = load_groups(&wt).unwrap();
        let out = merge_groups(root, &wt, &branch, "main", "任務", strategy, g, None).unwrap();
        assert!(out.contains("分成 3 組"), "{strategy:?}: {out}");
        for f in ["one/a.txt", "one/b.txt", "two.txt", "rest.txt"] {
            assert!(root.join(f).exists(), "{strategy:?}: {f}");
        }
        let merges = count(root, &["rev-list", "--count", "--merges", "main"]);
        let all = count(root, &["rev-list", "--count", "main"]);
        match strategy {
            Strategy::Merge => assert_eq!(merges, 3, "{strategy:?}"),
            _ => {
                assert_eq!(merges, 0, "{strategy:?}");
                assert_eq!(all, 4, "{strategy:?}"); // init + 3 組
            }
        }
        let branches = git(root, &["branch", "--list", &format!("{branch}-*")]).unwrap();
        assert!(branches.is_empty(), "{strategy:?}: {branches}");
    }
}

#[test]
fn merge_groups_rebase_keeps_every_commit_of_an_updated_group_branch() {
    // 分組 branch 在之前收尾 pr 時已追加過 commit（one 有兩個 commit、two 有追上的 merge commit）
    let (repo, _w, wt, branch) = stacked_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let g = load_groups(&wt).unwrap();
    groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[0], "任務").unwrap();
    groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[1], "任務").unwrap();
    std::fs::write(wt.join("one/b.txt"), "1b changed\n").unwrap();
    let g = load_groups(&wt).unwrap();
    merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Rebase,
        g,
        None,
    )
    .unwrap();
    assert_eq!(count(root, &["rev-list", "--count", "--merges", "main"]), 0);
    // init + one 的 2 個 commit + two + rest
    assert_eq!(count(root, &["rev-list", "--count", "main"]), 5);
    assert_eq!(
        std::fs::read_to_string(root.join("one/b.txt")).unwrap(),
        "1b changed\n"
    );
    assert!(root.join("two.txt").exists() && root.join("rest.txt").exists());
}

#[test]
fn preview_shows_which_group_a_stacked_group_follows() {
    let (repo, _w, wt, _b) = stacked_worktree();
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(
        p.contains("[two] 任務（two）（1 個路徑，接在 [one] 後面）"),
        "{p}"
    );
    assert!(p.contains("[one] 加入 one（1 個路徑）"), "{p}");
}

#[test]
fn stack_guard_warns_about_merge_order_only_for_stacked_groups() {
    let (repo, _w, wt, _b) = stacked_worktree();
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(p.contains("堆疊 PR：請由前往後合併（one → two"), "{p}");
    // 合併進 base 時不開 PR，不需要這段提醒
    let m = preview(repo.path(), &wt, true, "merge", false);
    assert!(!m.contains("堆疊 PR"), "{m}");
    // 沒有 AFTER 的分組沒有提醒
    let g = load_groups(&wt).unwrap();
    let plain: Vec<_> = g
        .into_iter()
        .map(|mut g| {
            g.after = None;
            g
        })
        .collect();
    assert!(groups::stack_guard_note(&plain, "main").is_none());
}

#[test]
fn stacked_pr_body_starts_with_a_banner_and_keeps_the_body() {
    let b = groups::stacked_pr_body("one", "task-one", "main", Some("做了什麼"));
    assert!(b.starts_with("> ⚠ **堆疊 PR**"), "{b}");
    assert!(b.contains("`task-one`") && b.contains("`main`"), "{b}");
    assert!(b.ends_with("做了什麼"), "{b}");
    let only = groups::stacked_pr_body("one", "task-one", "main", None);
    assert!(only.starts_with("> ⚠") && !only.contains("做了什麼"));
}

// ---- 合併後確認進了 base（landing）----

const OID: &str = "0123456789abcdef0123456789abcdef01234567";

fn landing_of(line: &str, in_base: bool) -> Result<Landing> {
    classify(line, "main", |_| Ok(in_base))
}

#[test]
fn landing_classifies_pr_states() {
    assert_eq!(landing_of("OPEN main ", false).unwrap(), Landing::Open);
    assert_eq!(landing_of("CLOSED main ", false).unwrap(), Landing::Closed);
    // squash / rebase：合併 commit 與推出去的不同，只要它在 base 裡就算進去
    assert_eq!(
        landing_of(&format!("MERGED main {OID}"), true).unwrap(),
        Landing::Landed
    );
    assert_eq!(
        landing_of(&format!("MERGED main {OID}"), false).unwrap(),
        Landing::Missing
    );
    assert_eq!(landing_of("MERGED main ", true).unwrap(), Landing::Missing);
    // 堆疊 PR 合進前一組的 branch：不是 main，不必再查 git
    let r = classify(&format!("MERGED agt/t-one {OID}"), "main", |_| {
        panic!("base 不符時不該查 git")
    });
    assert_eq!(r.unwrap(), Landing::WrongBase("agt/t-one".into()));
}

#[test]
fn landing_rejects_malformed_gh_output() {
    for bad in [
        "",
        "MERGED",
        "WEIRD main ",
        "MERGED main nothex",
        "MERGED main 0123",
        "MERGED -x ",
        "MERGED a..b ",
        "MERGED ma\u{1b}in ",
        &format!("MERGED main {OID} extra"),
    ] {
        assert!(landing_of(bad, true).is_err(), "{bad:?}");
    }
}

// ---- 收尾檢查（finish_check）----

#[test]
fn merge_groups_runs_check_on_every_group_checkout_before_merging() {
    let (repo, _w, wt, branch) = stacked_worktree();
    let root = repo.path();
    let g = load_groups(&wt).unwrap();
    let seen = std::cell::RefCell::new(vec![]);
    let check = |name: &str, dir: &Path| -> std::result::Result<(), String> {
        // 暫時目錄是這組 branch 的乾淨 checkout：有這組（與它接的組）的檔案，沒有別組與說明檔
        let has = |f: &str| dir.join(f).exists();
        seen.borrow_mut().push((
            name.to_string(),
            has("one/a.txt"),
            has("two.txt"),
            has("rest.txt"),
            has(".agt"),
            dir.to_path_buf(),
        ));
        // 檢查時 base 還沒被動過
        assert_eq!(count(root, &["rev-list", "--count", "main"]), 1);
        Ok(())
    };
    merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Squash,
        g,
        Some(&check),
    )
    .unwrap();
    let seen = seen.into_inner();
    let summary: Vec<(&str, bool, bool, bool, bool)> = seen
        .iter()
        .map(|(n, a, b, c, d, _)| (n.as_str(), *a, *b, *c, *d))
        .collect();
    assert_eq!(
        summary,
        [
            ("one", true, false, false, false),
            ("two", true, true, false, false), // 接在 one 後面
            ("rest", false, false, true, false),
        ]
    );
    // 暫時目錄都清掉了，git 也不再登記它們
    for (_, _, _, _, _, dir) in &seen {
        assert!(!dir.exists(), "{}", dir.display());
    }
    let list = git(root, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(!list.contains(".check-"), "{list}");
    assert_eq!(count(root, &["rev-list", "--count", "main"]), 4);
}

#[test]
fn failed_check_stops_before_touching_base() {
    let (repo, _w, wt, branch) = grouped_worktree();
    let root = repo.path();
    let g = load_groups(&wt).unwrap();
    let check = |name: &str, _dir: &Path| -> std::result::Result<(), String> {
        if name == "two" {
            Err("error[E0425]: cannot find value `x`".into())
        } else {
            Ok(())
        }
    };
    let e = merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Merge,
        g,
        Some(&check),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("分組 two 的收尾檢查沒有通過"), "{e}");
    assert!(e.contains("E0425"), "{e}");
    // 一組都沒有合併；說明檔保留，修好後可以再收尾一次
    assert_eq!(count(root, &["rev-list", "--count", "main"]), 1);
    assert!(wt.join(NOTES_DIR).join("groups/one").exists());
    let list = git(root, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(!list.contains(".check-"), "{list}");
}

// ---- SNAPSHOT（同一個檔案分到不同組）----

fn write_snapshot(wt: &Path, group: &str, path: &str, content: &str) {
    let f = wt
        .join(NOTES_DIR)
        .join("groups")
        .join(group)
        .join(groups::SNAPSHOT_DIR)
        .join(path);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, content).unwrap();
}

/// 兩個主題都改了 `src/shared.txt`（base 裡沒有這個檔案以外的內容）：
/// a 只放自己那一行（SNAPSHOT），b 接在 a 後面、拿最終內容（FILES）。
fn shared_file_worktree() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
    let repo = init_repo();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("ws");
    let branch = "agt/test/s-000010".to_string();
    add_worktree(repo.path(), &wt, &branch, "main").unwrap();
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("src/shared.txt"), "A\nB\n").unwrap();
    std::fs::write(wt.join("b-only.txt"), "b\n").unwrap();
    // a：只有 SNAPSHOT，沒有 FILES
    std::fs::create_dir_all(wt.join(NOTES_DIR).join("groups/a")).unwrap();
    std::fs::write(
        wt.join(NOTES_DIR).join("groups/a").join(COMMIT_MSG_FILE),
        "加入 A\n",
    )
    .unwrap();
    write_snapshot(&wt, "a", "src/shared.txt", "A\n");
    write_group(&wt, "b", "src/shared.txt\nb-only.txt\n", Some("加入 B\n"));
    write_after(&wt, "b", "a");
    (repo, wts, wt, branch)
}

#[test]
fn load_groups_lists_snapshots_and_allows_missing_files() {
    let (_r, _w, wt, _b) = shared_file_worktree();
    write_snapshot(&wt, "a", "deep/dir/x.rs", "x\n");
    let g = load_groups(&wt).unwrap();
    assert_eq!(g[0].name, "a");
    assert!(g[0].files.is_empty());
    assert_eq!(g[0].snapshots, ["deep/dir/x.rs", "src/shared.txt"]);
    assert!(g[1].snapshots.is_empty());
}

#[test]
fn load_groups_rejects_bad_snapshots() {
    // 同一組的 FILES 也列了它
    let (_r, _w, wt, _b) = shared_file_worktree();
    write_snapshot(&wt, "b", "src/shared.txt", "A\n");
    let e = refused(load_groups(&wt));
    assert!(
        e.contains("SNAPSHOT/src/shared.txt") && e.contains("FILES"),
        "{e}"
    );

    // 路徑規則與 FILES 相同（.git、.agt）
    for bad in [".git/config", ".agt/x"] {
        let (_r, _w, wt, _b) = shared_file_worktree();
        write_snapshot(&wt, "a", bad, "x");
        let e = refused(load_groups(&wt));
        assert!(e.contains("不合規定"), "{bad}: {e}");
    }

    // 沒有 FILES、SNAPSHOT 是空目錄：等於什麼都沒有
    let (_r, _w, wt, _b) = shared_file_worktree();
    let snap = wt
        .join(NOTES_DIR)
        .join("groups/a")
        .join(groups::SNAPSHOT_DIR);
    std::fs::remove_dir_all(&snap).unwrap();
    std::fs::create_dir_all(&snap).unwrap();
    assert!(refused(load_groups(&wt)).contains("FILES"));
}

#[cfg(unix)]
#[test]
fn load_groups_rejects_symlinked_snapshots() {
    let (_r, _w, wt, _b) = shared_file_worktree();
    let snap = wt
        .join(NOTES_DIR)
        .join("groups/a")
        .join(groups::SNAPSHOT_DIR);
    std::os::unix::fs::symlink("/etc/passwd", snap.join("leak.txt")).unwrap();
    assert!(refused(load_groups(&wt)).contains("連結"));

    let (_r, _w, wt, _b) = shared_file_worktree();
    let snap = wt
        .join(NOTES_DIR)
        .join("groups/a")
        .join(groups::SNAPSHOT_DIR);
    std::fs::remove_dir_all(&snap).unwrap();
    let other = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(other.path(), &snap).unwrap();
    assert!(refused(load_groups(&wt)).contains("連結"));
}

#[test]
fn snapshot_splits_one_file_across_stacked_groups() {
    let (repo, _w, wt, branch) = shared_file_worktree();
    let root = repo.path();
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let g = load_groups(&wt).unwrap();
    let (ab, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[0], "任務").unwrap();
    assert!(made);
    let (bb, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[1], "任務").unwrap();
    // a 的 branch 只有 A；b 的 branch 是最終內容，相對 a 只多了 B 那一行與 b-only.txt
    assert_eq!(
        git(root, &["show", &format!("{ab}:src/shared.txt")]).unwrap(),
        "A"
    );
    let a_files = git(root, &["ls-tree", "-r", "--name-only", &ab]).unwrap();
    assert!(!a_files.contains("b-only.txt"), "{a_files}");
    assert_eq!(
        git(root, &["show", &format!("{bb}:src/shared.txt")]).unwrap(),
        "A\nB"
    );
    let diff = git(root, &["diff", &ab, &bb, "--", "src/shared.txt"]).unwrap();
    assert!(diff.contains("+B") && !diff.contains("-A"), "{diff}");
    // .agt 沒有被放進任何 branch
    assert!(
        !git(root, &["ls-tree", "-r", "--name-only", &bb])
            .unwrap()
            .contains(".agt")
    );
    // 沒有新變更：不多 commit
    let (_, made) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[0], "任務").unwrap();
    assert!(!made);
}

#[test]
fn snapshot_keeps_executable_mode_of_existing_file() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("run.sh"), "echo 0\n").unwrap();
    git(root, &["add", "run.sh"]).unwrap();
    git(root, &["update-index", "--chmod=+x", "run.sh"]).unwrap();
    git(root, &["commit", "-q", "-m", "script"]).unwrap();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("wx");
    let branch = "agt/test/x-000011".to_string();
    add_worktree(root, &wt, &branch, "main").unwrap();
    std::fs::write(wt.join("run.sh"), "echo 1\necho 2\n").unwrap();
    write_snapshot(&wt, "first", "run.sh", "echo 1\n");
    let gitdir = trusted_gitdir(root, &wt).unwrap();
    let g = load_groups(&wt).unwrap();
    let (gb, _) =
        groups::make_group_branch(root, &wt, &gitdir, &branch, "main", &g[0], "任務").unwrap();
    let entry = git(root, &["ls-tree", &gb, "run.sh"]).unwrap();
    assert!(entry.starts_with("100755"), "{entry}");
}

#[test]
fn merge_groups_with_snapshots_ends_with_worktree_content() {
    for strategy in [Strategy::Merge, Strategy::Squash, Strategy::Rebase] {
        let (repo, _w, wt, branch) = shared_file_worktree();
        let root = repo.path();
        let g = load_groups(&wt).unwrap();
        let out = merge_groups(root, &wt, &branch, "main", "任務", strategy, g, None).unwrap();
        assert!(out.contains("分成 2 組"), "{strategy:?}: {out}");
        assert_eq!(
            std::fs::read_to_string(root.join("src/shared.txt")).unwrap(),
            "A\nB\n",
            "{strategy:?}"
        );
        assert!(root.join("b-only.txt").exists(), "{strategy:?}");
        let subjects = git(root, &["log", "--format=%s", "main"]).unwrap();
        assert!(
            subjects.contains("加入 A") && subjects.contains("加入 B"),
            "{subjects}"
        );
    }
}

#[test]
fn snapshot_only_file_goes_to_rest_with_final_content() {
    // 只有 SNAPSHOT、沒有任何組的 FILES 列它：最終內容歸 rest
    let (repo, _w, wt, branch) = shared_file_worktree();
    let root = repo.path();
    write_group(&wt, "b", "b-only.txt\n", Some("加入 B\n"));
    let g = load_groups(&wt).unwrap();
    merge_groups(
        root,
        &wt,
        &branch,
        "main",
        "任務",
        Strategy::Squash,
        g,
        None,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("src/shared.txt")).unwrap(),
        "A\nB\n"
    );
    let subjects = git(root, &["log", "--format=%s", "main"]).unwrap();
    assert!(subjects.contains("任務（rest）"), "{subjects}");
}

#[test]
fn preview_counts_snapshots() {
    let (repo, _w, wt, _b) = shared_file_worktree();
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(p.contains("[a] 加入 A（0 個路徑，1 個 SNAPSHOT）"), "{p}");
    assert!(p.contains("[b] 加入 B（2 個路徑，接在 [a] 後面）"), "{p}");
}

#[test]
fn final_content_group_must_come_after_snapshot_groups() {
    // b 有 shared.txt 的最終內容，卻沒有接在有它 SNAPSHOT 的 a 後面
    let (_r, _w, wt, _b) = shared_file_worktree();
    std::fs::remove_file(wt.join(NOTES_DIR).join("groups/b").join(groups::AFTER_FILE)).unwrap();
    let e = refused(load_groups(&wt));
    assert!(
        e.contains("沒有接在有它 SNAPSHOT 的 a 後面") && e.contains("groups/b/AFTER"),
        "{e}"
    );

    // 間接接在後面也可以：a ← mid ← b
    let (_r, _w, wt, _b) = shared_file_worktree();
    std::fs::write(wt.join("mid.txt"), "m\n").unwrap();
    write_group(&wt, "mid", "mid.txt\n", None);
    write_after(&wt, "mid", "a");
    write_after(&wt, "b", "mid");
    let names: Vec<String> = load_groups(&wt)
        .unwrap()
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, ["a", "mid", "b"]);
}

#[test]
fn rest_follows_the_snapshot_group_and_refuses_unchained_snapshots() {
    let (repo, _w, wt, _b) = shared_file_worktree();
    write_group(&wt, "b", "b-only.txt\n", Some("加入 B\n")); // shared.txt 歸 rest
    let p = preview(repo.path(), &wt, false, "ask", false);
    assert!(
        p.contains("[rest] 任務（rest）（1 個路徑，接在 [a] 後面）"),
        "{p}"
    );

    // 另一組（沒有接在 a 後面）也有 shared.txt 的 SNAPSHOT：rest 不知道該接在哪一組後面
    write_snapshot(&wt, "b", "src/shared.txt", "B\n");
    std::fs::remove_file(wt.join(NOTES_DIR).join("groups/b").join(groups::AFTER_FILE)).unwrap();
    let e = finish_preview(repo.path(), &wt, "main", "任務", false, "ask", false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("都有它們的 SNAPSHOT"), "{e}");
}

#[test]
fn changed_files_unions_committed_uncommitted_and_untracked() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w1");
    add_worktree(root, &wt, "agt/test/cf-000001", "main").unwrap();
    assert!(changed_files(root, &wt, "main").unwrap().is_empty());

    std::fs::write(wt.join("committed.txt"), "c\n").unwrap();
    git(&wt, &["add", "."]).unwrap();
    git(&wt, &["commit", "-q", "-m", "c"]).unwrap();
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap(); // 已追蹤、未 commit
    std::fs::write(wt.join("新增 檔案.txt"), "u\n").unwrap(); // 未追蹤，檔名含中文與空白
    std::fs::create_dir_all(wt.join(".agt")).unwrap();
    std::fs::write(wt.join(".agt/COMMIT_MSG"), "x\n").unwrap(); // agt 自己的說明檔不算

    let files: Vec<String> = changed_files(root, &wt, "main")
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(files, ["a.txt", "committed.txt", "新增 檔案.txt"]);
}

#[test]
fn changed_files_ignores_what_base_changed_after_branching() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w1");
    add_worktree(root, &wt, "agt/test/cf-000002", "main").unwrap();
    // base 之後多了 commit：用 base...HEAD（merge-base），不能把它算成這個分頁改的
    std::fs::write(root.join("on-main.txt"), "m\n").unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-q", "-m", "m"]).unwrap();
    assert!(changed_files(root, &wt, "main").unwrap().is_empty());
}

#[test]
fn sync_with_base_merges_refuses_dirty_and_aborts_on_conflict() {
    let repo = init_repo();
    let root = repo.path();
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("w1");
    add_worktree(root, &wt, "agt/test/sy-000001", "main").unwrap();

    // 已同步：不動
    assert!(sync_with_base(root, &wt, "main").unwrap().contains("已經"));

    // base 前進，分頁落後
    std::fs::write(root.join("on-main.txt"), "m\n").unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-q", "-m", "m"]).unwrap();
    assert_eq!(status(root, &wt, "main").unwrap().behind, 1);

    // 有未 commit 的變更：拒絕，內容不變
    std::fs::write(wt.join("a.txt"), "local\n").unwrap();
    assert!(matches!(
        sync_with_base(root, &wt, "main"),
        Err(GitError::Dirty(_))
    ));
    assert!(!wt.join("on-main.txt").exists());
    git(&wt, &["checkout", "--", "a.txt"]).unwrap();

    // 乾淨：合併成功
    let msg = sync_with_base(root, &wt, "main").unwrap();
    assert!(msg.contains('1'), "{msg}");
    assert!(wt.join("on-main.txt").exists());
    assert_eq!(status(root, &wt, "main").unwrap().behind, 0);

    // 衝突：還原，worktree 乾淨、HEAD 不變
    std::fs::write(root.join("a.txt"), "main side\n").unwrap();
    git(root, &["commit", "-q", "-am", "main edit"]).unwrap();
    std::fs::write(wt.join("a.txt"), "branch side\n").unwrap();
    git(&wt, &["commit", "-q", "-am", "branch edit"]).unwrap();
    let head = git(&wt, &["rev-parse", "HEAD"]).unwrap();
    match sync_with_base(root, &wt, "main") {
        Err(GitError::Conflict(m)) => assert!(m.contains("a.txt"), "{m}"),
        other => panic!("應該回報衝突：{other:?}"),
    }
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), head);
    assert!(!is_dirty_wt(root, &wt, true).unwrap());
    assert!(
        !git(&wt, &["status", "--porcelain=v1"])
            .unwrap()
            .contains("UU")
    );
}
