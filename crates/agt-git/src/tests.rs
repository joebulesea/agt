use super::*;

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
