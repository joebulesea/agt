use super::*;

#[test]
fn orphan_logs_are_removed() {
    let d = tempfile::tempdir().unwrap();
    let open_id = uuid::Uuid::new_v4().to_string();
    let gone_id = uuid::Uuid::new_v4().to_string();
    for name in [
        format!("{open_id}.log"),
        format!("{gone_id}.log"),
        format!("{gone_id}.log.1"),
        "notes.log".to_string(), // 不是 agt 的檔名：不動
    ] {
        std::fs::write(d.path().join(name), "x").unwrap();
    }
    let mut open = HashMap::new();
    open.insert(
        open_id.clone(),
        Entry {
            info: row_to_info(&SessionRow::default()),
            session: None,
            conversation: None,
            generation: 0,
            resume_attempt: None,
        },
    );
    assert_eq!(remove_orphan_logs(d.path(), &open), 2);
    assert!(d.path().join(format!("{open_id}.log")).exists());
    assert!(d.path().join("notes.log").exists());
    assert!(!d.path().join(format!("{gone_id}.log")).exists());
    assert_eq!(remove_orphan_logs(&d.path().join("missing"), &open), 0);
}

#[test]
fn stale_generation_cannot_overwrite_restarted_tab() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config_file: dir.path().join("config.toml"),
        data_dir: dir.path().join("data"),
        home: dir.path().to_path_buf(),
        logs_dir: None,
    };
    let d = Daemon::new(paths, "test".into(), "agt".into(), Config::defaults(), None).unwrap();
    let mut info = row_to_info(&SessionRow {
        id: "s1".into(),
        short_id: "aaaaaa".into(),
        status: "running".into(),
        ..Default::default()
    });
    info.status = AgentStatus::Running;
    d.entries.lock().unwrap().insert(
        "s1".into(),
        Entry {
            info,
            session: None,
            conversation: None,
            generation: 2, // 已重新啟動過一次
            resume_attempt: None,
        },
    );
    // 第 1 次啟動的結束事件晚到：不能改到目前（第 2 次）的狀態
    assert!(
        d.update_if_current("s1", 1, |i| i.status = AgentStatus::Exited)
            .is_none()
    );
    assert_eq!(d.get("s1").unwrap().status, AgentStatus::Running);
    // 目前這一次的事件照常套用
    assert!(
        d.update_if_current("s1", 2, |i| i.status = AgentStatus::Done)
            .is_some()
    );
    assert_eq!(d.get("s1").unwrap().status, AgentStatus::Done);
    // 分頁已移除
    assert!(d.update_if_current("nope", 2, |_| {}).is_none());
}
