//! 分頁的用量（context、訂閱額度）：讀取 Claude Code 的 status line 寫到 worktree 的 `.agt/usage.json`。

use super::*;
use agt_proto::usage::{USAGE_FILE, USAGE_MAX, Usage};

impl Daemon {
    /// 讀取各分頁的 `.agt/usage.json`，數值有變化時推播 `SessionUpdated`。
    /// 只有檔案修改時間變了才讀；檔案是 Agent 那一側寫的，讀取時不跟隨連結、限制大小（見 `agt_proto::usage`）。
    pub fn poll_usage(&self) {
        let targets: Vec<(String, String)> = self
            .entries
            .lock_or_recover()
            .values()
            .filter(|e| !e.info.cwd.is_empty())
            .map(|e| (e.info.id.clone(), e.info.cwd.clone()))
            .collect();
        self.usage_seen
            .lock_or_recover()
            .retain(|id, _| targets.iter().any(|(t, _)| t == id));
        for (id, cwd) in targets {
            let Some(usage) = self.read_usage(&id, Path::new(&cwd)) else {
                continue;
            };
            let changed = {
                let mut entries = self.entries.lock_or_recover();
                let Some(e) = entries.get_mut(&id) else {
                    continue;
                };
                let changed = !e
                    .info
                    .usage
                    .as_ref()
                    .is_some_and(|old| old.same_display(&usage));
                e.info.usage = Some(usage);
                changed.then(|| e.info.clone())
            };
            if let Some(info) = changed {
                self.publish(info);
            }
        }
    }

    /// 修改時間和上次不同時讀取並解析；沒有檔案、沒有變化或格式不對時回傳 `None`。
    fn read_usage(&self, id: &str, cwd: &Path) -> Option<Usage> {
        let path = cwd.join(agt_git::NOTES_DIR).join(USAGE_FILE);
        let m = std::fs::symlink_metadata(&path).ok()?;
        if m.file_type().is_symlink() || !m.is_file() || m.len() > USAGE_MAX {
            return None;
        }
        let mtime = m.modified().ok()?;
        {
            let mut seen = self.usage_seen.lock_or_recover();
            if seen.get(id) == Some(&mtime) {
                return None;
            }
            seen.insert(id.to_string(), mtime);
        }
        // read_note：不跟隨連結、上限 64KB、必須是 UTF-8
        let text = agt_git::read_note(cwd, USAGE_FILE).ok()??;
        agt_proto::usage::parse(&text, agt_store::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_usage_reads_changes_and_publishes_only_visible_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            home: dir.path().to_path_buf(),
            logs_dir: None,
        };
        let d = Daemon::new(paths, "test".into(), "agt".into(), Config::defaults(), None).unwrap();
        let wt = dir.path().join("wt");
        let notes = wt.join(agt_git::NOTES_DIR);
        std::fs::create_dir_all(&notes).unwrap();
        let mut info = row_to_info(&SessionRow {
            id: "s1".into(),
            short_id: "aaaaaa".into(),
            status: "running".into(),
            ..Default::default()
        });
        info.cwd = wt.to_string_lossy().into_owned();
        d.entries.lock_or_recover().insert(
            "s1".into(),
            Entry {
                info,
                session: None,
                conversation: None,
                generation: 1,
                resume_attempt: None,
            },
        );
        let mut rx = d.events.subscribe();
        let write = |pct: f64| {
            std::fs::write(
                notes.join(USAGE_FILE),
                format!(r#"{{"context_window": {{"used_percentage": {pct}}}, "rate_limits": {{"five_hour": {{"used_percentage": 50, "resets_at": 9999999999}}}}}}"#),
            )
            .unwrap();
        };
        // 還沒有檔案：沒有用量
        d.poll_usage();
        assert!(d.get("s1").unwrap().usage.is_none());

        write(40.0);
        d.poll_usage();
        let u = d.get("s1").unwrap().usage.unwrap();
        assert_eq!(u.context_percent, Some(40.0));
        assert_eq!(u.five_hour.unwrap().used_percent, 50.0);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerMsg::SessionUpdated { .. })
        ));

        // 修改時間沒變：不重讀
        d.poll_usage();
        assert!(rx.try_recv().is_err());

        // 內容變了但顯示相同（40 → 40.2）：更新但不推播。修改時間的精度可能是秒，先改成不同的時間
        write(40.2);
        let f = std::fs::File::options()
            .write(true)
            .open(notes.join(USAGE_FILE))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        d.poll_usage();
        assert_eq!(
            d.get("s1").unwrap().usage.unwrap().context_percent,
            Some(40.2)
        );
        assert!(rx.try_recv().is_err());

        // 格式不對：保留上一次的值
        std::fs::write(notes.join(USAGE_FILE), "garbage").unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(notes.join(USAGE_FILE))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
            .unwrap();
        drop(f);
        d.poll_usage();
        assert_eq!(
            d.get("s1").unwrap().usage.unwrap().context_percent,
            Some(40.2)
        );

        // 分頁關閉後不再保留紀錄
        d.entries.lock_or_recover().clear();
        d.poll_usage();
        assert!(d.usage_seen.lock_or_recover().is_empty());
    }
}
